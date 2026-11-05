//! The orchestration engine: validated domain operations over the store.
//!
//! Every mutating operation runs in a single SQLite transaction together with the events it
//! produces, then publishes those events on a broadcast channel *after* commit. The TUI, MCP
//! tools, the mock provider and the supervisor all use this same API.

use chrono::{DateTime, Utc};
use maw_core::*;
use maw_storage::{Db, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, Notify};

/// Who is performing an operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Actor {
    /// The human operator (TUI / CLI).
    User,
    /// The orchestrator itself (scheduler, recovery).
    System,
    /// An authenticated agent. Identity and role are assigned by MAW, never self-declared.
    Agent { id: AgentId, role: Role },
}

impl Actor {
    pub fn id(&self) -> AgentId {
        match self {
            Actor::User => AgentId::from("user"),
            Actor::System => AgentId::from("orchestrator"),
            Actor::Agent { id, .. } => id.clone(),
        }
    }
    pub fn role(&self) -> Option<Role> {
        match self {
            Actor::Agent { role, .. } => Some(*role),
            _ => None,
        }
    }
    pub fn is_privileged(&self) -> bool {
        matches!(self, Actor::User | Actor::System)
    }
    pub fn is_leader(&self) -> bool {
        self.is_privileged() || self.role() == Some(Role::Leader)
    }
    pub fn agent_id(&self) -> Option<AgentId> {
        match self {
            Actor::Agent { id, .. } => Some(id.clone()),
            _ => None,
        }
    }
}

/// Events produced inside a transaction; persisted by [`Emit::flush`], published after commit.
pub struct Emit {
    project: ProjectId,
    run: RunId,
    pub(crate) events: Vec<Event>,
}

impl Emit {
    pub fn push(
        &mut self,
        kind: EventKind,
        agent: Option<&AgentId>,
        task: Option<&TaskId>,
        payload: Value,
    ) {
        let mut e = Event::new(self.project.clone(), kind, payload).run(&self.run);
        e.agent_id = agent.cloned();
        e.task_id = task.cloned();
        self.events.push(e);
    }
    pub fn flush(&mut self, db: &Db) -> CoreResult<()> {
        for e in &mut self.events {
            db.append_event(e)?;
        }
        Ok(())
    }
}

/// A consistent view of everything the TUI needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub project: Project,
    pub goal: Option<Goal>,
    pub plan: Option<PlanRevision>,
    pub agents: Vec<Agent>,
    pub tasks: Vec<Task>,
    pub progress: Progress,
    pub approvals: Vec<Approval>,
    pub recent_events: Vec<Event>,
    pub recent_messages: Vec<Message>,
    pub workspaces: Vec<Workspace>,
    pub paused: bool,
    pub run_id: RunId,
}

pub struct Engine {
    pub(crate) store: Arc<Store>,
    pub(crate) project: Project,
    pub(crate) run_id: RunId,
    pub(crate) cfg: RwLock<OrchestratorConfig>,
    /// The working folder chosen for the project (see `folder.rs`); `None` = the project's own folder.
    pub(crate) work_dir: RwLock<Option<std::path::PathBuf>>,
    /// Places the Leader may propose a working folder in.
    pub(crate) workspace_roots: RwLock<Vec<std::path::PathBuf>>,
    events: broadcast::Sender<Event>,
    /// Wakes the scheduler/supervisor when state changes.
    pub(crate) wake: Arc<Notify>,
    paused: AtomicBool,
    tokens: Mutex<HashMap<String, AgentId>>,
    track_run: bool,
}

impl Engine {
    /// Open the engine for a project, creating the project row if needed. Starts a new run.
    pub fn open(
        store: Arc<Store>,
        name: &str,
        path: &str,
        cfg: OrchestratorConfig,
    ) -> CoreResult<Arc<Engine>> {
        Self::open_inner(store, name, path, cfg, true)
    }

    /// For short-lived utility commands (CLI queries/edits): no run is recorded, so such
    /// invocations are never mistaken for crashes by recovery and add no run events.
    pub fn open_utility(
        store: Arc<Store>,
        name: &str,
        path: &str,
        cfg: OrchestratorConfig,
    ) -> CoreResult<Arc<Engine>> {
        Self::open_inner(store, name, path, cfg, false)
    }

    fn open_inner(
        store: Arc<Store>,
        name: &str,
        path: &str,
        cfg: OrchestratorConfig,
        track_run: bool,
    ) -> CoreResult<Arc<Engine>> {
        let project = store.write(|db| {
            let project = match db.find_project(name)? {
                Some(p) => p,
                None => {
                    let p = Project {
                        id: ProjectId::new(),
                        name: name.to_string(),
                        path: path.to_string(),
                        created_at: Utc::now(),
                    };
                    db.insert_project(&p)?;
                    p
                }
            };
            let paused = db.kv_get(&project.id, "scheduler_paused")?.as_deref() == Some("1");
            // a chosen working folder that has since disappeared is forgotten, not trusted
            let work_dir = db
                .kv_get(&project.id, "work_dir")?
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_dir());
            Ok((project, paused, work_dir))
        })?;
        let (project, paused, work_dir) = (project.0, project.1, project.2);
        let run_id = RunId::new();
        if track_run {
            store.write(|db| {
                db.begin_run(&Run {
                    id: run_id.clone(),
                    project_id: project.id.clone(),
                    started_at: Utc::now(),
                    stopped_at: None,
                    clean_shutdown: false,
                })
            })?;
        }
        let (tx, _) = broadcast::channel(4096);
        let eng = Arc::new(Engine {
            store,
            project,
            run_id,
            cfg: RwLock::new(cfg),
            work_dir: RwLock::new(work_dir),
            workspace_roots: RwLock::new(Vec::new()),
            events: tx,
            wake: Arc::new(Notify::new()),
            paused: AtomicBool::new(paused),
            tokens: Mutex::new(HashMap::new()),
            track_run,
        });
        if track_run {
            eng.tx(|_, emit| {
                emit.push(
                    EventKind::RunStarted,
                    None,
                    None,
                    json!({ "project": eng.project.name }),
                );
                Ok(())
            })?;
        }
        Ok(eng)
    }

    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn project_id(&self) -> &ProjectId {
        &self.project.id
    }
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }
    pub fn config(&self) -> OrchestratorConfig {
        self.cfg.read().map(|c| c.clone()).unwrap_or_default()
    }
    pub fn set_config(&self, cfg: OrchestratorConfig) {
        if let Ok(mut c) = self.cfg.write() {
            *c = cfg;
        }
        self.wake.notify_waiters();
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
    pub fn wake_handle(&self) -> Arc<Notify> {
        self.wake.clone()
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Run `f` in a transaction; events pushed to `emit` are persisted atomically with the
    /// state change and broadcast after commit.
    pub fn tx<R>(&self, f: impl FnOnce(&Db, &mut Emit) -> CoreResult<R>) -> CoreResult<R> {
        let mut emit = Emit {
            project: self.project.id.clone(),
            run: self.run_id.clone(),
            events: vec![],
        };
        let r = self.store.write(|db| {
            let r = f(db, &mut emit)?;
            emit.flush(db)?;
            Ok(r)
        })?;
        // Only wake the scheduler when something changed; otherwise the scheduler's own
        // housekeeping transactions would re-trigger it forever.
        let changed = !emit.events.is_empty();
        for e in emit.events {
            let _ = self.events.send(e);
        }
        if changed {
            self.wake.notify_one();
        }
        Ok(r)
    }

    pub fn read<R>(&self, f: impl FnOnce(&Db) -> CoreResult<R>) -> CoreResult<R> {
        self.store.read(f)
    }

    // ------------------------------------------------------------------ tokens / identity

    /// Mint a bearer token bound to this agent. The MCP server resolves tokens to identities,
    /// so an agent can never claim to be someone else.
    pub fn mint_token(&self, agent: &AgentId) -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = format!(
            "maw_tok_{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        if let Ok(mut t) = self.tokens.lock() {
            // One live token per agent.
            t.retain(|_, a| a != agent);
            t.insert(token.clone(), agent.clone());
        }
        token
    }
    pub fn revoke_tokens(&self, agent: &AgentId) {
        if let Ok(mut t) = self.tokens.lock() {
            t.retain(|_, a| a != agent);
        }
    }
    /// Resolve a bearer token to an actor with the role currently stored for the agent.
    pub fn authenticate(&self, token: &str) -> Option<Actor> {
        let id = self.tokens.lock().ok()?.get(token).cloned()?;
        let agent = self.read(|db| db.get_agent(&id)).ok()??;
        Some(Actor::Agent {
            id: agent.id,
            role: agent.role,
        })
    }

    // ------------------------------------------------------------------ agents

    pub fn add_agent(
        &self,
        name: &str,
        provider: ProviderKind,
        role: Role,
        model: Option<String>,
        permission: PermissionProfile,
        workspace: Option<String>,
    ) -> CoreResult<Agent> {
        if name.trim().is_empty() || name.len() > 64 {
            return Err(CoreError::InvalidInput(
                "agent name must be 1..64 characters".into(),
            ));
        }
        self.tx(|db, emit| {
            let existing = db.list_agents(&self.project.id)?;
            if existing.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
                return Err(CoreError::Conflict(format!(
                    "agent '{name}' already exists"
                )));
            }
            if role == Role::Leader && existing.iter().any(|a| a.role == Role::Leader) {
                return Err(CoreError::Conflict(
                    "project already has a Leader; change its role first".into(),
                ));
            }
            let agent = Agent {
                id: AgentId::new(),
                project_id: self.project.id.clone(),
                name: name.to_string(),
                provider,
                model,
                effort: None,
                role,
                state: AgentState::Disconnected,
                permission,
                session_id: None,
                native_session_id: None,
                workspace,
                current_task_id: None,
                capabilities: Capabilities::default(),
                last_heartbeat: None,
                last_error: None,
                usage: UsageMetrics::default(),
                created_at: Utc::now(),
            };
            db.insert_agent(&agent)?;
            emit.push(
                EventKind::AgentRegistered,
                Some(&agent.id),
                None,
                json!({ "name": agent.name, "provider": provider.as_str(), "role": role.as_str() }),
            );
            Ok(agent)
        })
    }

    pub fn get_agent(&self, id: &AgentId) -> CoreResult<Agent> {
        self.read(|db| db.get_agent(id))?
            .ok_or_else(|| CoreError::not_found("agent", id))
    }

    /// Resolve an agent by id or (case-insensitive) name.
    pub fn resolve_agent(&self, id_or_name: &str) -> CoreResult<Agent> {
        let agents = self.read(|db| db.list_agents(&self.project.id))?;
        agents
            .into_iter()
            .find(|a| a.id.0 == id_or_name || a.name.eq_ignore_ascii_case(id_or_name))
            .ok_or_else(|| CoreError::not_found("agent", id_or_name))
    }

    pub fn list_agents(&self) -> CoreResult<Vec<Agent>> {
        self.read(|db| db.list_agents(&self.project.id))
    }

    pub fn leader(&self) -> CoreResult<Option<Agent>> {
        Ok(self
            .list_agents()?
            .into_iter()
            .find(|a| a.role == Role::Leader))
    }

    pub fn set_agent_role(&self, id: &AgentId, role: Role) -> CoreResult<Agent> {
        self.tx(|db, emit| {
            let mut a = db
                .get_agent(id)?
                .ok_or_else(|| CoreError::not_found("agent", id))?;
            if role == Role::Leader {
                if let Some(other) = db
                    .list_agents(&self.project.id)?
                    .into_iter()
                    .find(|x| x.role == Role::Leader && x.id != a.id)
                {
                    return Err(CoreError::Conflict(format!(
                        "{} is already the Leader",
                        other.name
                    )));
                }
            }
            a.role = role;
            db.update_agent(&a)?;
            emit.push(
                EventKind::AgentStateChanged,
                Some(&a.id),
                None,
                json!({ "name": a.name, "role": role.as_str(), "state": a.state.as_str() }),
            );
            Ok(a)
        })
    }

    /// Why removing this agent would lose unfinished work (empty = safe to remove).
    pub fn removal_blockers(&self, id: &AgentId) -> CoreResult<Vec<String>> {
        self.read(|db| removal_blockers_in(db, &self.project.id, id))
    }

    /// Remove an agent from the project. Unfinished work blocks it unless `force`; with `force`
    /// the agent's tasks are released back to the pool. Finished tasks keep their history, its git
    /// worktree and branch are left on disk (never deleted), and undelivered mail is closed.
    pub fn remove_agent(&self, id: &AgentId, force: bool) -> CoreResult<RemovedAgent> {
        self.tx(|db, emit| {
            let a = db.get_agent(id)?.ok_or_else(|| CoreError::not_found("agent", id))?;
            let blockers = removal_blockers_in(db, &self.project.id, id)?;
            if !force && !blockers.is_empty() {
                return Err(CoreError::Conflict(format!(
                    "{} has unfinished work: {}. Remove it with force to release its tasks",
                    a.name,
                    blockers.join("; ")
                )));
            }
            let mut released = vec![];
            for mut t in db.list_tasks(&self.project.id)? {
                let mut dirty = false;
                if t.reviewer_agent_id.as_ref() == Some(id) {
                    t.reviewer_agent_id = None;
                    dirty = true;
                }
                if t.owner_agent_id.as_ref() == Some(id) {
                    match t.status {
                        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => {}
                        TaskStatus::Running | TaskStatus::Assigned | TaskStatus::Blocked | TaskStatus::Ready => {
                            let was = t.status;
                            t.owner_agent_id = None;
                            t.blocked_reason = None;
                            t.progress = 0;
                            db.update_task(&t)?;
                            if was != TaskStatus::Ready {
                                self.transition(emit, db, &mut t, TaskStatus::Ready, EventKind::TaskReleased, json!({ "reason": "agent removed" }))?;
                            }
                            db.end_attempts(&t.id, "agent_removed", None)?;
                            released.push(t.key.clone());
                            dirty = false;
                        }
                        // finished work awaiting review/verification stays; only the owner link goes
                        TaskStatus::InReview | TaskStatus::Verifying | TaskStatus::Backlog => {
                            t.owner_agent_id = None;
                            dirty = true;
                        }
                    }
                }
                if dirty {
                    db.update_task(&t)?;
                }
            }
            if let Some(mut g) = db.current_goal(&self.project.id)? {
                if g.leader_agent_id.as_ref() == Some(id) {
                    g.leader_agent_id = None;
                    db.update_goal(&g)?;
                }
            }
            db.close_undelivered_for(id)?;
            db.expire_agent_approvals(id)?;
            let worktree = a.workspace.clone().filter(|w| w.contains("/worktrees/"));
            emit.push(
                EventKind::AgentRemoved,
                Some(id),
                None,
                json!({ "name": a.name, "role": a.role.as_str(), "provider": a.provider.as_str(), "released_tasks": released, "forced": force, "worktree": worktree }),
            );
            db.delete_agent(id)?;
            self.revoke_tokens(id);
            Ok(RemovedAgent { name: a.name, released_tasks: released, worktree })
        })
    }

    /// Change the model an agent's NEXT session starts with (`None` = the provider's default).
    /// A live session keeps its current model until it reconnects; see `Supervisor::reconfigure_agent`.
    pub fn set_agent_model(&self, id: &AgentId, model: Option<String>) -> CoreResult<Agent> {
        let model = model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("default"));
        if let Some(m) = &model {
            let ok = m.len() <= 100
                && m.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '.' | '-' | '_' | ':' | '/' | '[' | ']')
                });
            if !ok {
                return Err(CoreError::InvalidInput(format!("'{m}' is not a valid model id (letters, digits and . - _ : / [ ] only, up to 100 characters)")));
            }
        }
        self.tx(|db, emit| {
            let mut a = db.get_agent(id)?.ok_or_else(|| CoreError::not_found("agent", id))?;
            a.model = model.clone();
            db.update_agent(&a)?;
            emit.push(
                EventKind::AgentStateChanged,
                Some(&a.id),
                None,
                json!({ "name": a.name, "state": a.state.as_str(), "change": "model", "model": a.model }),
            );
            Ok(a)
        })
    }

    /// Change the reasoning effort for the agent's NEXT session (`default` / empty = provider default).
    /// Valid levels depend on the provider and model (`maw models`); an unsupported level is
    /// reported by the provider when the session starts.
    pub fn set_agent_effort(&self, id: &AgentId, effort: &str) -> CoreResult<Agent> {
        let effort = normalize_effort(effort).map_err(CoreError::InvalidInput)?;
        self.tx(|db, emit| {
            let mut a = db.get_agent(id)?.ok_or_else(|| CoreError::not_found("agent", id))?;
            a.effort = effort.clone();
            db.update_agent(&a)?;
            emit.push(
                EventKind::AgentStateChanged,
                Some(&a.id),
                None,
                json!({ "name": a.name, "state": a.state.as_str(), "change": "effort", "effort": a.effort }),
            );
            Ok(a)
        })
    }

    /// Change an agent's permission profile for its NEXT session.
    pub fn set_agent_permission(
        &self,
        id: &AgentId,
        permission: PermissionProfile,
    ) -> CoreResult<Agent> {
        self.tx(|db, emit| {
            let mut a = db.get_agent(id)?.ok_or_else(|| CoreError::not_found("agent", id))?;
            a.permission = permission;
            db.update_agent(&a)?;
            emit.push(
                EventKind::AgentStateChanged,
                Some(&a.id),
                None,
                json!({ "name": a.name, "state": a.state.as_str(), "change": "permission", "permission": permission.as_str() }),
            );
            Ok(a)
        })
    }

    /// Validated agent state transition. Invalid transitions are rejected (not silently coerced).
    pub fn set_agent_state(
        &self,
        id: &AgentId,
        to: AgentState,
        error: Option<String>,
    ) -> CoreResult<Agent> {
        self.tx(|db, emit| {
            let mut a = db.get_agent(id)?.ok_or_else(|| CoreError::not_found("agent", id))?;
            if a.state == to && error.is_none() {
                return Ok(a);
            }
            if !a.state.can_transition_to(to) {
                return Err(CoreError::InvalidTransition { what: "agent", from: a.state.to_string(), to: to.to_string() });
            }
            let from = a.state;
            a.state = to;
            if let Some(e) = error {
                a.last_error = Some(maw_core::redact::redact(&e));
            } else if from != to
                && !matches!(
                    to,
                    AgentState::Failed
                        | AgentState::AuthRequired
                        | AgentState::RateLimited
                        | AgentState::Disconnected
                )
            {
                // A new attempt or a healthy state: the previous failure is history (the event
                // log and the transcript keep it). Otherwise a working agent keeps showing the
                // error it had before it was started.
                a.last_error = None;
            }
            a.last_heartbeat = Some(Utc::now());
            db.update_agent(&a)?;
            if from != to {
                emit.push(
                    EventKind::AgentStateChanged,
                    Some(&a.id),
                    a.current_task_id.as_ref(),
                    json!({ "name": a.name, "from": from.as_str(), "state": to.as_str(), "error": a.last_error }),
                );
            }
            Ok(a)
        })
    }

    /// Record session facts after (re)connecting.
    pub fn update_agent_session(
        &self,
        id: &AgentId,
        native: Option<String>,
        caps: Capabilities,
        workspace: Option<String>,
    ) -> CoreResult<()> {
        self.tx(|db, _| {
            let mut a = db
                .get_agent(id)?
                .ok_or_else(|| CoreError::not_found("agent", id))?;
            if native.is_some() {
                a.native_session_id = native;
            }
            a.capabilities = caps;
            if workspace.is_some() {
                a.workspace = workspace;
            }
            a.session_id = Some(self.run_id.0.clone());
            a.last_heartbeat = Some(Utc::now());
            db.update_agent(&a)
        })
    }

    /// Record provider-reported usage. Providers report cumulative numbers per session/thread, so
    /// the increment since the previous report is computed here and attached to the event together
    /// with the task the agent was working on, which lets views attribute tokens to tasks.
    /// A report lower than the previous one means the provider session restarted: it then counts
    /// in full.
    pub fn record_usage(&self, id: &AgentId, usage: UsageMetrics) -> CoreResult<()> {
        self.tx(|db, emit| {
            let mut a = db
                .get_agent(id)?
                .ok_or_else(|| CoreError::not_found("agent", id))?;
            let inc = |new: Option<u64>, old: Option<u64>| match (new, old) {
                (Some(n), Some(o)) if n >= o => n - o,
                (Some(n), _) => n,
                _ => 0,
            };
            let d_in = inc(usage.input_tokens, a.usage.input_tokens);
            let d_out = inc(usage.output_tokens, a.usage.output_tokens);
            let d_total = inc(usage.total_tokens, a.usage.total_tokens).max(d_in + d_out);
            let d_cost = match (usage.cost_usd, a.usage.cost_usd) {
                (Some(n), Some(o)) if n >= o => n - o,
                (Some(n), _) => n,
                _ => 0.0,
            };
            a.usage = usage.clone();
            db.update_agent(&a)?;
            emit.push(
                EventKind::AgentUsage,
                Some(id),
                a.current_task_id.as_ref(),
                json!({
                    "agent": a.name, "model": a.model, "usage": usage,
                    "delta": { "input": d_in, "output": d_out, "total": d_total, "cost_usd": d_cost }
                }),
            );
            Ok(())
        })
    }

    pub fn heartbeat(&self, id: &AgentId) {
        let _ = self.store.write(|db| {
            if let Some(mut a) = db.get_agent(id)? {
                a.last_heartbeat = Some(Utc::now());
                db.update_agent(&a)?;
            }
            Ok(())
        });
    }

    // ------------------------------------------------------------------ goals & plans

    /// Create the project's goal (user objective). Fails if an active goal exists.
    pub fn create_goal(&self, text: &str, criteria: Vec<String>) -> CoreResult<Goal> {
        self.create_goal_as(&Actor::User, text, criteria)
    }

    /// Create the goal as the user or as the Leader. Either way it fails while another goal is
    /// active, so a Leader can never replace an objective the user set; only the user may change
    /// or cancel an existing one. The author is recorded in the revision history.
    pub fn create_goal_as(
        &self,
        actor: &Actor,
        text: &str,
        criteria: Vec<String>,
    ) -> CoreResult<Goal> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can create the goal".into(),
            ));
        }
        if text.trim().is_empty() {
            return Err(CoreError::InvalidInput("goal text is empty".into()));
        }
        self.tx(|db, emit| {
            if let Some(g) = db.current_goal(&self.project.id)? {
                if matches!(g.status, GoalStatus::Active | GoalStatus::Verifying) {
                    return Err(CoreError::Conflict(format!(
                        "goal {} is still {}; update or cancel it first",
                        g.id,
                        g.status.as_str()
                    )));
                }
            }
            let leader = db
                .list_agents(&self.project.id)?
                .into_iter()
                .find(|a| a.role == Role::Leader);
            let goal = Goal {
                id: GoalId::new(),
                project_id: self.project.id.clone(),
                text: text.trim().to_string(),
                success_criteria: criteria.clone(),
                status: GoalStatus::Active,
                revision: 1,
                leader_agent_id: leader.map(|l| l.id),
                current_milestone: None,
                current_focus: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            db.insert_goal(&goal)?;
            db.insert_goal_revision(&GoalRevision {
                goal_id: goal.id.clone(),
                revision: 1,
                text: goal.text.clone(),
                success_criteria: criteria,
                reason: if actor.is_privileged() {
                    "initial objective".into()
                } else {
                    "objective created by the Leader".into()
                },
                author: actor.id(),
                created_at: Utc::now(),
            })?;
            emit.push(
                EventKind::GoalCreated,
                actor.agent_id().as_ref(),
                None,
                json!({ "goal_id": goal.id, "text": goal.text, "by": actor.id() }),
            );
            Ok(goal)
        })
    }

    pub fn current_goal(&self) -> CoreResult<Option<Goal>> {
        self.read(|db| db.current_goal(&self.project.id))
    }

    /// Update goal text/criteria (new revision) and/or focus/milestone/status.
    pub fn update_goal(&self, actor: &Actor, patch: GoalPatch) -> CoreResult<Goal> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can update the goal".into(),
            ));
        }
        self.tx(|db, emit| {
            let mut g = db.current_goal(&self.project.id)?.ok_or_else(|| CoreError::not_found("goal", "current"))?;
            let mut revised = false;
            if let Some(t) = patch.text.as_ref().filter(|t| !t.trim().is_empty() && **t != g.text) {
                // Only the user may rewrite the objective itself; the Leader can refine criteria/focus.
                if !actor.is_privileged() {
                    return Err(CoreError::Forbidden("only the user can change the objective text; propose a plan revision instead".into()));
                }
                g.text = t.trim().to_string();
                revised = true;
            }
            if let Some(c) = patch.success_criteria.clone() {
                if c != g.success_criteria {
                    g.success_criteria = c;
                    revised = true;
                }
            }
            if revised {
                g.revision += 1;
                db.insert_goal_revision(&GoalRevision {
                    goal_id: g.id.clone(),
                    revision: g.revision,
                    text: g.text.clone(),
                    success_criteria: g.success_criteria.clone(),
                    reason: patch.reason.clone().unwrap_or_else(|| "goal updated".into()),
                    author: actor.id(),
                    created_at: Utc::now(),
                })?;
            }
            if let Some(m) = patch.current_milestone {
                g.current_milestone = Some(m);
            }
            if let Some(f) = patch.current_focus {
                g.current_focus = Some(f);
            }
            if let Some(status) = patch.status {
                if status != g.status {
                    match status {
                        GoalStatus::Completed => {
                            let tasks = db.list_tasks(&self.project.id)?;
                            let open: Vec<String> = tasks
                                .iter()
                                .filter(|t| t.goal_id == g.id && !matches!(t.status, TaskStatus::Done | TaskStatus::Canceled))
                                .map(|t| format!("{} ({})", t.key, t.status))
                                .collect();
                            if !open.is_empty() {
                                return Err(CoreError::Precondition(format!(
                                    "goal cannot complete: unfinished tasks: {}",
                                    open.join(", ")
                                )));
                            }
                            if !tasks.iter().any(|t| t.goal_id == g.id && t.status == TaskStatus::Done) {
                                return Err(CoreError::Precondition("goal cannot complete: no verified task exists".into()));
                            }
                        }
                        GoalStatus::Canceled if !actor.is_privileged() => {
                            return Err(CoreError::Forbidden("only the user can cancel the goal".into()));
                        }
                        _ => {}
                    }
                    g.status = status;
                }
            }
            g.updated_at = Utc::now();
            db.update_goal(&g)?;
            let kind = if g.status == GoalStatus::Completed { EventKind::GoalCompleted } else { EventKind::GoalUpdated };
            emit.push(
                kind,
                actor.agent_id().as_ref(),
                None,
                json!({ "goal_id": g.id, "revision": g.revision, "status": g.status.as_str(), "focus": g.current_focus, "milestone": g.current_milestone, "revised": revised, "by": actor.id() }),
            );
            Ok(g)
        })
    }

    pub fn goal_history(&self) -> CoreResult<Vec<GoalRevision>> {
        self.read(|db| match db.current_goal(&self.project.id)? {
            Some(g) => db.list_goal_revisions(&g.id),
            None => Ok(vec![]),
        })
    }

    pub fn plan_history(&self) -> CoreResult<Vec<PlanRevision>> {
        self.read(|db| match db.current_goal(&self.project.id)? {
            Some(g) => db.list_plan_revisions(&g.id),
            None => Ok(vec![]),
        })
    }

    /// Record a new plan revision (Leader). Task progress is recomputed from the live task set,
    /// so scope expansion shows up as a lower percentage tied to this revision.
    pub fn propose_plan(
        &self,
        actor: &Actor,
        summary: &str,
        milestones: Vec<Milestone>,
        next_actions: Vec<String>,
        reason: &str,
    ) -> CoreResult<PlanRevision> {
        if !actor.is_leader() {
            return Err(CoreError::Forbidden(
                "only the Leader (or the user) can revise the plan".into(),
            ));
        }
        if summary.trim().is_empty() {
            return Err(CoreError::InvalidInput("plan summary is empty".into()));
        }
        self.tx(|db, emit| {
            let mut g = db.current_goal(&self.project.id)?.ok_or_else(|| CoreError::not_found("goal", "current"))?;
            let rev = db.latest_plan(&g.id)?.map(|p| p.revision + 1).unwrap_or(1);
            let plan = PlanRevision {
                goal_id: g.id.clone(),
                revision: rev,
                summary: summary.to_string(),
                milestones,
                next_actions,
                reason: reason.to_string(),
                author: actor.id(),
                created_at: Utc::now(),
            };
            db.insert_plan_revision(&plan)?;
            if let Some(m) = plan.milestones.iter().find(|m| !m.done) {
                g.current_milestone = Some(m.title.clone());
            }
            g.updated_at = Utc::now();
            db.update_goal(&g)?;
            let progress = Progress::compute(db.list_tasks(&self.project.id)?.iter());
            emit.push(
                EventKind::PlanRevised,
                actor.agent_id().as_ref(),
                None,
                json!({ "revision": rev, "summary": plan.summary, "reason": plan.reason, "verified_pct": progress.verified_pct, "total_weight": progress.total_weight }),
            );
            Ok(plan)
        })
    }

    // ------------------------------------------------------------------ scheduling flags

    pub fn pause(&self) -> CoreResult<()> {
        self.paused.store(true, Ordering::SeqCst);
        self.tx(|db, emit| {
            db.kv_set(&self.project.id, "scheduler_paused", "1")?;
            emit.push(EventKind::SchedulerPaused, None, None, json!({}));
            Ok(())
        })
    }
    pub fn resume(&self) -> CoreResult<()> {
        self.paused.store(false, Ordering::SeqCst);
        self.tx(|db, emit| {
            db.kv_set(&self.project.id, "scheduler_paused", "0")?;
            emit.push(EventKind::SchedulerResumed, None, None, json!({}));
            Ok(())
        })
    }

    // ------------------------------------------------------------------ snapshot & shutdown

    pub fn snapshot(&self) -> CoreResult<Snapshot> {
        let paused = self.is_paused();
        self.read(|db| {
            let goal = db.current_goal(&self.project.id)?;
            let plan = match &goal {
                Some(g) => db.latest_plan(&g.id)?,
                None => None,
            };
            let tasks = db.list_tasks(&self.project.id)?;
            let goal_id = goal.as_ref().map(|g| g.id.clone());
            let progress = Progress::compute(
                tasks
                    .iter()
                    .filter(|t| Some(&t.goal_id) == goal_id.as_ref()),
            );
            Ok(Snapshot {
                project: self.project.clone(),
                goal,
                plan,
                agents: db.list_agents(&self.project.id)?,
                tasks,
                progress,
                approvals: db
                    .list_approvals(&self.project.id, false)?
                    .into_iter()
                    .take(50)
                    .collect(),
                recent_events: db.recent_events(&self.project.id, 300)?,
                recent_messages: db.recent_messages(&self.project.id, 100)?,
                workspaces: db.list_workspaces(&self.project.id)?,
                paused,
                run_id: self.run_id.clone(),
            })
        })
    }

    /// Record a clean shutdown (used by recovery to distinguish crashes).
    pub fn shutdown(&self) -> CoreResult<()> {
        if !self.track_run {
            return Ok(());
        }
        self.tx(|db, emit| {
            db.end_run(&self.run_id)?;
            emit.push(EventKind::RunStopped, None, None, json!({}));
            Ok(())
        })
    }

    pub fn emit_custom(
        &self,
        kind: EventKind,
        agent: Option<&AgentId>,
        task: Option<&TaskId>,
        payload: Value,
    ) {
        let _ = self.tx(|_, emit| {
            emit.push(kind, agent, task, payload);
            Ok(())
        });
    }

    pub fn now() -> DateTime<Utc> {
        Utc::now()
    }
}

/// What `Engine::remove_agent` did.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemovedAgent {
    pub name: String,
    pub released_tasks: Vec<String>,
    /// The agent's git worktree, left on disk for the user to inspect or delete.
    pub worktree: Option<String>,
}

fn removal_blockers_in(db: &Db, project: &ProjectId, id: &AgentId) -> CoreResult<Vec<String>> {
    let a = db
        .get_agent(id)?
        .ok_or_else(|| CoreError::not_found("agent", id))?;
    let mut v = vec![];
    for t in db.list_tasks(project)? {
        if t.owner_agent_id.as_ref() == Some(id)
            && matches!(
                t.status,
                TaskStatus::Running | TaskStatus::InReview | TaskStatus::Verifying
            )
        {
            v.push(format!("{} \"{}\" is {}", t.key, t.title, t.status));
        }
    }
    if a.role == Role::Leader {
        if let Some(g) = db.current_goal(project)? {
            if matches!(g.status, GoalStatus::Active | GoalStatus::Verifying) {
                v.push("it is the Leader of the active goal".into());
            }
        }
    }
    Ok(v)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GoalPatch {
    pub text: Option<String>,
    pub success_criteria: Option<Vec<String>>,
    pub current_milestone: Option<String>,
    pub current_focus: Option<String>,
    pub status: Option<GoalStatus>,
    pub reason: Option<String>,
}
