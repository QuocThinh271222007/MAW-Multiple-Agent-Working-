//! The `maw-coordination` tool surface. Transport-independent: the MCP server, the mock
//! provider and tests all call [`Engine::call_tool`]. Each tool declares which roles may call it;
//! the engine operations enforce authorization again (defense in depth).

use crate::engine::{Actor, Engine, GoalPatch};
use crate::tasks::{EvidenceInput, NewTask, TaskPatch};
use maw_core::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// Empty = every role.
    pub roles: &'static [Role],
    pub read_only: bool,
    pub schema: fn() -> Value,
}

const L: &[Role] = &[Role::Leader];
const EXEC: &[Role] = &[Role::Worker, Role::Specialist];
const REVIEW: &[Role] = &[Role::Reviewer, Role::Leader];
const ANY: &[Role] = &[];

fn schema_of<T: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|_| json!({ "type": "object" }));
    if let Value::Object(m) = &mut v {
        m.remove("$schema");
        m.remove("title");
        m.entry("type").or_insert(json!("object"));
    }
    v
}

// ---------------------------------------------------------------- argument types

#[derive(Deserialize, JsonSchema, Default)]
struct Empty {}

#[derive(Deserialize, JsonSchema)]
struct GoalCreateArgs {
    /// What the team should achieve.
    objective: String,
    /// How to tell it is done: concrete, checkable statements.
    #[serde(default)]
    success_criteria: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct GoalUpdateArgs {
    /// Replace the objective text (user only).
    objective: Option<String>,
    /// Replace the success criteria list.
    success_criteria: Option<Vec<String>>,
    /// Name of the milestone currently being worked on.
    current_milestone: Option<String>,
    /// One-line statement of what the team is focusing on right now.
    current_focus: Option<String>,
    /// `verifying` when every task is done and final verification starts; `completed` once verified.
    status: Option<String>,
    /// Why the goal changed (recorded in the revision history).
    reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct MilestoneArg {
    title: String,
    #[serde(default)]
    done: bool,
    /// Task keys (e.g. `T-3`) that belong to this milestone.
    #[serde(default)]
    task_keys: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct PlanArgs {
    /// Short summary of the plan.
    summary: String,
    milestones: Vec<MilestoneArg>,
    #[serde(default)]
    next_actions: Vec<String>,
    /// What changed relative to the previous revision.
    reason: String,
}

#[derive(Deserialize, JsonSchema)]
struct TaskCreateArgs {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    /// low | normal | high | critical
    priority: Option<String>,
    /// Parent task key/id for subtasks.
    parent: Option<String>,
    /// Task keys/ids that must be DONE before this task can start.
    #[serde(default)]
    dependencies: Vec<String>,
    /// Relative effort used to compute verified progress (default 1).
    weight: Option<f64>,
    /// Agent name/id to pre-assign (Worker/Specialist).
    assignee: Option<String>,
    /// Require a Reviewer's approval before the task can complete.
    #[serde(default)]
    requires_review: bool,
    reviewer: Option<String>,
    /// The task legitimately changes no files (analysis, documentation review...).
    #[serde(default)]
    no_file_changes_expected: bool,
    /// Retried calls with the same key return the original task instead of creating a duplicate.
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkspaceProposeArgs {
    /// The folder the project should live in: a WSL path (`/home/me/projects/app`, `~/projects/app`)
    /// or a Windows path (`C:\Users\me\projects\app`). Created if it does not exist.
    path: String,
    /// Why this folder fits the project (shown to the user).
    reason: String,
}

#[derive(Deserialize, JsonSchema, Default)]
struct TaskListArgs {
    /// Filter by status (e.g. READY, RUNNING, DONE).
    status: Option<String>,
    /// Filter by owner agent name/id.
    owner: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TaskRefArgs {
    /// Task key (`T-3`) or id.
    task: String,
}

#[derive(Deserialize, JsonSchema)]
struct TaskUpdateArgs {
    task: String,
    title: Option<String>,
    description: Option<String>,
    acceptance_criteria: Option<Vec<String>>,
    priority: Option<String>,
    weight: Option<f64>,
    #[serde(default)]
    add_dependencies: Vec<String>,
    #[serde(default)]
    remove_dependencies: Vec<String>,
    /// Cancel the task.
    cancel: Option<bool>,
    no_file_changes_expected: Option<bool>,
    reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TaskAssignArgs {
    task: String,
    /// Agent name or id.
    agent: String,
}

#[derive(Deserialize, JsonSchema)]
struct TaskClaimArgs {
    /// Specific READY task to claim; omit to claim the highest priority READY task.
    task: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TaskReleaseArgs {
    task: String,
    reason: String,
}

#[derive(Deserialize, JsonSchema)]
struct TaskBlockArgs {
    task: String,
    reason: String,
    /// What would unblock the task.
    needs: Option<String>,
}

#[derive(Deserialize, JsonSchema, Clone)]
struct EvidenceArg {
    /// file_changes | test_result | build_result | verification | note
    kind: String,
    summary: String,
    #[serde(default = "yes")]
    passed: bool,
    #[serde(default)]
    detail: Value,
}
fn yes() -> bool {
    true
}

#[derive(Deserialize, JsonSchema)]
struct SubmitArgs {
    task: String,
    /// What was done and how it satisfies the acceptance criteria.
    summary: String,
    /// Paths changed (relative to the workspace).
    #[serde(default)]
    changed_files: Vec<String>,
    /// Supporting evidence: tests run, builds, verification notes.
    #[serde(default)]
    evidence: Vec<EvidenceArg>,
}

#[derive(Deserialize, JsonSchema)]
struct RequestReviewArgs {
    task: String,
    /// Reviewer agent name/id (optional).
    reviewer: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ReviewArgs {
    task: String,
    /// `approve` or `reject`.
    verdict: String,
    /// Required for rejections: concrete, actionable feedback.
    feedback: String,
}

#[derive(Deserialize, JsonSchema)]
struct VerifyArgs {
    task: String,
    /// true: acceptance criteria are met and completion is accepted. false: send back for correction.
    pass: bool,
    /// What was checked (commands run, files inspected, criteria confirmed).
    notes: String,
}

#[derive(Deserialize, JsonSchema)]
struct ProgressArgs {
    task: String,
    /// Advisory 0-100. Never counts as verified completion.
    percent: Option<u8>,
    note: String,
}

#[derive(Deserialize, JsonSchema)]
struct MilestoneReportArgs {
    task: Option<String>,
    milestone: String,
    summary: String,
}

#[derive(Deserialize, JsonSchema)]
struct ArtifactArgs {
    task: Option<String>,
    /// e.g. file, diff, report, log
    kind: String,
    /// Path relative to the project.
    path: Option<String>,
    description: String,
}

#[derive(Deserialize, JsonSchema)]
struct TestReportArgs {
    task: String,
    /// Command that was run.
    command: String,
    passed: bool,
    #[serde(default)]
    total: Option<u32>,
    #[serde(default)]
    failed: Option<u32>,
    summary: String,
}

#[derive(Deserialize, JsonSchema)]
struct BlockerArgs {
    task: String,
    reason: String,
    needs: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct MessageSendArgs {
    /// Recipient agent name/id, or `leader`.
    to: String,
    /// QUESTION | ANSWER | STATUS | RESULT | BLOCKER | REVIEW_REQUEST | REVIEW_FEEDBACK | INSTRUCTION (Leader only)
    message_type: String,
    subject: String,
    content: String,
    task: Option<String>,
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema, Default)]
struct InboxArgs {
    /// Include already acknowledged messages.
    #[serde(default)]
    include_acknowledged: bool,
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct AckArgs {
    message_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct HandoffArgs {
    task: String,
    to: String,
    summary: String,
    #[serde(default)]
    context: String,
}

#[derive(Deserialize, JsonSchema)]
struct AgentRefArgs {
    /// Agent name or id; omit for yourself.
    agent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct HelpArgs {
    task: Option<String>,
    topic: String,
    details: String,
}

#[derive(Deserialize, JsonSchema)]
struct ContextPublishArgs {
    key: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    task: Option<String>,
}

#[derive(Deserialize, JsonSchema, Default)]
struct ContextReadArgs {
    kind: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct ContextSearchArgs {
    query: String,
    kind: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct DecisionArgs {
    /// Short title of the decision.
    title: String,
    /// The decision and its rationale.
    decision: String,
    #[serde(default)]
    tags: Vec<String>,
    task: Option<String>,
}

macro_rules! spec {
    ($name:expr, $desc:expr, $roles:expr, $ro:expr, $ty:ty) => {
        ToolSpec {
            name: $name,
            description: $desc,
            roles: $roles,
            read_only: $ro,
            schema: schema_of::<$ty>,
        }
    };
}

pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        spec!("maw_goal_get", "Get the project's current goal, status, milestone and focus.", ANY, true, Empty),
        spec!("maw_goal_create", "Leader: create the project's goal when none is active (fails while one is). Only the user can change or cancel an existing goal.", L, false, GoalCreateArgs),
        spec!("maw_goal_update","Leader: update goal criteria/focus/milestone/status. Completion requires every task verified. Revisions are recorded.", L, false, GoalUpdateArgs),
        spec!("maw_goal_get_history", "List all goal revisions with reasons and authors.", ANY, true, Empty),
        spec!("maw_workspace_get", "Show the folder the agents work in, the places you may propose, whether a change is possible now and any proposal waiting for the user.", ANY, true, Empty),
        spec!("maw_workspace_propose", "Leader: propose a different working folder for the project (WSL or Windows path), for instance a dedicated folder instead of a home folder. Nothing changes until the user approves it; every agent is then restarted in the new folder. Not possible while tasks are assigned, running, blocked or waiting for review.", L, false, WorkspaceProposeArgs),
        spec!("maw_plan_get", "Get the latest plan revision and its history.", ANY, true, Empty),
        spec!("maw_plan_propose_revision", "Leader: record a new plan revision (milestones, next actions, reason).", L, false, PlanArgs),
        spec!("maw_task_create", "Leader: create a task. Dependencies are validated (no cycles); retries with the same idempotency_key return the original task.", L, false, TaskCreateArgs),
        spec!("maw_task_list", "List tasks, optionally filtered by status or owner.", ANY, true, TaskListArgs),
        spec!("maw_task_get", "Get one task with dependencies, evidence, artifacts and review state.", ANY, true, TaskRefArgs),
        spec!("maw_task_update", "Leader: edit a task, change dependencies/weight, or cancel it.", L, false, TaskUpdateArgs),
        spec!("maw_task_assign", "Leader: atomically assign a task to a Worker/Specialist.", L, false, TaskAssignArgs),
        spec!("maw_task_claim", "Worker/Specialist: atomically claim a READY task (or the best one if none is named).", EXEC, false, TaskClaimArgs),
        spec!("maw_task_release", "Owner or Leader: give a task back to the pool.", ANY, false, TaskReleaseArgs),
        spec!("maw_task_block", "Owner or Leader: mark a task BLOCKED with the reason and what is needed. Notifies the Leader.", ANY, false, TaskBlockArgs),
        spec!("maw_task_submit_result", "Owner: submit finished work with a summary, changed files and evidence. The task then goes to review/verification; it is NOT done until verified.", EXEC, false, SubmitArgs),
        spec!("maw_task_request_review", "Owner or Leader: request a review of a task.", ANY, false, RequestReviewArgs),
        spec!("maw_review_submit", "Reviewer/Leader: approve or reject a task awaiting review (rejections need actionable feedback). Owners cannot review their own work.", REVIEW, false, ReviewArgs),
        spec!("maw_task_verify", "Leader: final acceptance. pass=true marks the task DONE only if passing evidence exists (and any required review is approved); pass=false sends it back.", L, false, VerifyArgs),
        spec!("maw_task_get_dependencies", "Show a task's dependencies, dependents and unmet prerequisites.", ANY, true, TaskRefArgs),
        spec!("maw_progress_report", "Owner: report advisory progress on a RUNNING task (also renews the task lease).", ANY, false, ProgressArgs),
        spec!("maw_milestone_report", "Report that a milestone was reached.", ANY, false, MilestoneReportArgs),
        spec!("maw_artifact_register", "Register an artifact (file, report, diff) for a task.", ANY, false, ArtifactArgs),
        spec!("maw_test_report", "Owner: attach a test/build result as evidence to a task.", ANY, false, TestReportArgs),
        spec!("maw_blocker_report", "Owner: report a blocker (same as maw_task_block) and notify the Leader.", ANY, false, BlockerArgs),
        spec!("maw_message_send", "Send a message to another agent. Delivery is performed by the orchestrator; check maw_message_inbox for replies.", ANY, false, MessageSendArgs),
        spec!("maw_message_inbox", "Read your inbox (unacknowledged messages by default).", ANY, true, InboxArgs),
        spec!("maw_message_ack", "Acknowledge that you received and will act on a message.", ANY, false, AckArgs),
        spec!("maw_handoff_create", "Owner or Leader: hand a task to another agent with context. Ownership changes when they accept.", ANY, false, HandoffArgs),
        spec!("maw_handoff_accept", "Accept a handoff addressed to you (pass the handoff message id).", ANY, false, AckArgs),
        spec!("maw_agent_list", "List agents with role, provider and state.", ANY, true, Empty),
        spec!("maw_agent_status", "Get one agent's state, current task and last error.", ANY, true, AgentRefArgs),
        spec!("maw_agent_capabilities", "Get what an agent's provider session supports.", ANY, true, AgentRefArgs),
        spec!("maw_agent_request_help", "Escalate to the Leader with a structured help request.", ANY, false, HelpArgs),
        spec!("maw_context_publish", "Publish shared context other agents can read (interfaces, findings, conventions).", ANY, false, ContextPublishArgs),
        spec!("maw_context_read", "Read recently published shared context.", ANY, true, ContextReadArgs),
        spec!("maw_context_search", "Search shared context and decisions.", ANY, true, ContextSearchArgs),
        spec!("maw_decision_record", "Record a durable decision with its rationale.", ANY, false, DecisionArgs),
    ]
}

// ---------------------------------------------------------------- views

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> CoreResult<T> {
    serde_json::from_value(if args.is_null() { json!({}) } else { args })
        .map_err(|e| CoreError::InvalidInput(format!("invalid arguments: {e}")))
}

fn agent_name(agents: &[Agent], id: &AgentId) -> String {
    agents
        .iter()
        .find(|a| &a.id == id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| id.0.clone())
}

pub fn task_view(t: &Task, tasks: &[Task], agents: &[Agent]) -> Value {
    let key_of = |id: &TaskId| {
        tasks
            .iter()
            .find(|x| &x.id == id)
            .map(|x| x.key.clone())
            .unwrap_or_else(|| id.0.clone())
    };
    json!({
        "task": t.key, "id": t.id, "title": t.title, "description": t.description,
        "acceptance_criteria": t.acceptance_criteria, "status": t.status.as_str(), "priority": t.priority.as_str(),
        "owner": t.owner_agent_id.as_ref().map(|o| agent_name(agents, o)),
        "dependencies": t.dependencies.iter().map(key_of).collect::<Vec<_>>(),
        "weight": t.estimated_weight, "reported_progress": t.progress, "attempts": t.attempt_count,
        "review_status": t.review_status.as_str(), "reviewer": t.reviewer_agent_id.as_ref().map(|o| agent_name(agents, o)),
        "blocked_reason": t.blocked_reason,
        "evidence": t.evidence.iter().map(|e| json!({ "kind": e.kind.as_str(), "passed": e.passed, "summary": e.summary })).collect::<Vec<_>>(),
        "artifacts": t.artifacts.iter().map(|a| json!({ "kind": a.kind, "path": a.path, "description": a.description })).collect::<Vec<_>>(),
        "workspace_id": t.workspace_id, "no_file_changes_expected": t.no_file_changes_expected,
        // where the owner's changes live (its isolated git worktree), for reviewers/the Leader to inspect
        "owner_workspace": t.owner_agent_id.as_ref().and_then(|o| agents.iter().find(|a| &a.id == o)).and_then(|a| a.workspace.clone()),
    })
}

fn agent_view(a: &Agent, tasks: &[Task]) -> Value {
    json!({
        "agent": a.name, "id": a.id, "role": a.role.as_str(), "provider": a.provider.as_str(), "model": a.model,
        "state": a.state.as_str(), "current_task": a.current_task_id.as_ref().and_then(|t| tasks.iter().find(|x| &x.id == t).map(|x| x.key.clone())),
        "last_error": a.last_error,
    })
}

fn message_view(m: &Message, agents: &[Agent], tasks: &[Task]) -> Value {
    json!({
        "message_id": m.id, "from": agent_name(agents, &m.sender_agent_id), "type": m.message_type.as_str(),
        "subject": m.subject, "content": m.content, "task": m.task_id.as_ref().and_then(|t| tasks.iter().find(|x| &x.id == t).map(|x| x.key.clone())),
        "created_at": m.created_at, "acknowledged": m.acknowledged_at.is_some(),
    })
}

fn evidence_kind(s: &str) -> CoreResult<EvidenceKind> {
    s.parse::<EvidenceKind>().map_err(CoreError::InvalidInput)
}

impl Engine {
    /// Names of every tool (for docs/tests).
    pub fn tool_names() -> Vec<&'static str> {
        tool_specs().iter().map(|s| s.name).collect()
    }

    /// Execute a coordination tool as `actor`. Role gates are checked first.
    pub fn call_tool(&self, actor: &Actor, name: &str, args: Value) -> CoreResult<Value> {
        let specs = tool_specs();
        let spec = specs
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| CoreError::not_found("tool", name))?;
        if let Actor::Agent { role, .. } = actor {
            if !spec.roles.is_empty() && !spec.roles.contains(role) {
                return Err(CoreError::Forbidden(format!(
                    "tool {name} requires role {}; you are {role}",
                    spec.roles
                        .iter()
                        .map(|r| r.as_str())
                        .collect::<Vec<_>>()
                        .join("|")
                )));
            }
        }
        self.emit_custom(
            EventKind::McpToolCalled,
            actor.agent_id().as_ref(),
            None,
            json!({ "tool": name, "agent": actor.id() }),
        );
        let res = self.dispatch_tool(actor, name, args);
        self.emit_custom(
            EventKind::McpToolCompleted,
            actor.agent_id().as_ref(),
            None,
            json!({ "tool": name, "agent": actor.id(), "ok": res.is_ok(), "error": res.as_ref().err().map(|e| e.code()) }),
        );
        res
    }

    fn dispatch_tool(&self, actor: &Actor, name: &str, args: Value) -> CoreResult<Value> {
        let tasks = || self.list_tasks();
        let agents = || self.list_agents();
        let one_task = |t: &Task| -> CoreResult<Value> { Ok(task_view(t, &tasks()?, &agents()?)) };
        match name {
            "maw_goal_get" => {
                let g = self.current_goal()?;
                let p = self.progress()?;
                Ok(json!({ "goal": g, "progress": p }))
            }
            "maw_goal_create" => {
                let a: GoalCreateArgs = parse(args)?;
                let g = self.create_goal_as(actor, &a.objective, a.success_criteria)?;
                Ok(json!({ "goal": g }))
            }
            "maw_goal_update" => {
                let a: GoalUpdateArgs = parse(args)?;
                let status = match a.status.as_deref() {
                    Some(s) => Some(s.parse::<GoalStatus>().map_err(CoreError::InvalidInput)?),
                    None => None,
                };
                let g = self.update_goal(
                    actor,
                    GoalPatch {
                        text: a.objective,
                        success_criteria: a.success_criteria,
                        current_milestone: a.current_milestone,
                        current_focus: a.current_focus,
                        status,
                        reason: a.reason,
                    },
                )?;
                Ok(json!({ "goal": g }))
            }
            "maw_workspace_get" => self.workspace_info(),
            "maw_workspace_propose" => {
                let a: WorkspaceProposeArgs = parse(args)?;
                let ap = self.propose_workspace(actor, &a.path, &a.reason)?;
                Ok(json!({
                    "status": "waiting_for_the_user",
                    "approval_id": ap.id,
                    "folder": ap.details.get("path"),
                    "message": "The user has been asked. Do not start work that depends on the new folder yet: you will get a message when they answer. Meanwhile you can end your turn."
                }))
            }
            "maw_goal_get_history" => Ok(json!({ "revisions": self.goal_history()? })),
            "maw_plan_get" => {
                let hist = self.plan_history()?;
                Ok(json!({ "current": hist.last(), "history": hist }))
            }
            "maw_plan_propose_revision" => {
                let a: PlanArgs = parse(args)?;
                let ms = a
                    .milestones
                    .into_iter()
                    .map(|m| Milestone {
                        title: m.title,
                        done: m.done,
                        task_keys: m.task_keys,
                    })
                    .collect();
                let p = self.propose_plan(actor, &a.summary, ms, a.next_actions, &a.reason)?;
                Ok(json!({ "plan": p, "progress": self.progress()? }))
            }
            "maw_task_create" => {
                let a: TaskCreateArgs = parse(args)?;
                let nt = NewTask {
                    title: a.title,
                    description: a.description,
                    acceptance_criteria: a.acceptance_criteria,
                    priority: match a.priority.as_deref() {
                        Some(p) => Some(p.parse::<Priority>().map_err(CoreError::InvalidInput)?),
                        None => None,
                    },
                    parent: a.parent,
                    dependencies: a.dependencies,
                    weight: a.weight,
                    assignee: a.assignee,
                    requires_review: a.requires_review,
                    reviewer: a.reviewer,
                    no_file_changes_expected: a.no_file_changes_expected,
                    idempotency_key: a.idempotency_key,
                };
                let (t, existed) = self.create_task_dedup(actor, nt)?;
                let mut v = one_task(&t)?;
                if existed {
                    v["already_existed"] = json!(true);
                    v["note"] = json!(format!(
                        "{} already exists with this title and assignee, so no new task was created. Do not create it again.",
                        t.key
                    ));
                }
                Ok(v)
            }
            "maw_task_list" => {
                let a: TaskListArgs = parse(args)?;
                let all = tasks()?;
                let ag = agents()?;
                let status = match a.status.as_deref() {
                    Some(s) => Some(s.parse::<TaskStatus>().map_err(CoreError::InvalidInput)?),
                    None => None,
                };
                let owner = match a.owner.as_deref() {
                    Some(o) => Some(self.resolve_agent(o)?.id),
                    None => None,
                };
                let list: Vec<Value> = all
                    .iter()
                    .filter(|t| status.map(|s| t.status == s).unwrap_or(true))
                    .filter(|t| {
                        owner
                            .as_ref()
                            .map(|o| t.owner_agent_id.as_ref() == Some(o))
                            .unwrap_or(true)
                    })
                    .map(|t| {
                        let mut v = task_view(t, &all, &ag);
                        if let Value::Object(m) = &mut v {
                            m.remove("description");
                            m.remove("evidence");
                            m.remove("artifacts");
                        }
                        v
                    })
                    .collect();
                Ok(json!({ "tasks": list, "progress": self.progress()? }))
            }
            "maw_task_get" => {
                let a: TaskRefArgs = parse(args)?;
                one_task(&self.get_task(&a.task)?)
            }
            "maw_task_update" => {
                let a: TaskUpdateArgs = parse(args)?;
                let patch = TaskPatch {
                    title: a.title,
                    description: a.description,
                    acceptance_criteria: a.acceptance_criteria,
                    priority: match a.priority.as_deref() {
                        Some(p) => Some(p.parse::<Priority>().map_err(CoreError::InvalidInput)?),
                        None => None,
                    },
                    weight: a.weight,
                    add_dependencies: a.add_dependencies,
                    remove_dependencies: a.remove_dependencies,
                    cancel: a.cancel,
                    reason: a.reason,
                    no_file_changes_expected: a.no_file_changes_expected,
                };
                one_task(&self.update_task(actor, &a.task, patch)?)
            }
            "maw_task_assign" => {
                let a: TaskAssignArgs = parse(args)?;
                one_task(&self.assign_task(actor, &a.task, &a.agent)?)
            }
            "maw_task_claim" => {
                let a: TaskClaimArgs = parse(args)?;
                one_task(&self.claim_task(actor, a.task.as_deref())?)
            }
            "maw_task_release" => {
                let a: TaskReleaseArgs = parse(args)?;
                one_task(&self.release_task(actor, &a.task, &a.reason)?)
            }
            "maw_task_block" | "maw_blocker_report" => {
                let (task, reason, needs) = if name == "maw_task_block" {
                    let a: TaskBlockArgs = parse(args)?;
                    (a.task, a.reason, a.needs)
                } else {
                    let a: BlockerArgs = parse(args)?;
                    (a.task, a.reason, a.needs)
                };
                one_task(&self.block_task(actor, &task, &reason, needs.as_deref())?)
            }
            "maw_task_submit_result" => {
                let a: SubmitArgs = parse(args)?;
                let mut ev = vec![];
                for e in a.evidence {
                    ev.push(EvidenceInput {
                        kind: evidence_kind(&e.kind)?,
                        summary: e.summary,
                        passed: e.passed,
                        detail: e.detail,
                    });
                }
                let t = self.submit_result(actor, &a.task, &a.summary, ev, a.changed_files)?;
                Ok(
                    json!({ "task": task_view(&t, &tasks()?, &agents()?), "next": match t.status { TaskStatus::InReview => "awaiting review", _ => "awaiting Leader verification" } }),
                )
            }
            "maw_task_request_review" => {
                let a: RequestReviewArgs = parse(args)?;
                one_task(&self.request_review(actor, &a.task, a.reviewer.as_deref())?)
            }
            "maw_review_submit" => {
                let a: ReviewArgs = parse(args)?;
                let approve = match a.verdict.to_ascii_lowercase().as_str() {
                    "approve" | "approved" => true,
                    "reject" | "rejected" => false,
                    o => {
                        return Err(CoreError::InvalidInput(format!(
                            "verdict must be approve or reject, got '{o}'"
                        )))
                    }
                };
                one_task(&self.submit_review(actor, &a.task, approve, &a.feedback)?)
            }
            "maw_task_verify" => {
                let a: VerifyArgs = parse(args)?;
                let t = self.verify_task(actor, &a.task, a.pass, &a.notes)?;
                let all = self.list_tasks()?;
                let goal_done = all
                    .iter()
                    .filter(|x| x.goal_id == t.goal_id)
                    .all(|x| matches!(x.status, TaskStatus::Done | TaskStatus::Canceled));
                Ok(
                    json!({ "task": task_view(&t, &all, &agents()?), "all_tasks_done": goal_done, "progress": self.progress()?,
                    "hint": if goal_done { "Every task is verified. Call maw_goal_update with status=completed when the goal's success criteria are met." } else { "" } }),
                )
            }
            "maw_task_get_dependencies" => {
                let a: TaskRefArgs = parse(args)?;
                self.dependencies_of(&a.task)
            }
            "maw_progress_report" => {
                let a: ProgressArgs = parse(args)?;
                one_task(&self.report_progress(actor, &a.task, a.percent, &a.note)?)
            }
            "maw_milestone_report" => {
                let a: MilestoneReportArgs = parse(args)?;
                if let (true, Actor::Agent { .. }) = (actor.is_leader(), actor) {
                    self.update_goal(
                        actor,
                        GoalPatch {
                            current_milestone: Some(a.milestone.clone()),
                            ..Default::default()
                        },
                    )?;
                }
                let task = match &a.task {
                    Some(t) => Some(self.get_task(t)?),
                    None => None,
                };
                self.emit_custom(
                    EventKind::TaskProgress,
                    actor.agent_id().as_ref(),
                    task.as_ref().map(|t| &t.id),
                    json!({ "kind": "milestone", "milestone": a.milestone, "note": a.summary, "task_key": task.as_ref().map(|t| t.key.clone()), "by_id": actor.id() }),
                );
                Ok(json!({ "recorded": true }))
            }
            "maw_artifact_register" => {
                let a: ArtifactArgs = parse(args)?;
                let art = self.register_artifact(
                    actor,
                    a.task.as_deref(),
                    &a.kind,
                    a.path,
                    &a.description,
                )?;
                Ok(json!({ "artifact_id": art.id }))
            }
            "maw_test_report" => {
                let a: TestReportArgs = parse(args)?;
                let t = self.add_evidence(
                    actor,
                    &a.task,
                    EvidenceInput {
                        kind: EvidenceKind::TestResult,
                        summary: format!("`{}`: {}", a.command, a.summary),
                        passed: a.passed,
                        detail: json!({ "command": a.command, "total": a.total, "failed": a.failed }),
                    },
                )?;
                Ok(
                    json!({ "task": t.key, "evidence_items": t.evidence.len(), "latest_passed": a.passed }),
                )
            }
            "maw_message_send" => {
                let a: MessageSendArgs = parse(args)?;
                let mt = a
                    .message_type
                    .parse::<MessageType>()
                    .map_err(CoreError::InvalidInput)?;
                let m = self.send_message(
                    actor,
                    &a.to,
                    mt,
                    &a.subject,
                    &a.content,
                    a.task.as_deref(),
                    a.idempotency_key,
                )?;
                Ok(
                    json!({ "message_id": m.id, "delivered": false, "note": "queued; the orchestrator delivers it to the recipient's session" }),
                )
            }
            "maw_message_inbox" => {
                let a: InboxArgs = parse(args)?;
                let msgs = self.inbox(actor, !a.include_acknowledged, a.limit.unwrap_or(50))?;
                let ag = agents()?;
                let ts = tasks()?;
                Ok(
                    json!({ "messages": msgs.iter().map(|m| message_view(m, &ag, &ts)).collect::<Vec<_>>() }),
                )
            }
            "maw_message_ack" => {
                let a: AckArgs = parse(args)?;
                let m = self.ack_message(actor, &a.message_id)?;
                Ok(json!({ "message_id": m.id, "acknowledged": true }))
            }
            "maw_handoff_create" => {
                let a: HandoffArgs = parse(args)?;
                let m = self.create_handoff(actor, &a.task, &a.to, &a.summary, &a.context)?;
                Ok(
                    json!({ "handoff_message_id": m.id, "note": "ownership transfers when the recipient accepts" }),
                )
            }
            "maw_handoff_accept" => {
                let a: AckArgs = parse(args)?;
                one_task(&self.accept_handoff(actor, &a.message_id)?)
            }
            "maw_agent_list" => {
                let ts = tasks()?;
                Ok(
                    json!({ "agents": agents()?.iter().map(|a| agent_view(a, &ts)).collect::<Vec<_>>() }),
                )
            }
            "maw_agent_status" | "maw_agent_capabilities" => {
                let a: AgentRefArgs = parse(args)?;
                let ag = match (&a.agent, actor.agent_id()) {
                    (Some(n), _) => self.resolve_agent(n)?,
                    (None, Some(me)) => self.get_agent(&me)?,
                    (None, None) => return Err(CoreError::InvalidInput("specify an agent".into())),
                };
                if name == "maw_agent_status" {
                    Ok(agent_view(&ag, &tasks()?))
                } else {
                    Ok(
                        json!({ "agent": ag.name, "provider": ag.provider.as_str(), "capabilities": ag.capabilities }),
                    )
                }
            }
            "maw_agent_request_help" => {
                let a: HelpArgs = parse(args)?;
                let m = self.request_help(actor, a.task.as_deref(), &a.topic, &a.details)?;
                Ok(json!({ "message_id": m.id, "routed_to": "leader" }))
            }
            "maw_context_publish" => {
                let a: ContextPublishArgs = parse(args)?;
                let c = self.publish_context(
                    actor,
                    "context",
                    &a.key,
                    &a.content,
                    a.tags,
                    a.task.as_deref(),
                )?;
                Ok(json!({ "context_id": c.id }))
            }
            "maw_decision_record" => {
                let a: DecisionArgs = parse(args)?;
                let c = self.publish_context(
                    actor,
                    "decision",
                    &a.title,
                    &a.decision,
                    a.tags,
                    a.task.as_deref(),
                )?;
                Ok(json!({ "decision_id": c.id }))
            }
            "maw_context_read" => {
                let a: ContextReadArgs = parse(args)?;
                let items = self.search_context(a.kind.as_deref(), None, a.limit.unwrap_or(20))?;
                Ok(
                    json!({ "entries": items.iter().map(|c| json!({ "id": c.id, "kind": c.kind, "key": c.key, "content": c.content, "tags": c.tags, "by": c.author, "at": c.created_at })).collect::<Vec<_>>() }),
                )
            }
            "maw_context_search" => {
                let a: ContextSearchArgs = parse(args)?;
                let items =
                    self.search_context(a.kind.as_deref(), Some(&a.query), a.limit.unwrap_or(20))?;
                Ok(
                    json!({ "entries": items.iter().map(|c| json!({ "id": c.id, "kind": c.kind, "key": c.key, "content": c.content, "tags": c.tags, "by": c.author, "at": c.created_at })).collect::<Vec<_>>() }),
                )
            }
            other => Err(CoreError::not_found("tool", other)),
        }
    }
}
