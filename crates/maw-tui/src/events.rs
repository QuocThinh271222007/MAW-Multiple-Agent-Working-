//! Human-readable one-line descriptions of stored events (for the event stream and logs).

use chrono::{DateTime, Utc};
use maw_core::{Event, EventKind};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventClass {
    Normal,
    Good,
    Warn,
    Bad,
    Dim,
}

#[derive(Clone, Debug)]
pub struct EventLine {
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub time: String,
    pub text: String,
    pub class: EventClass,
}

fn s<'a>(p: &'a Value, k: &str) -> &'a str {
    p.get(k).and_then(|v| v.as_str()).unwrap_or("")
}

/// Events worth showing in the main event stream (high-volume chatter is excluded).
pub fn is_stream_worthy(k: EventKind) -> bool {
    !matches!(
        k,
        EventKind::AgentMessage
            | EventKind::AgentToolStarted
            | EventKind::AgentToolCompleted
            | EventKind::AgentUsage
            | EventKind::McpToolCalled
            | EventKind::McpToolCompleted
            | EventKind::AgentCommandStarted
            | EventKind::AgentCommandCompleted
            | EventKind::AgentTurnStarted
            | EventKind::AgentTurnCompleted
            | EventKind::AgentFileChanged
            | EventKind::MessageDelivered
            | EventKind::MessageAcked
            | EventKind::TaskEvidence
    )
}

/// Events shown in Logs & Diagnostics.
pub fn is_diagnostic(k: EventKind) -> bool {
    matches!(
        k,
        EventKind::ProviderError
            | EventKind::ProviderRateLimited
            | EventKind::ProviderReconnecting
            | EventKind::RecoveryRequired
            | EventKind::SessionRecovered
            | EventKind::AgentDisconnected
            | EventKind::AgentConnected
            | EventKind::TaskLeaseExpired
            | EventKind::TaskFailed
            | EventKind::TaskBlocked
            | EventKind::PermissionRequested
            | EventKind::PermissionResolved
            | EventKind::WorkspaceUpdated
            | EventKind::RunStarted
            | EventKind::RunStopped
            | EventKind::SchedulerPaused
            | EventKind::SchedulerResumed
    )
}

pub fn describe(e: &Event, name_of: &dyn Fn(&str) -> String) -> EventLine {
    let p = &e.payload;
    let agent = e
        .agent_id
        .as_ref()
        .map(|a| name_of(a.as_str()))
        .unwrap_or_default();
    let task = s(p, "task_key");
    let from = {
        let f = s(p, "from_id");
        if f.is_empty() {
            let b = s(p, "by_id");
            if b.is_empty() {
                agent.clone()
            } else {
                name_of(b)
            }
        } else {
            name_of(f)
        }
    };
    use EventClass::*;
    use EventKind::*;
    let (text, class) = match e.event_type {
        AgentRegistered => (format!("{agent} registered as {}", s(p, "role")), Dim),
        AgentConnected => (format!("{agent} connected ({})", s(p, "provider")), Good),
        AgentDisconnected => (format!("{agent} disconnected: {}", s(p, "reason")), Warn),
        AgentStateChanged => (
            format!("{agent}: {} -> {}", s(p, "from"), s(p, "state")),
            Dim,
        ),
        GoalCreated => (format!("Goal created: {}", s(p, "text")), Good),
        GoalUpdated => (
            format!(
                "Goal updated (rev {}) {}",
                p.get("revision").and_then(|v| v.as_i64()).unwrap_or(0),
                s(p, "focus")
            ),
            Normal,
        ),
        GoalCompleted => ("Goal COMPLETED".to_string(), Good),
        PlanRevised => (
            format!(
                "Plan revised -> r{}: {}",
                p.get("revision").and_then(|v| v.as_i64()).unwrap_or(0),
                s(p, "reason")
            ),
            Normal,
        ),
        TaskCreated => (
            format!("{from} -> MCP : created {task} {}", s(p, "title")),
            Normal,
        ),
        TaskReady => (format!("{task} is READY"), Dim),
        TaskAssigned => (
            format!(
                "{} -> {} : assigned {task}",
                name_of(s(p, "by_id")),
                name_of(s(p, "to_id"))
            ),
            Normal,
        ),
        TaskClaimed => (format!("{} claimed {task}", name_of(s(p, "by_id"))), Normal),
        TaskStarted => (
            format!(
                "{agent} started {task} (attempt {})",
                p.get("attempt").and_then(|v| v.as_i64()).unwrap_or(1)
            ),
            Good,
        ),
        TaskProgress => {
            let kind = s(p, "kind");
            if kind == "milestone" {
                (
                    format!("{agent} -> MCP : milestone {}", s(p, "milestone")),
                    Normal,
                )
            } else if kind == "edited" {
                (format!("{task} edited"), Dim)
            } else {
                (
                    format!(
                        "{} -> MCP : {task} {}% {}",
                        name_of(s(p, "by_id")),
                        p.get("percent").and_then(|v| v.as_i64()).unwrap_or(0),
                        s(p, "note")
                    ),
                    Normal,
                )
            }
        }
        TaskBlocked => (
            format!(
                "{} BLOCKED {task}: {}",
                name_of(s(p, "by_id")),
                s(p, "reason")
            ),
            Bad,
        ),
        TaskUnblocked => (format!("{task} unblocked"), Good),
        TaskSubmitted => (
            format!("{} -> MCP : submitted {task}", name_of(s(p, "by_id"))),
            Normal,
        ),
        TaskReviewRequested => (format!("{task} awaiting review"), Warn),
        TaskReviewed => {
            let v = s(p, "verdict");
            let who = name_of(s(p, "reviewer_id"));
            (
                format!("{who} reviewed {task}: {v}"),
                if v == "approved" { Good } else { Warn },
            )
        }
        TaskVerified => (format!("{} verified {task}", name_of(s(p, "by"))), Good),
        TaskCompleted => (format!("{task} DONE"), Good),
        TaskFailed => (format!("{task} FAILED: {}", s(p, "reason")), Bad),
        TaskCanceled => (format!("{task} canceled"), Dim),
        TaskReleased => (format!("{task} released: {}", s(p, "reason")), Warn),
        TaskLeaseExpired => (format!("{task} lease expired"), Bad),
        TaskEvidence => (
            format!(
                "{task} evidence: {} {}",
                s(p, "kind"),
                if p.get("passed").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "pass"
                } else {
                    "FAIL"
                }
            ),
            Dim,
        ),
        ArtifactRegistered => (format!("artifact {} registered", s(p, "kind")), Dim),
        MessageSent => (
            format!(
                "{} -> {} : {} \"{}\"",
                name_of(s(p, "from_id")),
                name_of(s(p, "to_id")),
                s(p, "message_type"),
                s(p, "subject")
            ),
            Normal,
        ),
        MessageDelivered => (format!("delivered to {}", name_of(s(p, "to_id"))), Dim),
        MessageAcked => (format!("{} acked", s(p, "by")), Dim),
        HandoffCreated => (
            format!(
                "{} => {} : handoff {task}",
                name_of(s(p, "from_id")),
                name_of(s(p, "to_id"))
            ),
            Warn,
        ),
        HandoffAccepted => (
            format!("{} accepted handoff {task}", name_of(s(p, "to_id"))),
            Good,
        ),
        ContextPublished => (format!("{from} published context \"{}\"", s(p, "key")), Dim),
        DecisionRecorded => (
            format!("{from} recorded decision \"{}\"", s(p, "key")),
            Normal,
        ),
        HelpRequested => (
            format!(
                "{} -> {} : help: {}",
                name_of(s(p, "from_id")),
                name_of(s(p, "to_id")),
                s(p, "topic")
            ),
            Warn,
        ),
        PermissionRequested => (
            format!("{agent} requests approval: {}", s(p, "summary")),
            Warn,
        ),
        PermissionResolved => (
            format!("approval {}: {}", s(p, "status"), s(p, "summary")),
            Normal,
        ),
        ProviderRateLimited => (format!("{agent} rate limited: {}", s(p, "detail")), Warn),
        ProviderReconnecting => (
            format!(
                "{agent} reconnecting (attempt {})",
                p.get("attempt").and_then(|v| v.as_i64()).unwrap_or(0)
            ),
            Warn,
        ),
        ProviderError => (format!("{agent} provider error: {}", s(p, "error")), Bad),
        SessionRecovered => ("session recovered from previous run".to_string(), Warn),
        RecoveryRequired => (format!("{task} needs recovery decision"), Bad),
        WorkspaceUpdated if p.get("kind").and_then(|k| k.as_str()) == Some("folder") => {
            (format!("working folder is now {}", s(p, "folder")), Good)
        }
        WorkspaceUpdated => (
            format!("workspace {} {}", s(p, "branch"), s(p, "integration")),
            Dim,
        ),
        SchedulerPaused => ("scheduling PAUSED".to_string(), Warn),
        SchedulerResumed => ("scheduling resumed".to_string(), Good),
        RunStarted => ("run started".to_string(), Dim),
        RunStopped => ("run stopped".to_string(), Dim),
        other => (other.as_str().to_string(), Dim),
    };
    EventLine {
        seq: e.sequence,
        at: e.timestamp,
        time: e.timestamp.format("%H:%M:%S").to_string(),
        text: text.replace("  ", " "),
        class,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describes_assignment_with_names() {
        let mut e = Event::new(
            "p".into(),
            EventKind::TaskAssigned,
            json!({ "task_key": "T-4", "by_id": "agt_l", "to_id": "agt_w" }),
        );
        e.sequence = 5;
        let names = |id: &str| match id {
            "agt_l" => "Leader".to_string(),
            "agt_w" => "Codex-01".to_string(),
            o => o.to_string(),
        };
        let l = describe(&e, &names);
        assert_eq!(l.text, "Leader -> Codex-01 : assigned T-4");
        assert_eq!(l.seq, 5);
    }

    #[test]
    fn noisy_events_are_excluded_from_the_stream() {
        assert!(!is_stream_worthy(EventKind::AgentToolStarted));
        assert!(!is_stream_worthy(EventKind::McpToolCalled));
        assert!(is_stream_worthy(EventKind::TaskAssigned));
        assert!(is_diagnostic(EventKind::ProviderError));
        assert!(!is_diagnostic(EventKind::TaskProgress));
    }

    #[test]
    fn every_event_kind_has_a_description() {
        for k in EventKind::ALL {
            let e = Event::new("p".into(), *k, json!({}));
            assert!(!describe(&e, &|s| s.to_string()).text.is_empty(), "{k}");
        }
    }
}
