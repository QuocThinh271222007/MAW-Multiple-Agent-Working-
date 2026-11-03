//! Agent identity, roles, permission profiles and the validated execution state machine.

use crate::ids::{AgentId, ProjectId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Leader,
    Worker,
    Reviewer,
    Specialist,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Leader => "leader",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Specialist => "specialist",
        }
    }
    /// Roles that execute implementation tasks.
    pub fn is_executor(&self) -> bool {
        matches!(self, Role::Worker | Role::Specialist)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "leader" => Ok(Role::Leader),
            "worker" => Ok(Role::Worker),
            "reviewer" => Ok(Role::Reviewer),
            "specialist" => Ok(Role::Specialist),
            other => Err(format!("unknown role '{other}'")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Claude,
    Codex,
    /// Local models served by Ollama; MAW runs the agent loop itself.
    Ollama,
    /// Deterministic in-process provider used by tests and `maw demo`.
    Mock,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Claude => "claude",
            ProviderKind::Codex => "codex",
            ProviderKind::Ollama => "ollama",
            ProviderKind::Mock => "mock",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Ok(ProviderKind::Claude),
            "codex" => Ok(ProviderKind::Codex),
            "ollama" | "local" => Ok(ProviderKind::Ollama),
            "mock" => Ok(ProviderKind::Mock),
            other => Err(format!("unknown provider '{other}'")),
        }
    }
}

/// High level permission profile. Providers map these onto their own approval/sandbox
/// settings; MAW additionally enforces workspace boundaries independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    /// Read-only; everything else needs approval.
    Safe,
    /// Controlled workspace writes; approval outside the workspace boundary.
    #[default]
    Balanced,
    /// Broad execution inside the configured sandbox and workspace.
    Autonomous,
}

impl PermissionProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionProfile::Safe => "safe",
            PermissionProfile::Balanced => "balanced",
            PermissionProfile::Autonomous => "autonomous",
        }
    }
}

impl FromStr for PermissionProfile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "safe" => Ok(Self::Safe),
            "balanced" => Ok(Self::Balanced),
            "autonomous" => Ok(Self::Autonomous),
            other => Err(format!("unknown permission profile '{other}'")),
        }
    }
}

/// Execution state of an agent. Transitions are validated by [`AgentState::can_transition_to`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentState {
    Disconnected,
    Connecting,
    AuthRequired,
    Idle,
    Thinking,
    Working,
    WaitingApproval,
    WaitingDependency,
    Blocked,
    RateLimited,
    Recovering,
    Completed,
    Failed,
    Stopped,
}

impl AgentState {
    pub const ALL: [AgentState; 14] = [
        AgentState::Disconnected,
        AgentState::Connecting,
        AgentState::AuthRequired,
        AgentState::Idle,
        AgentState::Thinking,
        AgentState::Working,
        AgentState::WaitingApproval,
        AgentState::WaitingDependency,
        AgentState::Blocked,
        AgentState::RateLimited,
        AgentState::Recovering,
        AgentState::Completed,
        AgentState::Failed,
        AgentState::Stopped,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Disconnected => "DISCONNECTED",
            AgentState::Connecting => "CONNECTING",
            AgentState::AuthRequired => "AUTH_REQUIRED",
            AgentState::Idle => "IDLE",
            AgentState::Thinking => "THINKING",
            AgentState::Working => "WORKING",
            AgentState::WaitingApproval => "WAITING_APPROVAL",
            AgentState::WaitingDependency => "WAITING_DEPENDENCY",
            AgentState::Blocked => "BLOCKED",
            AgentState::RateLimited => "RATE_LIMITED",
            AgentState::Recovering => "RECOVERING",
            AgentState::Completed => "COMPLETED",
            AgentState::Failed => "FAILED",
            AgentState::Stopped => "STOPPED",
        }
    }

    /// True while the agent is executing a turn (and must not receive a new one).
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            AgentState::Thinking
                | AgentState::Working
                | AgentState::WaitingApproval
                | AgentState::WaitingDependency
        )
    }

    /// True when the agent can accept a new turn.
    pub fn is_available(&self) -> bool {
        matches!(self, AgentState::Idle | AgentState::Completed)
    }

    /// True when the underlying session is gone or unusable.
    pub fn is_offline(&self) -> bool {
        matches!(
            self,
            AgentState::Disconnected
                | AgentState::AuthRequired
                | AgentState::Failed
                | AgentState::Stopped
        )
    }

    pub fn can_transition_to(&self, to: AgentState) -> bool {
        use AgentState::*;
        if *self == to {
            return true;
        }
        // Any state may collapse to a terminal-ish offline state.
        if matches!(to, Disconnected | Failed | Stopped) {
            return true;
        }
        let active = [
            Thinking,
            Working,
            WaitingApproval,
            WaitingDependency,
            Blocked,
        ];
        match self {
            Disconnected => matches!(to, Connecting),
            Connecting => matches!(to, AuthRequired | Idle),
            AuthRequired => matches!(to, Connecting),
            Idle | Completed => {
                active.contains(&to) || matches!(to, RateLimited | Recovering | Idle | Connecting)
            }
            Thinking | Working | WaitingApproval | WaitingDependency | Blocked => {
                active.contains(&to) || matches!(to, Idle | Completed | RateLimited | Recovering)
            }
            RateLimited => active.contains(&to) || matches!(to, Idle | Recovering | Connecting),
            Recovering => matches!(to, Connecting | Idle | Thinking | Working | RateLimited),
            Failed => matches!(to, Recovering | Connecting),
            Stopped => matches!(to, Connecting),
        }
    }
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgentState::ALL
            .iter()
            .copied()
            .find(|st| st.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown agent state '{s}'"))
    }
}

/// What a provider session can do. Callers must check before relying on a feature.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub interrupt: bool,
    pub resume: bool,
    /// Can inject input while a turn is active.
    pub steer_active_turn: bool,
    /// Provider emits approval requests MAW can answer.
    pub approvals: bool,
    pub mcp_http: bool,
    pub usage_reporting: bool,
    pub model_selection: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageMetrics {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    /// `true` when the numbers were reported by the provider (never estimated).
    pub reported_by_provider: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub project_id: ProjectId,
    pub name: String,
    pub provider: ProviderKind,
    pub model: Option<String>,
    /// Reasoning effort for the provider (e.g. low, medium, high, xhigh, max); `None` = provider default.
    pub effort: Option<String>,
    pub role: Role,
    pub state: AgentState,
    pub permission: PermissionProfile,
    pub session_id: Option<String>,
    pub native_session_id: Option<String>,
    pub workspace: Option<String>,
    pub current_task_id: Option<TaskId>,
    pub capabilities: Capabilities,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub usage: UsageMetrics,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_transition_always_ok() {
        for s in AgentState::ALL {
            assert!(s.can_transition_to(s));
        }
    }

    #[test]
    fn cannot_skip_connecting() {
        assert!(!AgentState::Disconnected.can_transition_to(AgentState::Working));
        assert!(!AgentState::Disconnected.can_transition_to(AgentState::Idle));
        assert!(AgentState::Disconnected.can_transition_to(AgentState::Connecting));
        assert!(AgentState::Connecting.can_transition_to(AgentState::Idle));
    }

    #[test]
    fn auth_required_only_reconnects() {
        assert!(!AgentState::AuthRequired.can_transition_to(AgentState::Working));
        assert!(AgentState::AuthRequired.can_transition_to(AgentState::Connecting));
    }

    #[test]
    fn working_cycle() {
        assert!(AgentState::Idle.can_transition_to(AgentState::Thinking));
        assert!(AgentState::Thinking.can_transition_to(AgentState::Working));
        assert!(AgentState::Working.can_transition_to(AgentState::WaitingApproval));
        assert!(AgentState::WaitingApproval.can_transition_to(AgentState::Working));
        assert!(AgentState::Working.can_transition_to(AgentState::Idle));
    }

    #[test]
    fn failed_requires_recovery_or_reconnect() {
        assert!(!AgentState::Failed.can_transition_to(AgentState::Working));
        assert!(AgentState::Failed.can_transition_to(AgentState::Recovering));
    }

    #[test]
    fn roundtrip_str() {
        for s in AgentState::ALL {
            assert_eq!(s.as_str().parse::<AgentState>().unwrap(), s);
        }
    }
}

/// Validate a user supplied effort level. `default`/empty mean "provider default" (`None`).
/// The set of valid levels is provider- and model-specific (see `maw models`), so only the
/// shape is checked here; the provider reports an unsupported level when the session starts.
pub fn normalize_effort(input: &str) -> Result<Option<String>, String> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() || s == "default" || s == "auto" {
        return Ok(None);
    }
    if s.len() > 20
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "'{input}' is not a valid effort level (use e.g. low, medium, high, xhigh, max)"
        ));
    }
    Ok(Some(s))
}

#[cfg(test)]
mod effort_tests {
    use super::*;

    #[test]
    fn effort_normalisation() {
        assert_eq!(normalize_effort(" High ").unwrap().as_deref(), Some("high"));
        assert_eq!(normalize_effort("default").unwrap(), None);
        assert_eq!(normalize_effort("").unwrap(), None);
        assert!(normalize_effort("high; rm -rf").is_err());
        assert!(normalize_effort(&"x".repeat(30)).is_err());
    }
}
