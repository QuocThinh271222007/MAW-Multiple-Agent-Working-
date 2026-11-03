//! Typed queries. `Db` borrows a connection (or transaction) for the duration of a closure.

use crate::se;
use chrono::{DateTime, SecondsFormat, Utc};
use maw_core::*;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;

pub struct Db<'a>(pub(crate) &'a Connection);

fn ts(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn pts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
fn opt_ts(d: Option<DateTime<Utc>>) -> Option<String> {
    d.map(ts)
}
fn opt_pts(s: Option<String>) -> Option<DateTime<Utc>> {
    s.as_deref().map(pts)
}
fn js<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}
fn from_js<T: DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

const TASK_COLS: &str = "id, project_id, key_num, goal_id, parent_task_id, title, description, acceptance_criteria, \
    priority, status, owner_agent_id, estimated_weight, progress, created_at, updated_at, started_at, completed_at, \
    attempt_count, workspace_id, review_status, reviewer_agent_id, blocked_reason, lease_expires_at, last_heartbeat_at, \
    plan_revision, created_by, no_file_changes_expected";

fn task_from_row(r: &Row) -> rusqlite::Result<Task> {
    let key_num: i64 = r.get(2)?;
    Ok(Task {
        id: TaskId(r.get(0)?),
        key: format!("T-{key_num}"),
        project_id: ProjectId(r.get(1)?),
        goal_id: GoalId(r.get(3)?),
        parent_task_id: r.get::<_, Option<String>>(4)?.map(TaskId),
        title: r.get(5)?,
        description: r.get(6)?,
        acceptance_criteria: from_js(&r.get::<_, String>(7)?),
        priority: r.get::<_, String>(8)?.parse().unwrap_or(Priority::Normal),
        status: r
            .get::<_, String>(9)?
            .parse()
            .unwrap_or(TaskStatus::Backlog),
        owner_agent_id: r.get::<_, Option<String>>(10)?.map(AgentId),
        dependencies: vec![],
        estimated_weight: r.get(11)?,
        progress: r.get::<_, i64>(12)?.clamp(0, 100) as u8,
        created_at: pts(&r.get::<_, String>(13)?),
        updated_at: pts(&r.get::<_, String>(14)?),
        started_at: opt_pts(r.get(15)?),
        completed_at: opt_pts(r.get(16)?),
        attempt_count: r.get::<_, i64>(17)? as u32,
        workspace_id: r.get::<_, Option<String>>(18)?.map(WorkspaceId),
        artifacts: vec![],
        evidence: vec![],
        review_status: r.get::<_, String>(19)?.parse().unwrap_or_default(),
        reviewer_agent_id: r.get::<_, Option<String>>(20)?.map(AgentId),
        blocked_reason: r.get(21)?,
        lease_expires_at: opt_pts(r.get(22)?),
        last_heartbeat_at: opt_pts(r.get(23)?),
        plan_revision: r.get::<_, i64>(24)? as u32,
        created_by: AgentId(r.get(25)?),
        no_file_changes_expected: r.get::<_, i64>(26)? != 0,
    })
}

const AGENT_COLS: &str = "id, project_id, name, provider, model, role, state, permission, session_id, native_session_id, \
    workspace, current_task_id, capabilities, last_heartbeat, last_error, usage, created_at, effort";

fn agent_from_row(r: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: AgentId(r.get(0)?),
        project_id: ProjectId(r.get(1)?),
        name: r.get(2)?,
        provider: r.get::<_, String>(3)?.parse().unwrap_or(ProviderKind::Mock),
        model: r.get(4)?,
        role: r.get::<_, String>(5)?.parse().unwrap_or(Role::Worker),
        state: r
            .get::<_, String>(6)?
            .parse()
            .unwrap_or(AgentState::Disconnected),
        permission: r.get::<_, String>(7)?.parse().unwrap_or_default(),
        session_id: r.get(8)?,
        native_session_id: r.get(9)?,
        workspace: r.get(10)?,
        current_task_id: r.get::<_, Option<String>>(11)?.map(TaskId),
        capabilities: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
        last_heartbeat: opt_pts(r.get(13)?),
        last_error: r.get(14)?,
        usage: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
        created_at: pts(&r.get::<_, String>(16)?),
        effort: r.get(17)?,
    })
}

fn goal_from_row(r: &Row) -> rusqlite::Result<Goal> {
    Ok(Goal {
        id: GoalId(r.get(0)?),
        project_id: ProjectId(r.get(1)?),
        text: r.get(2)?,
        success_criteria: from_js(&r.get::<_, String>(3)?),
        status: r.get::<_, String>(4)?.parse().unwrap_or(GoalStatus::Active),
        revision: r.get::<_, i64>(5)? as u32,
        leader_agent_id: r.get::<_, Option<String>>(6)?.map(AgentId),
        current_milestone: r.get(7)?,
        current_focus: r.get(8)?,
        created_at: pts(&r.get::<_, String>(9)?),
        updated_at: pts(&r.get::<_, String>(10)?),
    })
}
const GOAL_COLS: &str = "id, project_id, text, success_criteria, status, revision, leader_agent_id, current_milestone, current_focus, created_at, updated_at";

fn message_from_row(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: MessageId(r.get(0)?),
        project_id: ProjectId(r.get(1)?),
        sender_agent_id: AgentId(r.get(2)?),
        recipient_agent_id: AgentId(r.get(3)?),
        task_id: r.get::<_, Option<String>>(4)?.map(TaskId),
        message_type: r
            .get::<_, String>(5)?
            .parse()
            .unwrap_or(MessageType::Status),
        subject: r.get(6)?,
        content: r.get(7)?,
        created_at: pts(&r.get::<_, String>(8)?),
        delivered_at: opt_pts(r.get(9)?),
        acknowledged_at: opt_pts(r.get(10)?),
        correlation_id: r.get(11)?,
    })
}
const MSG_COLS: &str = "id, project_id, sender_agent_id, recipient_agent_id, task_id, message_type, subject, content, created_at, delivered_at, acknowledged_at, correlation_id";

fn event_from_row(r: &Row) -> rusqlite::Result<Event> {
    Ok(Event {
        sequence: r.get(0)?,
        event_id: EventId(r.get(1)?),
        schema_version: r.get::<_, i64>(2)? as u32,
        timestamp: pts(&r.get::<_, String>(3)?),
        project_id: ProjectId(r.get(4)?),
        run_id: r.get::<_, Option<String>>(5)?.map(RunId),
        agent_id: r.get::<_, Option<String>>(6)?.map(AgentId),
        task_id: r.get::<_, Option<String>>(7)?.map(TaskId),
        correlation_id: r.get(8)?,
        event_type: r
            .get::<_, String>(9)?
            .parse()
            .unwrap_or(EventKind::ProviderError),
        payload: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or(serde_json::Value::Null),
    })
}
const EVENT_COLS: &str = "sequence, event_id, schema_version, ts, project_id, run_id, agent_id, task_id, correlation_id, event_type, payload";

fn workspace_from_row(r: &Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: WorkspaceId(r.get(0)?),
        project_id: ProjectId(r.get(1)?),
        agent_id: r.get::<_, Option<String>>(2)?.map(AgentId),
        task_id: r.get::<_, Option<String>>(3)?.map(TaskId),
        path: r.get(4)?,
        branch: r.get(5)?,
        base_revision: r.get(6)?,
        kind: r.get(7)?,
        status: r.get(8)?,
        changed_files: from_js(&r.get::<_, String>(9)?),
        integration_status: r.get(10)?,
        conflict: r.get(11)?,
        created_at: pts(&r.get::<_, String>(12)?),
        updated_at: pts(&r.get::<_, String>(13)?),
    })
}
const WS_COLS: &str = "id, project_id, agent_id, task_id, path, branch, base_revision, kind, status, changed_files, integration_status, conflict, created_at, updated_at";

fn approval_from_row(r: &Row) -> rusqlite::Result<Approval> {
    Ok(Approval {
        id: ApprovalId(r.get(0)?),
        project_id: ProjectId(r.get(1)?),
        agent_id: AgentId(r.get(2)?),
        task_id: r.get::<_, Option<String>>(3)?.map(TaskId),
        kind: r.get(4)?,
        summary: r.get(5)?,
        reason: r.get(6)?,
        details: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(serde_json::Value::Null),
        request_ref: r.get(8)?,
        status: ApprovalStatus::parse(&r.get::<_, String>(9)?),
        requested_at: pts(&r.get::<_, String>(10)?),
        resolved_at: opt_pts(r.get(11)?),
    })
}
const APR_COLS: &str = "id, project_id, agent_id, task_id, kind, summary, reason, details, request_ref, status, requested_at, resolved_at";

impl<'a> Db<'a> {
    pub fn applied_migrations(&self) -> CoreResult<Vec<String>> {
        let mut st = self
            .0
            .prepare("SELECT name FROM schema_migrations ORDER BY name")
            .map_err(se)?;
        let rows = st.query_map([], |r| r.get(0)).map_err(se)?;
        rows.collect::<Result<Vec<String>, _>>().map_err(se)
    }

    // ---- projects ----
    pub fn insert_project(&self, p: &Project) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO projects(id, name, path, created_at) VALUES (?1,?2,?3,?4)",
                params![p.id.0, p.name, p.path, ts(p.created_at)],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn list_projects(&self) -> CoreResult<Vec<Project>> {
        let mut st = self
            .0
            .prepare("SELECT id, name, path, created_at FROM projects ORDER BY name")
            .map_err(se)?;
        let rows = st
            .query_map([], |r| {
                Ok(Project {
                    id: ProjectId(r.get(0)?),
                    name: r.get(1)?,
                    path: r.get(2)?,
                    created_at: pts(&r.get::<_, String>(3)?),
                })
            })
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn find_project(&self, name_or_id: &str) -> CoreResult<Option<Project>> {
        Ok(self
            .list_projects()?
            .into_iter()
            .find(|p| p.name == name_or_id || p.id.0 == name_or_id))
    }

    // ---- goals ----
    pub fn insert_goal(&self, g: &Goal) -> CoreResult<()> {
        self.0
            .execute(
                &format!(
                    "INSERT INTO goals({GOAL_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
                ),
                params![
                    g.id.0,
                    g.project_id.0,
                    g.text,
                    js(&g.success_criteria),
                    g.status.as_str(),
                    g.revision,
                    g.leader_agent_id.as_ref().map(|a| a.0.clone()),
                    g.current_milestone,
                    g.current_focus,
                    ts(g.created_at),
                    ts(g.updated_at)
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn update_goal(&self, g: &Goal) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE goals SET text=?2, success_criteria=?3, status=?4, revision=?5, leader_agent_id=?6, \
                 current_milestone=?7, current_focus=?8, updated_at=?9 WHERE id=?1",
                params![
                    g.id.0, g.text, js(&g.success_criteria), g.status.as_str(), g.revision,
                    g.leader_agent_id.as_ref().map(|a| a.0.clone()), g.current_milestone, g.current_focus, ts(g.updated_at)
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn get_goal(&self, id: &GoalId) -> CoreResult<Option<Goal>> {
        self.0
            .query_row(
                &format!("SELECT {GOAL_COLS} FROM goals WHERE id=?1"),
                [&id.0],
                goal_from_row,
            )
            .optional()
            .map_err(se)
    }
    /// The most recent non-canceled goal of the project.
    pub fn current_goal(&self, project: &ProjectId) -> CoreResult<Option<Goal>> {
        self.0
            .query_row(
                &format!("SELECT {GOAL_COLS} FROM goals WHERE project_id=?1 AND status != 'canceled' ORDER BY created_at DESC, rowid DESC LIMIT 1"),
                [&project.0],
                goal_from_row,
            )
            .optional()
            .map_err(se)
    }
    pub fn insert_goal_revision(&self, r: &GoalRevision) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO goal_revisions(goal_id, revision, text, success_criteria, reason, author, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![r.goal_id.0, r.revision, r.text, js(&r.success_criteria), r.reason, r.author.0, ts(r.created_at)],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn list_goal_revisions(&self, goal: &GoalId) -> CoreResult<Vec<GoalRevision>> {
        let mut st = self
            .0
            .prepare("SELECT goal_id, revision, text, success_criteria, reason, author, created_at FROM goal_revisions WHERE goal_id=?1 ORDER BY revision")
            .map_err(se)?;
        let rows = st
            .query_map([&goal.0], |r| {
                Ok(GoalRevision {
                    goal_id: GoalId(r.get(0)?),
                    revision: r.get::<_, i64>(1)? as u32,
                    text: r.get(2)?,
                    success_criteria: from_js(&r.get::<_, String>(3)?),
                    reason: r.get(4)?,
                    author: AgentId(r.get(5)?),
                    created_at: pts(&r.get::<_, String>(6)?),
                })
            })
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn insert_plan_revision(&self, p: &PlanRevision) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO plan_revisions(goal_id, revision, summary, milestones, next_actions, reason, author, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![p.goal_id.0, p.revision, p.summary, js(&p.milestones), js(&p.next_actions), p.reason, p.author.0, ts(p.created_at)],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn list_plan_revisions(&self, goal: &GoalId) -> CoreResult<Vec<PlanRevision>> {
        let mut st = self
            .0
            .prepare("SELECT goal_id, revision, summary, milestones, next_actions, reason, author, created_at FROM plan_revisions WHERE goal_id=?1 ORDER BY revision")
            .map_err(se)?;
        let rows = st
            .query_map([&goal.0], |r| {
                Ok(PlanRevision {
                    goal_id: GoalId(r.get(0)?),
                    revision: r.get::<_, i64>(1)? as u32,
                    summary: r.get(2)?,
                    milestones: from_js(&r.get::<_, String>(3)?),
                    next_actions: from_js(&r.get::<_, String>(4)?),
                    reason: r.get(5)?,
                    author: AgentId(r.get(6)?),
                    created_at: pts(&r.get::<_, String>(7)?),
                })
            })
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn latest_plan(&self, goal: &GoalId) -> CoreResult<Option<PlanRevision>> {
        Ok(self.list_plan_revisions(goal)?.pop())
    }

    // ---- agents ----
    pub fn insert_agent(&self, a: &Agent) -> CoreResult<()> {
        self.0
            .execute(
                &format!("INSERT INTO agents({AGENT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"),
                params![
                    a.id.0, a.project_id.0, a.name, a.provider.as_str(), a.model, a.role.as_str(), a.state.as_str(),
                    a.permission.as_str(), a.session_id, a.native_session_id, a.workspace,
                    a.current_task_id.as_ref().map(|t| t.0.clone()), js(&a.capabilities), opt_ts(a.last_heartbeat),
                    a.last_error, js(&a.usage), ts(a.created_at), a.effort
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn update_agent(&self, a: &Agent) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE agents SET name=?2, model=?3, role=?4, state=?5, permission=?6, session_id=?7, native_session_id=?8, \
                 workspace=?9, current_task_id=?10, capabilities=?11, last_heartbeat=?12, last_error=?13, usage=?14, effort=?15 WHERE id=?1",
                params![
                    a.id.0, a.name, a.model, a.role.as_str(), a.state.as_str(), a.permission.as_str(), a.session_id,
                    a.native_session_id, a.workspace, a.current_task_id.as_ref().map(|t| t.0.clone()),
                    js(&a.capabilities), opt_ts(a.last_heartbeat), a.last_error, js(&a.usage), a.effort
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn get_agent(&self, id: &AgentId) -> CoreResult<Option<Agent>> {
        self.0
            .query_row(
                &format!("SELECT {AGENT_COLS} FROM agents WHERE id=?1"),
                [&id.0],
                agent_from_row,
            )
            .optional()
            .map_err(se)
    }
    pub fn list_agents(&self, project: &ProjectId) -> CoreResult<Vec<Agent>> {
        let mut st = self
            .0
            .prepare(&format!(
                "SELECT {AGENT_COLS} FROM agents WHERE project_id=?1 ORDER BY created_at, rowid"
            ))
            .map_err(se)?;
        let rows = st.query_map([&project.0], agent_from_row).map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    /// Mark every undelivered message addressed to the agent as delivered (it will never read
    /// them; the rows stay for history).
    pub fn close_undelivered_for(&self, agent: &AgentId) -> CoreResult<usize> {
        self.0
            .execute(
                "UPDATE messages SET delivered_at=?2 WHERE recipient_agent_id=?1 AND delivered_at IS NULL",
                params![agent.0, ts(Utc::now())],
            )
            .map_err(se)
    }
    pub fn delete_agent(&self, id: &AgentId) -> CoreResult<()> {
        self.0
            .execute("DELETE FROM agents WHERE id=?1", [&id.0])
            .map_err(se)?;
        Ok(())
    }

    // ---- tasks ----
    pub fn next_task_key(&self, project: &ProjectId) -> CoreResult<i64> {
        self.0
            .query_row(
                "SELECT COALESCE(MAX(key_num),0)+1 FROM tasks WHERE project_id=?1",
                [&project.0],
                |r| r.get(0),
            )
            .map_err(se)
    }
    /// Insert a new task (dependencies persisted too). Returns the stored task with its key.
    pub fn insert_task(&self, t: &Task, idempotency_key: Option<&str>) -> CoreResult<Task> {
        let key_num = self.next_task_key(&t.project_id)?;
        self.0
            .execute(
                &format!(
                    "INSERT INTO tasks({TASK_COLS}, idempotency_key) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)"
                ),
                params![
                    t.id.0, t.project_id.0, key_num, t.goal_id.0, t.parent_task_id.as_ref().map(|x| x.0.clone()),
                    t.title, t.description, js(&t.acceptance_criteria), t.priority.as_str(), t.status.as_str(),
                    t.owner_agent_id.as_ref().map(|x| x.0.clone()), t.estimated_weight, t.progress,
                    ts(t.created_at), ts(t.updated_at), opt_ts(t.started_at), opt_ts(t.completed_at),
                    t.attempt_count, t.workspace_id.as_ref().map(|x| x.0.clone()), t.review_status.as_str(),
                    t.reviewer_agent_id.as_ref().map(|x| x.0.clone()), t.blocked_reason, opt_ts(t.lease_expires_at),
                    opt_ts(t.last_heartbeat_at), t.plan_revision, t.created_by.0, t.no_file_changes_expected as i64,
                    idempotency_key
                ],
            )
            .map_err(se)?;
        for d in &t.dependencies {
            self.0
                .execute(
                    "INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES (?1,?2)",
                    params![t.id.0, d.0],
                )
                .map_err(se)?;
        }
        let mut stored = t.clone();
        stored.key = format!("T-{key_num}");
        Ok(stored)
    }
    pub fn update_task(&self, t: &Task) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE tasks SET parent_task_id=?2, title=?3, description=?4, acceptance_criteria=?5, priority=?6, status=?7, \
                 owner_agent_id=?8, estimated_weight=?9, progress=?10, updated_at=?11, started_at=?12, completed_at=?13, \
                 attempt_count=?14, workspace_id=?15, review_status=?16, reviewer_agent_id=?17, blocked_reason=?18, \
                 lease_expires_at=?19, last_heartbeat_at=?20, plan_revision=?21, no_file_changes_expected=?22 WHERE id=?1",
                params![
                    t.id.0, t.parent_task_id.as_ref().map(|x| x.0.clone()), t.title, t.description,
                    js(&t.acceptance_criteria), t.priority.as_str(), t.status.as_str(),
                    t.owner_agent_id.as_ref().map(|x| x.0.clone()), t.estimated_weight, t.progress, ts(t.updated_at),
                    opt_ts(t.started_at), opt_ts(t.completed_at), t.attempt_count,
                    t.workspace_id.as_ref().map(|x| x.0.clone()), t.review_status.as_str(),
                    t.reviewer_agent_id.as_ref().map(|x| x.0.clone()), t.blocked_reason, opt_ts(t.lease_expires_at),
                    opt_ts(t.last_heartbeat_at), t.plan_revision, t.no_file_changes_expected as i64
                ],
            )
            .map_err(se)?;
        // Replace dependency set.
        self.0
            .execute("DELETE FROM task_deps WHERE task_id=?1", [&t.id.0])
            .map_err(se)?;
        for d in &t.dependencies {
            self.0
                .execute(
                    "INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES (?1,?2)",
                    params![t.id.0, d.0],
                )
                .map_err(se)?;
        }
        Ok(())
    }
    fn hydrate(&self, tasks: &mut [Task], project: Option<&ProjectId>) -> CoreResult<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        let mut deps: HashMap<String, Vec<TaskId>> = HashMap::new();
        {
            let mut st = self
                .0
                .prepare("SELECT task_id, depends_on FROM task_deps")
                .map_err(se)?;
            for row in st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(se)?
            {
                let (a, b) = row.map_err(se)?;
                deps.entry(a).or_default().push(TaskId(b));
            }
        }
        let mut ev: HashMap<String, Vec<Evidence>> = HashMap::new();
        {
            let mut st = self
                .0
                .prepare("SELECT task_id, kind, summary, passed, detail, recorded_by, recorded_at FROM evidence ORDER BY id")
                .map_err(se)?;
            for row in st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Evidence {
                            kind: r.get::<_, String>(1)?.parse().unwrap_or(EvidenceKind::Note),
                            summary: r.get(2)?,
                            passed: r.get::<_, i64>(3)? != 0,
                            detail: serde_json::from_str(&r.get::<_, String>(4)?)
                                .unwrap_or(serde_json::Value::Null),
                            recorded_by: AgentId(r.get(5)?),
                            recorded_at: pts(&r.get::<_, String>(6)?),
                        },
                    ))
                })
                .map_err(se)?
            {
                let (t, e) = row.map_err(se)?;
                ev.entry(t).or_default().push(e);
            }
        }
        let mut arts: HashMap<String, Vec<Artifact>> = HashMap::new();
        {
            let mut st = self
                .0
                .prepare("SELECT id, task_id, kind, path, description, registered_by, registered_at FROM artifacts WHERE task_id IS NOT NULL ORDER BY registered_at")
                .map_err(se)?;
            for row in st
                .query_map([], |r| {
                    Ok(Artifact {
                        id: ArtifactId(r.get(0)?),
                        task_id: r.get::<_, Option<String>>(1)?.map(TaskId),
                        kind: r.get(2)?,
                        path: r.get(3)?,
                        description: r.get(4)?,
                        registered_by: AgentId(r.get(5)?),
                        registered_at: pts(&r.get::<_, String>(6)?),
                    })
                })
                .map_err(se)?
            {
                let a = row.map_err(se)?;
                if let Some(t) = &a.task_id {
                    arts.entry(t.0.clone()).or_default().push(a);
                }
            }
        }
        let _ = project;
        for t in tasks.iter_mut() {
            t.dependencies = deps.remove(&t.id.0).unwrap_or_default();
            t.evidence = ev.remove(&t.id.0).unwrap_or_default();
            t.artifacts = arts.remove(&t.id.0).unwrap_or_default();
        }
        Ok(())
    }
    pub fn get_task(&self, id: &TaskId) -> CoreResult<Option<Task>> {
        let t = self
            .0
            .query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE id=?1"),
                [&id.0],
                task_from_row,
            )
            .optional()
            .map_err(se)?;
        match t {
            Some(t) => {
                let mut v = vec![t];
                self.hydrate(&mut v, None)?;
                Ok(v.pop())
            }
            None => Ok(None),
        }
    }
    /// Resolve a task by id or by its `T-n` key.
    pub fn resolve_task(&self, project: &ProjectId, id_or_key: &str) -> CoreResult<Option<Task>> {
        let found = if let Some(n) = id_or_key
            .strip_prefix("T-")
            .and_then(|n| n.parse::<i64>().ok())
        {
            self.0
                .query_row(
                    &format!("SELECT {TASK_COLS} FROM tasks WHERE project_id=?1 AND key_num=?2"),
                    params![project.0, n],
                    task_from_row,
                )
                .optional()
                .map_err(se)?
        } else {
            self.0
                .query_row(
                    &format!("SELECT {TASK_COLS} FROM tasks WHERE id=?1 AND project_id=?2"),
                    params![id_or_key, project.0],
                    task_from_row,
                )
                .optional()
                .map_err(se)?
        };
        match found {
            Some(t) => {
                let mut v = vec![t];
                self.hydrate(&mut v, Some(project))?;
                Ok(v.pop())
            }
            None => Ok(None),
        }
    }
    pub fn find_task_by_idempotency(
        &self,
        project: &ProjectId,
        key: &str,
    ) -> CoreResult<Option<Task>> {
        let id: Option<String> = self
            .0
            .query_row(
                "SELECT id FROM tasks WHERE project_id=?1 AND idempotency_key=?2",
                params![project.0, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(se)?;
        match id {
            Some(id) => self.get_task(&TaskId(id)),
            None => Ok(None),
        }
    }
    pub fn list_tasks(&self, project: &ProjectId) -> CoreResult<Vec<Task>> {
        let mut st = self
            .0
            .prepare(&format!(
                "SELECT {TASK_COLS} FROM tasks WHERE project_id=?1 ORDER BY key_num"
            ))
            .map_err(se)?;
        let mut tasks: Vec<Task> = st
            .query_map([&project.0], task_from_row)
            .map_err(se)?
            .collect::<Result<_, _>>()
            .map_err(se)?;
        self.hydrate(&mut tasks, Some(project))?;
        Ok(tasks)
    }
    pub fn count_goal_tasks(&self, goal: &GoalId) -> CoreResult<usize> {
        self.0
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE goal_id=?1",
                [&goal.0],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(se)
    }
    pub fn add_evidence(&self, task: &TaskId, e: &Evidence) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO evidence(task_id, kind, summary, passed, detail, recorded_by, recorded_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![task.0, e.kind.as_str(), e.summary, e.passed as i64, js(&e.detail), e.recorded_by.0, ts(e.recorded_at)],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn add_artifact(&self, a: &Artifact) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO artifacts(id, task_id, kind, path, description, registered_by, registered_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![a.id.0, a.task_id.as_ref().map(|t| t.0.clone()), a.kind, a.path, a.description, a.registered_by.0, ts(a.registered_at)],
            )
            .map_err(se)?;
        Ok(())
    }

    // ---- attempts ----
    pub fn begin_attempt(&self, task: &TaskId, agent: &AgentId, no: u32) -> CoreResult<i64> {
        self.0
            .execute(
                "INSERT INTO attempts(task_id, agent_id, attempt_no, started_at) VALUES (?1,?2,?3,?4)",
                params![task.0, agent.0, no, ts(Utc::now())],
            )
            .map_err(se)?;
        Ok(self.0.last_insert_rowid())
    }
    /// Close any open attempts for the task.
    pub fn end_attempts(&self, task: &TaskId, outcome: &str, note: Option<&str>) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE attempts SET ended_at=?2, outcome=?3, note=?4 WHERE task_id=?1 AND ended_at IS NULL",
                params![task.0, ts(Utc::now()), outcome, note],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn list_attempts(&self, task: &TaskId) -> CoreResult<Vec<Attempt>> {
        let mut st = self
            .0
            .prepare("SELECT id, task_id, agent_id, attempt_no, started_at, ended_at, outcome, note FROM attempts WHERE task_id=?1 ORDER BY id")
            .map_err(se)?;
        let rows = st
            .query_map([&task.0], |r| {
                Ok(Attempt {
                    id: r.get(0)?,
                    task_id: TaskId(r.get(1)?),
                    agent_id: AgentId(r.get(2)?),
                    attempt_no: r.get::<_, i64>(3)? as u32,
                    started_at: pts(&r.get::<_, String>(4)?),
                    ended_at: opt_pts(r.get(5)?),
                    outcome: r.get(6)?,
                    note: r.get(7)?,
                })
            })
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }

    // ---- messages ----
    pub fn insert_message(&self, m: &Message, idempotency_key: Option<&str>) -> CoreResult<()> {
        self.0
            .execute(
                &format!("INSERT INTO messages({MSG_COLS}, idempotency_key) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
                params![
                    m.id.0, m.project_id.0, m.sender_agent_id.0, m.recipient_agent_id.0,
                    m.task_id.as_ref().map(|t| t.0.clone()), m.message_type.as_str(), m.subject, m.content,
                    ts(m.created_at), opt_ts(m.delivered_at), opt_ts(m.acknowledged_at), m.correlation_id, idempotency_key
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn find_message_by_idempotency(
        &self,
        project: &ProjectId,
        sender: &AgentId,
        key: &str,
    ) -> CoreResult<Option<Message>> {
        self.0
            .query_row(
                &format!("SELECT {MSG_COLS} FROM messages WHERE project_id=?1 AND sender_agent_id=?2 AND idempotency_key=?3"),
                params![project.0, sender.0, key],
                message_from_row,
            )
            .optional()
            .map_err(se)
    }
    pub fn get_message(&self, id: &MessageId) -> CoreResult<Option<Message>> {
        self.0
            .query_row(
                &format!("SELECT {MSG_COLS} FROM messages WHERE id=?1"),
                [&id.0],
                message_from_row,
            )
            .optional()
            .map_err(se)
    }
    /// Messages addressed to the agent. `unacked_only` hides acknowledged ones.
    pub fn inbox(
        &self,
        agent: &AgentId,
        unacked_only: bool,
        limit: usize,
    ) -> CoreResult<Vec<Message>> {
        let sql = format!(
            "SELECT {MSG_COLS} FROM messages WHERE recipient_agent_id=?1 {} ORDER BY created_at, rowid LIMIT ?2",
            if unacked_only { "AND acknowledged_at IS NULL" } else { "" }
        );
        let mut st = self.0.prepare(&sql).map_err(se)?;
        let rows = st
            .query_map(params![agent.0, limit as i64], message_from_row)
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn undelivered(&self, agent: &AgentId) -> CoreResult<Vec<Message>> {
        let mut st = self
            .0
            .prepare(&format!(
                "SELECT {MSG_COLS} FROM messages WHERE recipient_agent_id=?1 AND delivered_at IS NULL ORDER BY created_at, rowid"
            ))
            .map_err(se)?;
        let rows = st.query_map([&agent.0], message_from_row).map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn agents_with_undelivered(&self, project: &ProjectId) -> CoreResult<Vec<AgentId>> {
        let mut st = self
            .0
            .prepare("SELECT DISTINCT recipient_agent_id FROM messages WHERE project_id=?1 AND delivered_at IS NULL")
            .map_err(se)?;
        let rows = st
            .query_map([&project.0], |r| Ok(AgentId(r.get(0)?)))
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn mark_delivered(&self, id: &MessageId) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE messages SET delivered_at=?2 WHERE id=?1 AND delivered_at IS NULL",
                params![id.0, ts(Utc::now())],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn mark_acked(&self, id: &MessageId) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE messages SET acknowledged_at=?2 WHERE id=?1 AND acknowledged_at IS NULL",
                params![id.0, ts(Utc::now())],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn recent_messages(&self, project: &ProjectId, limit: usize) -> CoreResult<Vec<Message>> {
        let mut st = self
            .0
            .prepare(&format!("SELECT {MSG_COLS} FROM messages WHERE project_id=?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2"))
            .map_err(se)?;
        let mut v: Vec<Message> = st
            .query_map(params![project.0, limit as i64], message_from_row)
            .map_err(se)?
            .collect::<Result<_, _>>()
            .map_err(se)?;
        v.reverse();
        Ok(v)
    }

    // ---- events ----
    /// Append and return the assigned sequence number (also written into `e`).
    pub fn append_event(&self, e: &mut Event) -> CoreResult<i64> {
        self.0
            .execute(
                "INSERT INTO events(event_id, schema_version, ts, project_id, run_id, agent_id, task_id, correlation_id, event_type, verbose, payload) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    e.event_id.0, e.schema_version, ts(e.timestamp), e.project_id.0, e.run_id.as_ref().map(|x| x.0.clone()),
                    e.agent_id.as_ref().map(|x| x.0.clone()), e.task_id.as_ref().map(|x| x.0.clone()), e.correlation_id,
                    e.event_type.as_str(), e.event_type.is_verbose() as i64, e.payload.to_string()
                ],
            )
            .map_err(se)?;
        e.sequence = self.0.last_insert_rowid();
        Ok(e.sequence)
    }
    pub fn events_since(
        &self,
        project: &ProjectId,
        after: i64,
        limit: usize,
    ) -> CoreResult<Vec<Event>> {
        let mut st = self
            .0
            .prepare(&format!("SELECT {EVENT_COLS} FROM events WHERE project_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3"))
            .map_err(se)?;
        let rows = st
            .query_map(params![project.0, after, limit as i64], event_from_row)
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn recent_events(&self, project: &ProjectId, limit: usize) -> CoreResult<Vec<Event>> {
        let mut st = self
            .0
            .prepare(&format!("SELECT {EVENT_COLS} FROM events WHERE project_id=?1 ORDER BY sequence DESC LIMIT ?2"))
            .map_err(se)?;
        let mut v: Vec<Event> = st
            .query_map(params![project.0, limit as i64], event_from_row)
            .map_err(se)?
            .collect::<Result<_, _>>()
            .map_err(se)?;
        v.reverse();
        Ok(v)
    }
    /// The latest `limit` events of one kind, oldest first.
    pub fn events_of_kind(
        &self,
        project: &ProjectId,
        kind: EventKind,
        limit: usize,
    ) -> CoreResult<Vec<Event>> {
        let mut st = self
            .0
            .prepare(&format!(
                "SELECT {EVENT_COLS} FROM events WHERE project_id=?1 AND event_type=?2 ORDER BY sequence DESC LIMIT ?3"
            ))
            .map_err(se)?;
        let mut v: Vec<Event> = st
            .query_map(
                params![project.0, kind.as_str(), limit as i64],
                event_from_row,
            )
            .map_err(se)?
            .collect::<Result<_, _>>()
            .map_err(se)?;
        v.reverse();
        Ok(v)
    }
    pub fn events_for_task(&self, task: &TaskId, limit: usize) -> CoreResult<Vec<Event>> {
        let mut st = self
            .0
            .prepare(&format!(
                "SELECT {EVENT_COLS} FROM events WHERE task_id=?1 ORDER BY sequence LIMIT ?2"
            ))
            .map_err(se)?;
        let rows = st
            .query_map(params![task.0, limit as i64], event_from_row)
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn count_events(&self, project: &ProjectId) -> CoreResult<i64> {
        self.0
            .query_row(
                "SELECT COUNT(*) FROM events WHERE project_id=?1",
                [&project.0],
                |r| r.get(0),
            )
            .map_err(se)
    }
    /// Retention: drop the oldest verbose events beyond `keep`. Returns rows deleted.
    pub fn prune_verbose_events(&self, keep: i64) -> CoreResult<usize> {
        self.0
            .execute(
                "DELETE FROM events WHERE verbose=1 AND sequence NOT IN \
                 (SELECT sequence FROM events WHERE verbose=1 ORDER BY sequence DESC LIMIT ?1)",
                [keep],
            )
            .map_err(se)
    }

    // ---- context ----
    pub fn add_context(&self, c: &ContextEntry) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO context_entries(id, project_id, kind, key, content, tags, task_id, author, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![c.id, c.project_id.0, c.kind, c.key, c.content, js(&c.tags), c.task_id.as_ref().map(|t| t.0.clone()), c.author.0, ts(c.created_at)],
            )
            .map_err(se)?;
        Ok(())
    }
    /// List entries, optionally filtered by kind and a case-insensitive substring over key/content/tags.
    pub fn search_context(
        &self,
        project: &ProjectId,
        kind: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> CoreResult<Vec<ContextEntry>> {
        let mut st = self
            .0
            .prepare("SELECT id, project_id, kind, key, content, tags, task_id, author, created_at FROM context_entries WHERE project_id=?1 ORDER BY created_at DESC, rowid DESC")
            .map_err(se)?;
        let rows = st
            .query_map([&project.0], |r| {
                Ok(ContextEntry {
                    id: r.get(0)?,
                    project_id: ProjectId(r.get(1)?),
                    kind: r.get(2)?,
                    key: r.get(3)?,
                    content: r.get(4)?,
                    tags: from_js(&r.get::<_, String>(5)?),
                    task_id: r.get::<_, Option<String>>(6)?.map(TaskId),
                    author: AgentId(r.get(7)?),
                    created_at: pts(&r.get::<_, String>(8)?),
                })
            })
            .map_err(se)?;
        let q = query.map(|s| s.to_lowercase());
        let mut out = Vec::new();
        for row in rows {
            let c = row.map_err(se)?;
            if kind.is_some_and(|k| c.kind != k) {
                continue;
            }
            if let Some(q) = &q {
                let hay = format!("{} {} {}", c.key, c.content, c.tags.join(" ")).to_lowercase();
                if !hay.contains(q.as_str()) {
                    continue;
                }
            }
            out.push(c);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    // ---- workspaces ----
    pub fn upsert_workspace(&self, w: &Workspace) -> CoreResult<()> {
        self.0
            .execute(
                &format!(
                    "INSERT INTO workspaces({WS_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) \
                     ON CONFLICT(id) DO UPDATE SET agent_id=?3, task_id=?4, path=?5, branch=?6, base_revision=?7, kind=?8, status=?9, \
                     changed_files=?10, integration_status=?11, conflict=?12, updated_at=?14"
                ),
                params![
                    w.id.0, w.project_id.0, w.agent_id.as_ref().map(|x| x.0.clone()), w.task_id.as_ref().map(|x| x.0.clone()),
                    w.path, w.branch, w.base_revision, w.kind, w.status, js(&w.changed_files), w.integration_status,
                    w.conflict, ts(w.created_at), ts(w.updated_at)
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn get_workspace(&self, id: &WorkspaceId) -> CoreResult<Option<Workspace>> {
        self.0
            .query_row(
                &format!("SELECT {WS_COLS} FROM workspaces WHERE id=?1"),
                [&id.0],
                workspace_from_row,
            )
            .optional()
            .map_err(se)
    }
    pub fn list_workspaces(&self, project: &ProjectId) -> CoreResult<Vec<Workspace>> {
        let mut st = self
            .0
            .prepare(&format!(
                "SELECT {WS_COLS} FROM workspaces WHERE project_id=?1 ORDER BY created_at"
            ))
            .map_err(se)?;
        let rows = st.query_map([&project.0], workspace_from_row).map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }

    // ---- approvals ----
    pub fn insert_approval(&self, a: &Approval) -> CoreResult<()> {
        self.0
            .execute(
                &format!("INSERT INTO approvals({APR_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"),
                params![
                    a.id.0, a.project_id.0, a.agent_id.0, a.task_id.as_ref().map(|t| t.0.clone()), a.kind, a.summary,
                    a.reason, a.details.to_string(), a.request_ref, a.status.as_str(), ts(a.requested_at), opt_ts(a.resolved_at)
                ],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn get_approval(&self, id: &ApprovalId) -> CoreResult<Option<Approval>> {
        self.0
            .query_row(
                &format!("SELECT {APR_COLS} FROM approvals WHERE id=?1"),
                [&id.0],
                approval_from_row,
            )
            .optional()
            .map_err(se)
    }
    pub fn list_approvals(
        &self,
        project: &ProjectId,
        pending_only: bool,
    ) -> CoreResult<Vec<Approval>> {
        let sql =
            format!(
            "SELECT {APR_COLS} FROM approvals WHERE project_id=?1 {} ORDER BY requested_at DESC",
            if pending_only { "AND status='pending'" } else { "" }
        );
        let mut st = self.0.prepare(&sql).map_err(se)?;
        let rows = st.query_map([&project.0], approval_from_row).map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    pub fn resolve_approval(&self, id: &ApprovalId, status: ApprovalStatus) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE approvals SET status=?2, resolved_at=?3 WHERE id=?1",
                params![id.0, status.as_str(), ts(Utc::now())],
            )
            .map_err(se)?;
        Ok(())
    }
    /// Expire every pending approval of an agent whose session is gone.
    pub fn expire_agent_approvals(&self, agent: &AgentId) -> CoreResult<usize> {
        self.0
            .execute(
                "UPDATE approvals SET status='expired', resolved_at=?2 WHERE agent_id=?1 AND status='pending'",
                params![agent.0, ts(Utc::now())],
            )
            .map_err(se)
    }

    // ---- runs ----
    pub fn begin_run(&self, r: &Run) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO runs(id, project_id, started_at, clean_shutdown) VALUES (?1,?2,?3,0)",
                params![r.id.0, r.project_id.0, ts(r.started_at)],
            )
            .map_err(se)?;
        Ok(())
    }
    pub fn end_run(&self, id: &RunId) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE runs SET stopped_at=?2, clean_shutdown=1 WHERE id=?1",
                params![id.0, ts(Utc::now())],
            )
            .map_err(se)?;
        Ok(())
    }
    /// Runs that never recorded a clean shutdown (crash / kill).
    pub fn unclean_runs(&self, project: &ProjectId) -> CoreResult<Vec<Run>> {
        let mut st = self
            .0
            .prepare("SELECT id, project_id, started_at, stopped_at, clean_shutdown FROM runs WHERE project_id=?1 AND clean_shutdown=0 ORDER BY started_at")
            .map_err(se)?;
        let rows = st
            .query_map([&project.0], |r| {
                Ok(Run {
                    id: RunId(r.get(0)?),
                    project_id: ProjectId(r.get(1)?),
                    started_at: pts(&r.get::<_, String>(2)?),
                    stopped_at: opt_pts(r.get(3)?),
                    clean_shutdown: r.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(se)?;
        rows.collect::<Result<_, _>>().map_err(se)
    }
    /// Undo `close_unclean_runs` for the currently active run.
    pub fn reopen_run(&self, id: &RunId) -> CoreResult<()> {
        self.0
            .execute(
                "UPDATE runs SET clean_shutdown=0, stopped_at=NULL WHERE id=?1",
                [&id.0],
            )
            .map_err(se)?;
        Ok(())
    }
    /// Mark old unclean runs as closed (after recovery has handled them).
    pub fn close_unclean_runs(&self, project: &ProjectId) -> CoreResult<()> {
        self.0
            .execute("UPDATE runs SET clean_shutdown=1, stopped_at=COALESCE(stopped_at, ?2) WHERE project_id=?1 AND clean_shutdown=0", params![project.0, ts(Utc::now())])
            .map_err(se)?;
        Ok(())
    }

    // ---- kv ----
    pub fn kv_get(&self, project: &ProjectId, key: &str) -> CoreResult<Option<String>> {
        self.0
            .query_row(
                "SELECT value FROM kv WHERE project_id=?1 AND key=?2",
                params![project.0, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(se)
    }
    pub fn kv_set(&self, project: &ProjectId, key: &str, value: &str) -> CoreResult<()> {
        self.0
            .execute(
                "INSERT INTO kv(project_id, key, value) VALUES (?1,?2,?3) ON CONFLICT(project_id,key) DO UPDATE SET value=?3",
                params![project.0, key, value],
            )
            .map_err(se)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn project(db: &Db) -> Project {
        let p = Project {
            id: ProjectId::new(),
            name: "demo".into(),
            path: "/tmp/demo".into(),
            created_at: Utc::now(),
        };
        db.insert_project(&p).unwrap();
        p
    }

    fn goal(db: &Db, p: &Project) -> Goal {
        let g = Goal {
            id: GoalId::new(),
            project_id: p.id.clone(),
            text: "do it".into(),
            success_criteria: vec!["works".into()],
            status: GoalStatus::Active,
            revision: 1,
            leader_agent_id: None,
            current_milestone: None,
            current_focus: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        db.insert_goal(&g).unwrap();
        g
    }

    fn task(p: &Project, g: &Goal, title: &str, deps: Vec<TaskId>) -> Task {
        Task {
            id: TaskId::new(),
            key: String::new(),
            project_id: p.id.clone(),
            goal_id: g.id.clone(),
            parent_task_id: None,
            title: title.into(),
            description: "d".into(),
            acceptance_criteria: vec!["ac".into()],
            priority: Priority::High,
            status: TaskStatus::Backlog,
            owner_agent_id: None,
            dependencies: deps,
            estimated_weight: 2.0,
            progress: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            started_at: None,
            completed_at: None,
            attempt_count: 0,
            workspace_id: None,
            artifacts: vec![],
            evidence: vec![],
            review_status: ReviewStatus::NotRequired,
            reviewer_agent_id: None,
            blocked_reason: None,
            lease_expires_at: None,
            last_heartbeat_at: None,
            plan_revision: 1,
            created_by: AgentId::from("agt_leader"),
            no_file_changes_expected: false,
        }
    }

    #[test]
    fn migrations_apply_once_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maw.db");
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(
                s.applied_migrations().unwrap(),
                vec!["0001_init".to_string(), "0002_agent_effort".to_string()]
            );
            s.write(|db| {
                project(db);
                Ok(())
            })
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.applied_migrations().unwrap().len(), 2);
        assert_eq!(s.read(|db| db.list_projects()).unwrap().len(), 1);
    }

    #[test]
    fn task_roundtrip_with_deps_and_idempotency() {
        let s = Store::open_in_memory().unwrap();
        s.write(|db| {
            let p = project(db);
            let g = goal(db, &p);
            let a = db.insert_task(&task(&p, &g, "A", vec![]), Some("k1"))?;
            let b = db.insert_task(&task(&p, &g, "B", vec![a.id.clone()]), None)?;
            assert_eq!(a.key, "T-1");
            assert_eq!(b.key, "T-2");
            let got = db.resolve_task(&p.id, "T-2")?.unwrap();
            assert_eq!(got.dependencies, vec![a.id.clone()]);
            assert_eq!(db.find_task_by_idempotency(&p.id, "k1")?.unwrap().id, a.id);
            // duplicate idempotency key rejected by the unique index
            assert!(db
                .insert_task(&task(&p, &g, "dup", vec![]), Some("k1"))
                .is_err());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn write_rolls_back_on_error() {
        let s = Store::open_in_memory().unwrap();
        let r: CoreResult<()> = s.write(|db| {
            project(db);
            Err(CoreError::Conflict("boom".into()))
        });
        assert!(r.is_err());
        assert!(s.read(|db| db.list_projects()).unwrap().is_empty());
    }

    #[test]
    fn events_get_monotonic_sequence_and_prune() {
        let s = Store::open_in_memory().unwrap();
        s.write(|db| {
            let p = project(db);
            for i in 0..5 {
                let mut e = Event::new(
                    p.id.clone(),
                    EventKind::AgentMessage,
                    serde_json::json!({ "i": i }),
                );
                db.append_event(&mut e)?;
            }
            let mut keep = Event::new(p.id.clone(), EventKind::TaskCreated, serde_json::json!({}));
            let seq = db.append_event(&mut keep)?;
            assert_eq!(seq, 6);
            assert_eq!(db.prune_verbose_events(2)?, 3);
            let remaining = db.recent_events(&p.id, 10)?;
            assert_eq!(remaining.len(), 3);
            assert!(remaining.windows(2).all(|w| w[0].sequence < w[1].sequence));
            assert_eq!(db.events_since(&p.id, 5, 10)?.len(), 1);
            Ok(())
        })
        .unwrap();
    }
}
