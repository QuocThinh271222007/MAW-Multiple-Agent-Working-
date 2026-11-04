use maw_core::provider::ApprovalDecision;
use maw_core::*;
use maw_orchestrator::mock::{MockProvider, MockScript};
use maw_orchestrator::*;
use maw_storage::Store;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "# demo\n").unwrap();
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
}

fn fast_cfg() -> OrchestratorConfig {
    OrchestratorConfig {
        tick_ms: 100,
        leader_wake_debounce_ms: 40,
        ..Default::default()
    }
}

fn add_team(engine: &Engine) {
    engine
        .add_agent(
            "claude-lead",
            ProviderKind::Mock,
            Role::Leader,
            None,
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    engine
        .add_agent(
            "codex-backend",
            ProviderKind::Mock,
            Role::Worker,
            None,
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    engine
        .add_agent(
            "codex-tests",
            ProviderKind::Mock,
            Role::Worker,
            None,
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    engine
        .add_agent(
            "claude-review",
            ProviderKind::Mock,
            Role::Reviewer,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
}

async fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mock_team_completes_the_auth_scenario_end_to_end() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    let script = MockScript {
        block_first_backend: true,
        reject_first_review: true,
        ..Default::default()
    };
    sup.register_provider(MockProvider::new(engine.clone(), script));
    sup.connect_all().await;
    assert!(
        engine
            .list_agents()
            .unwrap()
            .iter()
            .all(|a| a.state == AgentState::Idle),
        "all four agents connect"
    );

    let runner = tokio::spawn(sup.clone().run());
    engine
        .create_goal(
            "Implement a simple authentication module with validation, tests and docs.",
            vec!["tests pass".into(), "docs exist".into()],
        )
        .unwrap();

    let mut max_parallel = 0usize;
    let done = wait_for(Duration::from_secs(60), || {
        let snap = engine.snapshot().unwrap();
        max_parallel = max_parallel.max(
            snap.tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Running)
                .count(),
        );
        snap.goal
            .as_ref()
            .map(|g| g.status == GoalStatus::Completed)
            .unwrap_or(false)
    })
    .await;
    let snap = engine.snapshot().unwrap();
    assert!(
        done,
        "goal did not complete; tasks: {:#?}",
        snap.tasks
            .iter()
            .map(|t| (&t.key, t.status, t.attempt_count))
            .collect::<Vec<_>>()
    );

    // every task verified, 100% verified progress
    assert_eq!(snap.tasks.len(), 6);
    assert!(
        snap.tasks.iter().all(|t| t.status == TaskStatus::Done),
        "{:?}",
        snap.tasks
            .iter()
            .map(|t| (&t.key, t.status))
            .collect::<Vec<_>>()
    );
    assert_eq!(snap.progress.verified_pct, 100.0);
    let _ = max_parallel;

    // dependency order: T-3 only started after T-2 completed
    let all_events = engine
        .read(|db| db.events_since(engine.project_id(), 0, 100_000))
        .unwrap();
    let seq = |kind: EventKind, key: &str| {
        all_events
            .iter()
            .find(|e| e.event_type == kind && e.payload["task_key"] == key)
            .map(|e| e.sequence)
    };
    let t2_done = seq(EventKind::TaskCompleted, "T-2").expect("T-2 completed");
    let t3_start = seq(EventKind::TaskStarted, "T-3").expect("T-3 started");
    assert!(
        t2_done < t3_start,
        "T-3 must not start before its dependency completes"
    );

    // two different workers executed at the same time: their [started, first submitted] intervals overlap
    let interval = |key: &str| {
        let start = all_events
            .iter()
            .find(|e| e.event_type == EventKind::TaskStarted && e.payload["task_key"] == key)
            .map(|e| e.sequence)
            .unwrap();
        let end = all_events
            .iter()
            .find(|e| e.event_type == EventKind::TaskSubmitted && e.payload["task_key"] == key)
            .map(|e| e.sequence)
            .unwrap();
        (start, end)
    };
    let (a, b) = (interval("T-3"), interval("T-4"));
    assert!(
        a.0 < b.1 && b.0 < a.1,
        "T-3 (backend worker) and T-4 (test worker) should overlap: {a:?} vs {b:?}"
    );

    // blocker -> leader answered -> retry; review rejection -> correction -> approval
    assert!(all_events
        .iter()
        .any(|e| e.event_type == EventKind::TaskBlocked && e.payload["task_key"] == "T-3"));
    assert!(all_events
        .iter()
        .any(|e| e.event_type == EventKind::TaskReviewed && e.payload["verdict"] == "rejected"));
    assert!(all_events
        .iter()
        .any(|e| e.event_type == EventKind::TaskReviewed && e.payload["verdict"] == "approved"));
    assert!(
        all_events
            .iter()
            .any(|e| e.event_type == EventKind::MessageDelivered),
        "messages are really delivered"
    );
    assert!(all_events
        .iter()
        .any(|e| e.event_type == EventKind::McpToolCompleted));
    let t3 = engine.get_task("T-3").unwrap();
    assert_eq!(
        t3.attempt_count, 3,
        "blocked once, rejected once, then approved"
    );
    assert!(t3.evidence.iter().any(|e| e.kind == EvidenceKind::Review));
    assert!(
        all_events.windows(2).all(|w| w[0].sequence < w[1].sequence),
        "sequence numbers are strictly ordered"
    );

    // work happened in isolated worktrees and never in the user's checkout
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]),
        "",
        "user checkout untouched"
    );
    let wt = repo.path().join(".maw/worktrees");
    assert!(wt.join("codex-backend").exists() && wt.join("codex-tests").exists());
    // Integration is continuous (as tasks complete), so by now both branches are merged cleanly;
    // an explicit extra pass is a no-op and reports no conflicts.
    let results = sup.integrate_done_work().await.unwrap();
    assert!(
        results.iter().all(|(_, o)| !matches!(
            o,
            maw_orchestrator::workspace::IntegrationOutcome::Conflict { .. }
        )),
        "{results:?}"
    );
    let snap = engine.snapshot().unwrap();
    let worktrees: Vec<_> = snap
        .workspaces
        .iter()
        .filter(|w| w.kind == "worktree")
        .collect();
    assert_eq!(worktrees.len(), 2);
    assert!(
        worktrees
            .iter()
            .all(|w| w.integration_status == "integrated"),
        "{:?}",
        worktrees
            .iter()
            .map(|w| (&w.branch, &w.integration_status))
            .collect::<Vec<_>>()
    );
    let integ = sup.integration_path();
    assert!(
        std::fs::read_dir(&integ).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("implement-backend")),
        "integration branch contains the backend work"
    );

    sup.shutdown().await;
    runner.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_are_recorded_and_resolved_through_the_provider() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(
        engine.clone(),
        MockScript {
            approval_demo: true,
            ..Default::default()
        },
    ));
    sup.connect_all().await;
    let runner = tokio::spawn(sup.clone().run());
    engine.create_goal("auth", vec![]).unwrap();

    assert!(
        wait_for(Duration::from_secs(30), || !engine
            .pending_approvals()
            .unwrap()
            .is_empty())
        .await,
        "an approval request should appear"
    );
    let ap = engine.pending_approvals().unwrap().remove(0);
    let agent = engine.get_agent(&ap.agent_id).unwrap();
    assert_eq!(agent.state, AgentState::WaitingApproval);
    assert!(ap.summary.contains("cargo test"));
    sup.resolve_approval(&ap.id, ApprovalDecision::Approve)
        .await
        .unwrap();
    assert!(engine
        .pending_approvals()
        .unwrap()
        .iter()
        .all(|a| a.id != ap.id));
    // double resolution is rejected
    assert!(engine
        .resolve_approval(&ap.id, ApprovalStatus::Denied)
        .is_err());
    sup.shutdown().await;
    runner.abort();
}

#[test]
fn crash_mid_task_is_recovered_conservatively_after_restart() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let db_path = data.path().join("maw.db");

    // Phase 1: run until a task is RUNNING, then "crash" (drop the runtime without shutdown).
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (db, repo_path) = (db_path.clone(), repo.path().to_path_buf());
        rt.block_on(async move {
            let store = Arc::new(Store::open(&db).unwrap());
            let engine =
                Engine::open(store, "demo", &repo_path.to_string_lossy(), fast_cfg()).unwrap();
            add_team(&engine);
            let sup = Supervisor::new(engine.clone());
            // slow steps keep the worker mid-task when we pull the plug
            sup.register_provider(MockProvider::new(
                engine.clone(),
                MockScript {
                    step_delay: Duration::from_millis(400),
                    ..Default::default()
                },
            ));
            sup.connect_all().await;
            tokio::spawn(sup.clone().run());
            engine.create_goal("auth", vec![]).unwrap();
            assert!(
                wait_for(Duration::from_secs(30), || engine
                    .list_tasks()
                    .unwrap()
                    .iter()
                    .any(|t| t.status == TaskStatus::Running))
                .await,
                "a task should be running"
            );
        });
        rt.shutdown_background(); // no engine.shutdown(): simulates kill -9
    }

    // Phase 2: restart on the same database.
    let store = Arc::new(Store::open(&db_path).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    let before = engine.list_tasks().unwrap();
    let running: Vec<_> = before
        .iter()
        .filter(|t| t.status == TaskStatus::Running)
        .map(|t| t.key.clone())
        .collect();
    assert!(
        !running.is_empty(),
        "persisted state shows the interrupted task"
    );
    assert_eq!(engine.list_agents().unwrap().len(), 4, "agents persisted");

    let report = engine.recover().unwrap();
    assert!(report.unclean_previous_runs >= 1, "{report:?}");
    assert_eq!(
        report.tasks_parked, running,
        "interrupted tasks are parked, not re-executed"
    );
    assert!(report
        .needs_attention
        .iter()
        .any(|s| s.contains("was running")));
    for key in &running {
        let t = engine.get_task(key).unwrap();
        assert_eq!(t.status, TaskStatus::Blocked);
        assert!(t.blocked_reason.unwrap().contains("restarted"));
    }
    assert!(engine
        .list_agents()
        .unwrap()
        .iter()
        .all(|a| a.state == AgentState::Disconnected && a.current_task_id.is_none()));
    // the goal, tasks and event history survived
    let snap = engine.snapshot().unwrap();
    assert!(snap.goal.is_some() && !snap.tasks.is_empty() && !snap.recent_events.is_empty());
    // leader is told to decide
    let leader = engine.leader().unwrap().unwrap();
    assert!(engine
        .undelivered_for(&leader.id)
        .unwrap()
        .iter()
        .any(|m| m.subject.contains("Recovery")));
    // a second recovery is a no-op (idempotent)
    assert!(engine.recover().unwrap().tasks_parked.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_leader_is_nudged_after_integration_to_complete_the_goal() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(
        engine.clone(),
        MockScript {
            leader_forgets_to_complete: true,
            ..Default::default()
        },
    ));
    sup.connect_all().await;
    let runner = tokio::spawn(sup.clone().run());
    engine.create_goal("auth", vec![]).unwrap();

    let done = wait_for(Duration::from_secs(60), || {
        engine
            .current_goal()
            .unwrap()
            .map(|g| g.status == GoalStatus::Completed)
            .unwrap_or(false)
    })
    .await;
    assert!(
        done,
        "the nudge should let the Leader finish; tasks: {:?}",
        engine
            .list_tasks()
            .unwrap()
            .iter()
            .map(|t| (&t.key, t.status))
            .collect::<Vec<_>>()
    );

    let events = engine
        .read(|db| db.events_since(engine.project_id(), 0, 100_000))
        .unwrap();
    let nudge = events
        .iter()
        .find(|e| {
            e.event_type == EventKind::MessageSent
                && e.payload["subject"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("All tasks verified")
        })
        .expect("completion nudge was sent");
    let completed = events
        .iter()
        .find(|e| e.event_type == EventKind::GoalCompleted)
        .unwrap();
    assert!(
        nudge.sequence < completed.sequence,
        "goal completed only after the nudge"
    );
    // worker branches were integrated before the Leader was nudged
    let snap = engine.snapshot().unwrap();
    assert!(
        snap.workspaces
            .iter()
            .filter(|w| w.kind == "worktree")
            .all(|w| w.integration_status == "integrated"),
        "{:?}",
        snap.workspaces
            .iter()
            .map(|w| (&w.branch, &w.integration_status))
            .collect::<Vec<_>>()
    );
    assert!(repo.path().join(".maw/worktrees/_integration").exists());
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]),
        "",
        "user checkout untouched"
    );
    sup.shutdown().await;
    runner.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconfiguring_an_idle_agent_reconnects_it_and_a_stopped_one_just_saves() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.connect_all().await;
    let w = engine.resolve_agent("codex-backend").unwrap();

    engine
        .set_agent_model(&w.id, Some("gpt-6-luna".into()))
        .unwrap();
    engine.set_agent_effort(&w.id, "high").unwrap();
    let note = sup.reconfigure_agent(&w.id).await.unwrap();
    assert!(note.starts_with("applied"), "{note}");
    assert!(sup.is_live(&w.id).await, "reconnected");
    assert_eq!(engine.get_agent(&w.id).unwrap().state, AgentState::Idle);

    sup.disconnect_agent(&w.id, true).await;
    engine.set_agent_effort(&w.id, "low").unwrap();
    let note = sup.reconfigure_agent(&w.id).await.unwrap();
    assert!(note.starts_with("saved"), "{note}");
    assert!(
        !sup.is_live(&w.id).await,
        "a stopped agent is not revived by a settings change"
    );
    sup.shutdown().await;
}

/// A provider that can only list models (what `maw demo` registers for Claude/Codex).
struct ListOnly;

#[async_trait::async_trait]
impl maw_core::provider::AgentProvider for ListOnly {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }
    async fn list_models(
        &self,
    ) -> maw_core::provider::ProviderResult<Vec<maw_core::provider::ModelChoice>> {
        Ok(vec![maw_core::provider::ModelChoice {
            id: "gpt-6-luna".into(),
            display_name: "Luna".into(),
            description: String::new(),
            is_default: true,
            source: "provider".into(),
            reasoning_efforts: vec![],
        }])
    }
    async fn detect(&self) -> maw_core::provider::ProviderResult<maw_core::provider::ProviderInfo> {
        Err(maw_core::provider::ProviderError::Unsupported(
            "list only".into(),
        ))
    }
    async fn auth_status(
        &self,
    ) -> maw_core::provider::ProviderResult<maw_core::provider::AuthStatus> {
        Err(maw_core::provider::ProviderError::Unsupported(
            "list only".into(),
        ))
    }
    async fn start_session(
        &self,
        _cfg: maw_core::provider::SessionConfig,
    ) -> maw_core::provider::ProviderResult<(
        maw_core::provider::SessionHandle,
        maw_core::provider::EventReceiver,
    )> {
        panic!("a catalog-only provider must never start a session");
    }
}

#[tokio::test]
async fn catalog_providers_list_models_but_can_never_run_an_agent() {
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", "/tmp/demo", fast_cfg()).unwrap();
    let a = engine
        .add_agent(
            "c",
            ProviderKind::Codex,
            Role::Worker,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
    let sup = Supervisor::new(engine.clone());
    sup.register_catalog_provider(Arc::new(ListOnly));
    // models and validation work through the catalog provider
    let list = sup.list_models(ProviderKind::Codex).await.unwrap();
    assert_eq!(list[0].id, "gpt-6-luna");
    assert!(sup
        .validate_model(ProviderKind::Codex, "gpt-6-luna")
        .await
        .is_ok());
    assert!(sup
        .validate_model(ProviderKind::Codex, "gpt-9")
        .await
        .is_err());
    // but connecting an agent of that kind fails instead of using it
    assert!(sup.connect_agent(&a.id).await.is_err());
    assert!(sup.provider(ProviderKind::Codex).is_none());
    // and it is never used for login / auth either: only providers that can run agents are
    assert!(sup.login_command(ProviderKind::Codex).is_none());
    assert!(sup.auth_status(ProviderKind::Codex).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_conversation_separates_thinking_and_maw_protocol_from_the_agents_own_messages() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.connect_all().await;
    let runner = tokio::spawn(sup.clone().run());
    engine.create_goal("auth", vec![]).unwrap();
    assert!(
        wait_for(Duration::from_secs(60), || engine
            .current_goal()
            .unwrap()
            .map(|g| g.status == GoalStatus::Completed)
            .unwrap_or(false))
        .await
    );

    let worker = engine.resolve_agent("codex-backend").unwrap();
    let lines = sup.transcript(&worker.id);
    use maw_orchestrator::LineKind::*;
    let count = |k| lines.iter().filter(|l| l.kind == k).count();
    assert!(
        count(Thinking) > 0,
        "reasoning the provider streamed is kept as its own kind"
    );
    assert!(
        count(Assistant) > 0,
        "the agent's own messages are untouched"
    );
    // calls to MAW's coordination tools are protocol, not generic tool lines
    assert!(lines
        .iter()
        .any(|l| l.kind == Protocol && l.text.contains("maw_task_submit_result")));
    assert!(
        lines
            .iter()
            .filter(|l| l.kind == Tool)
            .all(|l| !l.text.contains("maw_")),
        "no maw_* call is classified as a plain tool"
    );
    assert!(maw_orchestrator::supervisor::is_maw_tool(
        "mcp__maw__maw_message_ack"
    ));
    assert!(maw_orchestrator::supervisor::is_maw_tool(
        "maw/maw_message_inbox"
    ));
    assert!(!maw_orchestrator::supervisor::is_maw_tool("Bash"));
    assert!(!maw_orchestrator::supervisor::is_maw_tool(
        "github/create_issue"
    ));
    sup.shutdown().await;
    runner.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_live_agent_stops_its_session_and_protects_a_running_task() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.connect_all().await;
    let idle = engine.resolve_agent("codex-tests").unwrap();
    assert!(sup.is_live(&idle.id).await);
    let r = sup.remove_agent(&idle.id, false).await.unwrap();
    assert_eq!(r.name, "codex-tests");
    assert!(!sup.is_live(&idle.id).await, "its session is gone");
    assert!(engine.get_agent(&idle.id).is_err());
    assert!(engine
        .list_agents()
        .unwrap()
        .iter()
        .all(|a| a.name != "codex-tests"));

    // a worker mid-task cannot be removed without force, and the refusal leaves its session alone
    engine.create_goal("auth", vec![]).unwrap();
    engine
        .create_task(
            &Actor::User,
            NewTask {
                title: "busy".into(),
                assignee: Some("codex-backend".into()),
                ..Default::default()
            },
        )
        .unwrap();
    engine.start_task("T-1").unwrap();
    let busy = engine.resolve_agent("codex-backend").unwrap();
    let e = sup.remove_agent(&busy.id, false).await.unwrap_err();
    assert_eq!(e.code(), "conflict");
    assert!(
        sup.is_live(&busy.id).await,
        "still connected after a refused removal"
    );
    sup.shutdown().await;
}

/// What the user typed must be what the conversation shows - at once, and not MAW's wrapper
/// ("You have 1 new message to handle.") around it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_to_an_agent_shows_the_users_own_words_in_its_conversation() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.connect_all().await;
    let runner = tokio::spawn(sup.clone().run());
    let worker = engine.resolve_agent("codex-backend").unwrap();

    sup.message_agent("codex-backend", "please say hello in one sentence", false)
        .await
        .unwrap();
    // shown immediately, before the agent has had a chance to take it
    let lines = sup.transcript(&worker.id);
    assert!(
        lines
            .iter()
            .any(|l| l.kind == LineKind::User && l.text == "please say hello in one sentence"),
        "{lines:?}"
    );
    // and still exactly once after it was delivered to the agent
    let delivered = wait_for(Duration::from_secs(20), || {
        engine
            .inbox(
                &Actor::Agent {
                    id: worker.id.clone(),
                    role: Role::Worker,
                },
                false,
                10,
            )
            .map(|m| m.iter().any(|m| m.delivered_at.is_some()))
            .unwrap_or(false)
    })
    .await;
    assert!(delivered, "the message was delivered");
    let lines = sup.transcript(&worker.id);
    let mine = lines
        .iter()
        .filter(|l| l.kind == LineKind::User && l.text.contains("say hello"))
        .count();
    assert_eq!(mine, 1, "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.text.starts_with("You have")),
        "MAW's wrapper is not shown as if the user had said it: {lines:?}"
    );
    sup.shutdown().await;
    runner.abort();
}

/// `maw refresh` leaves a request in the database; a running MAW reconnects the agents that are
/// down. A request left over from an earlier run is not acted on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_request_reconnects_agents_that_are_down() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let write_request = |v: &str| {
        engine
            .store()
            .write(|db| db.kv_set(engine.project_id(), "refresh_request", v))
            .unwrap()
    };
    write_request("left over from an earlier run");
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    let worker = engine.resolve_agent("codex-backend").unwrap();
    let runner = tokio::spawn(sup.clone().run());

    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !sup.is_live(&worker.id).await,
        "an old request is not a request"
    );

    write_request("now");
    let mut up = false;
    for _ in 0..100 {
        if sup.is_live(&worker.id).await {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up, "the new request reconnected the agent");

    // the same request is handled once: stopping the agent by hand keeps it stopped
    sup.disconnect_agent(&worker.id, true).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!sup.is_live(&worker.id).await);
    sup.shutdown().await;
    runner.abort();
}

/// The configured location decides where a worker's worktree is made, and an idle worker's worktree
/// follows when the location is changed and the agent reconnects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_worktree_location_can_be_switched_and_an_idle_agent_follows() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let wsl_side = tempfile::tempdir().unwrap();
    let win_side = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.set_worktree_bases(
        Ok(wsl_side.path().join("worktrees")),
        Ok(win_side.path().join("worktrees")),
    );
    let worker = engine.resolve_agent("codex-backend").unwrap();
    let path_of = |engine: &Engine| {
        engine
            .snapshot()
            .unwrap()
            .workspaces
            .into_iter()
            .find(|w| w.agent_id.as_ref() == Some(&worker.id) && w.kind == "worktree")
            .map(|w| w.path)
            .unwrap_or_default()
    };

    let mut c = engine.config();
    c.worktree_location = "wsl".into();
    engine.set_config(c.clone());
    sup.connect_agent(&worker.id).await.unwrap();
    assert!(
        path_of(&engine).starts_with(&wsl_side.path().to_string_lossy().to_string()),
        "{}",
        path_of(&engine)
    );
    assert!(
        !repo.path().join(".maw").exists(),
        "nothing next to the project"
    );

    // switch to the Windows side; the idle agent moves when it reconnects
    c.worktree_location = "windows".into();
    engine.set_config(c.clone());
    sup.disconnect_agent(&worker.id, false).await;
    sup.connect_agent(&worker.id).await.unwrap();
    let now = path_of(&engine);
    assert!(
        now.starts_with(&win_side.path().to_string_lossy().to_string()),
        "{now}"
    );
    assert!(
        std::fs::read_dir(wsl_side.path().join("worktrees"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0,
        "the old folder was removed"
    );

    sup.shutdown().await;
}

/// A side that is not available (say, no Windows drive) is reported in the agent's conversation and
/// the project folder is used instead of failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unavailable_worktree_location_falls_back_to_the_project_folder_and_says_so() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let store = Arc::new(Store::open(data.path().join("maw.db")).unwrap());
    let engine = Engine::open(store, "demo", &repo.path().to_string_lossy(), fast_cfg()).unwrap();
    add_team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.set_worktree_bases(Err("no WSL".into()), Err("no Windows drive here".into()));
    let mut c = engine.config();
    c.worktree_location = "windows".into();
    engine.set_config(c);
    let worker = engine.resolve_agent("codex-backend").unwrap();
    sup.connect_agent(&worker.id).await.unwrap();
    let said = sup
        .transcript(&worker.id)
        .iter()
        .any(|l| l.text.contains("not available") && l.text.contains("no Windows drive here"));
    assert!(said, "{:?}", sup.transcript(&worker.id));
    assert!(repo.path().join(".maw").join("worktrees").exists());
    sup.shutdown().await;
}
