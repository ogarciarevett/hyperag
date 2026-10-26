use super::*;
use crate::{lexical_generation, lexical_snapshot::SourceDocument};
use chrono::{TimeZone, Utc};
use tantivy::{
    Index, IndexReader, TantivyDocument, Term,
    merge_policy::NoMergePolicy,
    query::{BooleanQuery, EnableScoring, Query, TermQuery},
    schema::{IndexRecordOption, Value},
};

#[derive(Clone)]
struct Fixture {
    id: String,
    scope: String,
    at: i64,
    from: Option<i64>,
    to: Option<i64>,
    terms: Vec<(String, i32)>,
}

fn build(rows: &[Fixture]) -> anyhow::Result<Searcher> {
    let schema = lexical_generation::schema();
    let index = Index::create_in_ram(schema.clone());
    let mut writer = index.writer_with_num_threads(1, 32 * 1024 * 1024)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for chunk in rows.chunks(512) {
        for row in chunk {
            writer.add_document(lexical_generation::document(
                SourceDocument {
                    id: row.id.clone(),
                    scope: row.scope.clone(),
                    version: 1,
                    recorded_at: Utc.timestamp_micros(row.at).unwrap(),
                    valid_from: row.from.map(|at| Utc.timestamp_micros(at).unwrap()),
                    valid_to: row.to.map(|at| Utc.timestamp_micros(at).unwrap()),
                    terms: sqlx::types::Json(row.terms.clone()),
                },
                &schema,
            )?)?;
        }
        writer.commit()?;
    }
    let reader: IndexReader = index.reader()?;
    Ok(reader.searcher())
}

fn reference(searcher: &Searcher, terms: &[String], limit: usize) -> anyhow::Result<Vec<Ranked>> {
    let field = searcher.schema().get_field("terms")?;
    let mut terms = terms.to_vec();
    terms.sort();
    terms.dedup();
    let query = BooleanQuery::union(
        terms
            .iter()
            .map(|text| {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, text),
                    IndexRecordOption::WithFreqs,
                )) as Box<dyn Query>
            })
            .collect(),
    );
    let weight = query.weight(EnableScoring::enabled_from_searcher(searcher))?;
    let schema = searcher.schema();
    let mut hits = Vec::new();
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        let mut scores = Vec::new();
        weight.for_each(segment, &mut |doc, score| scores.push((doc, score)))?;
        for (doc, score) in scores {
            let stored: TantivyDocument =
                searcher.doc(tantivy::DocAddress::new(ordinal as u32, doc))?;
            hits.push(Ranked {
                id: stored
                    .get_first(schema.get_field("id")?)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .into(),
                version: stored
                    .get_first(schema.get_field("version")?)
                    .unwrap()
                    .as_i64()
                    .unwrap(),
                at: stored
                    .get_first(schema.get_field("recorded_at")?)
                    .unwrap()
                    .as_i64()
                    .unwrap(),
                score,
            });
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.at.cmp(&a.at))
            .then(a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn fixture() -> Vec<Fixture> {
    let mut rows = Vec::new();
    for n in 0..1800 {
        let terms = if n < 40 {
            vec![
                ("alpha".into(), 3),
                ("beta".into(), 2),
                ("gamma".into(), 1),
                ("rare".into(), 1),
            ]
        } else {
            ["alpha", "beta", "gamma", "filler"]
                .into_iter()
                .enumerate()
                .filter(|(i, _)| (n + i) % 4 != 0)
                .map(|(i, term)| (term.into(), (1 + (n * (i + 1)) % 101) as i32))
                .collect()
        };
        rows.push(Fixture {
            id: format!("id-{:04}", 1800 - n),
            scope: if n >= 1400 {
                "hidden"
            } else if n % 7 == 0 {
                "second"
            } else {
                "allowed"
            }
            .into(),
            at: if n < 40 { (n % 3) as i64 } else { n as i64 },
            from: (n % 11 == 0).then_some(100),
            to: (n % 13 == 0).then_some(100),
            terms,
        });
    }
    rows
}

#[test]
fn masked_native_top_k_equals_an_exhaustive_authorized_index() -> anyhow::Result<()> {
    let rows = fixture();
    let shared = build(&rows)?;
    for scopes in [
        vec!["allowed".to_owned()],
        vec!["allowed".into(), "second".into()],
        vec!["missing".into()],
    ] {
        for as_of in [None, Some(99), Some(100)] {
            let allowed: Vec<_> = rows
                .iter()
                .filter(|row| {
                    scopes.contains(&row.scope)
                        && as_of.is_none_or(|at| {
                            row.from.is_none_or(|from| from <= at)
                                && row.to.is_none_or(|to| to > at)
                        })
                })
                .cloned()
                .collect();
            let authorized = build(&allowed)?;
            for query in [
                vec!["rare"],
                vec!["alpha"],
                vec!["alpha", "beta", "gamma"],
                vec!["rare", "alpha", "beta", "gamma"],
            ] {
                let terms: Vec<String> = query.into_iter().map(String::from).collect();
                let expected = reference(&authorized, &terms, 24)?;
                let actual = top(&shared, &scopes, as_of, &terms, 24)?;
                assert_eq!(
                    actual, expected,
                    "scopes={scopes:?},as_of={as_of:?},terms={terms:?}"
                );
                assert_eq!(
                    actual.iter().map(|h| h.score.to_bits()).collect::<Vec<_>>(),
                    expected
                        .iter()
                        .map(|h| h.score.to_bits())
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn scoped_average_cannot_prune_the_authorized_winner() -> anyhow::Result<()> {
    let mut rows = Vec::new();
    for n in 0..512 {
        let (scope, needle, length) = match n {
            0..128 => ("allowed", 5, 80),
            128 => ("allowed", 10, 100),
            129..256 => ("hidden", 1, 1),
            _ => ("hidden", 0, 1),
        };
        let mut terms = Vec::new();
        if needle > 0 {
            terms.push(("needle".into(), needle));
        }
        if length > needle {
            terms.push(("filler".into(), length - needle));
        }
        rows.push(Fixture {
            id: format!("bound-{n:03}"),
            scope: scope.into(),
            at: n as i64,
            from: None,
            to: None,
            terms,
        });
    }
    let shared = build(&rows)?;
    assert_eq!(shared.segment_readers().len(), 1);
    let allowed: Vec<_> = rows
        .iter()
        .filter(|row| row.scope == "allowed")
        .cloned()
        .collect();
    let authorized = build(&allowed)?;
    let terms = vec!["needle".into()];
    let expected = reference(&authorized, &terms, 1)?;
    let actual = top(&shared, &["allowed".into()], None, &terms, 1)?;
    eprintln!("scoped-bound candidate={actual:?}; authorized reference={expected:?}");
    assert_eq!(expected[0].id, "bound-128");
    assert_eq!(
        actual, expected,
        "scoped statistics must preserve safe native block bounds"
    );
    Ok(())
}
