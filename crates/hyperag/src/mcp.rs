//! THE MCP SERVER. Every tool is one call to the Hyperag HTTP service with the caller's bearer token,
//! so identity, grants and scope checks are the service's, on every call; an MCP process holds no
//! data, no model and no database access of its own.
//!
//! - stdio: `hyperag mcp --url http://127.0.0.1:7480` with the agent's token in `HYPERAG_TOKEN`.
//! - HTTP: the service also serves MCP at `/mcp`; each request carries `Authorization: Bearer <token>`.

use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData, Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct Mcp {
    http: reqwest::Client,
    base: String,
    /// stdio: the agent's token. HTTP transport: none here; each request's own header is used.
    token: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestArgs {
    /// Scope to write to; the caller needs write access to it.
    pub scope: String,
    /// The memory text: a decision, finding, fact or document.
    pub text: String,
    /// Where it came from, for citations (a PR, a file, a conversation).
    pub source: String,
    /// decision, finding, evidence, fact, document...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Ids of memories this one corrects; they stop ranking and keep their history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Vec<String>>,
    /// When the fact started or stopped holding (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchArgs {
    /// The question, in natural language (English or Spanish).
    pub query: String,
    /// Most passages to return (default 8, at most 50).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Restrict to these readable scopes (empty means all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// Exclude these readable scopes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_scopes: Option<Vec<String>>,
    /// Token budget for the packed context (default 1,000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// Only facts valid at this instant (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecentArgs {
    /// The scope to read, for example project:<name>.
    pub scope: String,
    /// Most memories (default 30, at most 200).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Byte budget of the returned text (default 16,384, at most 65,536).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateArgs {
    pub id: String,
    pub text: String,
    pub source: String,
    /// The version you read; if someone else updated it first you get version_conflict.
    pub expected_version: i32,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct IdReasonArgs {
    pub id: String,
    pub reason: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct IdArgs {
    pub id: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct RelateArgs {
    pub from: String,
    pub to: String,
    /// UPPER_SNAKE_CASE, for example SUPPORTED_BY or REFERS_TO.
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct ModelsArgs {
    /// Embedding spec, for example hf:BAAI/bge-small-en-v1.5 or none. Admin only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<String>,
    /// Re-ranker spec, for example hf:BAAI/bge-reranker-base or none. Admin only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reranker: Option<String>,
}

fn token_of(ctx: &rmcp::service::RequestContext<rmcp::RoleServer>) -> Option<String> {
    let parts = ctx.extensions.get::<axum::http::request::Parts>()?;
    crate::server::bearer(parts.headers.get("authorization")?.to_str().ok()?).map(str::to_owned)
}

#[tool_router]
impl Mcp {
    pub fn new(base: &str, token: Option<String>) -> Self {
        Mcp {
            http: reqwest::Client::builder().timeout(Duration::from_secs(120)).build().expect("http client"),
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    async fn call(&self, ctx: &rmcp::service::RequestContext<rmcp::RoleServer>, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<CallToolResult, ErrorData> {
        let Some(token) = self.token.clone().or_else(|| token_of(ctx)) else {
            // The same refusal the service gives, readable as a tool result.
            return Ok(CallToolResult::error(vec![ContentBlock::text(json!({ "error": { "code": "unauthenticated" } }).to_string())]));
        };
        let mut req = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let response = req.send().await.map_err(|e| ErrorData::internal_error(format!("hyperag service unreachable: {e}"), None))?;
        let ok = response.status().is_success();
        let raw = response.text().await.unwrap_or_default();
        // A non-JSON body (a proxy error, for example) is passed on as it is.
        let text = serde_json::from_str::<Value>(&raw).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or(raw);
        // A refusal (forbidden, not_found, version_conflict...) is a tool result the agent can read.
        Ok(if ok { CallToolResult::success(vec![ContentBlock::text(text)]) } else { CallToolResult::error(vec![ContentBlock::text(text)]) })
    }

    #[tool(description = "Store a memory (decision, finding, evidence, fact) in a scope. Returns its id and version. A retry with the same text is a no-op.")]
    async fn ingest(&self, Parameters(a): Parameters<IngestArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::POST, "/v1/memories", Some(json!(a))).await
    }

    #[tool(description = "Search your readable memory and get a context with numbered citations (memory id, version, source, author), the ranked items, the retrieval paths used, and insufficientEvidence when nothing is good enough.")]
    async fn search(&self, Parameters(a): Parameters<SearchArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::POST, "/v1/context", Some(json!(a))).await
    }

    #[tool(description = "The newest memories of one scope, newest first, as one compact text: what to read at the start of a session.")]
    async fn recent(&self, Parameters(a): Parameters<RecentArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        let mut path = format!("/v1/recent?scope={}", urlencode(&a.scope));
        if let Some(l) = a.limit {
            path.push_str(&format!("&limit={l}"));
        }
        if let Some(b) = a.max_bytes {
            path.push_str(&format!("&maxBytes={b}"));
        }
        self.call(&ctx, reqwest::Method::GET, &path, None).await
    }

    #[tool(description = "Write a new version of a memory. Pass the version you read as expectedVersion.")]
    async fn update(&self, Parameters(a): Parameters<UpdateArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        let body = json!({ "text": a.text, "source": a.source, "expectedVersion": a.expected_version });
        self.call(&ctx, reqwest::Method::PATCH, &format!("/v1/memories/{}", urlencode(&a.id)), Some(body)).await
    }

    #[tool(description = "Mark a memory as no longer valid. It stops ranking and keeps its history.")]
    async fn invalidate(&self, Parameters(a): Parameters<IdReasonArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::POST, &format!("/v1/memories/{}/invalidate", urlencode(&a.id)), Some(json!({ "reason": a.reason }))).await
    }

    #[tool(description = "Delete a memory: its text, vectors and graph edges go; a content-free tombstone stays.")]
    async fn delete(&self, Parameters(a): Parameters<IdReasonArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::DELETE, &format!("/v1/memories/{}?reason={}", urlencode(&a.id), urlencode(&a.reason)), None).await
    }

    #[tool(description = "Link two memories with a typed edge, for example a finding SUPPORTED_BY its evidence.")]
    async fn relate(&self, Parameters(a): Parameters<RelateArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::POST, "/v1/relations", Some(json!(a))).await
    }

    #[tool(description = "Explain a memory's provenance: every version with author, recorded and valid time, why it changed, what corrected it, and its relations.")]
    async fn explain(&self, Parameters(a): Parameters<IdArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::GET, &format!("/v1/memories/{}/explain", urlencode(&a.id)), None).await
    }

    #[tool(description = "List the active embedding and re-ranker models and every embedding index with its model, revision, dimensions and vector count.")]
    async fn models_list(&self, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::GET, "/v1/models", None).await
    }

    #[tool(description = "Switch the embedding and/or re-ranker model (admin only). A new embedding builds its own index first; documents, BM25 and the graph stay.")]
    async fn models_set(&self, Parameters(a): Parameters<ModelsArgs>, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::POST, "/v1/models", Some(json!(a))).await
    }

    #[tool(description = "Check the service is up and who you are.")]
    async fn health(&self, ctx: rmcp::service::RequestContext<rmcp::RoleServer>) -> Result<CallToolResult, ErrorData> {
        self.call(&ctx, reqwest::Method::GET, "/v1/whoami", None).await
    }
}

#[tool_handler]
impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("hyperag", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Hyperag: shared memory and retrieval for agents. Use `search` before answering from memory and cite the [n] \
                 passages; `ingest` decisions, findings and evidence with a source; `relate` a finding to its evidence; correct a \
                 fact by ingesting the correction with `supersedes`; `explain` shows a memory's history.",
            )
    }
}

pub(crate) fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
