//! Private native scoring; callers must supply an exact-fenced immutable searcher.
use tantivy::Searcher;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Ranked {
    pub id: String,
    pub version: i64,
    pub at: i64,
    pub score: f32,
}

pub(super) fn top(
    _searcher: &Searcher,
    _scopes: &[String],
    _as_of: Option<i64>,
    _terms: &[String],
    _limit: usize,
) -> anyhow::Result<Vec<Ranked>> {
    Ok(Vec::new())
}

#[cfg(test)]
#[path = "lexical_scorer_tests.rs"]
mod tests;
