pub mod cli;
pub mod core;
pub mod graph;
#[allow(dead_code)] // Private foundation; the index worker is introduced separately.
mod lexical_snapshot;
#[allow(dead_code)] // Private durable generation; no service worker is enabled.
mod lexical_generation;
#[allow(dead_code)] // Private explicit consumer; no background service is enabled.
mod lexical_consumer;
#[allow(dead_code)] // Private scoring feasibility; request integration follows separately.
mod lexical_scorer;
pub mod hooks;
pub mod hook_setup;
pub mod mcp;
pub mod models;
pub mod portable;
pub mod recommend;
pub mod server;
pub mod store;
pub mod temporal;
pub mod types;

pub use crate::core::{Config, Hyperag, RetrieveOptions};
pub use crate::types::{Error, Memory, MemoryUpdate, NewMemory, WriteResult};
