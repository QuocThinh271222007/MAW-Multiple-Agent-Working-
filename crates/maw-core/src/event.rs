//! Event envelope and typed event kinds. Events are an append-only audit log used by the
//! TUI timeline, the data-flow graph and crash recovery.

use crate::ids::{AgentId, EventId, ProjectId, RunId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

pub const SCHEMA_VERSION: u32 = 1;

macro_rules! event_kinds {
    ($($variant:ident => $s:expr),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum EventKind { $($variant),+ }

        impl EventKind {
            pub const ALL: &'static [EventKind] = &[$(EventKind::$variant),+];
            pub fn as_str(&self) -> &'static str {
                match self { $(EventKind::$variant => $s),+ }
            }
        }
        impl FromStr for EventKind {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s { $($s => Ok(EventKind::$variant),)+ o => Err(format!("unknown event type '{o}'")) }
            }
        }
    };
}

event_kinds! {
    AgentRegistered => "agent.registered",
    AgentConnected => "agent.connected",
    AgentDisconnected => "agent.disconnected",
    AgentStateChanged => "agent.state_changed",
    AgentMessage => "agent.message",
    AgentToolStarted => "agent.tool_started",
    AgentToolCompleted => "agent.tool_completed",
    AgentCommandStarted => "agent.command_started",
    AgentCommandCompleted => "agent.command_completed",
    AgentFileChanged => "agent.file_changed",
    AgentTurnStarted => "agent.turn_started",
    AgentTurnCompleted => "agent.turn_completed",
    AgentUsage => "agent.usage",

    GoalCreated => "goal.created",
    GoalUpdated => "goal.updated",
    GoalCompleted => "goal.completed",
    PlanRevised => "plan.revised",

    TaskCreated => "task.created",
    TaskReady => "task.ready",
    TaskAssigned => "task.assigned",
    TaskClaimed => "task.claimed",
    TaskStarted => "task.started",
    TaskProgress => "task.progress",
    TaskBlocked => "task.blocked",
    TaskUnblocked => "task.unblocked",
    TaskSubmitted => "task.submitted",
    TaskReviewRequested => "task.review_requested",
    TaskReviewed => "task.reviewed",
    TaskVerified => "task.verified",
    TaskCompleted => "task.completed",
    TaskFailed => "task.failed",
    TaskCanceled => "task.canceled",
    TaskReleased => "task.released",
    TaskLeaseExpired => "task.lease_expired",
    TaskEvidence => "task.evidence",
    ArtifactRegistered => "artifact.registered",

    MessageSent => "message.sent",
    MessageDelivered => "message.delivered",
    MessageAcked => "message.acked",
    HandoffCreated => "handoff.created",
    HandoffAccepted => "handoff.accepted",
    ContextPublished => "context.published",
    DecisionRecorded => "decision.recorded",
    HelpRequested => "agent.help_requested",

    McpToolCalled => "mcp.tool_called",
    McpToolCompleted => "mcp.tool_completed",

    PermissionRequested => "permission.requested",
    PermissionResolved => "permission.resolved",

    ProviderRateLimited => "provider.rate_limited",
    ProviderReconnecting => "provider.reconnecting",
    ProviderError => "provider.error",

    SessionRecovered => "session.recovered",
    RecoveryRequired => "recovery.required",
    WorkspaceUpdated => "workspace.updated",
    SchedulerPaused => "scheduler.paused",
    SchedulerResumed => "scheduler.resumed",
    RunStarted => "run.started",
    RunStopped => "run.stopped",
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl EventKind {
    /// High-volume events kept out of the durable log (token deltas etc.) are never
    /// represented as `EventKind`; this flags kinds that are cheap/noisy and may be
    /// pruned first by retention policy.
    pub fn is_verbose(&self) -> bool {
        matches!(
            self,
            EventKind::AgentMessage
                | EventKind::AgentToolStarted
                | EventKind::AgentToolCompleted
                | EventKind::AgentUsage
                | EventKind::McpToolCalled
                | EventKind::McpToolCompleted
        )
    }
}

/// The common event envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub event_id: EventId,
    /// Monotonic ordering assigned by the store on append (0 until persisted).
    pub sequence: i64,
    pub timestamp: DateTime<Utc>,
    pub project_id: ProjectId,
    pub run_id: Option<RunId>,
    pub agent_id: Option<AgentId>,
    pub task_id: Option<TaskId>,
    pub correlation_id: Option<String>,
    pub event_type: EventKind,
    pub payload: Value,
}

impl Event {
    pub fn new(project_id: ProjectId, kind: EventKind, payload: Value) -> Self {
        Event {
            schema_version: SCHEMA_VERSION,
            event_id: EventId::new(),
            sequence: 0,
            timestamp: Utc::now(),
            project_id,
            run_id: None,
            agent_id: None,
            task_id: None,
            correlation_id: None,
            event_type: kind,
            payload,
        }
    }
    pub fn agent(mut self, a: &AgentId) -> Self {
        self.agent_id = Some(a.clone());
        self
    }
    pub fn task(mut self, t: &TaskId) -> Self {
        self.task_id = Some(t.clone());
        self
    }
    pub fn run(mut self, r: &RunId) -> Self {
        self.run_id = Some(r.clone());
        self
    }
    pub fn correlation(mut self, c: impl Into<String>) -> Self {
        self.correlation_id = Some(c.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_roundtrip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in EventKind::ALL {
            assert!(seen.insert(k.as_str()), "duplicate {}", k.as_str());
            assert_eq!(k.as_str().parse::<EventKind>().unwrap(), *k);
        }
    }

    #[test]
    fn envelope_serializes_with_dotted_type() {
        let e = Event::new("p".into(), EventKind::TaskProgress, serde_json::json!({"pct": 5}));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["event_type"], "task.progress");
        assert_eq!(v["schema_version"], 1);
        let back: Event = serde_json::from_value(v).unwrap();
        assert_eq!(back.event_type, EventKind::TaskProgress);
    }
}
