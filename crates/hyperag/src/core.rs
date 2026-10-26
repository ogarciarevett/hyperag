//! THE HYPERAG CORE, a port of `src/core.ts`. Every call names the acting principal. In-process the
//! principal is trusted as given; the HTTP service and the MCP server are the trust boundary that
//! verify it. Scopes a principal may read or write always come from the grants table.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::graph::Graph;
use crate::models::{Embedder, Reranker};
use crate::store::{CreateInput, History, Store, VectorRow, VersionInput, identity_hash, parse_time, space_id, version_hash};
use crate::temporal;
use crate::types::{Error, Hit, Memory, MemoryUpdate, NewMemory, Relation, Result, WriteResult, not_found};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channels {
    pub bm25: Option<bool>,
    pub vector: Option<bool>,
    pub graph: Option<bool>,
    pub rerank: Option<bool>,
    /// Off unless asked for: the temporal channel (`docs/temporal.md`) is not yet measured.
    pub temporal: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrieveOptions {
    pub limit: Option<usize>,
    pub candidates_per_channel: Option<usize>,
    /// Cross-encode this many fused candidates; overrides the environment default.
    pub rerank_candidates: Option<usize>,
    /// Only facts valid at this instant (valid time).
    pub as_of: Option<String>,
    /// The date the question is asked, for the temporal channel only: "last week" in the question
    /// resolves against it. It filters nothing. Unset, `as_of`, else today.
    pub anchor: Option<String>,
    /// Turn single channels off to evaluate components one at a time.
    pub channels: Option<Channels>,
    pub max_tokens: Option<usize>,
    /// Opt-in stage wall times; no payload logging.
    pub profile: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievedItem {
    pub memory: Memory,
    pub score: f64,
    /// Rank (1-based) of this memory in each channel that found it.
    pub channels: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
    /// With the temporal channel: when the memory was said and the dates it means.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dates: Option<String>,
}


/// Cross-encode a bounded prefix after fusion; channel recall and the loaded pool stay unchanged.
fn rerank_input(items: &[RetrievedItem], requested: Option<usize>, configured: Option<&str>) -> Result<Vec<String>> {
    let count = match requested {
        Some(n) if (1..=150).contains(&n) => n,
        Some(_) => return Err(Error::InvalidInput("rerankCandidates must be an integer from 1 to 150".into())),
        None => match configured {
            None => items.len(),
            Some(value) => value.parse::<usize>().ok().filter(|n| (1..=150).contains(n))
                .ok_or_else(|| Error::InvalidInput("HYPERAG_RERANK_CANDIDATES must be an integer from 1 to 150".into()))?,
        },
    };
    Ok(items.iter().take(count).map(|i| i.memory.text.clone()).collect())
}

fn apply_rerank_scores(items: &mut [RetrievedItem], scores: &[f32]) {
    let count = items.len().min(scores.len());
    for (item, score) in items[..count].iter_mut().zip(scores) { item.rerank_score = Some(*score); }
    // Unscored rows must retain fused order, including when all scored logits are negative.
    items[..count].sort_by(|a, b| b.rerank_score.unwrap_or(0.0).total_cmp(&a.rerank_score.unwrap_or(0.0)));
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Retrieval {
    pub items: Vec<RetrievedItem>,
    /// True when no item clears the evidence bar: the caller should say it does not know.
    pub insufficient_evidence: bool,
    pub paths_used: Vec<String>,
    pub limitations: Vec<String>,
    pub embedding_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings_ms: Option<StageTimings>,
}


/// Wall time in the serial retrieval stages, including dependency/model queue waits.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageTimings {
    pub embedding: f64,
    pub lexical: f64,
    pub vector: f64,
    pub graph: f64,
    pub fusion: f64,
    pub rerank: f64,
    pub payload_load: f64,
    pub other: f64,
    pub total: f64,
}
impl StageTimings {
    fn finish(&mut self, started: Instant) {
        self.total = started.elapsed().as_secs_f64() * 1000.0;
        self.other = (self.total - self.embedding - self.lexical - self.vector - self.graph
            - self.fusion - self.rerank - self.payload_load).max(0.0);
    }
}
enum Stage { Embedding, Lexical, Vector, Graph, Fusion, Rerank, PayloadLoad }
fn record(timings: &mut Option<StageTimings>, stage: Stage, started: Option<Instant>) {
    let (Some(t), Some(started)) = (timings, started) else { return };
    let slot = match stage {
        Stage::Embedding => &mut t.embedding, Stage::Lexical => &mut t.lexical,
        Stage::Vector => &mut t.vector, Stage::Graph => &mut t.graph,
        Stage::Fusion => &mut t.fusion, Stage::Rerank => &mut t.rerank,
        Stage::PayloadLoad => &mut t.payload_load,
    };
    *slot += started.elapsed().as_secs_f64() * 1000.0;
}
async fn timed<T>(timings: &mut Option<StageTimings>, stage: Stage, operation: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    let started = timings.as_ref().map(|_| Instant::now());
    let result = operation.await;
    record(timings, stage, started);
    result
}
fn measured<T>(timings: &mut Option<StageTimings>, stage: Stage, operation: impl FnOnce() -> T) -> T {
    let started = timings.as_ref().map(|_| Instant::now());
    let result = operation();
    record(timings, stage, started);
    result
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub n: usize,
    pub memory_id: String,
    pub version: i32,
    pub source: String,
    pub author: String,
    pub scope: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub text: String,
    pub citations: Vec<Citation>,
    pub tokens: usize,
    /// Retrieved items that did not fit the budget.
    pub dropped: usize,
    pub retrieval: Retrieval,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub scope: String,
    /// One line per memory, newest first, within the byte budget.
    pub text: String,
    pub items: Vec<Memory>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Explained {
    #[serde(flatten)]
    pub history: History,
    /// `null` when the graph could not be read.
    pub relations: Option<Vec<Relation>>,
}

pub struct Models {
    pub embedder: Option<Arc<Embedder>>,
    pub reranker: Option<Arc<Reranker>>,
    pub space: Option<String>,
}

pub struct Config {
    pub store: Store,
    pub graph: Option<Graph>,
    pub embedder: Option<Embedder>,
    pub reranker: Option<Reranker>,
    /// Principals allowed to grant, revoke and switch models.
    pub admins: Vec<String>,
    pub rrf_k: f64,
    /// Least cross-encoder score for evidence; below it the answer is `insufficient_evidence`.
    pub min_rerank_score: f32,
    /// Least best cosine similarity for evidence when no re-ranker runs.
    pub min_vector_score: f64,
}

impl Config {
    pub fn new(store: Store) -> Self {
        Config { store, graph: None, embedder: None, reranker: None, admins: vec![], rrf_k: 60.0, min_rerank_score: 0.0, min_vector_score: 0.82 }
    }
}

pub struct Hyperag {
    pub store: Store,
    pub graph: Option<Graph>,
    models: RwLock<Models>,
    admins: Vec<String>,
    rrf_k: f64,
    min_rerank_score: f32,
    min_vector_score: f64,
}

/// RECIPROCAL RANK FUSION, from the original agent memory: each list adds 1 / (k + rank), so an item
/// two channels rank beats an item one channel ranks a little higher. Ties keep the newer item first.
pub fn fuse(lists: &[&Vec<Hit>], k: f64) -> Vec<Hit> {
    let mut order: Vec<String> = vec![];
    let mut scores: HashMap<String, Hit> = HashMap::new();
    for list in lists {
        for (index, hit) in list.iter().enumerate() {
            let entry = scores.entry(hit.id.clone()).or_insert_with(|| {
                order.push(hit.id.clone());
                Hit { id: hit.id.clone(), score: 0.0, at: 0.0 }
            });
            entry.score += 1.0 / (k + index as f64 + 1.0);
            entry.at = entry.at.max(hit.at);
        }
    }
    let mut fused: Vec<Hit> = order.into_iter().filter_map(|id| scores.remove(&id)).collect();
    fused.sort_by(|a, b| b.score.total_cmp(&a.score).then(b.at.total_cmp(&a.at)).then(a.id.cmp(&b.id)));
    fused
}

// shortcut: tokens are estimated as ceil(chars / 4), not counted by the generator's tokenizer;
// pass a real tokenizer here when a generator with a hard context limit is wired in.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Packs passages, best first, into `max_tokens`; a passage that does not fit whole is left out.
pub fn build_context(retrieval: Retrieval, max_tokens: usize) -> Context {
    if retrieval.insufficient_evidence {
        let note = "No sufficient evidence was found for this question.".to_owned();
        let dropped = retrieval.items.len();
        return Context { tokens: estimate_tokens(&note), text: note, citations: vec![], dropped, retrieval };
    }
    let (mut parts, mut citations, mut tokens) = (vec![], vec![], 0);
    for item in &retrieval.items {
        let n = citations.len() + 1;
        let m = &item.memory;
        let mut part = format!("[{n}] ({}, {}, v{}) {}", m.source, m.author, m.version, m.text);
        if let Some(d) = &item.dates {
            part.push_str(&format!(" [{d}]"));
        }
        let cost = estimate_tokens(&part) + 1;
        if tokens + cost > max_tokens {
            continue;
        }
        tokens += cost;
        parts.push(part);
        citations.push(Citation { n, memory_id: m.id.clone(), version: m.version, source: m.source.clone(), author: m.author.clone(), scope: m.scope.clone() });
    }
    let dropped = retrieval.items.len() - citations.len();
    Context { text: parts.join("\n"), citations, tokens, dropped, retrieval }
}

fn refs<'a>(ls: &'a [(&str, Vec<Hit>)]) -> Vec<&'a Vec<Hit>> {
    ls.iter().map(|(_, l)| l).collect()
}

fn valid_dates(from: &Option<String>, to: &Option<String>) -> Result<()> {
    for t in [from, to].into_iter().flatten() {
        if parse_time(t).is_none() {
            return Err(Error::InvalidInput(format!("not a date: {t}")));
        }
    }
    Ok(())
}

fn valid_new(input: &NewMemory) -> Result<()> {
    if input.text.trim().is_empty() {
        return Err(Error::InvalidInput("text is empty".into()));
    }
    if input.id.as_deref().is_some_and(|id| id.is_empty() || id.contains('/')) {
        return Err(Error::InvalidInput("an explicit id is non-empty and has no '/'".into()));
    }
    valid_dates(&input.valid_from, &input.valid_to)
}

fn now() -> String {
    crate::store::iso_ms(chrono::Utc::now())
}

impl Hyperag {
    /// Applies the schema, registers the embedding space and the graph constraint. Idempotent.
    pub async fn init(config: Config) -> Result<Hyperag> {
        config.store.migrate().await?;
        let space = match &config.embedder {
            Some(e) => Some(config.store.ensure_space(e.describe()).await?),
            None => None,
        };
        // Catch up memories written while another process still served an older model (for example
        // after `hyperag models set` from the CLI): one cheap query when nothing is missing.
        if let (Some(e), Some(space)) = (&config.embedder, &space) {
            loop {
                let rows = config.store.missing_vectors(space, 64).await?;
                if rows.is_empty() {
                    break;
                }
                let texts: Vec<String> = rows.iter().map(|r| r.2.clone()).collect();
                let vectors = e.embed_documents(&texts).await?;
                let put: Vec<(String, i32, Vec<f32>)> = rows.into_iter().zip(vectors).map(|((id, v, _), vec)| (id, v, vec)).collect();
                config.store.put_vectors(space, &put).await?;
            }
        }
        if let Some(g) = &config.graph {
            g.ensure_schema().await?;
        }
        Ok(Hyperag {
            store: config.store,
            graph: config.graph,
            models: RwLock::new(Models { embedder: config.embedder.map(Arc::new), reranker: config.reranker.map(Arc::new), space }),
            admins: config.admins,
            rrf_k: config.rrf_k,
            min_rerank_score: config.min_rerank_score,
            min_vector_score: config.min_vector_score,
        })
    }

    pub fn must_admin(&self, actor: &str) -> Result<()> {
        if self.admins.iter().any(|a| a == actor) {
            Ok(())
        } else {
            Err(Error::Forbidden("only an admin manages grants and models".into()))
        }
    }

    pub async fn grant(&self, admin: &str, principal: &str, scope: &str, permission: &str) -> Result<()> {
        self.must_admin(admin)?;
        self.store.grant(principal, scope, permission, admin).await
    }

    pub async fn revoke(&self, admin: &str, principal: &str, scope: &str, permission: &str) -> Result<()> {
        self.must_admin(admin)?;
        self.store.revoke(principal, scope, permission, admin).await
    }

    async fn must_write(&self, principal: &str, scope: &str) -> Result<()> {
        if self.store.scopes(principal, "write").await?.iter().any(|s| s == scope) {
            Ok(())
        } else {
            Err(Error::Forbidden(format!("no write access to scope {scope}")))
        }
    }

    /// Missing and unreadable are the same `not_found`, so a write never confirms another scope's id.
    async fn readable_scope_of(&self, principal: &str, id: &str) -> Result<String> {
        let scope = self.store.scope_of(&[id.to_owned()]).await?.remove(id);
        match scope {
            Some(s) if self.store.scopes(principal, "read").await?.contains(&s) => Ok(s),
            _ => Err(not_found(id)),
        }
    }

    async fn writable_scope_of(&self, principal: &str, id: &str) -> Result<String> {
        let scope = self.readable_scope_of(principal, id).await?;
        self.must_write(principal, &scope).await?;
        Ok(scope)
    }

    /// Callers hold the models read lock across embedding and the write, so a model switch (which
    /// takes the write lock for its last catch-up pass) never misses a memory written meanwhile.
    async fn vectors_of(models: &Models, texts: &[String]) -> Result<Option<Vec<VectorRow>>> {
        let (Some(e), Some(space)) = (&models.embedder, &models.space) else { return Ok(None) };
        if &space_id(e.describe()) != space {
            return Err(Error::EmbeddingSpaceMismatch("embedder changed after init".into()));
        }
        let vectors = e.embed_documents(texts).await?;
        Ok(Some(vectors.into_iter().map(|values| VectorRow { space: space.clone(), values }).collect()))
    }

    pub async fn ingest(&self, principal: &str, input: NewMemory) -> Result<WriteResult> {
        valid_new(&input)?;
        self.must_write(principal, &input.scope).await?;
        let models = self.models.read().await;
        let vector = Self::vectors_of(&models, std::slice::from_ref(&input.text)).await?.and_then(|mut v| v.pop());
        self.ingest_with(principal, input, vector).await
    }

    /// Many memories with one embedding call per batch; same checks and result as `ingest` each.
    pub async fn ingest_many(&self, principal: &str, inputs: Vec<NewMemory>, batch: usize) -> Result<Vec<WriteResult>> {
        for i in &inputs {
            valid_new(i)?;
        }
        let mut scopes: Vec<&String> = inputs.iter().map(|i| &i.scope).collect();
        scopes.sort();
        scopes.dedup();
        for s in scopes {
            self.must_write(principal, s).await?;
        }
        let mut results = Vec::with_capacity(inputs.len());
        let models = self.models.read().await;
        let mut inputs = inputs.into_iter().peekable();
        while inputs.peek().is_some() {
            let chunk: Vec<NewMemory> = inputs.by_ref().take(batch.max(1)).collect();
            let texts: Vec<String> = chunk.iter().map(|c| c.text.clone()).collect();
            let mut vectors = Self::vectors_of(&models, &texts).await?.map(|v| v.into_iter());
            for input in chunk {
                let vector = vectors.as_mut().and_then(|v| v.next());
                results.push(self.ingest_with(principal, input, vector).await?);
            }
        }
        Ok(results)
    }

    async fn ingest_with(&self, principal: &str, input: NewMemory, vector: Option<VectorRow>) -> Result<WriteResult> {
        let mut old_scopes = vec![];
        for old in &input.supersedes {
            old_scopes.push(self.writable_scope_of(principal, old).await?);
        }
        // An explicit id is namespaced by its scope, so a colliding id never reveals another scope's memory.
        let id = match &input.id {
            None => format!("m_{}", identity_hash(&input.scope, &input.source, &input.text)),
            Some(given) => format!("{}/{given}", input.scope),
        };
        let version = VersionInput {
            text: input.text.clone(),
            source: input.source.clone(),
            valid_from: input.valid_from.clone(),
            valid_to: input.valid_to.clone(),
            metadata: input.metadata.clone(),
        };
        let hash = version_hash(&input.scope, &version);
        let kind = input.kind.clone().unwrap_or_else(|| "document".into());
        let create = CreateInput { id: &id, scope: &input.scope, kind: &kind, supersedes: &input.supersedes, idempotency_key: &input.idempotency_key, version };
        let result = self.store.create(principal, &create, &hash, vector.as_ref()).await?;
        // Graph writes are MERGEs, so they also run on a replay: a retry repairs a graph write that
        // failed. A replay of a memory deleted since then writes nothing.
        let live = result.created || self.store.scope_of(std::slice::from_ref(&result.id)).await?.contains_key(&result.id);
        if let (Some(g), true) = (&self.graph, live) {
            g.upsert_memory(&result.id, &input.scope).await?;
            for (old, old_scope) in input.supersedes.iter().zip(&old_scopes) {
                let rel = Relation { from: result.id.clone(), to: old.clone(), kind: "SUPERSEDES".into(), created_by: principal.into(), created_at: now() };
                g.relate(&rel, &input.scope, old_scope).await?;
            }
        }
        Ok(result)
    }

    pub async fn update(&self, principal: &str, id: &str, input: MemoryUpdate) -> Result<WriteResult> {
        valid_dates(&input.valid_from, &input.valid_to)?;
        let scope = self.writable_scope_of(principal, id).await?;
        let models = self.models.read().await;
        let vector = Self::vectors_of(&models, std::slice::from_ref(&input.text)).await?.and_then(|mut v| v.pop());
        let version = VersionInput { text: input.text, source: input.source, valid_from: input.valid_from, valid_to: input.valid_to, metadata: input.metadata };
        let hash = version_hash(&scope, &version);
        self.store.update(principal, id, &version, input.expected_version, &input.idempotency_key, &hash, vector.as_ref()).await
    }

    /// The fact stops ranking and its validity ends now; its history stays explainable.
    pub async fn invalidate(&self, principal: &str, id: &str, reason: &str) -> Result<()> {
        self.writable_scope_of(principal, id).await?;
        self.store.invalidate(id, reason, principal).await
    }

    /// Text, vectors and graph node go; a content-free tombstone stays. Deleting an already deleted
    /// memory repeats only the graph step, so a retry finishes a graph delete that failed.
    pub async fn delete(&self, principal: &str, id: &str, reason: &str) -> Result<()> {
        if let Some(tomb) = self.store.history(id).await? {
            if tomb.deleted.is_some() && self.store.scopes(principal, "write").await?.contains(&tomb.scope) {
                if let Some(g) = &self.graph {
                    g.delete_memory(id).await?;
                }
                return Ok(());
            }
        }
        self.writable_scope_of(principal, id).await?;
        self.store.delete(id, reason, principal).await?;
        if let Some(g) = &self.graph {
            g.delete_memory(id).await?;
        }
        Ok(())
    }

    /// A typed edge between two memories: write on the source's scope, read on the target's.
    pub async fn relate(&self, principal: &str, from: &str, to: &str, kind: &str) -> Result<()> {
        let g = self.graph.as_ref().ok_or_else(|| Error::InvalidInput("no graph store configured".into()))?;
        let valid = kind.len() <= 64
            && kind.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && kind.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return Err(Error::InvalidInput("relation type must be UPPER_SNAKE_CASE".into()));
        }
        let from_scope = self.writable_scope_of(principal, from).await?;
        let to_scope = self.readable_scope_of(principal, to).await?;
        let rel = Relation { from: from.into(), to: to.into(), kind: kind.into(), created_by: principal.into(), created_at: now() };
        g.relate(&rel, &from_scope, &to_scope).await
    }

    /// Edges whose both ends are readable and not deleted in Postgres, whatever the graph still holds.
    pub async fn relations(&self, principal: &str, id: &str) -> Result<Vec<Relation>> {
        let g = self.graph.as_ref().ok_or_else(|| Error::InvalidInput("no graph store configured".into()))?;
        let readable = self.store.scopes(principal, "read").await?;
        let rels = g.relations(id, &readable).await?;
        let ends: Vec<String> = rels.iter().flat_map(|r| [r.from.clone(), r.to.clone()]).collect();
        let live = self.store.scope_of(&ends).await?;
        let ok = |id: &str| live.get(id).is_some_and(|s| readable.contains(s));
        Ok(rels.into_iter().filter(|r| ok(&r.from) && ok(&r.to)).collect())
    }

    pub async fn retrieve(&self, principal: &str, query: &str, options: &RetrieveOptions) -> Result<Retrieval> {
        let mut timings = options.profile.unwrap_or(false).then(StageTimings::default);
        let started = timings.as_ref().map(|_| Instant::now());
        let limit = options.limit.unwrap_or(8);
        let candidates = options.candidates_per_channel.unwrap_or(24) as i64;
        let as_of = match &options.as_of {
            None => None,
            Some(t) => Some(parse_time(t).ok_or_else(|| Error::InvalidInput(format!("not a date: {t}")))?),
        };
        let ch = options.channels.clone().unwrap_or_default();
        let on = |c: Option<bool>| c.unwrap_or(true);
        let mut lexical = if on(ch.bm25) && crate::store::LexicalScorer::selected()? == crate::store::LexicalScorer::Tantivy {
            Some(timed(&mut timings, Stage::Lexical, crate::lexical_read::Read::open(&self.store.pool, principal, query, as_of)).await?)
        } else { None };
        let readable = match &lexical { Some(read) => read.scopes.clone(), None => self.store.scopes(principal, "read").await? };
        let (mut limitations, mut paths) = (vec![], vec![]);
        let mut lists: Vec<(&'static str, Vec<Hit>)> = vec![];
        let models = self.models.read().await;

        if on(ch.bm25) {
            let hits = match lexical.as_mut() {
                Some(read) => timed(&mut timings, Stage::Lexical, read.search(&self.store.lexical_reader, candidates)).await?,
                None => timed(&mut timings, Stage::Lexical, self.store.bm25(&readable, query, candidates, as_of)).await?,
            };
            lists.push(("bm25", hits));
            paths.push("bm25".to_owned());
        }
        if on(ch.vector) {
            match (&models.embedder, &models.space) {
                (Some(e), Some(space)) => match timed(&mut timings, Stage::Embedding, e.embed_query(query)).await {
                    Ok(q) => match timed(&mut timings, Stage::Vector, self.store.vector(space, &readable, &q, candidates, as_of)).await {
                        Ok(hits) => {
                            lists.push(("vector", hits));
                            paths.push("vector".to_owned());
                        }
                        Err(err) => limitations.push(format!("vector channel failed: {err}")),
                    },
                    Err(err) => limitations.push(format!("vector channel failed: {err}")),
                },
                _ => limitations.push("no embedder configured: lexical retrieval only".into()),
            }
        }
        let seeds: Vec<String> = measured(&mut timings, Stage::Fusion, || fuse(&refs(&lists), self.rrf_k)).into_iter().take(8).map(|h| h.id).collect();
        let mut via: HashMap<String, Vec<String>> = HashMap::new();
        if on(ch.graph) {
            match &self.graph {
                Some(g) => match timed(&mut timings, Stage::Graph, g.expand(&seeds, &readable, 2, candidates)).await {
                    Ok(expanded) => {
                        let hits = expanded.iter().map(|e| Hit { id: e.id.clone(), score: e.paths, at: 0.0 }).collect();
                        for e in expanded {
                            via.insert(e.id, e.via);
                        }
                        lists.push(("graph", hits));
                        paths.push("graph".to_owned());
                    }
                    Err(err) => limitations.push(format!("graph channel failed: {err}")),
                },
                None => limitations.push("no graph configured: no relationship expansion".into()),
            }
        }

        if ch.temporal.unwrap_or(false) {
            let anchor = match &options.anchor {
                Some(t) => parse_time(t).ok_or_else(|| Error::InvalidInput(format!("not a date: {t}")))?,
                None => as_of.unwrap_or_else(chrono::Utc::now),
            }
            .date_naive();
            let anchor = temporal::plain(anchor).ok_or_else(|| Error::InvalidInput("anchor year must be from 1 to 9999".into()))?;
            let asked = temporal::spans(query, anchor);
            if !asked.is_empty() || temporal::intent(query).is_some() {
                // shortcut: ranks only the fused candidates, not the whole scope; store spans at ingest if dev shows misses.
                let order = measured(&mut timings, Stage::Fusion, || fuse(&refs(&lists), self.rrf_k));
                let ids: Vec<String> = order.iter().take(candidates as usize).map(|h| h.id.clone()).collect();
                let loaded = match lexical.as_mut() {
                    Some(read) => timed(&mut timings, Stage::PayloadLoad, read.load(&ids)).await?,
                    None => timed(&mut timings, Stage::PayloadLoad, self.store.load(&ids, &readable, as_of)).await?,
                };
                let keep: HashMap<&str, bool> = loaded
                    .iter()
                    .map(|m| {
                        let (_, said) = temporal::mentioned(m);
                        // A question with a time: memories that talk about that time. Without one: memories that talk about any time.
                        let hit = if asked.is_empty() { !said.is_empty() } else { said.iter().any(|s| asked.iter().any(|q| temporal::overlaps(s, q))) };
                        (m.id.as_str(), hit)
                    })
                    .collect();
                let hits: Vec<Hit> = order.into_iter().filter(|h| keep.get(h.id.as_str()).copied().unwrap_or(false)).collect();
                lists.push(("temporal", hits));
                paths.push("temporal".to_owned());
            }
        }

        let fused = measured(&mut timings, Stage::Fusion, || fuse(&refs(&lists), self.rrf_k));
        let rerank = on(ch.rerank) && models.reranker.is_some();
        let pool = if rerank { limit.max(candidates as usize) } else { limit };
        // Graph hits and stale candidates are re-checked against the store with the same filters.
        let ids: Vec<String> = fused.iter().take(pool).map(|h| h.id.clone()).collect();
        let memories = match lexical.as_mut() {
            Some(read) => timed(&mut timings, Stage::PayloadLoad, read.load(&ids)).await?,
            None => timed(&mut timings, Stage::PayloadLoad, self.store.load(&ids, &readable, as_of)).await?,
        };
        if let Some(read) = lexical { read.close().await?; }
        let fused_score: HashMap<&str, f64> = fused.iter().map(|h| (h.id.as_str(), h.score)).collect();
        let mut items: Vec<RetrievedItem> = memories
            .into_iter()
            .map(|memory| {
                let channels = lists
                    .iter()
                    .filter_map(|(name, l)| l.iter().position(|h| h.id == memory.id).map(|at| (name.to_string(), at + 1)))
                    .collect();
                RetrievedItem {
                    score: fused_score.get(memory.id.as_str()).copied().unwrap_or(0.0),
                    via: via.get(&memory.id).cloned(),
                    channels,
                    rerank_score: None,
                    dates: None,
                    memory,
                }
            })
            .collect();

        let mut reranked = false;
        if let (true, Some(r)) = (rerank && !items.is_empty(), &models.reranker) {
            let cap = std::env::var("HYPERAG_RERANK_CANDIDATES").ok();
            let docs = rerank_input(&items, options.rerank_candidates, cap.as_deref())?;
            match timed(&mut timings, Stage::Rerank, r.rerank(query, &docs)).await {
                Ok(scores) => {
                    apply_rerank_scores(&mut items, &scores);
                    reranked = true;
                    paths.push(format!("rerank:{}", r.name));
                }
                Err(err) => limitations.push(format!("re-ranker failed, fused order kept: {err}")),
            }
        }
        items.truncate(limit);
        if paths.iter().any(|p| p == "temporal") {
            for item in &mut items {
                item.dates = temporal::describe(&item.memory);
            }
        }

        let insufficient = !self.has_evidence(&items, &lists, reranked);
        if insufficient {
            limitations.push("insufficient evidence: no result clears the evidence threshold".into());
        }
        if readable.is_empty() {
            limitations.push("principal has no readable scope".into());
        }
        if let (Some(t), Some(started)) = (timings.as_mut(), started) { t.finish(started); }
        Ok(Retrieval { timings_ms: timings, items, insufficient_evidence: insufficient, paths_used: paths, limitations, embedding_space: models.space.clone() })
    }

    fn has_evidence(&self, items: &[RetrievedItem], lists: &[(&str, Vec<Hit>)], reranked: bool) -> bool {
        let Some(top) = items.first() else { return false };
        if reranked {
            return top.rerank_score.unwrap_or(f32::NEG_INFINITY) >= self.min_rerank_score;
        }
        // Lexical-only retrieval cannot abstain: any shared word is a match.
        let Some((_, vector)) = lists.iter().find(|(n, _)| *n == "vector") else { return true };
        vector.iter().map(|h| h.score).fold(0.0, f64::max) >= self.min_vector_score
    }

    /// Retrieval packed into a token budget, each passage numbered and cited.
    pub async fn context(&self, principal: &str, query: &str, options: &RetrieveOptions) -> Result<Context> {
        let retrieval = self.retrieve(principal, query, options).await?;
        Ok(build_context(retrieval, options.max_tokens.unwrap_or(1_000)))
    }

    /// The newest memories of one readable scope, newest first, packed into `max_bytes` of text:
    /// what an agent sees when a session starts. An unreadable scope is `not_found`.
    pub async fn recent(&self, principal: &str, scope: &str, limit: usize, max_bytes: usize) -> Result<Recent> {
        if !self.store.scopes(principal, "read").await?.iter().any(|s| s == scope) {
            return Err(Error::NotFound(format!("scope {scope} not found")));
        }
        let memories = self.store.recent(scope, limit.clamp(1, 200) as i64).await?;
        let (mut text, mut items) = (String::new(), vec![]);
        for m in memories {
            let line = format!("- [{}] ({}, {}, {}) {}\n", m.id, m.kind, m.source, &m.recorded_at[..m.recorded_at.len().min(10)], m.text.replace('\n', " "));
            // A memory too large for what is left is skipped, so it does not hide older ones.
            if text.len() + line.len() > max_bytes {
                continue;
            }
            text.push_str(&line);
            items.push(m);
        }
        Ok(Recent { scope: scope.to_owned(), text, items })
    }

    /// Where a memory came from: every version, who wrote it, what corrected it, its relations.
    pub async fn explain(&self, principal: &str, id: &str) -> Result<Explained> {
        let readable = self.store.scopes(principal, "read").await?;
        let mut history = match self.store.history(id).await? {
            Some(h) if readable.contains(&h.scope) => h,
            _ => return Err(not_found(id)),
        };
        let relations = match (&self.graph, &history.deleted) {
            (Some(_), None) => self.relations(principal, id).await.ok(),
            _ => Some(vec![]),
        };
        // Ids of memories the reader cannot read are hidden, so explain is no cross-scope oracle.
        let linked_ids: Vec<String> = history.corrects.iter().cloned().chain(history.versions.iter().filter_map(|v| v.superseded_by.clone())).collect();
        let linked = self.store.scope_of(&linked_ids).await?;
        let visible = |other: &str| linked.get(other).is_some_and(|s| readable.contains(s));
        history.corrects.retain(|c| visible(c));
        for v in &mut history.versions {
            if v.superseded_by.as_deref().is_some_and(|s| !visible(s)) {
                v.superseded_by = Some("[not readable]".into());
            }
        }
        Ok(Explained { history, relations })
    }

    /// Embeds every current memory into `embedder`'s space, in its own vector table.
    pub async fn reindex(&self, embedder: &Embedder, batch: i64) -> Result<(String, usize)> {
        let space = self.store.ensure_space(embedder.describe()).await?;
        let mut embedded = 0;
        loop {
            let rows = self.store.missing_vectors(&space, batch).await?;
            if rows.is_empty() {
                return Ok((space, embedded));
            }
            let texts: Vec<String> = rows.iter().map(|r| r.2.clone()).collect();
            let vectors = embedder.embed_documents(&texts).await?;
            let put: Vec<(String, i32, Vec<f32>)> = rows.into_iter().zip(vectors).map(|((id, v, _), vec)| (id, v, vec)).collect();
            embedded += put.len();
            self.store.put_vectors(&space, &put).await?;
        }
    }

    /// Embeds what the active embedder's space is missing (after an import); BM25 only: nothing.
    pub async fn reindex_active(&self) -> Result<Option<(String, usize)>> {
        let embedder = self.models.read().await.embedder.clone();
        match embedder {
            Some(e) => Ok(Some(self.reindex(&e, 64).await?)),
            None => Ok(None),
        }
    }

    /// Switches the live models. A new embedding gets its own index, filled before the switch, so
    /// queries never mix spaces; documents, BM25 and the graph stay. The choice is persisted.
    pub async fn set_models(&self, admin: &str, embedding: Option<Embedder>, reranker: Option<Option<Reranker>>) -> Result<()> {
        self.must_admin(admin)?;
        if let Some(e) = embedding {
            // Most of the reindex runs while writes continue; the write lock then stops new writes
            // for one last pass over what arrived meanwhile, before the switch.
            self.reindex(&e, 64).await?;
            let mut models = self.models.write().await;
            let (space, _) = self.reindex(&e, 64).await?;
            self.store.set_setting("embedding", &e.spec).await?;
            models.embedder = Some(Arc::new(e));
            models.space = Some(space);
        }
        if let Some(r) = reranker {
            self.store.set_setting("reranker", r.as_ref().map(|r| r.spec.as_str()).unwrap_or("none")).await?;
            self.models.write().await.reranker = r.map(Arc::new);
        }
        Ok(())
    }

    /// Drops the embedder (BM25 only) without touching any index.
    pub async fn clear_embedding(&self, admin: &str) -> Result<()> {
        self.must_admin(admin)?;
        self.store.set_setting("embedding", "none").await?;
        let mut models = self.models.write().await;
        models.embedder = None;
        models.space = None;
        Ok(())
    }

    pub async fn active_models(&self) -> (Option<String>, Option<String>, Option<String>) {
        let m = self.models.read().await;
        (m.embedder.as_ref().map(|e| e.spec.clone()), m.space.clone(), m.reranker.as_ref().map(|r| r.spec.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, at: f64) -> Hit {
        Hit { id: id.into(), score: 0.0, at }
    }

    #[test]
    fn fusion_prefers_agreement_then_newer() {
        let a = vec![hit("x", 1.0), hit("y", 2.0)];
        let b = vec![hit("z", 3.0), hit("y", 2.0)];
        let fused = fuse(&[&a, &b], 60.0);
        assert_eq!(fused[0].id, "y");
        // x and z tie (each first in one list): the newer, z, comes first.
        let order: Vec<&str> = fused.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(order, ["y", "z", "x"]);
    }

    #[test]
    fn context_skips_what_does_not_fit() {
        let m = |id: &str, text: &str| RetrievedItem {
            memory: Memory { id: id.into(), scope: "s".into(), kind: "k".into(), version: 1, text: text.into(), source: "src".into(), author: "a".into(),
                valid_from: None, valid_to: None, recorded_at: "t".into(), status: "active".into(), status_reason: None, superseded_by: None, metadata: Default::default() },
            score: 1.0, channels: Default::default(), via: None, rerank_score: None, dates: None,
        };
        let r = Retrieval { items: vec![m("1", &"x".repeat(400)), m("2", "short")], insufficient_evidence: false, paths_used: vec![], limitations: vec![], embedding_space: None, timings_ms: None };
        let c = build_context(r, 50);
        assert_eq!(c.citations.len(), 1);
        assert_eq!(c.citations[0].memory_id, "2");
        assert_eq!(c.dropped, 1);
    }

    fn rerank_item(id: String) -> RetrievedItem {
        RetrievedItem {
            memory: Memory { id: id.clone(), scope: "fixture".into(), kind: "fact".into(), version: 1, text: id,
                source: "synthetic".into(), author: "fixture".into(), valid_from: None, valid_to: None,
                recorded_at: "2026-01-01".into(), status: "active".into(), status_reason: None,
                superseded_by: None, metadata: Default::default() },
            score: 0.0, channels: Default::default(), via: None, rerank_score: None, dates: None,
        }
    }

    #[test]
    fn rerank_cap_preserves_channel_recall_and_only_sorts_the_scored_prefix() {
        let channel = |prefix: &str| (1..=24).map(|n| hit(&if n == 9 { "needle".into() } else { format!("{prefix}-{n}") }, 0.0)).collect::<Vec<_>>();
        let (a, b) = (channel("a"), channel("b"));
        let mut items = fuse(&[&a, &b], 60.0).into_iter().take(24).map(|h| rerank_item(h.id)).collect::<Vec<_>>();
        // The agreement at rank9 would be lost by reducing either channel to8 candidates.
        let docs = rerank_input(&items, Some(8), Some("10")).unwrap();
        assert_eq!(docs.len(), 8);
        assert_eq!(docs[0], "needle");
        let tail = items[8..].iter().map(|i| i.memory.id.clone()).collect::<Vec<_>>();
        let scores = [-8.0, -7.0, -6.0, -5.0, -4.0, -3.0, -2.0, -1.0];
        apply_rerank_scores(&mut items, &scores);
        assert_eq!(items[0].memory.id, docs[7]);
        assert!(items[..8].iter().all(|i| i.rerank_score.is_some()));
        assert!(items[8..].iter().all(|i| i.rerank_score.is_none()));
        assert_eq!(items[8..].iter().map(|i| i.memory.id.clone()).collect::<Vec<_>>(), tail);
    }

    #[test]
    fn rerank_cap_defaults_to_all_and_rejects_invalid_configuration() {
        let items = (1..=24).map(|n| rerank_item(format!("doc-{n}"))).collect::<Vec<_>>();
        assert_eq!(rerank_input(&items, None, None).unwrap().len(), 24);
        assert_eq!(rerank_input(&items, None, Some("150")).unwrap().len(), 24);
        assert_eq!(rerank_input(&items, None, Some("10")).unwrap().len(), 10);
        for invalid in ["0", "151", "invalid", "-8"] { assert!(rerank_input(&items, None, Some(invalid)).is_err()); }
    }

    #[test]
    fn rerank_default_preserves_full_pool_sorting() {
        let mut items = (1..=24).map(|n| rerank_item(format!("doc-{n}"))).collect::<Vec<_>>();
        let scores = (1..=24).map(|n| n as f32).collect::<Vec<_>>();
        apply_rerank_scores(&mut items, &scores);
        assert_eq!(items[0].memory.id, "doc-24");
        assert!(items.iter().all(|i| i.rerank_score.is_some()));
        assert!(items.windows(2).all(|p| p[0].rerank_score >= p[1].rerank_score));
    }

    #[test]
    fn rerank_request_overrides_environment_and_has_the_same_bounds() {
        let items = (1..=24).map(|n| rerank_item(format!("doc-{n}"))).collect::<Vec<_>>();
        assert_eq!(rerank_input(&items, Some(8), Some("10")).unwrap().len(), 8);
        assert_eq!(rerank_input(&items, Some(10), Some("invalid")).unwrap().len(), 10);
        assert_eq!(rerank_input(&items, Some(150), Some("1")).unwrap().len(), 24);
        for n in [0,151] { assert!(rerank_input(&items, Some(n), Some("8")).is_err()); }
    }
}

#[cfg(test)]
mod profiling_tests {
    use super::*;
    #[test]
    fn retrieval_profile_is_opt_in_and_durations_have_no_memory_payload() {
        let defaults: RetrieveOptions = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!defaults.profile.unwrap_or(false));
        let opts: RetrieveOptions = serde_json::from_str(r#"{"profile":true}"#).unwrap();
        assert_eq!(opts.profile, Some(true));
        let mut response = Retrieval { items: vec![], insufficient_evidence: true, paths_used: vec![], limitations: vec![], embedding_space: None, timings_ms: None };
        assert!(serde_json::to_value(&response).unwrap().get("timingsMs").is_none());
        response.timings_ms = Some(StageTimings::default());
        let value = serde_json::to_value(response).unwrap();
        let object = value["timingsMs"].as_object().unwrap();
        assert_eq!(object.len(), 9);
        assert!(object.values().all(serde_json::Value::is_number));
    }

    #[tokio::test]
    async fn profiling_records_each_await_and_preserves_errors() {
        let started = std::time::Instant::now();
        let mut timings = Some(StageTimings::default());
        let value = timed(&mut timings, Stage::Lexical, async {
            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
            Ok::<_, Error>(17)
        }).await.unwrap();
        assert_eq!(value, 17);
        let error = timed(&mut timings, Stage::Vector, async {
            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
            Err::<(), _>(Error::InvalidInput("synthetic fixture".into()))
        }).await;
        assert!(matches!(error, Err(Error::InvalidInput(_))));
        let t = timings.as_mut().unwrap();
        t.finish(started);
        assert!(t.lexical >= 1.0 && t.vector >= 1.0);
        assert_eq!(t.embedding, 0.0);
        assert_eq!(t.rerank, 0.0);
        assert_eq!(t.graph, 0.0);
        assert!(t.other >= 0.0);
        assert!((t.total - t.lexical - t.vector - t.other).abs() < 0.000001);
    }

    #[tokio::test]
    async fn disabled_profiling_awaits_the_operation_once_without_timings() {
        let mut timings = None;
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let value = timed(&mut timings, Stage::Embedding, async {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok::<_, Error>("fixture")
        }).await.unwrap();
        assert_eq!(value, "fixture");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(timings.is_none());
    }
    #[test]
    fn synchronous_fusion_records_once_and_leaves_other_stages_zero() {
        let mut timings = Some(StageTimings::default());
        let calls = std::cell::Cell::new(0);
        let result = measured(&mut timings, Stage::Fusion, || { calls.set(calls.get()+1); 42 });
        assert_eq!((result,calls.get()),(42,1));
        let t=timings.unwrap();
        assert!(t.fusion>=0.0);
        assert_eq!(t.payload_load,0.0);
        assert_eq!(t.graph,0.0);
        assert_eq!(t.rerank,0.0);
    }

}
