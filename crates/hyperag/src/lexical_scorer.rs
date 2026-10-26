//! Private native scoring; callers must supply an exact-fenced immutable searcher.
use std::collections::HashMap;
use tantivy::{
    DocSet, Searcher, TERMINATED, TantivyDocument, Term,
    query::{Bm25StatisticsProvider, BooleanQuery, EnableScoring, Query, TermQuery},
    schema::{Field, IndexRecordOption, Schema, Value},
};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Ranked {
    pub id: String,
    pub version: i64,
    pub at: i64,
    pub score: f32,
}

struct Statistics {
    n: u64,
    tokens: u64,
    df: HashMap<Term, u64>,
}

impl Bm25StatisticsProvider for Statistics {
    fn total_num_tokens(&self, _: Field) -> tantivy::Result<u64> {
        Ok(self.tokens)
    }
    fn total_num_docs(&self) -> tantivy::Result<u64> {
        Ok(self.n)
    }
    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        Ok(self.df[term])
    }
}

fn integer(doc: &TantivyDocument, schema: &Schema, name: &str) -> anyhow::Result<i64> {
    doc.get_first(schema.get_field(name)?)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| anyhow::anyhow!("missing integer metadata: {name}"))
}

pub(super) fn top(
    searcher: &Searcher,
    scopes: &[String],
    as_of: Option<i64>,
    terms: &[String],
    limit: usize,
) -> anyhow::Result<Vec<Ranked>> {
    if limit == 0 || terms.is_empty() || scopes.is_empty() {
        return Ok(Vec::new());
    }
    let schema = searcher.schema();
    let mut stats = Statistics {
        n: 0,
        tokens: 0,
        df: HashMap::new(),
    };
    let mut masks = Vec::new();
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        let mut mask = vec![None; segment.max_doc() as usize];
        for doc in 0..segment.max_doc() {
            if segment.is_deleted(doc) {
                continue;
            }
            let stored: TantivyDocument =
                searcher.doc(tantivy::DocAddress::new(ordinal as u32, doc))?;
            let scope = stored
                .get_first(schema.get_field("scope")?)
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing scope metadata"))?;
            let from = integer(&stored, schema, "valid_from")?;
            let to = integer(&stored, schema, "valid_to")?;
            if !scopes.iter().any(|allowed| allowed == scope)
                || as_of.is_some_and(|at| {
                    (from != i64::MIN && from > at) || (to != i64::MAX && to <= at)
                })
            {
                continue;
            }
            let tokens = stored
                .get_first(schema.get_field("num_tokens")?)
                .and_then(|value| value.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing token count metadata"))?;
            stats.n += 1;
            stats.tokens = stats
                .tokens
                .checked_add(tokens)
                .ok_or_else(|| anyhow::anyhow!("token count overflow"))?;
            mask[doc as usize] = Some(Ranked {
                id: stored
                    .get_first(schema.get_field("id")?)
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing ID metadata"))?
                    .into(),
                version: integer(&stored, schema, "version")?,
                at: integer(&stored, schema, "recorded_at")?,
                score: 0.0,
            });
        }
        masks.push(mask);
    }
    if stats.n == 0 || stats.tokens == 0 {
        return Ok(Vec::new());
    }
    let field = schema.get_field("terms")?;
    let mut terms: Vec<_> = terms
        .iter()
        .map(|text| Term::from_field_text(field, text))
        .collect();
    terms.sort();
    terms.dedup();
    let mut present = vec![Vec::new(); masks.len()];
    for term in &terms {
        let mut df = 0;
        for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
            if let Some(mut postings) = segment
                .inverted_index(field)?
                .read_postings(term, IndexRecordOption::Basic)?
            {
                present[ordinal].push(term.clone());
                while postings.doc() != TERMINATED {
                    df += u64::from(masks[ordinal][postings.doc() as usize].is_some());
                    postings.advance();
                }
            }
        }
        stats.df.insert(term.clone(), df);
    }
    let mut hits: Vec<Ranked> = Vec::new();
    let mut threshold = f32::NEG_INFINITY;
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        let queries: Vec<Box<dyn Query>> = present[ordinal]
            .iter()
            .filter(|term| stats.df[*term] > 0)
            .map(|term| {
                Box::new(TermQuery::new(term.clone(), IndexRecordOption::WithFreqs))
                    as Box<dyn Query>
            })
            .collect();
        let query: Box<dyn Query> = match queries.len() {
            0 => continue,
            1 => queries.into_iter().next().unwrap(),
            _ => Box::new(BooleanQuery::union(queries)),
        };
        let weight = query.weight(EnableScoring::enabled_from_statistics_provider(
            &stats, searcher,
        ))?;
        weight.for_each_pruning(threshold, segment, &mut |doc, score| {
            if let Some(metadata) = &masks[ordinal][doc as usize] {
                let mut hit = metadata.clone();
                hit.score = score;
                hits.push(hit);
                hits.sort_by(|a, b| {
                    b.score
                        .total_cmp(&a.score)
                        .then(b.at.cmp(&a.at))
                        .then(a.id.cmp(&b.id))
                });
                hits.truncate(limit);
                if hits.len() == limit {
                    // Native pruning uses strict >; equal scores still need time/ID ordering.
                    threshold = hits.last().unwrap().score.next_down();
                }
            }
            threshold
        })?;
    }
    Ok(hits)
}

#[cfg(test)]
#[path = "lexical_scorer_tests.rs"]
mod tests;
