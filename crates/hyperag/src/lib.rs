pub mod cli;
pub mod core;
pub mod graph;
#[cfg(unix)]
mod hook_credentials;
pub mod hook_setup;
#[cfg(unix)]
pub mod hook_writer;
pub mod hooks;
#[allow(dead_code)] // Private explicit consumer; no background service is enabled.
mod lexical_consumer;
#[allow(dead_code)] // Private durable generation; no service worker is enabled.
mod lexical_generation;
#[allow(dead_code)] // Authorized request snapshots are wired separately from the index worker.
mod lexical_read;
mod lexical_root;
#[allow(dead_code)] // Private scoring feasibility; request integration follows separately.
mod lexical_scorer;
#[allow(dead_code)] // Private foundation; the index worker is introduced separately.
mod lexical_snapshot;
mod lexical_worker;
pub mod mcp;
pub mod models;
pub mod portable;
pub mod recommend;
pub mod server;
pub mod store;
pub mod temporal;
pub mod types;

pub use crate::core::{
    Config, Hyperag, RerankCache, RetrieveOptions, parse_rerank_cache, rerank_cache_key,
};
pub use crate::types::{Error, Memory, MemoryUpdate, NewMemory, WriteResult};
