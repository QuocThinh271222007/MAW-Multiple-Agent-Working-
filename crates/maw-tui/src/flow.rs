//! Data-flow graph derived from the stored event log. Nothing here is static: every node
//! comes from a registered agent (plus the orchestrator/MCP and the user) and every edge from
//! recorded events, with counts and recency.

use chrono::{DateTime, Utc};
use maw_core::{Agent, AgentState, Event, EventKind, Role, Task, TaskStatus};
use std::collections::HashMap;

pub const ORCHESTRATOR: &str = "orchestrator";
pub const USER: &str = "user";

#[derive(Clone, Debug, PartialEq)]
pub struct FlowNode {
    pub id: String,
    pub label: String,
    pub role: Option<Role>,
    pub state: Option<AgentState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeKind {
    Create,
    Assign,
    Claim,
    Deliver,
    Message,
    Handoff,
    Help,
    Progress,
    Submit,
    Review,
    Feedback,
    Block,
    Verify,
}

impl EdgeKind {
    pub fn label(&self) -> &'static str {
        match self {
            EdgeKind::Create => "CREATE TASKS",
            EdgeKind::Assign => "ASSIGN",
            EdgeKind::Claim => "CLAIM",
            EdgeKind::Deliver => "DELIVER",
            EdgeKind::Message => "MESSAGE",
            EdgeKind::Handoff => "HANDOFF",
            EdgeKind::Help => "HELP",
            EdgeKind::Progress => "PROGRESS",
            EdgeKind::Submit => "RESULT",
            EdgeKind::Review => "REVIEW",
            EdgeKind::Feedback => "FEEDBACK",
            EdgeKind::Block => "BLOCKER",
            EdgeKind::Verify => "VERIFY",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub count: usize,
    pub last_seq: i64,
    pub last_at: DateTime<Utc>,
    pub detail: String,
    /// Seen within the activity window (vs. historical).
    pub active: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowGraph {
    pub nodes: Vec<FlowNode>,
    /// Most recently active first.
    pub edges: Vec<FlowEdge>,
}

impl FlowGraph {
    pub fn node(&self, id: &str) -> Option<&FlowNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn label(&self, id: &str) -> String {
        self.node(id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| id.to_string())
    }
    /// Total count of edges of `kind` between `from` and `to`.
    pub fn count(&self, from: &str, to: &str, kind: EdgeKind) -> usize {
        self.edges
            .iter()
            .filter(|e| e.from == from && e.to == to && e.kind == kind)
            .map(|e| e.count)
            .sum()
    }
    /// (agent -> orchestrator, orchestrator -> agent) edge totals for one node.
    pub fn up_down(&self, id: &str) -> (usize, usize) {
        let up = self
            .edges
            .iter()
            .filter(|e| e.from == id && e.to == ORCHESTRATOR)
            .map(|e| e.count)
            .sum();
        let down = self
            .edges
            .iter()
            .filter(|e| e.from == ORCHESTRATOR && e.to == id)
            .map(|e| e.count)
            .sum();
        (up, down)
    }
    /// Edges directly between two agents (messages, handoffs, review feedback, help).
    pub fn direct_edges(&self) -> Vec<&FlowEdge> {
        self.edges
            .iter()
            .filter(|e| e.from != ORCHESTRATOR && e.to != ORCHESTRATOR)
            .collect()
    }
}

fn str_of<'a>(e: &'a Event, key: &str) -> &'a str {
    e.payload.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Derive nodes and edges. `active_secs` is the recency window for "active" edges.
pub fn derive_flow(
    agents: &[Agent],
    events: &[Event],
    now: DateTime<Utc>,
    active_secs: i64,
) -> FlowGraph {
    let mut nodes: Vec<FlowNode> = agents
        .iter()
        .map(|a| FlowNode {
            id: a.id.0.clone(),
            label: a.name.clone(),
            role: Some(a.role),
            state: Some(a.state),
        })
        .collect();
    nodes.push(FlowNode {
        id: ORCHESTRATOR.into(),
        label: "MAW MCP / Orchestrator".into(),
        role: None,
        state: None,
    });

    let mut edges: HashMap<(String, String, EdgeKind), FlowEdge> = HashMap::new();
    let mut used_user = false;

    let mut add = |from: &str, to: &str, kind: EdgeKind, e: &Event, detail: String| {
        if from.is_empty() || to.is_empty() || from == to {
            return;
        }
        let known =
            |id: &str| id == ORCHESTRATOR || id == USER || agents.iter().any(|a| a.id.0 == id);
        // map unknown pseudo ids ("handoff", ...) away; keep deleted agents out of the graph
        if !known(from) || !known(to) {
            return;
        }
        if from == USER || to == USER {
            used_user = true;
        }
        let entry = edges
            .entry((from.to_string(), to.to_string(), kind))
            .or_insert_with(|| FlowEdge {
                from: from.to_string(),
                to: to.to_string(),
                kind,
                count: 0,
                last_seq: e.sequence,
                last_at: e.timestamp,
                detail: String::new(),
                active: false,
            });
        entry.count += 1;
        if e.sequence >= entry.last_seq {
            entry.last_seq = e.sequence;
            entry.last_at = e.timestamp;
            entry.detail = detail;
        }
    };

    for e in events {
        let task = str_of(e, "task_key").to_string();
        match e.event_type {
            EventKind::TaskCreated => add(
                str_of(e, "by_id"),
                ORCHESTRATOR,
                EdgeKind::Create,
                e,
                format!("{task} {}", str_of(e, "title")),
            ),
            EventKind::TaskAssigned => {
                let by = str_of(e, "by_id");
                let by = if by == "orchestrator" || by.is_empty() {
                    ORCHESTRATOR
                } else {
                    by
                };
                let to = str_of(e, "to_id");
                if by == ORCHESTRATOR || by == USER {
                    add(ORCHESTRATOR, to, EdgeKind::Assign, e, task.clone());
                } else {
                    // leader -> MCP (decision) -> worker (dispatch)
                    add(by, ORCHESTRATOR, EdgeKind::Assign, e, task.clone());
                    add(ORCHESTRATOR, to, EdgeKind::Assign, e, task.clone());
                }
            }
            EventKind::TaskClaimed => {
                add(str_of(e, "by_id"), ORCHESTRATOR, EdgeKind::Claim, e, task)
            }
            EventKind::MessageSent => {
                let kind = if e
                    .payload
                    .get("handoff")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    EdgeKind::Handoff
                } else {
                    EdgeKind::Message
                };
                add(
                    str_of(e, "from_id"),
                    str_of(e, "to_id"),
                    kind,
                    e,
                    format!("{} {}", str_of(e, "message_type"), str_of(e, "subject")),
                );
            }
            EventKind::MessageDelivered => add(
                ORCHESTRATOR,
                str_of(e, "to_id"),
                EdgeKind::Deliver,
                e,
                String::new(),
            ),
            EventKind::HandoffCreated | EventKind::HandoffAccepted => {
                let kind = EdgeKind::Handoff;
                if e.event_type == EventKind::HandoffAccepted {
                    add(
                        str_of(e, "to_id"),
                        str_of(e, "from_id"),
                        kind,
                        e,
                        format!("accepted {task}"),
                    );
                }
                // creation is already represented by the HANDOFF message edge
            }
            EventKind::HelpRequested => add(
                str_of(e, "from_id"),
                str_of(e, "to_id"),
                EdgeKind::Help,
                e,
                str_of(e, "topic").to_string(),
            ),
            EventKind::TaskProgress => {
                let by = str_of(e, "by_id");
                if !by.is_empty() {
                    add(by, ORCHESTRATOR, EdgeKind::Progress, e, task);
                }
            }
            EventKind::TaskSubmitted => {
                add(str_of(e, "by_id"), ORCHESTRATOR, EdgeKind::Submit, e, task)
            }
            EventKind::TaskBlocked => add(
                str_of(e, "by_id"),
                ORCHESTRATOR,
                EdgeKind::Block,
                e,
                format!("{task} {}", str_of(e, "reason")),
            ),
            EventKind::TaskReviewed => {
                let reviewer = str_of(e, "reviewer_id");
                add(
                    reviewer,
                    ORCHESTRATOR,
                    EdgeKind::Review,
                    e,
                    format!("{task} {}", str_of(e, "verdict")),
                );
                if let Some(owner) = e.payload.get("owner").and_then(|v| v.as_str()) {
                    add(
                        reviewer,
                        owner,
                        EdgeKind::Feedback,
                        e,
                        format!("{task} {}", str_of(e, "verdict")),
                    );
                }
            }
            EventKind::TaskVerified => {
                add(str_of(e, "by_id"), ORCHESTRATOR, EdgeKind::Verify, e, task)
            }
            _ => {}
        }
    }

    if used_user {
        nodes.push(FlowNode {
            id: USER.into(),
            label: "user".into(),
            role: None,
            state: None,
        });
    }
    let mut edges: Vec<FlowEdge> = edges.into_values().collect();
    for e in &mut edges {
        e.active = (now - e.last_at).num_seconds() <= active_secs;
    }
    edges.sort_by(|a, b| b.last_seq.cmp(&a.last_seq).then(a.kind.cmp(&b.kind)));
    FlowGraph { nodes, edges }
}

/// Task dependency edges `(task, depends_on, satisfied)` for blocked-dependency display.
pub fn dependency_edges(tasks: &[Task]) -> Vec<(String, String, bool)> {
    let mut out = vec![];
    for t in tasks {
        if t.status == TaskStatus::Canceled {
            continue;
        }
        for d in &t.dependencies {
            if let Some(dep) = tasks.iter().find(|x| &x.id == d) {
                out.push((
                    t.key.clone(),
                    dep.key.clone(),
                    dep.status == TaskStatus::Done,
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;

    fn ev(seq: i64, kind: EventKind, payload: serde_json::Value, at: DateTime<Utc>) -> Event {
        let mut e = Event::new("p".into(), kind, payload);
        e.sequence = seq;
        e.timestamp = at;
        e
    }

    #[test]
    fn edges_are_counted_per_kind_and_direction() {
        let now = Utc::now();
        let agents = vec![
            agent("lead", Role::Leader, AgentState::Idle),
            agent("w1", Role::Worker, AgentState::Working),
        ];
        let (l, w) = (agents[0].id.0.clone(), agents[1].id.0.clone());
        let events = vec![
            ev(
                1,
                EventKind::TaskCreated,
                json!({ "by_id": l, "task_key": "T-1", "title": "A" }),
                now,
            ),
            ev(
                2,
                EventKind::TaskCreated,
                json!({ "by_id": l, "task_key": "T-2", "title": "B" }),
                now,
            ),
            ev(
                3,
                EventKind::TaskAssigned,
                json!({ "by_id": "orchestrator", "to_id": w, "task_key": "T-1" }),
                now,
            ),
            ev(
                4,
                EventKind::TaskProgress,
                json!({ "by_id": w, "task_key": "T-1", "percent": 50 }),
                now,
            ),
            ev(
                5,
                EventKind::TaskSubmitted,
                json!({ "by_id": w, "task_key": "T-1" }),
                now,
            ),
            ev(
                6,
                EventKind::MessageSent,
                json!({ "from_id": w, "to_id": l, "message_type": "QUESTION", "subject": "q" }),
                now,
            ),
        ];
        let g = derive_flow(&agents, &events, now, 30);
        assert_eq!(g.count(&l, ORCHESTRATOR, EdgeKind::Create), 2);
        assert_eq!(g.count(ORCHESTRATOR, &w, EdgeKind::Assign), 1);
        assert_eq!(g.count(&w, ORCHESTRATOR, EdgeKind::Submit), 1);
        assert_eq!(g.count(&w, &l, EdgeKind::Message), 1);
        assert_eq!(g.up_down(&w), (2, 1));
        assert_eq!(g.direct_edges().len(), 1);
        assert!(g.edges.iter().all(|e| e.active));
        // most recent first
        assert_eq!(g.edges[0].kind, EdgeKind::Message);
        assert!(g.node("orchestrator").is_some());
    }

    #[test]
    fn old_edges_are_historical_not_active() {
        let now = Utc::now();
        let agents = vec![
            agent("lead", Role::Leader, AgentState::Idle),
            agent("w1", Role::Worker, AgentState::Idle),
        ];
        let l = agents[0].id.0.clone();
        let old = now - chrono::Duration::seconds(600);
        let events = vec![ev(
            1,
            EventKind::TaskCreated,
            json!({ "by_id": l, "task_key": "T-1" }),
            old,
        )];
        let g = derive_flow(&agents, &events, now, 30);
        assert_eq!(g.edges.len(), 1);
        assert!(!g.edges[0].active);
    }

    #[test]
    fn leader_assignment_routes_through_the_orchestrator_and_review_loops_back() {
        let now = Utc::now();
        let agents = vec![
            agent("lead", Role::Leader, AgentState::Idle),
            agent("w1", Role::Worker, AgentState::Idle),
            agent("rev", Role::Reviewer, AgentState::Idle),
        ];
        let (l, w, r) = (
            agents[0].id.0.clone(),
            agents[1].id.0.clone(),
            agents[2].id.0.clone(),
        );
        let events = vec![
            ev(
                1,
                EventKind::TaskAssigned,
                json!({ "by_id": l, "to_id": w, "task_key": "T-1" }),
                now,
            ),
            ev(
                2,
                EventKind::TaskReviewed,
                json!({ "reviewer_id": r, "verdict": "rejected", "owner": w, "task_key": "T-1" }),
                now,
            ),
            ev(
                3,
                EventKind::TaskBlocked,
                json!({ "by_id": w, "task_key": "T-1", "reason": "x" }),
                now,
            ),
        ];
        let g = derive_flow(&agents, &events, now, 30);
        assert_eq!(g.count(&l, ORCHESTRATOR, EdgeKind::Assign), 1);
        assert_eq!(g.count(ORCHESTRATOR, &w, EdgeKind::Assign), 1);
        assert_eq!(
            g.count(&r, &w, EdgeKind::Feedback),
            1,
            "review feedback loop edge"
        );
        assert_eq!(g.count(&w, ORCHESTRATOR, EdgeKind::Block), 1);
    }

    #[test]
    fn unknown_ids_and_pseudo_senders_are_ignored() {
        let now = Utc::now();
        let agents = vec![agent("lead", Role::Leader, AgentState::Idle)];
        let events = vec![
            ev(
                1,
                EventKind::MessageSent,
                json!({ "from_id": "agt_gone", "to_id": "handoff" }),
                now,
            ),
            ev(
                2,
                EventKind::MessageSent,
                json!({ "from_id": "user", "to_id": agents[0].id.0 }),
                now,
            ),
        ];
        let g = derive_flow(&agents, &events, now, 30);
        assert_eq!(g.edges.len(), 1);
        assert!(
            g.node("user").is_some(),
            "the user node appears once referenced"
        );
    }

    #[test]
    fn dependency_edges_report_satisfaction() {
        let a = task("T-1", TaskStatus::Done);
        let mut b = task("T-2", TaskStatus::Backlog);
        b.dependencies.push(a.id.clone());
        let e = dependency_edges(&[a, b]);
        assert_eq!(e, vec![("T-2".to_string(), "T-1".to_string(), true)]);
    }
}
