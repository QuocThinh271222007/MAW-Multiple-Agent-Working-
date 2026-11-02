//! Task lifecycle operations (impl Engine). Every function validates authorization, state
//! transitions and invariants, persists the change and emits events in a single transaction.

use crate::comms::NewMessage;
use crate::engine::{Actor, Emit, Engine};
use chrono::{Duration, Utc};
use maw_core::*;
use maw_storage::Db;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub priority: Option<Priority>,
    /// Parent task (id or `T-n`) for subtasks.
    pub parent: Option<String>,
    /// Dependency tasks (ids or `T-n`).
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub weight: Option<f64>,
    /// Agent id or name to pre-assign.
    pub assignee: Option<String>,
    #[serde(default)]
    pub requires_review: bool,
    pub reviewer: Option<String>,
    #[serde(default)]
    pub no_file_changes_expected: bool,
    /// Retried creations with the same key return the original task.
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub priority: Option<Priority>,
    pub weight: Option<f64>,
    #[serde(default)]
    pub add_dependencies: Vec<String>,
    #[serde(default)]
    pub remove_dependencies: Vec<String>,
    /// Only `canceled` is accepted; other transitions go through dedicated operations.
    pub cancel: Option<bool>,
    pub reason: Option<String>,
    /// Declare that this task legitimately changes no files (analysis, review, ...).
    pub no_file_changes_expected: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceInput {
    pub kind: EvidenceKind,
    pub summary: String,
    #[serde(default = "default_true")]
    pub passed: bool,
    #[serde(default)]
    pub detail: Value,
}

fn default_true() -> bool {
    true
}

fn task_json(t: &Task) -> Value {
    json!({ "task_key": t.key, "task_id": t.id, "title": t.title, "status": t.status.as_str() })
}

impl Engine {
    // ------------------------------------------------------------------ lookups

    pub(crate) fn task_ref(&self, db: &Db, r: &str) -> CoreResult<Task> {
        db.resolve_task(&self.project.id, r.trim())?
            .ok_or_else(|| CoreError::not_found("task", r))
    }

    pub fn get_task(&self, r: &str) -> CoreResult<Task> {
        self.read(|db| self.task_ref(db, r))
    }

    pub fn list_tasks(&self) -> CoreResult<Vec<Task>> {
        self.read(|db| db.list_tasks(&self.project.id))
    }

    pub fn progress(&self) -> CoreResult<Progress> {
        let goal = self.current_goal()?;
        let tasks = self.list_tasks()?;
        Ok(Progress::compute(tasks.iter().filter(|t| {
            goal.as_ref().map(|g| g.id == t.goal_id).unwrap_or(true)
        })))
    }

    fn agent_name(db: &Db, id: &AgentId) -> String {
        db.get_agent(id)
            .ok()
            .flatten()
            .map(|a| a.name)
            .unwrap_or_else(|| id.0.clone())
    }

    fn resolve_executor(&self, db: &Db, r: &str) -> CoreResult<Agent> {
        let a = db
            .list_agents(&self.project.id)?
            .into_iter()
            .find(|a| a.id.0 == r || a.name.eq_ignore_ascii_case(r))
            .ok_or_else(|| CoreError::not_found("agent", r))?;
        if !a.role.is_executor() {
            return Err(CoreError::InvalidInput(format!(
                "agent '{}' has role {} and cannot own implementation tasks",
                a.name, a.role
            )));
        }
        Ok(a)
    }

    // ------------------------------------------------------------------ transitions

    /// Validated status change that also maintains lifecycle timestamps.
    pub(crate) fn transition(
        &self,
        emit: &mut Emit,
        db: &Db,
        t: &mut Task,
        to: TaskStatus,
        kind: EventKind,
        mut payload: Value,
    ) -> CoreResult<()> {
        if !t.status.can_transition_to(to) {
            return Err(CoreError::InvalidTransition {
                what: "task",
                from: t.status.to_string(),
                to: to.to_string(),
            });
        }
        let from = t.status;
        t.status = to;
        t.updated_at = Utc::now();
        if to == TaskStatus::Running && t.started_at.is_none() {
            t.started_at = Some(Utc::now());
        }
        if to == TaskStatus::Done {
            t.completed_at = Some(Utc::now());
            t.progress = 100;
        }
        if !matches!(to, TaskStatus::Running | TaskStatus::Assigned) {
            // lease only meaningful while running
            if to != TaskStatus::Blocked {
                t.lease_expires_at = None;
            }
        }
        db.update_task(t)?;
        if let Value::Object(m) = &mut payload {
            m.insert("task_key".into(), json!(t.key));
            m.insert("title".into(), json!(t.title));
            m.insert("from".into(), json!(from.as_str()));
            m.insert("status".into(), json!(to.as_str()));
        }
        emit.push(kind, t.owner_agent_id.as_ref(), Some(&t.id), payload);
        Ok(())
    }

    fn release_agent_slot(
        &self,
        db: &Db,
        agent: &Option<AgentId>,
        task: &TaskId,
    ) -> CoreResult<()> {
        if let Some(aid) = agent {
            if let Some(mut a) = db.get_agent(aid)? {
                if a.current_task_id.as_ref() == Some(task) {
                    a.current_task_id = None;
                    db.update_agent(&a)?;
                }
            }
        }
        Ok(())
    }

    /// Promote BACKLOG tasks whose dependencies are all DONE and move pre-assigned READY tasks to ASSIGNED.
    pub(crate) fn refresh_ready(&self, db: &Db, emit: &mut Emit) -> CoreResult<()> {
        let tasks = db.list_tasks(&self.project.id)?;
        let status: HashMap<TaskId, TaskStatus> =
            tasks.iter().map(|t| (t.id.clone(), t.status)).collect();
        for mut t in tasks {
            if t.status == TaskStatus::Backlog
                && t.dependencies
                    .iter()
                    .all(|d| status.get(d) == Some(&TaskStatus::Done))
            {
                self.transition(
                    emit,
                    db,
                    &mut t,
                    TaskStatus::Ready,
                    EventKind::TaskReady,
                    json!({}),
                )?;
            }
            if t.status == TaskStatus::Ready {
                if let Some(owner) = t.owner_agent_id.clone() {
                    let name = Self::agent_name(db, &owner);
                    self.transition(emit, db, &mut t, TaskStatus::Assigned, EventKind::TaskAssigned, json!({ "to": name, "to_id": owner, "by": "orchestrator", "by_id": "orchestrator" }))?;
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ create / update / assign

    pub fn create_task(&self, actor: &Actor, nt: NewTask) -> CoreResult<Task> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can create tasks".into(),
            ));
        }
        let title = nt.title.trim().to_string();
        if title.is_empty() || title.len() > 200 {
            return Err(CoreError::InvalidInput(
                "task title must be 1..200 characters".into(),
            ));
        }
        let weight = nt.weight.unwrap_or(1.0);
        if !(weight.is_finite() && weight > 0.0 && weight <= 1000.0) {
            return Err(CoreError::InvalidInput(
                "weight must be in (0, 1000]".into(),
            ));
        }
        let cfg = self.config();
        self.tx(|db, emit| {
            if let Some(key) = &nt.idempotency_key {
                if let Some(existing) = db.find_task_by_idempotency(&self.project.id, key)? {
                    return Ok(existing);
                }
            }
            let goal = db
                .current_goal(&self.project.id)?
                .filter(|g| matches!(g.status, GoalStatus::Active | GoalStatus::Verifying))
                .ok_or_else(|| CoreError::Precondition("no active goal; create a goal first".into()))?;
            if db.count_goal_tasks(&goal.id)? >= cfg.max_tasks_per_goal {
                return Err(CoreError::LimitExceeded(format!("goal already has {} tasks (max {})", cfg.max_tasks_per_goal, cfg.max_tasks_per_goal)));
            }
            // parent & depth
            let mut parent_id = None;
            if let Some(p) = &nt.parent {
                let parent = self.task_ref(db, p)?;
                let mut depth = 1;
                let mut cur = parent.parent_task_id.clone();
                while let Some(id) = cur {
                    depth += 1;
                    cur = db.get_task(&id)?.and_then(|t| t.parent_task_id);
                    if depth > cfg.max_task_depth + 1 {
                        break;
                    }
                }
                if depth >= cfg.max_task_depth {
                    return Err(CoreError::LimitExceeded(format!("task nesting deeper than {}", cfg.max_task_depth)));
                }
                parent_id = Some(parent.id);
            }
            let mut deps = Vec::new();
            for d in &nt.dependencies {
                let dep = self.task_ref(db, d)?;
                if dep.status == TaskStatus::Canceled {
                    return Err(CoreError::InvalidInput(format!("dependency {} is canceled", dep.key)));
                }
                if !deps.contains(&dep.id) {
                    deps.push(dep.id);
                }
            }
            let owner = match &nt.assignee {
                Some(a) => Some(self.resolve_executor(db, a)?.id),
                None => None,
            };
            let reviewer = match &nt.reviewer {
                Some(r) => {
                    let a = db.list_agents(&self.project.id)?.into_iter().find(|a| a.id.0 == *r || a.name.eq_ignore_ascii_case(r)).ok_or_else(|| CoreError::not_found("agent", r))?;
                    if matches!(a.role, Role::Worker | Role::Specialist) && owner.as_ref() == Some(&a.id) {
                        return Err(CoreError::InvalidInput("reviewer cannot be the task owner".into()));
                    }
                    Some(a.id)
                }
                None => None,
            };
            let plan_rev = db.latest_plan(&goal.id)?.map(|p| p.revision).unwrap_or(0);
            let task = Task {
                id: TaskId::new(),
                key: String::new(),
                project_id: self.project.id.clone(),
                goal_id: goal.id.clone(),
                parent_task_id: parent_id,
                title,
                description: nt.description.clone(),
                acceptance_criteria: nt.acceptance_criteria.clone(),
                priority: nt.priority.unwrap_or(Priority::Normal),
                status: TaskStatus::Backlog,
                owner_agent_id: owner.clone(),
                dependencies: deps,
                estimated_weight: weight,
                progress: 0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                started_at: None,
                completed_at: None,
                attempt_count: 0,
                workspace_id: None,
                artifacts: vec![],
                evidence: vec![],
                review_status: if nt.requires_review { ReviewStatus::Pending } else { ReviewStatus::NotRequired },
                reviewer_agent_id: reviewer,
                blocked_reason: None,
                lease_expires_at: None,
                last_heartbeat_at: None,
                plan_revision: plan_rev,
                created_by: actor.id(),
                no_file_changes_expected: nt.no_file_changes_expected,
            };
            let task = db.insert_task(&task, nt.idempotency_key.as_deref())?;
            emit.push(
                EventKind::TaskCreated,
                actor.agent_id().as_ref(),
                Some(&task.id),
                json!({
                    "task_key": task.key, "title": task.title, "by": actor.id(), "by_id": actor.id(),
                    "dependencies": nt.dependencies, "weight": weight, "assignee": nt.assignee,
                    "plan_revision": plan_rev, "requires_review": nt.requires_review
                }),
            );
            self.refresh_ready(db, emit)?;
            db.get_task(&task.id)?.ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    pub fn update_task(&self, actor: &Actor, r: &str, patch: TaskPatch) -> CoreResult<Task> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can modify tasks".into(),
            ));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            if t.status.is_terminal() {
                return Err(CoreError::Precondition(format!("{} is {} and cannot be modified", t.key, t.status)));
            }
            let mut changes = vec![];
            if let Some(x) = patch.title.filter(|x| !x.trim().is_empty()) {
                t.title = x;
                changes.push("title");
            }
            if let Some(x) = patch.description {
                t.description = x;
                changes.push("description");
            }
            if let Some(x) = patch.acceptance_criteria {
                t.acceptance_criteria = x;
                changes.push("acceptance_criteria");
            }
            if let Some(x) = patch.priority {
                t.priority = x;
                changes.push("priority");
            }
            if let Some(x) = patch.no_file_changes_expected {
                t.no_file_changes_expected = x;
                changes.push("no_file_changes_expected");
            }
            let mut weight_changed = false;
            if let Some(w) = patch.weight {
                if !(w.is_finite() && w > 0.0 && w <= 1000.0) {
                    return Err(CoreError::InvalidInput("weight must be in (0, 1000]".into()));
                }
                weight_changed = (w - t.estimated_weight).abs() > f64::EPSILON;
                t.estimated_weight = w;
                changes.push("weight");
            }
            if !patch.add_dependencies.is_empty() || !patch.remove_dependencies.is_empty() {
                if matches!(t.status, TaskStatus::Running | TaskStatus::InReview | TaskStatus::Verifying) {
                    return Err(CoreError::Precondition("cannot change dependencies of a task in progress".into()));
                }
                let mut deps = t.dependencies.clone();
                for d in &patch.remove_dependencies {
                    let dep = self.task_ref(db, d)?;
                    deps.retain(|x| *x != dep.id);
                }
                for d in &patch.add_dependencies {
                    let dep = self.task_ref(db, d)?;
                    if dep.status == TaskStatus::Canceled {
                        return Err(CoreError::InvalidInput(format!("dependency {} is canceled", dep.key)));
                    }
                    if !deps.contains(&dep.id) {
                        deps.push(dep.id);
                    }
                }
                let mut edges: HashMap<TaskId, Vec<TaskId>> = db.list_tasks(&self.project.id)?.into_iter().map(|x| (x.id, x.dependencies)).collect();
                edges.insert(t.id.clone(), deps.clone());
                validate_acyclic(&edges)?;
                t.dependencies = deps;
                // A READY/ASSIGNED task that gained an unfinished dependency goes back to the backlog.
                if matches!(t.status, TaskStatus::Ready | TaskStatus::Assigned) {
                    let unfinished = t.dependencies.iter().any(|d| db.get_task(d).ok().flatten().map(|x| x.status != TaskStatus::Done).unwrap_or(true));
                    if unfinished {
                        t.status = TaskStatus::Backlog;
                    }
                }
                changes.push("dependencies");
            }
            t.updated_at = Utc::now();
            db.update_task(&t)?;
            if patch.cancel == Some(true) {
                let owner = t.owner_agent_id.clone();
                self.transition(emit, db, &mut t, TaskStatus::Canceled, EventKind::TaskCanceled, json!({ "by": actor.id(), "reason": patch.reason }))?;
                self.release_agent_slot(db, &owner, &t.id)?;
                db.end_attempts(&t.id, "canceled", patch.reason.as_deref())?;
            } else {
                let p = json!({ "task_key": t.key, "title": t.title, "changes": changes, "by": actor.id(), "weight": t.estimated_weight, "weight_changed": weight_changed });
                emit.push(EventKind::TaskProgress, actor.agent_id().as_ref(), Some(&t.id), json!({ "kind": "edited", "detail": p }));
            }
            self.refresh_ready(db, emit)?;
            db.get_task(&t.id)?.ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    pub fn assign_task(&self, actor: &Actor, r: &str, agent_ref: &str) -> CoreResult<Task> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can assign tasks".into(),
            ));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            let agent = self.resolve_executor(db, agent_ref)?;
            if matches!(t.status, TaskStatus::Running | TaskStatus::InReview | TaskStatus::Verifying | TaskStatus::Done | TaskStatus::Canceled | TaskStatus::Failed) {
                return Err(CoreError::Precondition(format!("{} is {}; release it before reassigning", t.key, t.status)));
            }
            if t.owner_agent_id.as_ref() == Some(&agent.id) && t.status == TaskStatus::Assigned {
                return Ok(t); // idempotent
            }
            let prev_owner = t.owner_agent_id.replace(agent.id.clone());
            if let Some(prev) = &prev_owner {
                if prev != &agent.id {
                    self.release_agent_slot(db, &Some(prev.clone()), &t.id)?;
                }
            }
            t.blocked_reason = None;
            // BLOCKED or ASSIGNED tasks go through READY on reassignment.
            if matches!(t.status, TaskStatus::Blocked | TaskStatus::Assigned) {
                self.transition(emit, db, &mut t, TaskStatus::Ready, EventKind::TaskUnblocked, json!({ "reason": "reassigned" }))?;
            }
            db.update_task(&t)?;
            if t.status == TaskStatus::Ready {
                self.transition(
                    emit,
                    db,
                    &mut t,
                    TaskStatus::Assigned,
                    EventKind::TaskAssigned,
                    json!({ "to": agent.name, "to_id": agent.id, "by": Self::agent_name(db, &actor.id()), "by_id": actor.id() }),
                )?;
            } else {
                // Still waiting on dependencies: ownership recorded, dispatch happens once READY.
                emit.push(
                    EventKind::TaskAssigned,
                    actor.agent_id().as_ref(),
                    Some(&t.id),
                    json!({ "task_key": t.key, "to": agent.name, "to_id": agent.id, "by": actor.id(), "by_id": actor.id(), "deferred": true }),
                );
            }
            db.get_task(&t.id)?.ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    /// Atomically claim a READY task. With no reference the best READY unowned task is chosen.
    pub fn claim_task(&self, actor: &Actor, r: Option<&str>) -> CoreResult<Task> {
        let (aid, role) = match actor {
            Actor::Agent { id, role } => (id.clone(), *role),
            _ => return Err(CoreError::Forbidden("only agents claim tasks".into())),
        };
        if !role.is_executor() {
            return Err(CoreError::Forbidden(format!(
                "role {role} cannot claim implementation tasks"
            )));
        }
        self.tx(|db, emit| {
            let agent = db
                .get_agent(&aid)?
                .ok_or_else(|| CoreError::not_found("agent", &aid))?;
            if let Some(cur) = &agent.current_task_id {
                let cur_task = db.get_task(cur)?;
                if cur_task
                    .map(|t| t.status == TaskStatus::Running)
                    .unwrap_or(false)
                    && r.map(|x| x != cur.0).unwrap_or(true)
                {
                    return Err(CoreError::Conflict(
                        "agent already has a running task; submit or release it first".into(),
                    ));
                }
            }
            let mut t = match r {
                Some(r) => self.task_ref(db, r)?,
                None => {
                    let mut ready: Vec<Task> = db
                        .list_tasks(&self.project.id)?
                        .into_iter()
                        .filter(|t| t.status == TaskStatus::Ready && t.owner_agent_id.is_none())
                        .collect();
                    ready.sort_by(|a, b| {
                        b.priority
                            .rank()
                            .cmp(&a.priority.rank())
                            .then(a.created_at.cmp(&b.created_at))
                    });
                    ready.into_iter().next().ok_or_else(|| {
                        CoreError::Precondition("no_ready_task: nothing is ready to claim".into())
                    })?
                }
            };
            match (t.status, &t.owner_agent_id) {
                (TaskStatus::Ready, None) => {
                    t.owner_agent_id = Some(aid.clone());
                    db.update_task(&t)?;
                    self.transition(
                        emit,
                        db,
                        &mut t,
                        TaskStatus::Assigned,
                        EventKind::TaskClaimed,
                        json!({ "by": agent.name, "by_id": aid, "to": agent.name, "to_id": aid }),
                    )?;
                }
                (TaskStatus::Assigned, Some(o)) | (TaskStatus::Running, Some(o)) if *o == aid => {}
                _ => {
                    return Err(CoreError::Conflict(format!(
                        "{} cannot be claimed (status {}, owner {})",
                        t.key,
                        t.status,
                        t.owner_agent_id
                            .as_ref()
                            .map(|o| Self::agent_name(db, o))
                            .unwrap_or_else(|| "none".into())
                    )))
                }
            }
            db.get_task(&t.id)?
                .ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    /// Orchestrator: begin execution of an ASSIGNED task by its owner (attempt + lease).
    pub fn start_task(&self, r: &str) -> CoreResult<Task> {
        let cfg = self.config();
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            let owner = t
                .owner_agent_id
                .clone()
                .ok_or_else(|| CoreError::Precondition("task has no owner".into()))?;
            if t.status == TaskStatus::Running {
                return Ok(t);
            }
            // One task at a time per agent: never overwrite another running task's slot.
            if let Some(a) = db.get_agent(&owner)? {
                if let Some(cur) = &a.current_task_id {
                    if cur != &t.id
                        && db
                            .get_task(cur)?
                            .map(|c| c.status == TaskStatus::Running)
                            .unwrap_or(false)
                    {
                        return Err(CoreError::Conflict(format!(
                            "{} is already running another task",
                            a.name
                        )));
                    }
                }
            }
            if t.attempt_count >= cfg.max_attempts {
                self.transition(
                    emit,
                    db,
                    &mut t,
                    TaskStatus::Failed,
                    EventKind::TaskFailed,
                    json!({ "reason": format!("exceeded {} attempts", cfg.max_attempts) }),
                )?;
                return Ok(t);
            }
            t.attempt_count += 1;
            t.lease_expires_at = Some(Utc::now() + Duration::seconds(cfg.lease_secs));
            t.last_heartbeat_at = Some(Utc::now());
            t.blocked_reason = None;
            let attempt = t.attempt_count;
            self.transition(
                emit,
                db,
                &mut t,
                TaskStatus::Running,
                EventKind::TaskStarted,
                json!({ "attempt": attempt, "agent_id": owner }),
            )?;
            // transition() clears nothing for Running, but persists; re-save the lease explicitly.
            t.lease_expires_at = Some(Utc::now() + Duration::seconds(cfg.lease_secs));
            db.update_task(&t)?;
            db.begin_attempt(&t.id, &owner, t.attempt_count)?;
            if let Some(mut a) = db.get_agent(&owner)? {
                a.current_task_id = Some(t.id.clone());
                db.update_agent(&a)?;
            }
            Ok(t)
        })
    }

    pub fn attach_workspace(&self, r: &str, ws: &Workspace) -> CoreResult<()> {
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            db.upsert_workspace(ws)?;
            t.workspace_id = Some(ws.id.clone());
            db.update_task(&t)?;
            emit.push(
                EventKind::WorkspaceUpdated,
                ws.agent_id.as_ref(),
                Some(&t.id),
                json!({ "workspace_id": ws.id, "path": ws.path, "branch": ws.branch, "kind": ws.kind, "status": ws.status, "integration": ws.integration_status }),
            );
            Ok(())
        })
    }

    // ------------------------------------------------------------------ progress & blockers

    fn require_owner(&self, actor: &Actor, t: &Task) -> CoreResult<()> {
        match actor {
            Actor::Agent { id, .. } if t.owner_agent_id.as_ref() == Some(id) => Ok(()),
            _ if actor.is_leader() => Ok(()),
            _ => Err(CoreError::Forbidden(format!(
                "only the owner of {} (or the Leader) may do this",
                t.key
            ))),
        }
    }

    pub fn report_progress(
        &self,
        actor: &Actor,
        r: &str,
        percent: Option<u8>,
        note: &str,
    ) -> CoreResult<Task> {
        let cfg = self.config();
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            self.require_owner(actor, &t)?;
            if t.status != TaskStatus::Running {
                return Err(CoreError::Precondition(format!("{} is {}; progress can only be reported while RUNNING", t.key, t.status)));
            }
            if let Some(p) = percent {
                if p > 100 {
                    return Err(CoreError::InvalidInput("percent must be 0..=100".into()));
                }
                // Reported progress is advisory and capped below 100: only verification completes a task.
                t.progress = p.min(99);
            }
            t.last_heartbeat_at = Some(Utc::now());
            t.lease_expires_at = Some(Utc::now() + Duration::seconds(cfg.lease_secs));
            t.updated_at = Utc::now();
            db.update_task(&t)?;
            emit.push(
                EventKind::TaskProgress,
                actor.agent_id().as_ref(),
                Some(&t.id),
                json!({ "task_key": t.key, "percent": t.progress, "note": note, "by": Self::agent_name(db, &actor.id()), "by_id": actor.id() }),
            );
            Ok(t)
        })
    }

    pub fn block_task(
        &self,
        actor: &Actor,
        r: &str,
        reason: &str,
        needs: Option<&str>,
    ) -> CoreResult<Task> {
        if reason.trim().is_empty() {
            return Err(CoreError::InvalidInput("a blocker needs a reason".into()));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            self.require_owner(actor, &t)?;
            if !matches!(t.status, TaskStatus::Running | TaskStatus::Assigned | TaskStatus::Ready) {
                return Err(CoreError::Precondition(format!("{} is {}; only active tasks can be blocked", t.key, t.status)));
            }
            t.blocked_reason = Some(match needs {
                Some(n) => format!("{reason} (needs: {n})"),
                None => reason.to_string(),
            });
            let owner = t.owner_agent_id.clone();
            self.transition(emit, db, &mut t, TaskStatus::Blocked, EventKind::TaskBlocked, json!({ "reason": reason, "needs": needs, "by": Self::agent_name(db, &actor.id()), "by_id": actor.id() }))?;
            self.release_agent_slot(db, &owner, &t.id)?;
            db.end_attempts(&t.id, "blocked", Some(reason))?;
            if !actor.is_leader() {
                self.notify_leader_tx(db, emit, actor, Some(&t), MessageType::Blocker, &format!("{} blocked", t.key), &format!("{} is blocked: {}{}", t.key, reason, needs.map(|n| format!("\nNeeds: {n}")).unwrap_or_default()))?;
            }
            Ok(t)
        })
    }

    pub fn release_task(&self, actor: &Actor, r: &str, reason: &str) -> CoreResult<Task> {
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            self.require_owner(actor, &t)?;
            if !matches!(
                t.status,
                TaskStatus::Running
                    | TaskStatus::Assigned
                    | TaskStatus::Blocked
                    | TaskStatus::Ready
            ) {
                return Err(CoreError::Precondition(format!(
                    "{} is {} and cannot be released",
                    t.key, t.status
                )));
            }
            let prev = t.owner_agent_id.take();
            t.blocked_reason = None;
            t.progress = 0;
            if t.status == TaskStatus::Ready {
                db.update_task(&t)?;
            } else {
                db.update_task(&t)?;
                self.transition(
                    emit,
                    db,
                    &mut t,
                    TaskStatus::Ready,
                    EventKind::TaskReleased,
                    json!({ "reason": reason, "by": actor.id() }),
                )?;
            }
            self.release_agent_slot(db, &prev, &t.id)?;
            db.end_attempts(&t.id, "released", Some(reason))?;
            db.get_task(&t.id)?
                .ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    // ------------------------------------------------------------------ evidence

    fn add_evidence_tx(
        &self,
        db: &Db,
        emit: &mut Emit,
        t: &mut Task,
        actor: &Actor,
        e: &EvidenceInput,
    ) -> CoreResult<()> {
        if e.summary.trim().is_empty() {
            return Err(CoreError::InvalidInput("evidence needs a summary".into()));
        }
        let ev = Evidence {
            kind: e.kind,
            summary: maw_core::redact::redact(&e.summary),
            passed: e.passed,
            detail: e.detail.clone(),
            recorded_by: actor.id(),
            recorded_at: Utc::now(),
        };
        db.add_evidence(&t.id, &ev)?;
        t.evidence.push(ev);
        emit.push(
            EventKind::TaskEvidence,
            actor.agent_id().as_ref(),
            Some(&t.id),
            json!({ "task_key": t.key, "kind": e.kind.as_str(), "passed": e.passed, "summary": e.summary }),
        );
        Ok(())
    }

    pub fn add_evidence(&self, actor: &Actor, r: &str, e: EvidenceInput) -> CoreResult<Task> {
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            self.require_owner(actor, &t)?;
            if t.status.is_terminal() {
                return Err(CoreError::Precondition(format!(
                    "{} is {}",
                    t.key, t.status
                )));
            }
            self.add_evidence_tx(db, emit, &mut t, actor, &e)?;
            db.get_task(&t.id)?
                .ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    pub fn register_artifact(
        &self,
        actor: &Actor,
        r: Option<&str>,
        kind: &str,
        path: Option<String>,
        description: &str,
    ) -> CoreResult<Artifact> {
        self.tx(|db, emit| {
            let task = match r {
                Some(r) => Some(self.task_ref(db, r)?),
                None => None,
            };
            if let Some(t) = &task {
                self.require_owner(actor, t)?;
            }
            if let Some(p) = &path {
                if p.contains("..") || p.starts_with('/') && !p.starts_with(&self.project.path) {
                    // artifact paths must be project relative or inside the project
                    return Err(CoreError::InvalidInput("artifact path must be inside the project".into()));
                }
            }
            let a = Artifact {
                id: ArtifactId::new(),
                task_id: task.as_ref().map(|t| t.id.clone()),
                kind: kind.to_string(),
                path,
                description: description.to_string(),
                registered_by: actor.id(),
                registered_at: Utc::now(),
            };
            db.add_artifact(&a)?;
            emit.push(
                EventKind::ArtifactRegistered,
                actor.agent_id().as_ref(),
                a.task_id.as_ref(),
                json!({ "artifact_id": a.id, "kind": a.kind, "path": a.path, "task_key": task.map(|t| t.key) }),
            );
            Ok(a)
        })
    }

    // ------------------------------------------------------------------ submit / review / verify

    pub fn submit_result(
        &self,
        actor: &Actor,
        r: &str,
        summary: &str,
        evidence: Vec<EvidenceInput>,
        changed_files: Vec<String>,
    ) -> CoreResult<Task> {
        if summary.trim().is_empty() {
            return Err(CoreError::InvalidInput("a result needs a summary".into()));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            match actor {
                Actor::Agent { id, .. } if t.owner_agent_id.as_ref() == Some(id) => {}
                _ => return Err(CoreError::Forbidden(format!("only the owner of {} can submit its result", t.key))),
            }
            if t.status != TaskStatus::Running {
                return Err(CoreError::Precondition(format!("{} is {}; only RUNNING tasks can be submitted", t.key, t.status)));
            }
            if !changed_files.is_empty() {
                self.add_evidence_tx(
                    db,
                    emit,
                    &mut t,
                    actor,
                    &EvidenceInput { kind: EvidenceKind::FileChanges, summary: format!("{} file(s) changed", changed_files.len()), passed: true, detail: json!({ "files": changed_files }) },
                )?;
            }
            for e in &evidence {
                self.add_evidence_tx(db, emit, &mut t, actor, e)?;
            }
            let needs_review = matches!(t.review_status, ReviewStatus::Pending | ReviewStatus::Rejected);
            if needs_review {
                t.review_status = ReviewStatus::Pending;
            }
            t.progress = 99;
            let owner = t.owner_agent_id.clone();
            let name = Self::agent_name(db, &actor.id());
            let (to, kind) = if needs_review { (TaskStatus::InReview, EventKind::TaskReviewRequested) } else { (TaskStatus::Verifying, EventKind::TaskSubmitted) };
            let evidence_count = t.evidence.len();
            self.transition(emit, db, &mut t, to, kind, json!({ "summary": summary, "by": name, "by_id": actor.id(), "evidence": evidence_count }))?;
            if needs_review {
                emit.push(EventKind::TaskSubmitted, actor.agent_id().as_ref(), Some(&t.id), json!({ "task_key": t.key, "summary": summary, "by": name, "by_id": actor.id() }));
            }
            self.release_agent_slot(db, &owner, &t.id)?;
            db.end_attempts(&t.id, "submitted", Some(summary))?;
            self.notify_leader_tx(
                db,
                emit,
                actor,
                Some(&t),
                MessageType::Result,
                &format!("{} submitted ({})", t.key, if needs_review { "awaiting review" } else { "awaiting verification" }),
                &format!("{name} submitted {} \"{}\".\nSummary: {summary}\nEvidence items: {}", t.key, t.title, t.evidence.len()),
            )?;
            Ok(t)
        })
    }

    /// Owner or Leader asks for a review of a RUNNING/VERIFYING task.
    pub fn request_review(
        &self,
        actor: &Actor,
        r: &str,
        reviewer: Option<&str>,
    ) -> CoreResult<Task> {
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            self.require_owner(actor, &t)?;
            if !matches!(t.status, TaskStatus::Running | TaskStatus::Verifying) {
                return Err(CoreError::Precondition(format!(
                    "{} is {}; reviews apply to RUNNING or VERIFYING tasks",
                    t.key, t.status
                )));
            }
            if let Some(rv) = reviewer {
                let a = db
                    .list_agents(&self.project.id)?
                    .into_iter()
                    .find(|a| a.id.0 == rv || a.name.eq_ignore_ascii_case(rv))
                    .ok_or_else(|| CoreError::not_found("agent", rv))?;
                if Some(&a.id) == t.owner_agent_id.as_ref() {
                    return Err(CoreError::InvalidInput(
                        "a task cannot be reviewed by its owner".into(),
                    ));
                }
                t.reviewer_agent_id = Some(a.id);
            }
            t.review_status = ReviewStatus::Pending;
            let owner = t.owner_agent_id.clone();
            let reviewer_id = t.reviewer_agent_id.clone();
            self.transition(
                emit,
                db,
                &mut t,
                TaskStatus::InReview,
                EventKind::TaskReviewRequested,
                json!({ "by": actor.id(), "reviewer": reviewer_id }),
            )?;
            self.release_agent_slot(db, &owner, &t.id)?;
            Ok(t)
        })
    }

    /// Orchestrator picks a reviewer for an IN_REVIEW task.
    pub fn assign_reviewer(&self, r: &str, reviewer: &AgentId) -> CoreResult<Task> {
        self.tx(|db, _| {
            let mut t = self.task_ref(db, r)?;
            if t.status != TaskStatus::InReview {
                return Err(CoreError::Precondition("task is not in review".into()));
            }
            if t.owner_agent_id.as_ref() == Some(reviewer) {
                return Err(CoreError::InvalidInput(
                    "owner cannot review own task".into(),
                ));
            }
            t.reviewer_agent_id = Some(reviewer.clone());
            db.update_task(&t)?;
            if let Some(mut a) = db.get_agent(reviewer)? {
                a.current_task_id = Some(t.id.clone());
                db.update_agent(&a)?;
            }
            Ok(t)
        })
    }

    pub fn submit_review(
        &self,
        actor: &Actor,
        r: &str,
        approve: bool,
        feedback: &str,
    ) -> CoreResult<Task> {
        if !approve && feedback.trim().is_empty() {
            return Err(CoreError::InvalidInput(
                "a rejection must include actionable feedback".into(),
            ));
        }
        match actor.role() {
            Some(Role::Reviewer) | Some(Role::Leader) | None => {}
            Some(r) => return Err(CoreError::Forbidden(format!("role {r} cannot review"))),
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            if t.status != TaskStatus::InReview {
                return Err(CoreError::Precondition(format!("{} is {}; not awaiting review", t.key, t.status)));
            }
            if actor.agent_id().is_some() && actor.agent_id() == t.owner_agent_id {
                return Err(CoreError::Forbidden("a task owner cannot review its own work".into()));
            }
            let reviewer_name = Self::agent_name(db, &actor.id());
            self.add_evidence_tx(
                db,
                emit,
                &mut t,
                actor,
                &EvidenceInput { kind: EvidenceKind::Review, summary: format!("{}: {}", if approve { "approved" } else { "rejected" }, feedback), passed: approve, detail: json!({ "reviewer": reviewer_name }) },
            )?;
            if let Some(rid) = actor.agent_id().filter(|_| true) {
                self.release_agent_slot(db, &Some(rid), &t.id)?;
            }
            let owner = t.owner_agent_id.clone();
            if approve {
                t.review_status = ReviewStatus::Approved;
                self.transition(emit, db, &mut t, TaskStatus::Verifying, EventKind::TaskReviewed, json!({ "verdict": "approved", "reviewer": reviewer_name, "reviewer_id": actor.id(), "feedback": feedback }))?;
            } else {
                t.review_status = ReviewStatus::Rejected;
                t.progress = 50;
                let next = if owner.is_some() { TaskStatus::Assigned } else { TaskStatus::Ready };
                self.transition(emit, db, &mut t, next, EventKind::TaskReviewed, json!({ "verdict": "rejected", "reviewer": reviewer_name, "reviewer_id": actor.id(), "feedback": feedback, "owner": owner }))?;
                if let Some(o) = owner {
                    self.send_message_tx(
                        db,
                        emit,
                        &actor.id(),
                        &reviewer_name,
                        NewMessage {
                            recipient: o,
                            message_type: MessageType::ReviewFeedback,
                            subject: format!("{} review: changes requested", t.key),
                            content: feedback.to_string(),
                            task_id: Some(t.id.clone()),
                            correlation_id: Some(t.id.0.clone()),
                            idempotency_key: None,
                        },
                    )?;
                }
            }
            self.notify_leader_tx(
                db,
                emit,
                actor,
                Some(&t),
                MessageType::ReviewFeedback,
                &format!("{} review {}", t.key, if approve { "approved" } else { "rejected" }),
                &format!("{reviewer_name} {} {}.\n{feedback}", if approve { "approved" } else { "rejected" }, t.key),
            )?;
            db.get_task(&t.id)?.ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    /// Final acceptance of a VERIFYING task. DONE requires passing evidence (and an approved
    /// review where one is required); an agent's claim alone never completes a task.
    pub fn verify_task(&self, actor: &Actor, r: &str, pass: bool, notes: &str) -> CoreResult<Task> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can verify tasks".into(),
            ));
        }
        if notes.trim().is_empty() {
            return Err(CoreError::InvalidInput(
                "verification needs notes describing what was checked".into(),
            ));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            if t.status != TaskStatus::Verifying {
                return Err(CoreError::Precondition(format!(
                    "{} is {}; only VERIFYING tasks can be verified",
                    t.key, t.status
                )));
            }
            if pass {
                // Evidence must exist independently of the verifier's own sign-off.
                let blockers = t.completion_blockers();
                if !blockers.is_empty() {
                    return Err(CoreError::Precondition(format!(
                        "{} cannot be completed: {}",
                        t.key,
                        blockers.join("; ")
                    )));
                }
            }
            let criteria = t.acceptance_criteria.clone();
            self.add_evidence_tx(
                db,
                emit,
                &mut t,
                actor,
                &EvidenceInput {
                    kind: EvidenceKind::Verification,
                    summary: notes.to_string(),
                    passed: pass,
                    detail: json!({ "criteria": criteria }),
                },
            )?;
            if pass {
                emit.push(
                    EventKind::TaskVerified,
                    actor.agent_id().as_ref(),
                    Some(&t.id),
                    json!({ "task_key": t.key, "by": actor.id(), "notes": notes }),
                );
                self.transition(
                    emit,
                    db,
                    &mut t,
                    TaskStatus::Done,
                    EventKind::TaskCompleted,
                    json!({ "by": actor.id(), "notes": notes }),
                )?;
                db.end_attempts(&t.id, "done", None)?;
                self.refresh_ready(db, emit)?;
                // Tell the Leader when everything is verified.
                let tasks = db.list_tasks(&self.project.id)?;
                if tasks
                    .iter()
                    .filter(|x| x.goal_id == t.goal_id)
                    .all(|x| matches!(x.status, TaskStatus::Done | TaskStatus::Canceled))
                    && !actor.is_leader()
                {
                    // only relevant for non-leader actors; Leader sees it in the response
                }
            } else {
                let owner = t.owner_agent_id.clone();
                t.progress = 50;
                let next = if owner.is_some() {
                    TaskStatus::Assigned
                } else {
                    TaskStatus::Ready
                };
                self.transition(
                    emit,
                    db,
                    &mut t,
                    next,
                    EventKind::TaskReviewed,
                    json!({ "verdict": "failed_verification", "notes": notes }),
                )?;
                if let Some(o) = owner {
                    self.send_message_tx(
                        db,
                        emit,
                        &actor.id(),
                        &Self::agent_name(db, &actor.id()),
                        NewMessage {
                            recipient: o,
                            message_type: MessageType::ReviewFeedback,
                            subject: format!("{} failed verification", t.key),
                            content: notes.to_string(),
                            task_id: Some(t.id.clone()),
                            correlation_id: Some(t.id.0.clone()),
                            idempotency_key: None,
                        },
                    )?;
                }
            }
            db.get_task(&t.id)?
                .ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    pub fn fail_task(&self, actor: &Actor, r: &str, reason: &str) -> CoreResult<Task> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the orchestrator) can fail a task".into(),
            ));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            let owner = t.owner_agent_id.clone();
            self.transition(
                emit,
                db,
                &mut t,
                TaskStatus::Failed,
                EventKind::TaskFailed,
                json!({ "reason": reason, "by": actor.id() }),
            )?;
            self.release_agent_slot(db, &owner, &t.id)?;
            db.end_attempts(&t.id, "failed", Some(reason))?;
            Ok(t)
        })
    }

    /// Re-queue a failed or blocked task for another bounded attempt (explicit recovery action).
    pub fn requeue_task(&self, actor: &Actor, r: &str) -> CoreResult<Task> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can requeue tasks".into(),
            ));
        }
        self.tx(|db, emit| {
            let mut t = self.task_ref(db, r)?;
            if !matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked) {
                return Err(CoreError::Precondition(format!(
                    "{} is {}; only FAILED or BLOCKED tasks can be requeued",
                    t.key, t.status
                )));
            }
            if t.status == TaskStatus::Failed {
                // explicit human/Leader decision: grant one more attempt
                t.attempt_count = t.attempt_count.saturating_sub(1);
            }
            t.blocked_reason = None;
            let to = TaskStatus::Ready;
            self.transition(
                emit,
                db,
                &mut t,
                to,
                EventKind::TaskUnblocked,
                json!({ "by": actor.id() }),
            )?;
            self.refresh_ready(db, emit)?;
            db.get_task(&t.id)?
                .ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    // ------------------------------------------------------------------ leases & recovery

    /// Block RUNNING tasks whose lease expired. They are never silently re-executed because the
    /// side effects of the previous attempt are unknown; the Leader/user decides.
    pub fn sweep_leases(&self) -> CoreResult<Vec<TaskId>> {
        self.tx(|db, emit| {
            let now = Utc::now();
            let mut out = vec![];
            for mut t in db.list_tasks(&self.project.id)? {
                if t.status == TaskStatus::Running && t.lease_expires_at.map(|l| l < now).unwrap_or(false) {
                    let owner = t.owner_agent_id.clone();
                    t.blocked_reason = Some("lease expired: the owner stopped reporting; previous side effects are unknown".into());
                    self.transition(emit, db, &mut t, TaskStatus::Blocked, EventKind::TaskLeaseExpired, json!({ "owner": owner }))?;
                    self.release_agent_slot(db, &owner, &t.id)?;
                    db.end_attempts(&t.id, "lease_expired", None)?;
                    self.notify_leader_tx(
                        db,
                        emit,
                        &Actor::System,
                        Some(&t),
                        MessageType::Blocker,
                        &format!("{} lease expired", t.key),
                        &format!("{} stopped reporting and its lease expired. Decide: release/reassign it or cancel it.", t.key),
                    )?;
                    out.push(t.id.clone());
                }
            }
            Ok(out)
        })
    }

    pub fn task_events(&self, r: &str, limit: usize) -> CoreResult<Vec<Event>> {
        self.read(|db| {
            let t = self.task_ref(db, r)?;
            db.events_for_task(&t.id, limit)
        })
    }

    pub fn dependencies_of(&self, r: &str) -> CoreResult<Value> {
        self.read(|db| {
            let t = self.task_ref(db, r)?;
            let all = db.list_tasks(&self.project.id)?;
            let by_id: HashMap<&TaskId, &Task> = all.iter().map(|x| (&x.id, x)).collect();
            let deps: Vec<Value> = t.dependencies.iter().filter_map(|d| by_id.get(d)).map(|d| task_json(d)).collect();
            let dependents: Vec<Value> = all.iter().filter(|x| x.dependencies.contains(&t.id)).map(task_json).collect();
            let unmet: Vec<String> = t.dependencies.iter().filter_map(|d| by_id.get(d)).filter(|d| d.status != TaskStatus::Done).map(|d| d.key.clone()).collect();
            Ok(json!({ "task": task_json(&t), "depends_on": deps, "dependents": dependents, "unmet": unmet }))
        })
    }
}
