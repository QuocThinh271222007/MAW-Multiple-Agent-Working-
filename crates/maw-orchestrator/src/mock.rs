//! Deterministic scripted provider for CI and `maw demo`.
//!
//! The mock behaves like a real agent: it never touches engine internals, only the public
//! coordination tools via [`Engine::call_tool`] with its MAW-assigned identity. This exercises the
//! same authorization and state rules that real Claude/Codex agents hit through MCP.
//! It is NOT evidence that the real provider integrations work.

use crate::engine::{Actor, Engine};
use async_trait::async_trait;
use maw_core::provider::*;
use maw_core::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct MockScript {
    /// The first attempt at a task whose title contains "backend" reports a blocker.
    pub block_first_backend: bool,
    /// The first review of any task is rejected with feedback.
    pub reject_first_review: bool,
    /// The first worker turn asks for an approval before writing files.
    pub approval_demo: bool,
    /// The Leader does not complete the goal on its own; it needs MAW's "all tasks verified" nudge.
    pub leader_forgets_to_complete: bool,
    /// Pause between simulated steps.
    pub step_delay: Duration,
}

impl Default for MockScript {
    fn default() -> Self {
        MockScript {
            block_first_backend: false,
            reject_first_review: false,
            approval_demo: false,
            leader_forgets_to_complete: false,
            step_delay: Duration::from_millis(15),
        }
    }
}

pub struct MockProvider {
    engine: Arc<Engine>,
    script: MockScript,
    /// Shared across sessions so "first review" means first in the whole project.
    rejected_once: Arc<AtomicBool>,
}

impl MockProvider {
    pub fn new(engine: Arc<Engine>, script: MockScript) -> Arc<MockProvider> {
        Arc::new(MockProvider {
            engine,
            script,
            rejected_once: Arc::new(AtomicBool::new(false)),
        })
    }
}

#[async_trait]
impl AgentProvider for MockProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Mock
    }
    async fn detect(&self) -> ProviderResult<ProviderInfo> {
        Ok(ProviderInfo {
            kind: ProviderKind::Mock,
            installed: true,
            executable: None,
            version: Some("mock".into()),
            execution_modes: vec!["in-process".into()],
            notes: vec!["deterministic test provider".into()],
        })
    }
    async fn auth_status(&self) -> ProviderResult<AuthStatus> {
        Ok(AuthStatus::Authenticated {
            method: "mock".into(),
            account: None,
            plan: None,
        })
    }
    async fn start_session(
        &self,
        cfg: SessionConfig,
    ) -> ProviderResult<(SessionHandle, EventReceiver)> {
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let inner = Arc::new(Inner {
            engine: self.engine.clone(),
            script: self.script.clone(),
            agent_id: cfg.agent_id.clone(),
            cwd: cfg.cwd.clone(),
            tx,
            busy: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            turns: AtomicU32::new(0),
            worker_turns: AtomicU32::new(0),
            approvals: Mutex::new(HashMap::new()),
            native: format!("mock-{}", cfg.agent_id),
            rejected_once: self.rejected_once.clone(),
        });
        let _ = inner
            .tx
            .send(ProviderEvent::Connected {
                native_session_id: Some(inner.native.clone()),
            })
            .await;
        Ok((Arc::new(MockSession { inner }), rx))
    }
}

struct Inner {
    engine: Arc<Engine>,
    script: MockScript,
    agent_id: AgentId,
    cwd: PathBuf,
    tx: mpsc::Sender<ProviderEvent>,
    busy: AtomicBool,
    interrupted: AtomicBool,
    turns: AtomicU32,
    worker_turns: AtomicU32,
    approvals: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
    native: String,
    rejected_once: Arc<AtomicBool>,
}

pub struct MockSession {
    inner: Arc<Inner>,
}

#[async_trait]
impl AgentSession for MockSession {
    fn native_session_id(&self) -> Option<String> {
        Some(self.inner.native.clone())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            resume: false,
            steer_active_turn: false,
            approvals: true,
            mcp_http: false,
            usage_reporting: false,
            model_selection: false,
        }
    }
    async fn send_input(&self, input: AgentInput) -> ProviderResult<()> {
        let me = self.inner.clone();
        if me.busy.swap(true, Ordering::SeqCst) {
            return Err(ProviderError::Other(
                "mock session already has an active turn".into(),
            ));
        }
        me.interrupted.store(false, Ordering::SeqCst);
        tokio::spawn(async move {
            let turn_id = format!("turn-{}", me.turns.fetch_add(1, Ordering::SeqCst) + 1);
            let _ = me
                .tx
                .send(ProviderEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                })
                .await;
            let result = me.run_turn(&input.text).await;
            let (outcome, error) = match result {
                Ok(()) if me.interrupted.load(Ordering::SeqCst) => (TurnOutcome::Interrupted, None),
                Ok(()) => (TurnOutcome::Completed, None),
                Err(e) => (TurnOutcome::Failed, Some(e)),
            };
            me.busy.store(false, Ordering::SeqCst);
            let _ = me
                .tx
                .send(ProviderEvent::TurnCompleted {
                    turn_id,
                    outcome,
                    error,
                })
                .await;
        });
        Ok(())
    }
    async fn interrupt(&self) -> ProviderResult<()> {
        self.inner.interrupted.store(true, Ordering::SeqCst);
        if let Ok(mut a) = self.inner.approvals.lock() {
            for (_, s) in a.drain() {
                let _ = s.send(ApprovalDecision::Abort);
            }
        }
        Ok(())
    }
    async fn resolve_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> ProviderResult<()> {
        let sender = self
            .inner
            .approvals
            .lock()
            .ok()
            .and_then(|mut a| a.remove(request_id));
        match sender {
            Some(s) => {
                let _ = s.send(decision);
                Ok(())
            }
            None => Err(ProviderError::Protocol(format!(
                "unknown approval {request_id}"
            ))),
        }
    }
    async fn shutdown(&self) -> ProviderResult<()> {
        self.inner.interrupted.store(true, Ordering::SeqCst);
        let _ = self
            .inner
            .tx
            .send(ProviderEvent::Disconnected {
                reason: "shutdown".into(),
            })
            .await;
        Ok(())
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

impl Inner {
    async fn say(&self, text: &str) {
        let _ = self
            .tx
            .send(ProviderEvent::Message {
                text: text.to_string(),
            })
            .await;
    }

    async fn pause(&self) {
        tokio::time::sleep(self.script.step_delay).await;
    }

    fn stopped(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    /// Call a coordination tool exactly like an MCP client would, emitting tool events.
    async fn call(&self, tool: &str, args: Value) -> Result<Value, CoreError> {
        let actor = match self.engine.get_agent(&self.agent_id) {
            Ok(a) => Actor::Agent {
                id: a.id,
                role: a.role,
            },
            Err(e) => return Err(e),
        };
        let call_id = format!("{tool}-{}", self.turns.load(Ordering::SeqCst));
        let _ = self
            .tx
            .send(ProviderEvent::ToolStarted {
                call_id: call_id.clone(),
                name: tool.to_string(),
                detail: Some(args.to_string().chars().take(120).collect()),
            })
            .await;
        let res = self.engine.call_tool(&actor, tool, args);
        let _ = self
            .tx
            .send(ProviderEvent::ToolCompleted {
                call_id,
                name: tool.to_string(),
                ok: res.is_ok(),
                detail: res.as_ref().err().map(|e| e.to_string()),
            })
            .await;
        self.pause().await;
        res
    }

    async fn run_turn(&self, _prompt: &str) -> Result<(), String> {
        let agent = self
            .engine
            .get_agent(&self.agent_id)
            .map_err(|e| e.to_string())?;
        match agent.role {
            Role::Leader => self.leader_turn().await,
            Role::Worker | Role::Specialist => self.worker_turn().await,
            Role::Reviewer => self.reviewer_turn().await,
        }
    }

    // ---------------------------------------------------------------- leader

    async fn leader_turn(&self) -> Result<(), String> {
        let e = |x: CoreError| x.to_string();
        let tasks_v = self.call("maw_task_list", json!({})).await.map_err(e)?;
        let tasks = tasks_v["tasks"].as_array().cloned().unwrap_or_default();
        if tasks.is_empty() {
            self.plan_and_create_tasks().await?;
            return Ok(());
        }
        // 1) messages: answer blockers, acknowledge the rest
        let mut nudged = false;
        let inbox = self.call("maw_message_inbox", json!({})).await.map_err(e)?;
        for m in inbox["messages"].as_array().cloned().unwrap_or_default() {
            if self.stopped() {
                return Ok(());
            }
            let id = m["message_id"].as_str().unwrap_or_default().to_string();
            if m["subject"]
                .as_str()
                .unwrap_or_default()
                .starts_with("All tasks verified")
            {
                nudged = true;
            }
            if m["type"] == "BLOCKER" {
                if let (Some(task), Some(from)) = (m["task"].as_str(), m["from"].as_str()) {
                    if from != "orchestrator" {
                        self.say(&format!(
                            "{from} is blocked on {task}; clarifying and re-assigning"
                        ))
                        .await;
                        let _ = self
                            .call("maw_message_send", json!({ "to": from, "message_type": "ANSWER", "subject": format!("Re: {task} blocker"), "content": "Use bcrypt with cost 12 and store only the hash. Proceed with that assumption.", "task": task }))
                            .await;
                        let _ = self.call("maw_task_release", json!({ "task": task, "reason": "blocker resolved by Leader; retrying" })).await;
                        let _ = self
                            .call("maw_task_assign", json!({ "task": task, "agent": from }))
                            .await;
                    }
                }
            }
            let _ = self
                .call("maw_message_ack", json!({ "message_id": id }))
                .await;
        }
        // 2) reviews nobody else can do
        let agents = self.call("maw_agent_list", json!({})).await.map_err(e)?;
        let has_reviewer = agents["agents"]
            .as_array()
            .map(|a| a.iter().any(|x| x["role"] == "reviewer"))
            .unwrap_or(false);
        let tasks_v = self.call("maw_task_list", json!({})).await.map_err(e)?;
        for t in tasks_v["tasks"].as_array().cloned().unwrap_or_default() {
            if self.stopped() {
                return Ok(());
            }
            let key = t["task"].as_str().unwrap_or_default().to_string();
            match t["status"].as_str().unwrap_or_default() {
                "IN_REVIEW" if !has_reviewer => {
                    let _ = self.call("maw_review_submit", json!({ "task": key, "verdict": "approve", "feedback": "Leader review: matches acceptance criteria." })).await;
                }
                "VERIFYING" => {
                    let full = self
                        .call("maw_task_get", json!({ "task": key }))
                        .await
                        .map_err(e)?;
                    let evidence = full["evidence"].as_array().map(|a| a.len()).unwrap_or(0);
                    self.say(&format!("verifying {key}: {evidence} evidence item(s)"))
                        .await;
                    let _ = self.call("maw_task_verify", json!({ "task": key, "pass": true, "notes": format!("Checked {evidence} evidence items against the acceptance criteria; all satisfied.") })).await;
                }
                _ => {}
            }
        }
        // 3) completion
        let tasks_v = self.call("maw_task_list", json!({})).await.map_err(e)?;
        let all = tasks_v["tasks"].as_array().cloned().unwrap_or_default();
        let may_complete = !self.script.leader_forgets_to_complete || nudged;
        if may_complete
            && !all.is_empty()
            && all
                .iter()
                .all(|t| matches!(t["status"].as_str(), Some("DONE") | Some("CANCELED")))
        {
            self.say("all tasks verified; completing the goal").await;
            let _ = self.call("maw_goal_update", json!({ "status": "completed", "reason": "all tasks verified", "current_focus": "done" })).await;
        }
        Ok(())
    }

    async fn plan_and_create_tasks(&self) -> Result<(), String> {
        let e = |x: CoreError| x.to_string();
        let goal = self.call("maw_goal_get", json!({})).await.map_err(e)?;
        self.say(&format!(
            "analyzing objective: {}",
            goal["goal"]["text"].as_str().unwrap_or("")
        ))
        .await;
        let agents = self.call("maw_agent_list", json!({})).await.map_err(e)?;
        let workers: Vec<String> = agents["agents"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| matches!(x["role"].as_str(), Some("worker") | Some("specialist")))
                    .filter_map(|x| x["agent"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let w1 = workers.first().cloned();
        let w2 = workers.get(1).cloned().or_else(|| w1.clone());
        let has_reviewer = agents["agents"]
            .as_array()
            .map(|a| a.iter().any(|x| x["role"] == "reviewer"))
            .unwrap_or(false);
        let _ = self.call("maw_goal_update", json!({ "current_milestone": "Foundation", "current_focus": "planning the work breakdown" })).await;
        let specs = [
            (
                "AUTH-01 Analyze existing architecture",
                "Survey the repository layout and conventions.",
                vec![],
                w1.clone(),
                true,
                false,
                1.0,
            ),
            (
                "AUTH-02 Design interfaces",
                "Define the authentication module interfaces.",
                vec!["T-1"],
                w1.clone(),
                false,
                false,
                1.0,
            ),
            (
                "AUTH-03 Implement backend logic",
                "Implement credential hashing and validation.",
                vec!["T-2"],
                w1.clone(),
                false,
                true,
                3.0,
            ),
            (
                "AUTH-04 Write tests",
                "Write unit tests for the module.",
                vec!["T-2"],
                w2.clone(),
                false,
                false,
                2.0,
            ),
            (
                "AUTH-05 Write documentation",
                "Document the module.",
                vec!["T-2"],
                w2.clone(),
                false,
                false,
                1.0,
            ),
            (
                "AUTH-06 Integrate and verify",
                "Run the full suite and confirm the module works end to end.",
                vec!["T-3", "T-4", "T-5"],
                w1.clone(),
                false,
                false,
                1.0,
            ),
        ];
        let mut milestones = vec![];
        for (i, (title, desc, deps, assignee, no_files, review, weight)) in specs.iter().enumerate()
        {
            let mut args = json!({
                "title": title, "description": desc, "dependencies": deps, "weight": weight,
                "acceptance_criteria": [format!("{title} is complete and evidenced")],
                "no_file_changes_expected": no_files, "requires_review": *review && has_reviewer,
                "idempotency_key": format!("plan1-{i}"),
            });
            if let Some(a) = assignee {
                args["assignee"] = json!(a);
            }
            self.call("maw_task_create", args).await.map_err(e)?;
            milestones.push(json!({ "title": title, "task_keys": [format!("T-{}", i + 1)] }));
        }
        self.call(
            "maw_plan_propose_revision",
            json!({ "summary": "Analyze, design, then implement/test/document in parallel, then integrate.", "milestones": milestones, "next_actions": ["dispatch T-1"], "reason": "initial plan" }),
        )
        .await
        .map_err(e)?;
        self.say("plan recorded and tasks created").await;
        Ok(())
    }

    // ---------------------------------------------------------------- worker

    async fn worker_turn(&self) -> Result<(), String> {
        let e = |x: CoreError| x.to_string();
        let me = self.engine.get_agent(&self.agent_id).map_err(e)?;
        let Some(tid) = me.current_task_id.clone() else {
            // nothing assigned: just acknowledge messages
            let inbox = self.call("maw_message_inbox", json!({})).await.map_err(e)?;
            for m in inbox["messages"].as_array().cloned().unwrap_or_default() {
                let _ = self
                    .call("maw_message_ack", json!({ "message_id": m["message_id"] }))
                    .await;
            }
            return Ok(());
        };
        let task = self.engine.get_task(&tid.0).map_err(e)?;
        if task.status != TaskStatus::Running {
            return Ok(());
        }
        let key = task.key.clone();
        let n = self.worker_turns.fetch_add(1, Ordering::SeqCst);
        self.say(&format!("starting {key}: {}", task.title)).await;

        if self.script.approval_demo && n == 0 {
            let req = format!("apr-{}", self.agent_id);
            let (tx, rx) = oneshot::channel();
            if let Ok(mut a) = self.approvals.lock() {
                a.insert(req.clone(), tx);
            }
            let _ = self
                .tx
                .send(ProviderEvent::ApprovalRequested {
                    request_id: req,
                    kind: ApprovalKind::Command,
                    summary: "run `cargo test` in the workspace".into(),
                    reason: Some("verify the implementation".into()),
                    paths: vec![self.cwd.to_string_lossy().to_string()],
                    details: json!({ "command": "cargo test" }),
                })
                .await;
            match tokio::time::timeout(Duration::from_secs(300), rx).await {
                Ok(Ok(ApprovalDecision::Approve | ApprovalDecision::ApproveForSession)) => {}
                _ => {
                    let _ = self.call("maw_task_block", json!({ "task": key, "reason": "the operator denied the command needed to verify this task", "needs": "approval or an alternative verification method" })).await;
                    return Ok(());
                }
            }
        }

        if self.script.block_first_backend
            && task.title.to_lowercase().contains("backend")
            && task.attempt_count == 1
        {
            self.say("the requirements don't say which password hashing scheme to use")
                .await;
            self.call("maw_task_block", json!({ "task": key, "reason": "Unclear which password hashing algorithm to use", "needs": "a decision from the Leader" })).await.map_err(e)?;
            return Ok(());
        }

        let _ = self.call("maw_progress_report", json!({ "task": key, "percent": 25, "note": "read the code and planned the change" })).await;
        if self.stopped() {
            return Ok(());
        }
        let mut changed = vec![];
        let mut evidence = vec![];
        if task.no_file_changes_expected {
            evidence.push(json!({ "kind": "verification", "summary": format!("{} complete: findings recorded as shared context", task.title), "passed": true }));
            let _ = self.call("maw_context_publish", json!({ "key": format!("{key}-findings"), "content": format!("Findings of {}: plain module layout, no existing auth code.", task.title), "tags": ["analysis"], "task": key })).await;
        } else {
            let file = format!("{}.md", slugify(&task.title));
            let path = self.cwd.join(&file);
            std::fs::write(
                &path,
                format!("# {}\n\nProduced by {} for {key}.\n", task.title, me.name),
            )
            .map_err(|x| x.to_string())?;
            let _ = self
                .tx
                .send(ProviderEvent::FileChanged {
                    paths: vec![file.clone()],
                })
                .await;
            changed.push(file);
            let _ = self
                .call(
                    "maw_progress_report",
                    json!({ "task": key, "percent": 70, "note": "implementation written" }),
                )
                .await;
            if task.title.to_lowercase().contains("test")
                || task.title.to_lowercase().contains("backend")
                || task.title.to_lowercase().contains("integrate")
            {
                let _ = self
                    .tx
                    .send(ProviderEvent::CommandStarted {
                        call_id: "c1".into(),
                        command: "cargo test".into(),
                        cwd: Some(self.cwd.to_string_lossy().to_string()),
                    })
                    .await;
                self.pause().await;
                let _ = self
                    .tx
                    .send(ProviderEvent::CommandCompleted {
                        call_id: "c1".into(),
                        exit_code: Some(0),
                    })
                    .await;
                evidence.push(json!({ "kind": "test_result", "summary": "cargo test: 18 passed, 0 failed", "passed": true }));
            }
        }
        if self.stopped() {
            return Ok(());
        }
        self.call(
            "maw_task_submit_result",
            json!({ "task": key, "summary": format!("Completed {}", task.title), "changed_files": changed, "evidence": evidence }),
        )
        .await
        .map_err(e)?;
        Ok(())
    }

    // ---------------------------------------------------------------- reviewer

    async fn reviewer_turn(&self) -> Result<(), String> {
        let e = |x: CoreError| x.to_string();
        let me = self.engine.get_agent(&self.agent_id).map_err(e)?;
        let Some(tid) = me.current_task_id.clone() else {
            return Ok(());
        };
        let task = self.engine.get_task(&tid.0).map_err(e)?;
        if task.status != TaskStatus::InReview {
            return Ok(());
        }
        self.say(&format!(
            "reviewing {}: {} evidence item(s)",
            task.key,
            task.evidence.len()
        ))
        .await;
        let reject =
            self.script.reject_first_review && !self.rejected_once.swap(true, Ordering::SeqCst);
        if reject {
            self.call("maw_review_submit", json!({ "task": task.key, "verdict": "reject", "feedback": "Add input validation for empty passwords and cover it with a test." })).await.map_err(e)?;
        } else {
            self.call("maw_review_submit", json!({ "task": task.key, "verdict": "approve", "feedback": "Diff is focused, tests pass, criteria met." })).await.map_err(e)?;
        }
        Ok(())
    }
}
