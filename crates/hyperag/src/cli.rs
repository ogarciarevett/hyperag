//! Configuration from the environment and the `hyperag` commands.
//!
//!   HYPERAG_PG_URL          Postgres with pgvector (required)
//!   HYPERAG_NEO4J_URL       Neo4j HTTP endpoint; unset, no graph channel
//!   HYPERAG_NEO4J_USER / HYPERAG_NEO4J_PASSWORD
//!   HYPERAG_EMBEDDING       embedding spec (see `models.rs`); unset, the last one set with
//!                           `hyperag models set`, else EmbeddingGemma 2 (q8)
//!   HYPERAG_RERANKER        re-ranker spec, same rule, else bge-reranker-v2-m3 (q8)
//!   HYPERAG_ADMINS          comma-separated principals that may grant, revoke and switch models
//!   HYPERAG_TOKENS_FILE     JSON { "<sha256 of token>": "<principal>" } for the service and MCP

use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;

use crate::core::{Config, Hyperag};
use crate::graph::Graph;
use crate::models::{Embedder, Reranker};
use crate::store::Store;
use crate::types::{Error, Result};

/// The default template: EmbeddingGemma 2 (local, q8, 768 dims) and bge-reranker-v2-m3 (local, q8).
pub const DEFAULT_EMBEDDING: &str = "hf:onnx-community/embeddinggemma-2-ONNX?revision=daa72c51243991dfcaf9f9137d2c573d8f7790c0&dtype=q8&pooling=none&output=sentence_embedding&dims=768";
pub const DEFAULT_RERANKER: &str = "hf:onnx-community/bge-reranker-v2-m3-ONNX?revision=6f5ff65298512715a1e669753bc754d2bc8f367b&dtype=q8";

#[derive(Parser)]
#[command(
    name = "hyperag",
    version,
    about = "Shared memory and retrieval for AI agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the HTTP service.
    Serve {
        #[arg(long, env = "HYPERAG_HOST", default_value = "127.0.0.1")]
        host: String,
        #[arg(long, env = "HYPERAG_PORT", default_value_t = 7480)]
        port: u16,
    },
    /// Run the MCP server on stdio, as a client of a running Hyperag service (token in HYPERAG_TOKEN).
    Mcp {
        #[arg(long, env = "HYPERAG_URL", default_value = "http://127.0.0.1:7480")]
        url: String,
    },
    /// Agent harness hook: session-start, prompt or session-end (hook JSON on stdin).
    Hook {
        event: String,
        #[arg(long)]
        harness: Option<String>,
    },
    /// Internal detached writer for opt-in asynchronous hooks.
    #[command(hide = true)]
    HookWriter { root: std::path::PathBuf },
    /// Inspect private async counters and classify a dead worker's unconfirmed queue.
    HookWriterStatus,
    /// Configure the same hooks across supported harnesses.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Create a token for a principal: prints the token once and adds its hash to the tokens file.
    Token {
        principal: String,
        #[arg(
            long,
            env = "HYPERAG_TOKENS_FILE",
            default_value = "config/tokens.json"
        )]
        file: std::path::PathBuf,
    },
    /// Write everything the store and graph know to one portable file (JSON Lines, no vectors).
    Export {
        /// The file to write; `-` for stdout.
        #[arg(long, short)]
        out: std::path::PathBuf,
    },
    /// Read an export file into the configured store (HYPERAG_PG_URL, HYPERAG_NEO4J_URL), then embed
    /// it with the chosen embedder: `--embedding`, else HYPERAG_EMBEDDING, the stored choice or the default.
    Import {
        file: std::path::PathBuf,
        #[arg(long)]
        embedding: Option<String>,
        #[arg(long)]
        reranker: Option<String>,
        /// The admin principal acting, needed with `--embedding` or `--reranker`.
        #[arg(long = "as", env = "HYPERAG_ACTOR")]
        actor: Option<String>,
        /// Import into a store with no graph, dropping the file's relations.
        #[arg(long)]
        without_graph: bool,
    },
    /// Show or switch the embedding and re-ranker models.
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
}

#[derive(Subcommand)]
pub enum ModelsAction {
    /// The active models and every embedding space with its vector count.
    List {
        /// Also list the local models a spec can name.
        #[arg(long)]
        available: bool,
    },
    /// Download and load the active (or given) models: flags, then HYPERAG_EMBEDDING and
    /// HYPERAG_RERANKER, then what `models set` stored (with HYPERAG_PG_URL), then the defaults.
    /// Files Hyperag downloads itself are checked against the hub's SHA-256 and their licences shown.
    Pull {
        #[arg(long)]
        embedding: Option<String>,
        #[arg(long)]
        reranker: Option<String>,
    },
    /// Suggest one model per slot for the resources free right now.
    Recommend {
        #[arg(long)]
        json: bool,
    },
    /// Switch models, for example `hyperag models set embedding=hf:BAAI/bge-small-en-v1.5 reranker=none`.
    Set {
        /// `embedding=<spec>` and/or `reranker=<spec>`.
        assignments: Vec<String>,
        /// The admin principal acting (must be in HYPERAG_ADMINS).
        #[arg(long = "as", env = "HYPERAG_ACTOR")]
        actor: String,
    },
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Builds Hyperag from the environment; the stored model choice applies when no env var overrides it.
pub async fn open() -> anyhow::Result<Hyperag> {
    let url = env("HYPERAG_PG_URL").ok_or_else(|| anyhow::anyhow!("HYPERAG_PG_URL is required"))?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(2))
        // sqlx reuses prepared statements; a generic plan for the BM25 and vector queries was slower
        // than planning each call with its own scopes, as node-pg's unnamed statements do.
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("set plan_cache_mode = force_custom_plan")
                    .execute(c)
                    .await
                    .map(|_| ())
            })
        })
        .connect(&url)
        .await?;
    let store = Store::new(pool);
    store.migrate().await?;
    let embedding = match env("HYPERAG_EMBEDDING") {
        Some(s) => s,
        None => store
            .setting("embedding")
            .await?
            .unwrap_or_else(|| DEFAULT_EMBEDDING.into()),
    };
    let reranker = match env("HYPERAG_RERANKER") {
        Some(s) => s,
        None => store
            .setting("reranker")
            .await?
            .unwrap_or_else(|| DEFAULT_RERANKER.into()),
    };
    let mut config = Config::new(store);
    config.embedder = Embedder::from_spec(&embedding).await?;
    config.reranker = Reranker::from_spec(&reranker).await?;
    config.graph = env("HYPERAG_NEO4J_URL").map(|u| {
        Graph::new(
            &u,
            &env("HYPERAG_NEO4J_USER").unwrap_or_else(|| "neo4j".into()),
            &env("HYPERAG_NEO4J_PASSWORD").unwrap_or_default(),
            None,
        )
    });
    config.admins = env("HYPERAG_ADMINS")
        .map(|a| {
            a.split(',')
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(Hyperag::init(config).await?)
}

/// Serve owns the writer separately from request state and command-line utilities.
pub struct Service {
    pub hyperag: std::sync::Arc<Hyperag>,
    writer: Option<crate::lexical_worker::Worker>,
}

impl Service {
    /// Called only after HTTP requests have drained.
    pub async fn close(self) -> anyhow::Result<()> {
        let result = match self.writer {
            Some(writer) => writer.close().await,
            None => Ok(()),
        };
        self.hyperag.store.pool.close().await;
        result
    }
}

pub async fn open_service() -> anyhow::Result<Service> {
    let mut hyperag = open().await?;
    let writer = if crate::store::LexicalScorer::selected()? == crate::store::LexicalScorer::Tantivy
    {
        let database_id: String =
            sqlx::query_scalar("select database_id::text from lexical_index_state")
                .fetch_one(&hyperag.store.pool)
                .await?;
        let configured = std::env::var_os("HYPERAG_LEXICAL_INDEX_ROOT")
            .filter(|root| !root.is_empty())
            .map(std::path::PathBuf::from);
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let root =
            crate::lexical_root::resolve(configured.as_deref(), home.as_deref(), &database_id)?;
        let consumer = crate::lexical_consumer::Consumer::open(&root)?;
        hyperag.store.lexical_reader = consumer.reader();
        Some(crate::lexical_worker::Worker::start(
            consumer,
            hyperag.store.pool.clone(),
        ))
    } else {
        None
    };
    Ok(Service {
        hyperag: std::sync::Arc::new(hyperag),
        writer,
    })
}

/// One switch, used by the CLI, the service and MCP. `None` leaves that model as it is.
pub async fn switch(
    h: &Hyperag,
    actor: &str,
    embedding: Option<&str>,
    reranker: Option<&str>,
) -> Result<()> {
    // Admin first: parsing a spec may download and load a model.
    h.must_admin(actor)?;
    match embedding {
        Some("none") => h.clear_embedding(actor).await?,
        Some(spec) => {
            let e = Embedder::from_spec(spec)
                .await?
                .ok_or_else(|| Error::InvalidInput("no embedder".into()))?;
            h.set_models(actor, Some(e), None).await?
        }
        None => {}
    }
    if let Some(spec) = reranker {
        h.set_models(actor, None, Some(Reranker::from_spec(spec).await?))
            .await?;
    }
    Ok(())
}

pub fn parse_assignments(items: &[String]) -> anyhow::Result<(Option<String>, Option<String>)> {
    let (mut e, mut r) = (None, None);
    for item in items {
        match item.split_once('=') {
            Some(("embedding", v)) => e = Some(v.to_owned()),
            Some(("reranker", v)) => r = Some(v.to_owned()),
            _ => anyhow::bail!("expected embedding=<spec> or reranker=<spec>, got {item:?}"),
        }
    }
    anyhow::ensure!(e.is_some() || r.is_some(), "nothing to set");
    Ok((e, r))
}

#[derive(Subcommand)]
pub enum HooksAction {
    /// Merge hooks into existing harness profiles; missing profiles are skipped.
    Install {
        #[arg(long, default_value = "all")]
        harness: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show installed hook configuration (does not contact the service).
    Status,
    /// Remove only unchanged entries recorded by install.
    Uninstall {
        #[arg(long, default_value = "all")]
        harness: String,
    },
}
