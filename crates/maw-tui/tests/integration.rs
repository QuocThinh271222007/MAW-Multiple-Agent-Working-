//! Integration: a real `Engine` + `Supervisor` + the deterministic mock provider run the
//! full team scenario; the TUI then renders the resulting snapshot and event-derived flow.

use maw_core::provider::ApprovalDecision;
use maw_core::*;
use maw_orchestrator::mock::{MockProvider, MockScript};
use maw_orchestrator::{Engine, Supervisor};
use maw_storage::Store;
use maw_tui::app::{handle_key, Action, View};
use maw_tui::flow::{derive_flow, EdgeKind, ORCHESTRATOR};
use maw_tui::testutil::render_text;
use maw_tui::{App, Theme};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
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

async fn wait_for(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn team(engine: &Engine) {
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

fn cfg() -> OrchestratorConfig {
    OrchestratorConfig {
        tick_ms: 100,
        leader_wake_debounce_ms: 40,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tui_renders_a_completed_mock_team_run() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let engine = Engine::open(
        Arc::new(Store::open_in_memory().unwrap()),
        "demo",
        &repo.path().to_string_lossy(),
        cfg(),
    )
    .unwrap();
    team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(
        engine.clone(),
        MockScript {
            block_first_backend: true,
            reject_first_review: true,
            ..Default::default()
        },
    ));
    sup.connect_all().await;
    let runner = tokio::spawn(sup.clone().run());
    engine
        .create_goal(
            "Implement a simple authentication module",
            vec!["tests pass".into()],
        )
        .unwrap();
    assert!(
        wait_for(60, || engine
            .current_goal()
            .ok()
            .flatten()
            .map(|g| g.status == GoalStatus::Completed)
            .unwrap_or(false))
        .await,
        "mock team should finish"
    );

    // Build the UI state exactly like the runtime does.
    let mut app = App::new(Theme::new(false));
    app.apply_snapshot(engine.snapshot().unwrap());
    app.extra_events = engine
        .read(|db| db.recent_events(engine.project_id(), 4000))
        .unwrap();
    app.goal_history = engine.goal_history().unwrap();
    app.plan_history = engine.plan_history().unwrap();
    app.config = engine.config();
    for a in app.agents().to_vec() {
        app.transcripts.insert(a.id.clone(), sup.transcript(&a.id));
    }
    app.now = chrono::Utc::now();
    app.size = (140, 44);

    let dash = render_text(&app, 140, 44);
    assert!(dash.contains("100%"), "verified progress at 100%\n{dash}");
    assert!(
        dash.contains("claude-lead")
            && dash.contains("codex-backend")
            && dash.contains("codex-tests")
            && dash.contains("claude-review"),
        "{dash}"
    );
    assert!(dash.contains("completed"), "goal status shown\n{dash}");
    assert!(dash.contains("Blockers: 0"), "{dash}");
    // live output made it from provider events into the cards
    assert!(
        app.transcripts
            .values()
            .any(|t| t.iter().any(|l| l.text.contains("maw_task"))),
        "tool activity in transcripts"
    );

    app.view = View::Tasks;
    let tasks = render_text(&app, 170, 44);
    assert!(tasks.contains("DONE (6)"), "{tasks}");

    app.view = View::Goal;
    let goal = render_text(&app, 120, 60);
    assert!(
        goal.contains("Verified complete") && goal.contains("goal r1") && goal.contains("plan r1"),
        "{goal}"
    );

    app.view = View::Flow;
    let flow = render_text(&app, 150, 70);
    assert!(
        flow.contains("CREATE TASKS")
            && flow.contains("ASSIGN")
            && flow.contains("RESULT")
            && flow.contains("FEEDBACK"),
        "real edges\n{flow}"
    );

    // The flow graph's edges agree with the stored events.
    let g = derive_flow(app.agents(), &app.extra_events, chrono::Utc::now(), 3600);
    let leader = app
        .agents()
        .iter()
        .find(|a| a.role == Role::Leader)
        .unwrap()
        .id
        .0
        .clone();
    assert_eq!(
        g.count(&leader, ORCHESTRATOR, EdgeKind::Create),
        6,
        "leader created six tasks"
    );
    let rev = app
        .agents()
        .iter()
        .find(|a| a.role == Role::Reviewer)
        .unwrap()
        .id
        .0
        .clone();
    assert!(
        g.edges
            .iter()
            .any(|e| e.from == rev && e.kind == EdgeKind::Feedback),
        "review rejection looped back to the owner"
    );
    assert!(
        g.edges.iter().any(|e| e.kind == EdgeKind::Block),
        "the blocker is visible"
    );

    app.view = View::Logs;
    let logs = render_text(&app, 120, 30);
    assert!(
        logs.contains("LOGS & DIAGNOSTICS") && logs.contains("BLOCKED T-3"),
        "{logs}"
    );

    sup.shutdown().await;
    runner.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tui_reducer_actions_drive_the_real_engine() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let engine = Engine::open(
        Arc::new(Store::open_in_memory().unwrap()),
        "demo",
        &repo.path().to_string_lossy(),
        cfg(),
    )
    .unwrap();
    team(&engine);
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(
        engine.clone(),
        MockScript {
            approval_demo: true,
            step_delay: Duration::from_millis(30),
            ..Default::default()
        },
    ));
    sup.connect_all().await;
    let runner = tokio::spawn(sup.clone().run());

    let mut app = App::new(Theme::new(false));
    app.apply_snapshot(engine.snapshot().unwrap());

    // 'G' with no goal opens the set-goal dialog; fill it in like a user would.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut actions = handle_key(&mut app, key(KeyCode::Char('G')));
    assert!(actions.is_empty());
    for c in "Build auth".chars() {
        handle_key(&mut app, key(KeyCode::Char(c)));
    }
    handle_key(&mut app, key(KeyCode::Enter)); // -> criteria
    actions = handle_key(&mut app, key(KeyCode::Enter)); // submit
    let Some(Action::CreateGoal { text, criteria }) = actions.pop() else {
        panic!("expected CreateGoal")
    };
    engine.create_goal(&text, criteria).unwrap();

    // wait for the approval demo to ask, then answer it through the reducer's action
    assert!(
        wait_for(30, || !engine.pending_approvals().unwrap().is_empty()).await,
        "approval requested"
    );
    app.apply_snapshot(engine.snapshot().unwrap());
    app.now = chrono::Utc::now();
    app.size = (120, 30);
    handle_key(&mut app, key(KeyCode::Char('o')));
    let t = render_text(&app, 120, 30);
    assert!(t.contains("1 pending") && t.contains("cargo test"), "{t}");
    let mut acts = handle_key(&mut app, key(KeyCode::Char('y')));
    let Some(Action::ResolveApproval { id, decision }) = acts.pop() else {
        panic!("expected ResolveApproval")
    };
    assert_eq!(decision, ApprovalDecision::Approve);
    sup.resolve_approval(&id, decision).await.unwrap();
    assert!(engine.pending_approvals().unwrap().is_empty());

    // pause/resume through the reducer's actions
    assert!(matches!(
        handle_key(&mut app, key(KeyCode::Char('p')))[..],
        [Action::Pause]
    ));
    engine.pause().unwrap();
    app.apply_snapshot(engine.snapshot().unwrap());
    assert!(render_text(&app, 120, 30).contains("PAUSED"));
    engine.resume().unwrap();

    sup.shutdown().await;
    runner.abort();
}
