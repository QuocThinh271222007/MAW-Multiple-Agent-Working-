//! The board: an unbounded node graph of the whole project, in the spirit of Blender's node editor.
//!
//! Everything on it is derived from live state, nothing is drawn by hand: the objective, the plan,
//! every task (placed by how deep it sits in the dependency chain), every agent, each agent's
//! conversation, and the latest messages between them. Nodes can be moved and the view panned and
//! zoomed without limit; moved positions are remembered.
//!
//! Positions are in *grid units* (one unit = one node slot), so zooming scales the whole layout
//! instead of moving nodes around: the zoom level only decides how much of each node is drawn.
//! This module is pure (no terminal, no I/O) so the layout and the navigation can be tested.

use crate::app::App;
use maw_core::{AgentState, Task, TaskStatus};
use std::collections::{HashMap, HashSet};

/// Which thing a node stands for. The string forms are what is stored in the saved layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKey {
    User,
    Goal,
    Plan,
    Task(String),
    Agent(String),
    Chat(String),
    Mail(String),
}

impl NodeKey {
    pub fn id(&self) -> String {
        match self {
            NodeKey::User => "user".into(),
            NodeKey::Goal => "goal".into(),
            NodeKey::Plan => "plan".into(),
            NodeKey::Task(k) => format!("task:{k}"),
            NodeKey::Agent(a) => format!("agent:{a}"),
            NodeKey::Chat(a) => format!("chat:{a}"),
            NodeKey::Mail(m) => format!("mail:{m}"),
        }
    }
    pub fn parse(s: &str) -> Option<NodeKey> {
        Some(match s {
            "user" => NodeKey::User,
            "goal" => NodeKey::Goal,
            "plan" => NodeKey::Plan,
            _ => {
                let (k, v) = s.split_once(':')?;
                let v = v.to_string();
                match k {
                    "task" => NodeKey::Task(v),
                    "agent" => NodeKey::Agent(v),
                    "chat" => NodeKey::Chat(v),
                    "mail" => NodeKey::Mail(v),
                    _ => return None,
                }
            }
        })
    }
}

/// How a node is coloured: what state it is in, not what kind it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Active,
    Warn,
    Bad,
    Dim,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub key: NodeKey,
    pub icon: char,
    /// Short label in the border: kind and state ("T-3 RUNNING").
    pub tag: String,
    /// What the smallest zoom levels call it: a task key, an agent's name ...
    pub short: String,
    pub title: String,
    /// Detail lines for the inspector and for the largest zoom.
    pub details: Vec<String>,
    pub tone: Tone,
    /// Position in grid units (top-left of the slot).
    pub at: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// objective -> plan -> first tasks
    Plan,
    /// a task waits for another one
    Depends,
    /// a task belongs to an agent
    Assigned,
    /// an agent and its conversation
    Talks,
    /// a message travelling between two parties
    Mail,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: NodeKey,
    pub to: NodeKey,
    pub kind: EdgeKind,
    /// Happening now (a running task, a busy agent, a message from the last minute).
    pub active: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Scene {
    pub fn node(&self, k: &NodeKey) -> Option<&Node> {
        self.nodes.iter().find(|n| &n.key == k)
    }
}

// ---------------------------------------------------------------- geometry

/// Zoom is a smooth scale, 16 steps of about 22% each from a tenth of the normal size (a dot per
/// node, to see a very large board at once) to more than twice it (cards with all their details).
pub const ZOOM_LEVELS: u8 = 16;
/// The level whose scale is about 1: cards of 30x5 cells.
pub const DEFAULT_ZOOM: u8 = 11;
/// Bumped when the meaning of the saved zoom level changes (old saves keep their places only).
pub const ZOOM_SCALE_VERSION: u64 = 2;

/// The scale factor of a zoom level.
pub fn scale(zoom: u8) -> f32 {
    0.12 * 20f32.powf(zoom.min(ZOOM_LEVELS - 1) as f32 / (ZOOM_LEVELS - 1) as f32)
}

/// Size of a node's card in terminal cells at a zoom level. One row tall means a one-line label
/// (icon and name), taller means a framed card with a title and details.
pub fn card(zoom: u8) -> (u16, u16) {
    let s = scale(zoom);
    let w = (30.0 * s).round().clamp(4.0, 72.0) as u16;
    let h = if s < 0.55 {
        1
    } else {
        (5.0 * s).round().clamp(4.0, 14.0) as u16
    };
    (w, h)
}

/// Distance between neighbouring slots (card plus the gap the edges run through).
pub fn pitch(zoom: u8) -> (u16, u16) {
    let s = scale(zoom);
    let (w, h) = card(zoom);
    let gap_w = (10.0 * s).round().max(2.0) as u16;
    let gap_h = if h == 1 {
        1
    } else {
        (2.0 * s).round().max(2.0) as u16
    };
    (w + gap_w, h + gap_h)
}

// ---------------------------------------------------------------- scene

fn task_num(k: &str) -> (u64, String) {
    (
        k.rsplit('-')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(u64::MAX),
        k.to_string(),
    )
}

fn task_tone(s: TaskStatus) -> Tone {
    match s {
        TaskStatus::Done => Tone::Ok,
        TaskStatus::Running | TaskStatus::Assigned => Tone::Active,
        TaskStatus::Verifying | TaskStatus::InReview => Tone::Warn,
        TaskStatus::Blocked | TaskStatus::Failed => Tone::Bad,
        TaskStatus::Canceled => Tone::Dim,
        _ => Tone::Plain,
    }
}

fn agent_tone(s: AgentState) -> Tone {
    match s {
        AgentState::Working | AgentState::Thinking => Tone::Active,
        AgentState::WaitingApproval | AgentState::RateLimited | AgentState::Connecting => {
            Tone::Warn
        }
        AgentState::Failed | AgentState::AuthRequired | AgentState::Blocked => Tone::Bad,
        AgentState::Disconnected => Tone::Dim,
        _ => Tone::Plain,
    }
}

fn depth_of(
    t: &Task,
    by_id: &HashMap<String, &Task>,
    memo: &mut HashMap<String, usize>,
    visiting: &mut HashSet<String>,
) -> usize {
    if let Some(d) = memo.get(&t.id.0) {
        return *d;
    }
    if !visiting.insert(t.id.0.clone()) {
        return 0; // a cycle cannot happen (the engine refuses it) but must not hang the drawing
    }
    let d = t
        .dependencies
        .iter()
        .filter_map(|d| by_id.get(&d.0))
        .map(|dep| depth_of(dep, by_id, memo, visiting) + 1)
        .max()
        .unwrap_or(0);
    visiting.remove(&t.id.0);
    memo.insert(t.id.0.clone(), d);
    d
}

fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

/// Build the graph from the current state, with the positions the user moved nodes to applied.
pub fn build_scene(app: &App) -> Scene {
    let mut nodes: Vec<Node> = vec![];
    let mut edges: Vec<Edge> = vec![];
    let tasks = app.tasks();
    let agents = app.agents();
    let snap = app.snap.as_ref();
    let goal = snap.and_then(|s| s.goal.as_ref());
    let plan = snap.and_then(|s| s.plan.as_ref());
    let fresh = |at: chrono::DateTime<chrono::Utc>| (app.now - at).num_seconds() < 45;

    // ---- tasks: one column per depth, rows in key order
    let by_id: HashMap<String, &Task> = tasks.iter().map(|t| (t.id.0.clone(), t)).collect();
    let mut memo = HashMap::new();
    let mut order: Vec<&Task> = tasks.iter().collect();
    order.sort_by_key(|t| task_num(&t.key));
    let mut next_row: HashMap<usize, usize> = HashMap::new();
    let mut task_at: HashMap<String, (f32, f32)> = HashMap::new();
    let mut max_depth = 0usize;
    for t in &order {
        let d = depth_of(t, &by_id, &mut memo, &mut HashSet::new());
        max_depth = max_depth.max(d);
        let row = next_row.entry(d).or_insert(0);
        task_at.insert(t.key.clone(), ((2 + d) as f32, *row as f32));
        *row += 1;
    }
    let task_rows = next_row.values().copied().max().unwrap_or(0);
    let mid = if task_rows > 0 {
        (task_rows as f32 - 1.0) / 2.0
    } else {
        0.0
    };
    let c_agent = (2 + max_depth + 1) as f32;

    // ---- the user, the objective, the plan
    let has_user_mail = snap.is_some_and(|s| {
        s.recent_messages
            .iter()
            .any(|m| m.sender_agent_id.as_str() == "user")
    });
    if goal.is_some() || has_user_mail {
        nodes.push(Node {
            key: NodeKey::User,
            icon: '☺',
            tag: "YOU".into(),
            short: "you".into(),
            title: "you".into(),
            details: vec!["Your messages and the objective start here.".into()],
            tone: Tone::Plain,
            at: (0.0, mid - 1.6),
        });
    }
    if let Some(g) = goal {
        nodes.push(Node {
            key: NodeKey::Goal,
            icon: '◎',
            tag: format!("OBJECTIVE {}", g.status.as_str().to_uppercase()),
            short: "goal".into(),
            title: clip(&g.text, 80),
            details: {
                let mut d = vec![g.text.clone()];
                for c in &g.success_criteria {
                    d.push(format!("✓ {c}"));
                }
                if let Some(m) = &g.current_milestone {
                    d.push(format!("milestone: {m}"));
                }
                d.push(format!("revision {}", g.revision));
                d
            },
            tone: match g.status.as_str() {
                "completed" => Tone::Ok,
                "canceled" | "failed" => Tone::Bad,
                _ => Tone::Active,
            },
            at: (0.0, mid),
        });
        if has_user_mail || goal.is_some() {
            edges.push(Edge {
                from: NodeKey::User,
                to: NodeKey::Goal,
                kind: EdgeKind::Plan,
                active: fresh(g.updated_at),
            });
        }
    }
    if let Some(p) = plan {
        nodes.push(Node {
            key: NodeKey::Plan,
            icon: '▤',
            tag: format!("PLAN r{}", p.revision),
            short: format!("plan r{}", p.revision),
            title: clip(&p.summary, 80),
            details: {
                let mut d = vec![p.summary.clone()];
                for m in &p.milestones {
                    d.push(format!(
                        "{} {}{}",
                        if m.done { "✓" } else { "○" },
                        m.title,
                        if m.task_keys.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", m.task_keys.join(" "))
                        }
                    ));
                }
                for a in &p.next_actions {
                    d.push(format!("next: {a}"));
                }
                if !p.reason.is_empty() {
                    d.push(format!("why: {}", p.reason));
                }
                d
            },
            tone: Tone::Plain,
            at: (1.0, mid),
        });
        if goal.is_some() {
            edges.push(Edge {
                from: NodeKey::Goal,
                to: NodeKey::Plan,
                kind: EdgeKind::Plan,
                active: fresh(p.created_at),
            });
        }
    }

    // ---- tasks and what links them
    let root = if plan.is_some() {
        Some(NodeKey::Plan)
    } else if goal.is_some() {
        Some(NodeKey::Goal)
    } else {
        None
    };
    for t in &order {
        let owner = t
            .owner_agent_id
            .as_ref()
            .and_then(|o| agents.iter().find(|a| &a.id == o));
        let mut details = vec![format!(
            "{} · {}% reported · priority {}",
            t.status,
            t.progress,
            t.priority.as_str()
        )];
        details.push(format!(
            "owner: {}",
            owner.map(|a| a.name.as_str()).unwrap_or("nobody yet")
        ));
        if !t.dependencies.is_empty() {
            let keys: Vec<&str> = t
                .dependencies
                .iter()
                .filter_map(|d| by_id.get(&d.0))
                .map(|d| d.key.as_str())
                .collect();
            details.push(format!("waits for: {}", keys.join(", ")));
        }
        if !t.description.is_empty() {
            details.push(t.description.clone());
        }
        for c in t.acceptance_criteria.iter().take(4) {
            details.push(format!("✓ {c}"));
        }
        details.push(format!("evidence: {} item(s)", t.evidence.len()));
        if let Some(b) = &t.blocked_reason {
            details.push(format!("blocked: {b}"));
        }
        nodes.push(Node {
            key: NodeKey::Task(t.key.clone()),
            icon: match t.status {
                TaskStatus::Done => '✔',
                TaskStatus::Blocked | TaskStatus::Failed => '✖',
                TaskStatus::Running => '▶',
                _ => '■',
            },
            tag: format!("{} {}", t.key, t.status),
            short: t.key.clone(),
            title: t.title.clone(),
            details,
            tone: task_tone(t.status),
            at: task_at[&t.key],
        });
        if t.dependencies.is_empty() {
            if let Some(r) = &root {
                edges.push(Edge {
                    from: r.clone(),
                    to: NodeKey::Task(t.key.clone()),
                    kind: EdgeKind::Plan,
                    active: false,
                });
            }
        }
        for d in &t.dependencies {
            if let Some(dep) = by_id.get(&d.0) {
                edges.push(Edge {
                    from: NodeKey::Task(dep.key.clone()),
                    to: NodeKey::Task(t.key.clone()),
                    kind: EdgeKind::Depends,
                    active: dep.status == TaskStatus::Running,
                });
            }
        }
        if let Some(o) = owner {
            edges.push(Edge {
                from: NodeKey::Task(t.key.clone()),
                to: NodeKey::Agent(o.id.0.clone()),
                kind: EdgeKind::Assigned,
                active: matches!(t.status, TaskStatus::Running | TaskStatus::Verifying),
            });
        }
    }

    // ---- agents (beside the rows of their tasks), their conversations
    let row_of_agent = |id: &str| -> Option<f32> {
        let rows: Vec<f32> = tasks
            .iter()
            .filter(|t| t.owner_agent_id.as_ref().map(|o| o.0.as_str()) == Some(id))
            .filter_map(|t| task_at.get(&t.key).map(|p| p.1))
            .collect();
        (!rows.is_empty()).then(|| rows.iter().sum::<f32>() / rows.len() as f32)
    };
    let mut wanted: Vec<(f32, String, usize)> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| (row_of_agent(&a.id.0).unwrap_or(f32::MAX), a.name.clone(), i))
        .collect();
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut used_rows: HashSet<i32> = HashSet::new();
    let mut agent_row: HashMap<usize, f32> = HashMap::new();
    let mut spill = task_rows as i32;
    for (want, _, i) in &wanted {
        let mut r = if want.is_finite() && *want < f32::MAX {
            want.round() as i32
        } else {
            let r = spill;
            spill += 1;
            r
        };
        while used_rows.contains(&r) {
            r += 1;
        }
        used_rows.insert(r);
        agent_row.insert(*i, r as f32);
    }
    for (i, a) in agents.iter().enumerate() {
        let row = agent_row[&i];
        let cur = a
            .current_task_id
            .as_ref()
            .and_then(|id| by_id.get(&id.0))
            .map(|t| t.key.clone());
        let mut details = vec![
            format!(
                "{} · {}{}",
                a.provider,
                a.model.as_deref().unwrap_or("default model"),
                a.effort
                    .as_ref()
                    .map(|e| format!(" · effort {e}"))
                    .unwrap_or_default()
            ),
            format!("state: {}", a.state),
            format!("permission: {}", a.permission.as_str()),
            format!("working on: {}", cur.as_deref().unwrap_or("nothing")),
        ];
        // the branch this agent works on, and where its work stands
        if let Some(w) = snap.and_then(|s| {
            s.workspaces
                .iter()
                .find(|w| w.agent_id.as_ref() == Some(&a.id) && w.kind == "worktree")
        }) {
            details.push(format!(
                "branch: {} ({}{})",
                w.branch.as_deref().unwrap_or("?"),
                w.integration_status,
                if w.changed_files.is_empty() {
                    String::new()
                } else {
                    format!(", {} file(s) changed", w.changed_files.len())
                }
            ));
        }
        if let Some(t) = a
            .usage
            .total_tokens
            .filter(|_| a.usage.reported_by_provider)
        {
            details.push(format!("tokens: {}", crate::usage::fmt_tokens(t)));
        }
        if let Some(e) = &a.last_error {
            details.push(format!("error: {e}"));
        }
        nodes.push(Node {
            key: NodeKey::Agent(a.id.0.clone()),
            icon: match a.role {
                maw_core::Role::Leader => '♛',
                maw_core::Role::Reviewer => '✎',
                _ => '⚙',
            },
            tag: format!("{} {}", a.role.as_str().to_uppercase(), a.state),
            short: a.name.clone(),
            title: a.name.clone(),
            details,
            tone: agent_tone(a.state),
            at: (c_agent, row),
        });
        // the conversation: its last lines as the node's details
        let lines = app.transcripts.get(&a.id);
        let tail: Vec<String> = lines
            .map(|l| {
                l.iter()
                    .rev()
                    .filter(|x| {
                        matches!(
                            x.kind,
                            maw_orchestrator::LineKind::User
                                | maw_orchestrator::LineKind::Assistant
                                | maw_orchestrator::LineKind::Error
                        )
                    })
                    .take(5)
                    .map(|x| {
                        let who = match x.kind {
                            maw_orchestrator::LineKind::User => "›",
                            maw_orchestrator::LineKind::Error => "!",
                            _ => "◂",
                        };
                        format!("{who} {}", clip(&x.text.replace('\n', " "), 120))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let n_lines = lines.map(|l| l.len()).unwrap_or(0);
        let mut details = vec![format!(
            "{n_lines} lines in the conversation (Enter opens it)"
        )];
        details.extend(tail.into_iter().rev());
        nodes.push(Node {
            key: NodeKey::Chat(a.id.0.clone()),
            icon: '✉',
            tag: "CONVERSATION".into(),
            short: format!("chat {}", a.name),
            title: format!("chat with {}", a.name),
            details,
            tone: if a.state.is_busy() {
                Tone::Active
            } else {
                Tone::Dim
            },
            at: (c_agent + 1.0, row),
        });
        edges.push(Edge {
            from: NodeKey::Agent(a.id.0.clone()),
            to: NodeKey::Chat(a.id.0.clone()),
            kind: EdgeKind::Talks,
            active: a.state.is_busy(),
        });
    }

    // ---- the latest messages, between the parties
    if let Some(s) = snap {
        let mut msgs: Vec<&maw_core::Message> = s.recent_messages.iter().collect();
        msgs.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        msgs.truncate(6);
        msgs.reverse();
        for (i, m) in msgs.iter().enumerate() {
            let from = if m.sender_agent_id.as_str() == "user" {
                Some(NodeKey::User)
            } else {
                agents
                    .iter()
                    .find(|a| a.id == m.sender_agent_id)
                    .map(|a| NodeKey::Agent(a.id.0.clone()))
            };
            let to = agents
                .iter()
                .find(|a| a.id == m.recipient_agent_id)
                .map(|a| NodeKey::Agent(a.id.0.clone()));
            let (Some(from), Some(to)) = (from, to) else {
                continue;
            };
            let name = |id: &maw_core::AgentId| app.agent_name(id.as_str());
            nodes.push(Node {
                key: NodeKey::Mail(m.id.0.clone()),
                icon: '✉',
                tag: format!("{}", m.message_type),
                short: "mail".into(),
                title: if m.subject.trim().is_empty() {
                    clip(&m.content, 60)
                } else {
                    m.subject.clone()
                },
                details: vec![
                    format!(
                        "{} → {}",
                        name(&m.sender_agent_id),
                        name(&m.recipient_agent_id)
                    ),
                    m.content.clone(),
                    if m.acknowledged_at.is_some() {
                        "acknowledged".into()
                    } else if m.delivered_at.is_some() {
                        "delivered, not acknowledged".into()
                    } else {
                        "waiting to be delivered".into()
                    },
                ],
                tone: if m.acknowledged_at.is_some() {
                    Tone::Dim
                } else {
                    Tone::Plain
                },
                at: (c_agent + 2.0, i as f32),
            });
            let active = fresh(m.created_at);
            edges.push(Edge {
                from,
                to: NodeKey::Mail(m.id.0.clone()),
                kind: EdgeKind::Mail,
                active,
            });
            edges.push(Edge {
                from: NodeKey::Mail(m.id.0.clone()),
                to,
                kind: EdgeKind::Mail,
                active,
            });
        }
    }

    // ---- where the user put things
    for n in &mut nodes {
        if let Some(p) = app.canvas.moved.get(&n.key.id()) {
            n.at = *p;
        }
    }
    // an edge only exists between nodes that exist
    let present: HashSet<&NodeKey> = nodes.iter().map(|n| &n.key).collect();
    edges.retain(|e| present.contains(&e.from) && present.contains(&e.to));
    Scene { nodes, edges }
}

// ---------------------------------------------------------------- navigation

/// The node to select when moving in a direction (`dx`, `dy` each -1, 0 or 1): the nearest one in
/// that half-plane, preferring the ones straight ahead.
pub fn neighbor(scene: &Scene, from: &NodeKey, dx: i32, dy: i32) -> Option<NodeKey> {
    let a = scene.node(from)?.at;
    scene
        .nodes
        .iter()
        .filter(|n| &n.key != from)
        .filter_map(|n| {
            let (rx, ry) = (n.at.0 - a.0, n.at.1 - a.1);
            let (along, across) = if dx != 0 {
                (rx * dx as f32, ry.abs())
            } else {
                (ry * dy as f32, rx.abs())
            };
            // only what lies ahead, and not mostly sideways
            (along > 0.05 && across <= along * 2.5 + 0.5)
                .then(|| (along + across * 2.0, n.key.clone()))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, k)| k)
}

/// Next node in reading order (columns left to right, rows top to bottom) for Tab.
pub fn cycle(scene: &Scene, from: Option<&NodeKey>, back: bool) -> Option<NodeKey> {
    let mut order: Vec<&Node> = scene.nodes.iter().collect();
    order.sort_by(|a, b| a.at.0.total_cmp(&b.at.0).then(a.at.1.total_cmp(&b.at.1)));
    if order.is_empty() {
        return None;
    }
    let cur = from.and_then(|k| order.iter().position(|n| &n.key == k));
    let i = match (cur, back) {
        (None, false) => 0,
        (None, true) => order.len() - 1,
        (Some(i), false) => (i + 1) % order.len(),
        (Some(i), true) => (i + order.len() - 1) % order.len(),
    };
    Some(order[i].key.clone())
}

/// Smallest and largest grid position of the nodes, `None` when the board is empty.
pub fn bounds(scene: &Scene) -> Option<((f32, f32), (f32, f32))> {
    let mut it = scene.nodes.iter().map(|n| n.at);
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), p| {
        (
            (lo.0.min(p.0), lo.1.min(p.1)),
            (hi.0.max(p.0), hi.1.max(p.1)),
        )
    }))
}

// ---------------------------------------------------------------- view state

/// A rectangle on screen: x, y, width, height.
pub type Rect4 = (i32, i32, u16, u16);

/// What the user is doing with the mouse right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Drag {
    /// Moving a node; `grab` is where inside the node it was taken.
    Node { key: NodeKey, grab: (i32, i32) },
    /// Panning the whole view; the origin and the pointer when it began.
    Pan {
        origin: (f32, f32),
        from: (u16, u16),
    },
}

/// The buttons of the board's top bar (for terminals where only clicks are dependable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    ZoomOut,
    ZoomIn,
    Fit,
    Left,
    Right,
    Up,
    Down,
    Panels,
}

#[derive(Clone, Debug)]
pub struct CanvasState {
    /// Grid position shown at the top-left of the canvas.
    pub origin: (f32, f32),
    pub zoom: u8,
    pub selected: Option<NodeKey>,
    /// Positions the user chose (node id -> grid position).
    pub moved: HashMap<String, (f32, f32)>,
    pub drag: Option<Drag>,
    /// Only the board itself: no inspector and no map (`h`).
    pub clean: bool,
    /// A node picked up with `g`: the next click puts it down, the arrows nudge it.
    pub grabbed: Option<NodeKey>,
    /// Set by the renderer: floating panels that swallow mouse events (inspector, map).
    pub shield: std::cell::RefCell<Vec<Rect4>>,
    /// Set by the renderer: the clickable buttons of the top bar.
    pub buttons: std::cell::RefCell<Vec<(Rect4, Button)>>,
    /// The last click (for double click), by time and node.
    pub last_click: Option<(chrono::DateTime<chrono::Utc>, NodeKey)>,
    /// Set by the renderer: the canvas rectangle on screen (x, y, w, h) and where each node is drawn.
    pub view: std::cell::Cell<(u16, u16, u16, u16)>,
    pub hits: std::cell::RefCell<Vec<(Rect4, NodeKey)>>,
    /// The layout changed in a way worth saving.
    pub dirty: bool,
    pub framed: bool,
}

impl Default for CanvasState {
    fn default() -> Self {
        CanvasState {
            origin: (-0.3, -1.0),
            zoom: DEFAULT_ZOOM,
            selected: None,
            moved: HashMap::new(),
            drag: None,
            clean: false,
            grabbed: None,
            shield: std::cell::RefCell::new(vec![]),
            buttons: std::cell::RefCell::new(vec![]),
            last_click: None,
            view: std::cell::Cell::new((0, 0, 0, 0)),
            hits: std::cell::RefCell::new(vec![]),
            dirty: false,
            framed: false,
        }
    }
}

impl CanvasState {
    /// Screen cell (relative to the canvas) of a grid position.
    pub fn to_cell(&self, g: (f32, f32)) -> (i32, i32) {
        let (pw, ph) = pitch(self.zoom);
        (
            ((g.0 - self.origin.0) * pw as f32).round() as i32,
            ((g.1 - self.origin.1) * ph as f32).round() as i32,
        )
    }
    /// Grid position of a cell relative to the canvas.
    pub fn to_grid(&self, c: (i32, i32)) -> (f32, f32) {
        let (pw, ph) = pitch(self.zoom);
        (
            self.origin.0 + c.0 as f32 / pw as f32,
            self.origin.1 + c.1 as f32 / ph as f32,
        )
    }
    /// Pan by whole terminal cells.
    pub fn pan_cells(&mut self, dx: i32, dy: i32) {
        let (pw, ph) = pitch(self.zoom);
        self.origin.0 += dx as f32 / pw as f32;
        self.origin.1 += dy as f32 / ph as f32;
        self.dirty = true;
    }
    /// Change the zoom level keeping the grid point under `anchor` (a cell relative to the canvas) in place.
    pub fn set_zoom(&mut self, zoom: u8, anchor: (i32, i32)) {
        let zoom = zoom.min(ZOOM_LEVELS - 1);
        if zoom == self.zoom {
            return;
        }
        let g = self.to_grid(anchor);
        self.zoom = zoom;
        let (pw, ph) = pitch(zoom);
        self.origin = (
            g.0 - anchor.0 as f32 / pw as f32,
            g.1 - anchor.1 as f32 / ph as f32,
        );
        self.dirty = true;
    }
    /// Make the whole scene visible: the largest zoom at which everything fits, else the smallest.
    pub fn fit(&mut self, scene: &Scene, w: u16, h: u16) {
        let Some((lo, hi)) = bounds(scene) else {
            self.origin = (-0.3, -1.0);
            return;
        };
        for z in (0..ZOOM_LEVELS).rev() {
            let (pw, ph) = pitch(z);
            let (cw, ch) = card(z);
            let need_w = (hi.0 - lo.0) * pw as f32 + cw as f32;
            let need_h = (hi.1 - lo.1) * ph as f32 + ch as f32;
            if (need_w <= w as f32 && need_h <= h as f32) || z == 0 {
                self.zoom = z;
                // centred
                self.origin = (
                    lo.0 - ((w as f32 - need_w).max(0.0) / 2.0) / pw as f32,
                    lo.1 - ((h as f32 - need_h).max(0.0) / 2.0) / ph as f32,
                );
                self.dirty = true;
                return;
            }
        }
    }
    /// Scroll just enough to bring a node fully into view.
    pub fn reveal(&mut self, at: (f32, f32)) {
        let (_, _, w, h) = self.view.get();
        if w == 0 || h == 0 {
            return;
        }
        let (cw, ch) = card(self.zoom);
        let (x, y) = self.to_cell(at);
        let (mut dx, mut dy) = (0, 0);
        if x < 1 {
            dx = x - 2;
        } else if x + cw as i32 > w as i32 - 1 {
            dx = x + cw as i32 - w as i32 + 2;
        }
        if y < 0 {
            dy = y - 1;
        } else if y + ch as i32 > h as i32 {
            dy = y + ch as i32 - h as i32 + 1;
        }
        if dx != 0 || dy != 0 {
            self.pan_cells(dx, dy);
        }
    }
    pub fn to_json(&self) -> String {
        let moved: serde_json::Map<String, serde_json::Value> = self
            .moved
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::json!([v.0, v.1])))
            .collect();
        serde_json::json!({
            "zoom": self.zoom,
            "zv": ZOOM_SCALE_VERSION,
            "origin": [self.origin.0, self.origin.1],
            "moved": moved,
        })
        .to_string()
    }
    /// Restore a saved layout; anything unreadable is ignored (a fresh board is the fallback).
    pub fn load_json(&mut self, s: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
            return;
        };
        // a zoom saved with the older four-level scale means something else now: keep the default
        if v.get("zv").and_then(|z| z.as_u64()) == Some(ZOOM_SCALE_VERSION) {
            if let Some(z) = v.get("zoom").and_then(|z| z.as_u64()) {
                self.zoom = (z.min(ZOOM_LEVELS as u64 - 1)) as u8;
            }
        }
        if let Some(o) = v.get("origin").and_then(|o| o.as_array()) {
            if let (Some(x), Some(y)) = (
                o.first().and_then(|x| x.as_f64()),
                o.get(1).and_then(|y| y.as_f64()),
            ) {
                self.origin = (x as f32, y as f32);
                self.framed = true;
            }
        }
        if let Some(m) = v.get("moved").and_then(|m| m.as_object()) {
            for (k, p) in m {
                if NodeKey::parse(k).is_none() {
                    continue;
                }
                if let (Some(x), Some(y)) = (
                    p.get(0).and_then(|x| x.as_f64()),
                    p.get(1).and_then(|y| y.as_f64()),
                ) {
                    if x.is_finite() && y.is_finite() {
                        self.moved.insert(k.clone(), (x as f32, y as f32));
                    }
                }
            }
        }
    }
}
