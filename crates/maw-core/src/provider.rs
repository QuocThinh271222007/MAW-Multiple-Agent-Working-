//! Provider-neutral abstraction over coding-agent runtimes (Codex, Claude Code, mocks).
//!
//! Providers translate native protocols into [`ProviderEvent`]s. Anything provider specific
//! stays inside the adapter crates. Capability flags describe what a session really supports.

use crate::agent::{Capabilities, PermissionProfile, ProviderKind, Role, UsageMetrics};
use crate::ids::AgentId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider executable not found: {0}")]
    NotInstalled(String),
    #[error("authentication required: {0}")]
    AuthRequired(String),
    #[error("operation not supported by this provider: {0}")]
    Unsupported(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("session closed: {0}")]
    SessionClosed(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for ProviderError {
    fn from(e: std::io::Error) -> Self {
        ProviderError::Io(e.to_string())
    }
}

pub type ProviderResult<T> = Result<T, ProviderError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub kind: ProviderKind,
    pub installed: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub execution_modes: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AuthStatus {
    Authenticated {
        method: String,
        account: Option<String>,
        plan: Option<String>,
    },
    NotAuthenticated {
        hint: String,
    },
    Unknown {
        reason: String,
    },
}

impl AuthStatus {
    pub fn is_authenticated(&self) -> bool {
        matches!(self, AuthStatus::Authenticated { .. })
    }
}

/// How an agent reaches the MAW coordination MCP server. Identity is carried by the
/// bearer token, which MAW minted for this specific agent.
#[derive(Clone, Debug)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub agent_id: AgentId,
    pub name: String,
    pub role: Role,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission: PermissionProfile,
    pub mcp: Option<McpEndpoint>,
    /// Role instructions appended to the provider's system/developer prompt.
    pub instructions: String,
    pub env: HashMap<String, String>,
    /// Provider-native id to resume, if any.
    pub resume_native_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AgentInput {
    pub text: String,
    /// Allow the adapter to steer an in-flight turn instead of queueing a new one.
    pub steer_if_active: bool,
}

impl AgentInput {
    pub fn text(t: impl Into<String>) -> Self {
        AgentInput {
            text: t.into(),
            steer_if_active: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Command,
    FileChange,
    Permissions,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    ApproveForSession,
    Deny,
    /// Deny and interrupt the turn.
    Abort,
}

/// Normalized events emitted by every provider adapter.
#[derive(Clone, Debug)]
pub enum ProviderEvent {
    Connected {
        native_session_id: Option<String>,
    },
    TurnStarted {
        turn_id: String,
    },
    /// Streaming text chunk (not persisted as a durable event).
    MessageDelta {
        text: String,
    },
    /// A complete assistant message.
    Message {
        text: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
        detail: Option<String>,
    },
    ToolCompleted {
        call_id: String,
        name: String,
        ok: bool,
        detail: Option<String>,
    },
    CommandStarted {
        call_id: String,
        command: String,
        cwd: Option<String>,
    },
    CommandOutput {
        call_id: String,
        chunk: String,
    },
    CommandCompleted {
        call_id: String,
        exit_code: Option<i64>,
    },
    FileChanged {
        paths: Vec<String>,
    },
    ApprovalRequested {
        request_id: String,
        kind: ApprovalKind,
        summary: String,
        reason: Option<String>,
        paths: Vec<String>,
        details: serde_json::Value,
    },
    TurnCompleted {
        turn_id: String,
        outcome: TurnOutcome,
        error: Option<String>,
    },
    Usage(UsageMetrics),
    RateLimited {
        retry_after_ms: Option<u64>,
        detail: String,
    },
    Error {
        message: String,
        fatal: bool,
    },
    Disconnected {
        reason: String,
    },
}

/// A live provider session. All methods take `&self` so sessions can be shared.
#[async_trait]
pub trait AgentSession: Send + Sync {
    fn native_session_id(&self) -> Option<String>;
    fn capabilities(&self) -> Capabilities;
    /// Begin a new turn (or steer the active one when allowed and supported).
    async fn send_input(&self, input: AgentInput) -> ProviderResult<()>;
    async fn interrupt(&self) -> ProviderResult<()>;
    async fn resolve_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> ProviderResult<()>;
    /// Terminate the session and release every owned process.
    async fn shutdown(&self) -> ProviderResult<()>;
}

pub type SessionHandle = Arc<dyn AgentSession>;
pub type EventReceiver = mpsc::Receiver<ProviderEvent>;

#[async_trait]
pub trait AgentProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    async fn detect(&self) -> ProviderResult<ProviderInfo>;
    async fn auth_status(&self) -> ProviderResult<AuthStatus>;
    /// Start a session (or resume when `cfg.resume_native_id` is set and supported).
    async fn start_session(
        &self,
        cfg: SessionConfig,
    ) -> ProviderResult<(SessionHandle, EventReceiver)>;
}

/// Bounded event channel capacity: slow consumers apply backpressure to the adapter instead
/// of growing memory without limit.
pub const PROVIDER_EVENT_BUFFER: usize = 1024;
