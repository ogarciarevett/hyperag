//! Exhaustive masked scoring; callers must supply an exact-fenced immutable searcher.
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
};
use tantivy::{
    DocSet, Searcher, TERMINATED, TantivyDocument, Term,
    postings::Postings,
    query::Bm25Weight,
    schema::{IndexRecordOption, Schema, Value},
};

#[derive(serde::Serialize)]
pub(super) struct ExactHit {
    memory_id: String,
    version: i64,
    recorded_at: chrono::DateTime<chrono::Utc>,
    score: f64,
}

/// Score native postings in float8; retain cutoff ties for the database's ID collation.
pub(super) fn exact_top(
    searcher: &Searcher,
    scopes: &[String],
    as_of: Option<i64>,
    terms: &[String],
    limit: usize,
) -> anyhow::Result<Vec<ExactHit>> {
    if limit == 0 || terms.is_empty() || scopes.is_empty() {
        return Ok(Vec::new());
    }
    let schema = searcher.schema();
    let field = schema.get_field("terms")?;
    let length = schema.get_field("num_lexemes")?;
    let (mut n, mut total_len) = (0u64, 0u64);
    let mut masks = Vec::new();
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        let mut mask = vec![None; segment.max_doc() as usize];
        for doc in 0..segment.max_doc() {
            if segment.is_deleted(doc) {
                continue;
            }
            let stored: TantivyDocument =
                searcher.doc(tantivy::DocAddress::new(ordinal as u32, doc))?;
            let text = |name| {
                stored
                    .get_first(schema.get_field(name)?)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing text metadata: {name}"))
            };
            let from = integer(&stored, &schema, "valid_from")?;
            let to = integer(&stored, &schema, "valid_to")?;
            let scope = text("scope")?;
            if !scopes.iter().any(|allowed| allowed == scope)
                || as_of.is_some_and(|at| {
                    (from != i64::MIN && from > at) || (to != i64::MAX && to <= at)
                })
            {
                continue;
            }
            let dl = stored
                .get_first(length)
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing exact lexeme count"))?;
            n += 1;
            total_len = total_len
                .checked_add(dl)
                .ok_or_else(|| anyhow::anyhow!("lexeme count overflow"))?;
            let at = integer(&stored, &schema, "recorded_at")?;
            // Outside this range SQL float8 epochs can collapse distinct microseconds.
            anyhow::ensure!(at.unsigned_abs() <= (1u64 << 52), "inexact time ordering");
            mask[doc as usize] = Some((
                text("id")?.to_owned(),
                integer(&stored, &schema, "version")?,
                at,
                dl,
            ));
        }
        masks.push(mask);
    }
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut terms: Vec<_> = terms
        .iter()
        .map(|s| Term::from_field_text(field, s))
        .collect();
    terms.sort();
    terms.dedup();
    let average = (total_len as f64 / n as f64).max(1.0);
    let mut weights = Vec::new();
    for term in &terms {
        let mut df = 0u64;
        for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
            if let Some(mut postings) = segment
                .inverted_index(field)?
                .read_postings(term, IndexRecordOption::Basic)?
            {
                while postings.doc() != TERMINATED {
                    df += u64::from(masks[ordinal][postings.doc() as usize].is_some());
                    postings.advance();
                }
            }
        }
        let df = df as f64;
        weights.push((1.0 + (n as f64 - df + 0.5) / (df + 0.5)).ln());
    }
    let mut heap: BinaryHeap<std::cmp::Reverse<(u64, i64)>> = BinaryHeap::new();
    let mut hits = Vec::new();
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        let inverted = segment.inverted_index(field)?;
        let mut scores = vec![None; segment.max_doc() as usize];
        for (term, idf) in terms.iter().zip(&weights) {
            if let Some(mut postings) =
                inverted.read_postings(term, IndexRecordOption::WithFreqs)?
            {
                while postings.doc() != TERMINATED {
                    let doc = postings.doc() as usize;
                    if let Some((_, _, _, dl)) = &masks[ordinal][doc] {
                        let tf = f64::from(postings.term_freq());
                        *scores[doc].get_or_insert(0.0f64) += idf * (tf * (1.2 + 1.0))
                            / (tf + 1.2 * (1.0 - 0.75 + 0.75 * *dl as f64 / average));
                    }
                    postings.advance();
                }
            }
        }
        for (doc, score) in scores.into_iter().enumerate() {
            if let Some(score) = score {
                anyhow::ensure!(score.is_finite() && score >= 0.0, "invalid lexical score");
                let (id, version, at, _) = masks[ordinal][doc].take().unwrap();
                heap.push(std::cmp::Reverse((score.to_bits(), at)));
                if heap.len() > limit {
                    heap.pop();
                }
                hits.push(ExactHit {
                    memory_id: id,
                    version,
                    recorded_at: chrono::DateTime::from_timestamp_micros(at)
                        .ok_or_else(|| anyhow::anyhow!("invalid recorded time"))?,
                    score,
                });
            }
        }
    }
    if heap.len() == limit {
        let cutoff = heap.peek().unwrap().0;
        hits.retain(|h| (h.score.to_bits(), h.recorded_at.timestamp_micros()) >= cutoff);
    }
    Ok(hits)
}

#[derive(Clone, Debug)]
pub(super) struct Ranked {
    pub id: String,
    pub version: i64,
    pub at: i64,
    pub score: f32,
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ranked {}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then(other.at.cmp(&self.at))
            .then(self.id.cmp(&other.id))
            .then(self.version.cmp(&other.version))
    }
}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
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
    let (mut n, mut total_tokens) = (0u64, 0u64);
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
            n += 1;
            total_tokens = total_tokens
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
    if n == 0 || total_tokens == 0 {
        return Ok(Vec::new());
    }
    let field = schema.get_field("terms")?;
    let mut terms: Vec<_> = terms
        .iter()
        .map(|text| Term::from_field_text(field, text))
        .collect();
    terms.sort();
    terms.dedup();
    let mut frequencies = HashMap::new();
    for term in &terms {
        let mut df = 0;
        for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
            if let Some(mut postings) = segment
                .inverted_index(field)?
                .read_postings(term, IndexRecordOption::Basic)?
            {
                while postings.doc() != TERMINATED {
                    df += u64::from(masks[ordinal][postings.doc() as usize].is_some());
                    postings.advance();
                }
            }
        }
        frequencies.insert(term.clone(), df);
    }
    let average = total_tokens as f32 / n as f32;
    let weights: Vec<_> = terms
        .iter()
        .map(|term| Bm25Weight::for_one_term(frequencies[term], n, average))
        .collect();
    let mut heap = BinaryHeap::new();
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        let norms = segment.get_fieldnorms_reader(field)?;
        let inverted = segment.inverted_index(field)?;
        let mut scores = vec![None; segment.max_doc() as usize];
        // The reference adds native term scores in lexeme order using f32.
        for (term, weight) in terms.iter().zip(&weights) {
            if let Some(mut postings) =
                inverted.read_postings(term, IndexRecordOption::WithFreqs)?
            {
                while postings.doc() != TERMINATED {
                    let doc = postings.doc() as usize;
                    if masks[ordinal][doc].is_some() {
                        *scores[doc].get_or_insert(0.0f32) +=
                            weight.score(norms.fieldnorm_id(doc as u32), postings.term_freq());
                    }
                    postings.advance();
                }
            }
        }
        for (doc, score) in scores.into_iter().enumerate() {
            if let Some(score) = score {
                let mut hit = masks[ordinal][doc]
                    .take()
                    .expect("only authorized documents were scored");
                anyhow::ensure!(score.is_finite(), "nonfinite lexical score");
                hit.score = score;
                if heap.len() < limit {
                    heap.push(hit);
                } else if &hit < heap.peek().unwrap() {
                    heap.pop();
                    heap.push(hit);
                }
            }
        }
    }
    Ok(heap.into_sorted_vec())
}

#[cfg(test)]
#[path = "lexical_scorer_tests.rs"]
mod tests;
