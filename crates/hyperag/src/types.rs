use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The memory record a caller sees. One stable id, many versions; only the current one ranks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub version: i32,
    pub text: String,
    pub source: String,
    pub author: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub recorded_at: String,
    pub status: String,
    pub status_reason: Option<String>,
    pub superseded_by: Option<String>,
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMemory {
    pub scope: String,
    pub text: String,
    pub source: String,
    pub kind: Option<String>,
    /// Stable identity; omitted, it is derived from scope, source and text, so a retry is a no-op.
    pub id: Option<String>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    /// Memories this one corrects: each one's current version becomes `superseded`.
    #[serde(default)]
    pub supersedes: Vec<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUpdate {
    pub text: String,
    pub source: String,
    /// The version the writer read. Another writer got there first: `version_conflict`.
    pub expected_version: i32,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct WriteResult {
    pub id: String,
    pub version: i32,
    pub created: bool,
}

/// One channel's hit; `at` is the version's recorded time (epoch seconds), newest first on ties.
#[derive(Clone, Debug)]
pub struct Hit {
    pub id: String,
    pub score: f64,
    pub at: f64,
}

/// Everything that makes two vectors comparable. Its hash is the embedding space id.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EmbeddingSpace {
    pub model: String,
    pub revision: String,
    pub dimensions: usize,
    pub normalized: bool,
    pub preprocessing: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{message}")]
    VersionConflict { message: String, current_version: i32 },
    #[error("{message}")]
    AlreadyExists { message: String, deleted: bool },
    #[error("{0}")]
    InvalidInput(String),
    #[error("{message}")]
    NotActive { message: String, status: String, memory_id: Option<String> },
    #[error("{0}")]
    EmbeddingSpaceMismatch(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("{0}")]
    Dependency(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::Forbidden(_) => "forbidden",
            Error::NotFound(_) => "not_found",
            Error::VersionConflict { .. } => "version_conflict",
            Error::AlreadyExists { .. } => "already_exists",
            Error::InvalidInput(_) => "invalid_input",
            Error::NotActive { .. } => "not_active",
            Error::EmbeddingSpaceMismatch(_) => "embedding_space_mismatch",
            Error::Database(_) | Error::Dependency(_) => "internal",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn not_found(id: &str) -> Error {
    Error::NotFound(format!("memory {id} not found"))
}
