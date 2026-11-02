//! Structured domain errors. Each variant has a stable machine-readable `code` so MCP
//! clients and the TUI can react without parsing prose.

use thiserror::Error;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{what} not found: {id}")]
    NotFound { what: &'static str, id: String },
    #[error("permission denied: {0}")]
    Forbidden(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid state transition for {what}: {from} -> {to}")]
    InvalidTransition {
        what: &'static str,
        from: String,
        to: String,
    },
    #[error("dependency cycle detected: {0}")]
    DependencyCycle(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("precondition failed: {0}")]
    Precondition(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl CoreError {
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::NotFound { .. } => "not_found",
            CoreError::Forbidden(_) => "forbidden",
            CoreError::InvalidInput(_) => "invalid_input",
            CoreError::InvalidTransition { .. } => "invalid_transition",
            CoreError::DependencyCycle(_) => "dependency_cycle",
            CoreError::Conflict(_) => "conflict",
            CoreError::LimitExceeded(_) => "limit_exceeded",
            CoreError::Precondition(_) => "precondition_failed",
            CoreError::Unsupported(_) => "unsupported",
            CoreError::Storage(_) => "storage_error",
            CoreError::Internal(_) => "internal_error",
        }
    }

    pub fn not_found(what: &'static str, id: impl ToString) -> Self {
        CoreError::NotFound {
            what,
            id: id.to_string(),
        }
    }

    /// JSON body used for structured MCP tool errors.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "error": { "code": self.code(), "message": self.to_string() } })
    }
}
