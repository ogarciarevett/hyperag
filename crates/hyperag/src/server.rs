//! THE TRUST BOUNDARY over HTTP, a port of `src/server.ts` with the same routes. A caller proves who
//! it is with a bearer token; the service maps the token's SHA-256 to a principal from its own
//! registry and never reads an identity from the request body.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::core::{Hyperag, RetrieveOptions};
use crate::types::{Error, MemoryUpdate, NewMemory};

pub const MAX_BODY_BYTES: usize = 1_048_576;

/// sha256(token) hex -> principal id. Plain tokens are never stored.
#[derive(Clone, Default)]
pub struct Tokens(pub Vec<([u8; 32], String)>);

impl Tokens {
    pub fn from_json(raw: &str) -> anyhow::Result<Tokens> {
        let map: HashMap<String, String> = serde_json::from_str(raw)?;
        let mut out = vec![];
        for (hash, who) in map {
            let bytes = hex::decode(&hash).ok().filter(|b| b.len() == 32 && hash.chars().all(|c| !c.is_ascii_uppercase()));
            anyhow::ensure!(bytes.is_some() && !who.is_empty(), "tokens file must map sha256 hex strings to principal names");
            out.push((bytes.unwrap().try_into().unwrap(), who));
        }
        Ok(Tokens(out))
    }

    pub fn principal(&self, token: &str) -> Option<String> {
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        // Compare against every entry, in constant time per entry.
        let mut found = None;
        for (hash, who) in &self.0 {
            if bool::from(hash.ct_eq(&presented)) {
                found = Some(who.clone());
            }
        }
        found
    }
}

/// The tokens file, re-read when its modification time changes, so `hyperag token` takes effect
/// without a restart.
pub struct TokenFile {
    path: std::path::PathBuf,
    loaded: std::sync::RwLock<(Option<std::time::SystemTime>, Tokens)>,
}

impl TokenFile {
    pub fn open(path: std::path::PathBuf) -> anyhow::Result<TokenFile> {
        // A fresh install starts with no tokens; `hyperag token` adds them while the service runs.
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, "{}")?;
        }
        let file = TokenFile { path, loaded: std::sync::RwLock::new((None, Tokens::default())) };
        file.reload()?;
        Ok(file)
    }

    fn reload(&self) -> anyhow::Result<()> {
        let modified = std::fs::metadata(&self.path)?.modified().ok();
        if self.loaded.read().expect("tokens lock").0 == modified && modified.is_some() {
            return Ok(());
        }
        let tokens = Tokens::from_json(&std::fs::read_to_string(&self.path)?)?;
        *self.loaded.write().expect("tokens lock") = (modified, tokens);
        Ok(())
    }

    pub fn principal(&self, token: &str) -> Option<String> {
        // A file that became unreadable or invalid keeps the last good registry.
        if let Err(e) = self.reload() {
            tracing::error!("tokens file not reloaded: {e}");
        }
        self.loaded.read().expect("tokens lock").1.principal(token)
    }
}

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[derive(Clone)]
pub struct App {
    pub hyperag: Arc<Hyperag>,
    pub tokens: Arc<TokenFile>,
}

pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let e = self.0;
        let status = match &e {
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::VersionConflict { .. } | Error::AlreadyExists { .. } | Error::NotActive { .. } | Error::EmbeddingSpaceMismatch(_) => StatusCode::CONFLICT,
            Error::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Error::Database(_) | Error::Dependency(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut body = json!({ "code": e.code() });
        match &e {
            Error::Database(_) | Error::Dependency(_) => tracing::error!("hyperag request failed: {e}"),
            Error::VersionConflict { current_version, .. } => {
                body["message"] = json!(e.to_string());
                body["currentVersion"] = json!(current_version);
            }
            Error::NotActive { status, memory_id, .. } => {
                body["message"] = json!(e.to_string());
                body["status"] = json!(status);
                if let Some(id) = memory_id {
                    body["memoryId"] = json!(id);
                }
            }
            Error::AlreadyExists { deleted, .. } => {
                body["message"] = json!(e.to_string());
                body["deleted"] = json!(deleted);
            }
            _ => body["message"] = json!(e.to_string()),
        }
        (status, Json(json!({ "error": body }))).into_response()
    }
}

type R = Result<Json<Value>, ApiError>;

pub struct Who(pub String);

impl axum::extract::FromRequestParts<App> for Who {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, app: &App) -> Result<Self, Self::Rejection> {
        principal_of(&parts.headers, &app.tokens)
            .map(Who)
            .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "code": "unauthenticated" } }))).into_response())
    }
}

/// The token of an `Authorization: Bearer <token>` header; the scheme is case-insensitive (RFC 6750).
pub fn bearer(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token.trim()).filter(|t| !t.is_empty())
}

fn principal_of(headers: &HeaderMap, tokens: &TokenFile) -> Option<String> {
    tokens.principal(bearer(headers.get("authorization")?.to_str().ok()?)?)
}

/// A JSON body that is invalid for the route is 400, never 500.
pub struct Body<T>(pub T);

impl<T: serde::de::DeserializeOwned, S: Send + Sync> axum::extract::FromRequest<S> for Body<T> {
    type Rejection = ApiError;
    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|_| Error::InvalidInput("body too large or unreadable".into()))?;
        let text = if bytes.is_empty() { &b"{}"[..] } else { &bytes[..] };
        serde_json::from_slice(text).map(Body).map_err(|e| ApiError(Error::InvalidInput(format!("invalid body: {e}"))))
    }
}

#[derive(Deserialize)]
struct QueryBody {
    query: String,
    #[serde(flatten)]
    options: RetrieveBounds,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RetrieveBounds {
    limit: Option<usize>,
    candidates_per_channel: Option<usize>,
    as_of: Option<String>,
    anchor: Option<String>,
    channels: Option<crate::core::Channels>,
    max_tokens: Option<usize>,
}

fn bounded(b: RetrieveBounds) -> Result<RetrieveOptions, ApiError> {
    let check = |v: Option<usize>, key: &str, max: usize| match v {
        Some(n) if n < 1 || n > max => Err(ApiError(Error::InvalidInput(format!("{key} must be an integer from 1 to {max}")))),
        _ => Ok(v),
    };
    Ok(RetrieveOptions {
        limit: check(b.limit, "limit", 50)?,
        candidates_per_channel: check(b.candidates_per_channel, "candidatesPerChannel", 150)?,
        max_tokens: check(b.max_tokens, "maxTokens", 32_000)?,
        as_of: b.as_of,
        anchor: b.anchor,
        channels: b.channels,
    })
}

#[derive(Deserialize)]
struct Reason {
    reason: String,
}

#[derive(Deserialize)]
struct RelateBody {
    from: String,
    to: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct GrantBody {
    principal: String,
    scope: String,
    permission: String,
}

#[derive(Deserialize)]
struct BatchBody {
    memories: Vec<NewMemory>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsBody {
    embedding: Option<String>,
    reranker: Option<String>,
}

/// Required strings are non-empty, as in the reference service.
fn required(fields: &[(&str, &str)]) -> Result<(), ApiError> {
    match fields.iter().find(|(_, v)| v.is_empty()) {
        Some((k, _)) => Err(ApiError(Error::InvalidInput(format!("{k} is required")))),
        None => Ok(()),
    }
}

fn new_memory_ok(m: &NewMemory) -> Result<(), ApiError> {
    required(&[("scope", &m.scope), ("text", &m.text), ("source", &m.source)])
}

fn permission(p: &str) -> Result<&str, ApiError> {
    if p == "read" || p == "write" { Ok(p) } else { Err(ApiError(Error::InvalidInput("permission is read or write".into()))) }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/healthz", get(|| async { Json(json!({ "ok": true })) }))
        .route("/v1/whoami", get(|Who(who): Who| async move { Json(json!({ "principal": who })) }))
        .route("/v1/memories", post(ingest))
        .route("/v1/memories/batch", post(ingest_batch))
        .route("/v1/memories/{id}", axum::routing::patch(update).delete(delete))
        .route("/v1/memories/{id}/invalidate", post(invalidate))
        .route("/v1/memories/{id}/explain", get(explain))
        .route("/v1/memories/{id}/relations", get(relations))
        .route("/v1/relations", post(relate))
        .route("/v1/retrieve", post(retrieve))
        .route("/v1/context", post(context))
        .route("/v1/grants", post(grant).delete(revoke))
        .route("/v1/models", get(models).post(set_models))
        .route("/v1/recent", get(recent))
        // Unknown routes and methods: 401 without a token, else a JSON 404, as the reference does.
        .fallback(no_route)
        .method_not_allowed_fallback(no_route)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(app)
}

async fn no_route(Who(_): Who) -> ApiError {
    ApiError(Error::NotFound("no such route".into()))
}

async fn ingest(State(a): State<App>, Who(w): Who, Body(b): Body<NewMemory>) -> R {
    new_memory_ok(&b)?;
    Ok(Json(json!(a.hyperag.ingest(&w, b).await?)))
}

async fn ingest_batch(State(a): State<App>, Who(w): Who, Body(b): Body<BatchBody>) -> R {
    for m in &b.memories {
        new_memory_ok(m)?;
    }
    if b.memories.len() > 512 {
        return Err(ApiError(Error::InvalidInput("at most 512 memories per batch".into())));
    }
    Ok(Json(json!(a.hyperag.ingest_many(&w, b.memories, 32).await?)))
}

async fn update(State(a): State<App>, Who(w): Who, Path(id): Path<String>, Body(b): Body<MemoryUpdate>) -> R {
    required(&[("text", &b.text), ("source", &b.source)])?;
    if b.expected_version < 1 {
        return Err(ApiError(Error::InvalidInput("expectedVersion must be a positive integer".into())));
    }
    Ok(Json(json!(a.hyperag.update(&w, &id, b).await?)))
}

async fn delete(State(a): State<App>, Who(w): Who, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> R {
    a.hyperag.delete(&w, &id, q.get("reason").map(String::as_str).unwrap_or("deleted")).await?;
    Ok(Json(json!({ "deleted": id })))
}

async fn invalidate(State(a): State<App>, Who(w): Who, Path(id): Path<String>, Body(b): Body<Reason>) -> R {
    required(&[("reason", &b.reason)])?;
    a.hyperag.invalidate(&w, &id, &b.reason).await?;
    Ok(Json(json!({ "invalidated": id })))
}

async fn explain(State(a): State<App>, Who(w): Who, Path(id): Path<String>) -> R {
    Ok(Json(json!(a.hyperag.explain(&w, &id).await?)))
}

async fn relations(State(a): State<App>, Who(w): Who, Path(id): Path<String>) -> R {
    Ok(Json(json!(a.hyperag.relations(&w, &id).await?)))
}

async fn relate(State(a): State<App>, Who(w): Who, Body(b): Body<RelateBody>) -> R {
    required(&[("from", &b.from), ("to", &b.to), ("type", &b.kind)])?;
    a.hyperag.relate(&w, &b.from, &b.to, &b.kind).await?;
    Ok(Json(json!({ "related": true })))
}

async fn retrieve(State(a): State<App>, Who(w): Who, Body(b): Body<QueryBody>) -> R {
    required(&[("query", &b.query)])?;
    Ok(Json(json!(a.hyperag.retrieve(&w, &b.query, &bounded(b.options)?).await?)))
}

async fn context(State(a): State<App>, Who(w): Who, Body(b): Body<QueryBody>) -> R {
    required(&[("query", &b.query)])?;
    Ok(Json(json!(a.hyperag.context(&w, &b.query, &bounded(b.options)?).await?)))
}

async fn grant(State(a): State<App>, Who(w): Who, Body(b): Body<GrantBody>) -> R {
    required(&[("principal", &b.principal), ("scope", &b.scope)])?;
    a.hyperag.grant(&w, &b.principal, &b.scope, permission(&b.permission)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn revoke(State(a): State<App>, Who(w): Who, Body(b): Body<GrantBody>) -> R {
    required(&[("principal", &b.principal), ("scope", &b.scope)])?;
    a.hyperag.revoke(&w, &b.principal, &b.scope, permission(&b.permission)?).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn models_view(h: &Hyperag) -> Result<Value, Error> {
    let (embedding, space, reranker) = h.active_models().await;
    let spaces: Vec<Value> = h
        .store
        .spaces()
        .await?
        .into_iter()
        .map(|(id, s, vectors)| json!({ "id": id, "active": Some(&id) == space.as_ref(), "model": s.model, "revision": s.revision,
            "dimensions": s.dimensions, "normalized": s.normalized, "preprocessing": s.preprocessing, "vectors": vectors }))
        .collect();
    Ok(json!({ "embedding": embedding.unwrap_or_else(|| "none".into()), "reranker": reranker.unwrap_or_else(|| "none".into()), "spaces": spaces }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentQuery {
    scope: String,
    limit: Option<usize>,
    max_bytes: Option<usize>,
}

async fn recent(State(a): State<App>, Who(w): Who, Query(q): Query<RecentQuery>) -> R {
    required(&[("scope", &q.scope)])?;
    let r = a.hyperag.recent(&w, &q.scope, q.limit.unwrap_or(30), q.max_bytes.unwrap_or(16_384).min(65_536)).await?;
    Ok(Json(json!(r)))
}

async fn models(State(a): State<App>, Who(_): Who) -> R {
    Ok(Json(models_view(&a.hyperag).await?))
}

/// A model switch from the service: admin only, one string per model.
async fn set_models(State(a): State<App>, Who(w): Who, Body(b): Body<ModelsBody>) -> R {
    crate::cli::switch(&a.hyperag, &w, b.embedding.as_deref(), b.reranker.as_deref()).await?;
    Ok(Json(models_view(&a.hyperag).await?))
}

#[cfg(test)]
mod rerank_request_tests {
    use super::*;
    #[test]
    fn rerank_request_cap_is_forwarded_independently_and_bounded() {
        let request: QueryBody = serde_json::from_str(r#"{"query":"synthetic fixture","limit":5,"candidatesPerChannel":24,"rerankCandidates":8}"#).unwrap();
        let options = bounded(request.options).unwrap_or_else(|_| panic!("valid request rejected"));
        assert_eq!(options.rerank_candidates, Some(8));
        assert_eq!(options.candidates_per_channel, Some(24));
        assert_eq!(options.limit, Some(5));
        let default: QueryBody = serde_json::from_str(r#"{"query":"synthetic fixture"}"#).unwrap();
        assert_eq!(bounded(default.options).unwrap_or_else(|_| panic!("valid default rejected")).rerank_candidates, None);
        for n in [0,151] {
            let request: QueryBody = serde_json::from_value(serde_json::json!({"query":"synthetic fixture","rerankCandidates":n})).unwrap();
            assert!(bounded(request.options).is_err());
        }
    }
}
