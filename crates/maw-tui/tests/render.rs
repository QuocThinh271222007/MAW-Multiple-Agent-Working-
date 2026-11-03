//! Render tests against ratatui's in-memory backend: content assertions at several sizes and
//! "never panics" sweeps including absurdly small terminals.

use chrono::Utc;
use maw_core::*;
use maw_orchestrator::{LineKind, TranscriptLine};
use maw_tui::app::{handle_key, Dialog, Overlay, PaletteState, SearchState, View};
use maw_tui::input::TextInput;
use maw_tui::testutil::*;
use maw_tui::{App, Theme};
use ratatui::style::Color;
use serde_json::json;

fn app() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.now = Utc::now();
    a
}

fn at(a: &mut App, v: View, w: u16, h: u16) -> String {
    a.view = v;
    a.size = (w, h);
    render_text(a, w, h)
}

fn line(kind: LineKind, text: &str) -> TranscriptLine {
    TranscriptLine {
        at: Utc::now(),
        kind,
        text: text.into(),
        open: false,
    }
}

#[test]
fn dashboard_shows_goal_agents_tasks_and_progress_at_common_sizes() {
    let mut a = app();
    let pct = format!("{:.0}%", a.snap.as_ref().unwrap().progress.verified_pct);
    for (w, h) in [(80, 24), (100, 30), (120, 36), (200, 50)] {
        let t = at(&mut a, View::Dashboard, w, h);
        assert!(t.contains("MAW"), "{w}x{h}: header\n{t}");
        assert!(t.contains("MyApp"), "{w}x{h}: project name");
        assert!(t.contains("authentication"), "{w}x{h}: goal text\n{t}");
        assert!(t.contains(&pct), "{w}x{h}: verified progress {pct}\n{t}");
        assert!(t.contains("claude-lead"), "{w}x{h}: leader name\n{t}");
        assert!(t.contains("codex-backend"), "{w}x{h}: worker name");
        assert!(
            t.to_uppercase().contains("WORKING"),
            "{w}x{h}: working state"
        );
        assert!(t.contains("T-3"), "{w}x{h}: task key\n{t}");
        assert!(t.contains("Blockers: 1"), "{w}x{h}: blocker count");
        assert!(t.contains("Review pending: 1"), "{w}x{h}: review pending");
    }
}

#[test]
fn large_dashboard_includes_event_stream_and_agent_cards() {
    let mut a = app();
    a.transcripts.insert(
        a.agents()[1].id.clone(),
        vec![
            line(LineKind::Tool, "▶ edit auth.rs"),
            line(LineKind::Command, "$ cargo test"),
            line(LineKind::Output, "18 passed"),
        ],
    );
    let t = at(&mut a, View::Dashboard, 160, 44);
    assert!(t.contains("EVENT STREAM"));
    assert!(
        t.contains("assigned T-4"),
        "assignment shown in stream\n{t}"
    );
    assert!(
        t.contains("cargo test"),
        "agent card shows recent output/current command"
    );
    assert!(t.contains("APPROVAL"), "waiting-approval badge");
    assert!(t.contains("[LEADER]") && t.contains("[WORKER]"));
}

#[test]
fn paused_scheduler_is_visible_in_header() {
    let mut a = app();
    a.snap.as_mut().unwrap().paused = true;
    let t = at(&mut a, View::Dashboard, 120, 30);
    assert!(t.contains("PAUSED"));
}

#[test]
fn every_view_renders_at_every_size_without_panicking() {
    let views = [
        View::Dashboard,
        View::Agents,
        View::AgentDetail,
        View::Tasks,
        View::Goal,
        View::Flow,
        View::Approvals,
        View::Logs,
        View::Config,
    ];
    let sizes = [
        (0, 0),
        (1, 1),
        (5, 2),
        (10, 3),
        (19, 4),
        (20, 5),
        (21, 6),
        (30, 8),
        (40, 10),
        (60, 14),
        (80, 24),
        (99, 17),
        (101, 17),
        (120, 36),
        (151, 41),
        (200, 60),
        (300, 80),
    ];
    let mut a = app();
    a.transcripts.insert(
        a.agents()[0].id.clone(),
        (0..50)
            .map(|i| {
                line(
                    LineKind::Assistant,
                    &format!("message number {i} with some wide text 日本語 ✓"),
                )
            })
            .collect(),
    );
    a.search = Some(SearchState {
        input: TextInput::with_text("number"),
        editing: false,
        current: 0,
    });
    for v in views {
        for (w, h) in sizes {
            let t = at(&mut a, v, w, h);
            if w >= 20 && h >= 5 {
                assert!(!t.trim().is_empty(), "{v:?} at {w}x{h} rendered nothing");
            }
        }
    }
}

#[test]
fn empty_app_renders_gracefully() {
    let mut a = App::new(Theme::new(false));
    for v in [
        View::Dashboard,
        View::Agents,
        View::AgentDetail,
        View::Tasks,
        View::Goal,
        View::Flow,
        View::Approvals,
        View::Logs,
        View::Config,
    ] {
        for (w, h) in [(20, 5), (80, 24), (140, 40)] {
            let _ = at(&mut a, v, w, h);
        }
    }
    let t = at(&mut a, View::Dashboard, 100, 30);
    assert!(t.contains("No agents yet") || t.contains("loading") || t.contains("No objective"));
}

#[test]
fn too_small_terminal_shows_a_message() {
    let mut a = app();
    let t = at(&mut a, View::Dashboard, 12, 3);
    let flat: String = t.split_whitespace().collect();
    assert!(flat.contains("toosmall"), "{t}");
}

#[test]
fn overlays_render_at_small_and_large_sizes() {
    let mut a = app();
    for (w, h) in [(20, 5), (40, 12), (80, 24), (140, 40)] {
        a.overlay = Overlay::Help;
        let t = at(&mut a, View::Dashboard, w, h);
        if w >= 80 {
            assert!(t.contains("Command palette") || t.contains("HELP"), "{t}");
        }
        a.overlay = Overlay::Palette(PaletteState {
            input: TextInput::with_text("pause"),
            selected: 0,
        });
        let t = at(&mut a, View::Dashboard, w, h);
        if w >= 80 {
            assert!(t.contains("scheduling pause"), "{t}");
        }
        a.overlay = Overlay::Dialog(Dialog::add_agent());
        let t = at(&mut a, View::Dashboard, w, h);
        if w >= 80 && h >= 24 {
            assert!(
                t.contains("Add agent") && t.contains("Provider") && t.contains("claude"),
                "{t}"
            );
        }
        a.overlay = Overlay::None;
    }
}

#[test]
fn task_board_columns_and_detail_pane() {
    let mut a = app();
    let t = at(&mut a, View::Tasks, 170, 44);
    for col in [
        "BACKLOG (1)",
        "RUNNING (2)",
        "BLOCKED (1)",
        "IN REVIEW (1)",
        "DONE (2)",
    ] {
        assert!(t.contains(col), "missing column {col}\n{t}");
    }
    assert!(t.contains("TASK DETAIL"));
    assert!(t.contains("Evidence"));
    // medium: grouped columns
    let t = at(&mut a, View::Tasks, 120, 36);
    assert!(t.contains("QUEUED") && t.contains("REVIEW"), "{t}");
    // narrow: list mode with status tags
    let t = at(&mut a, View::Tasks, 80, 24);
    assert!(
        t.contains("TASKS (7)") && t.contains("RUN") && t.contains("T-7"),
        "{t}"
    );
    // selecting the running task shows honest, unverified progress
    a.task_sel = a
        .board_order()
        .iter()
        .position(|&i| a.tasks()[i].key == "T-3")
        .unwrap();
    let t = at(&mut a, View::Tasks, 170, 44);
    assert!(t.contains("40% agent-reported (unverified)"), "{t}");
    assert!(t.contains("none yet"), "no-evidence warning\n{t}");
}

#[test]
fn blocked_task_detail_shows_reason_and_dependencies() {
    let mut a = app();
    a.task_sel = a
        .board_order()
        .iter()
        .position(|&i| a.tasks()[i].key == "T-6")
        .unwrap();
    let t = at(&mut a, View::Tasks, 170, 44);
    assert!(t.contains("Depends on") && t.contains("T-3 RUNNING"), "{t}");
    a.task_sel = a
        .board_order()
        .iter()
        .position(|&i| a.tasks()[i].key == "T-7")
        .unwrap();
    let t = at(&mut a, View::Tasks, 170, 44);
    assert!(t.contains("needs a decision"), "{t}");
}

#[test]
fn goal_view_separates_verified_from_reported_progress() {
    let mut a = app();
    let t = at(&mut a, View::Goal, 120, 50);
    assert!(t.contains("OBJECTIVE") && t.contains("SUCCESS CRITERIA"));
    assert!(t.contains("Verified complete") && t.contains("counted"));
    assert!(
        t.contains("Agent-reported (unverified)") && t.contains("NOT counted"),
        "{t}"
    );
    assert!(t.contains("Blocked") && t.contains("Failed") && t.contains("Unknown"));
    assert!(
        t.contains("[x] Foundation"),
        "completed milestone derived from task state\n{t}"
    );
    assert!(
        t.contains("[>] Implementation"),
        "in-progress milestone\n{t}"
    );
    assert!(t.contains("[ ] Integration"));
    assert!(t.contains("verify T-5"), "next actions");
}

#[test]
fn goal_view_lists_revision_history() {
    let mut a = app();
    a.goal_history = vec![
        GoalRevision {
            goal_id: "g".into(),
            revision: 1,
            text: "x".into(),
            success_criteria: vec![],
            reason: "initial objective".into(),
            author: "user".into(),
            created_at: Utc::now(),
        },
        GoalRevision {
            goal_id: "g".into(),
            revision: 2,
            text: "y".into(),
            success_criteria: vec![],
            reason: "pivot to oauth".into(),
            author: "user".into(),
            created_at: Utc::now(),
        },
    ];
    let t = at(&mut a, View::Goal, 120, 60);
    assert!(t.contains("goal r2") && t.contains("pivot to oauth"), "{t}");
}

#[test]
fn flow_view_graph_is_derived_from_events() {
    let mut a = app();
    let t = at(&mut a, View::Flow, 140, 60);
    assert!(t.contains("MAW MCP / Orchestrator"), "{t}");
    assert!(
        t.contains("claude-lead") && t.contains("codex-backend") && t.contains("claude-review")
    );
    // edges from the synthetic events
    assert!(t.contains("CREATE TASKS"), "{t}");
    assert!(t.contains("ASSIGN"), "{t}");
    assert!(t.contains("RESULT"), "task.submitted edge\n{t}");
    assert!(t.contains("FEEDBACK"), "review loop edge\n{t}");
    assert!(t.contains("MESSAGE"), "{t}");
    assert!(
        t.contains("TASK DEPENDENCIES") && t.contains("WAITING"),
        "blocked dependency edges\n{t}"
    );
}

#[test]
fn flow_view_without_events_has_no_fake_edges() {
    let mut a = app();
    a.snap.as_mut().unwrap().recent_events.clear();
    let t = at(&mut a, View::Flow, 140, 60);
    assert!(t.contains("no coordination events yet"), "{t}");
    assert!(!t.contains("RESULT ×"), "no static arrows: {t}");
}

#[test]
fn flow_view_falls_back_to_edge_list_when_narrow() {
    let mut a = app();
    // 60 columns still fit three boxes side by side
    let t = at(&mut a, View::Flow, 60, 30);
    assert!(t.contains("MAW MCP / Orchestrator"), "{t}");
    // 40 columns cannot: only the edge list and timeline are shown
    let t = at(&mut a, View::Flow, 40, 30);
    assert!(t.contains("EDGES"), "{t}");
    assert!(t.contains("edge list"), "{t}");
    assert!(
        !t.contains("MAW MCP / Orchestrator"),
        "no diagram at 40 cols\n{t}"
    );
    assert!(t.contains("TIMELINE"), "{t}");
    // list mode explicitly requested on a large terminal
    a.flow_list_mode = true;
    let t = at(&mut a, View::Flow, 140, 60);
    assert!(t.contains("list mode") && t.contains("EDGES"));
}

#[test]
fn approval_center_lists_pending_requests_with_details() {
    let mut a = app();
    let t = at(&mut a, View::Approvals, 120, 30);
    assert!(
        t.contains("APPROVAL CENTER") && t.contains("1 pending"),
        "{t}"
    );
    assert!(t.contains("codex-tests") && t.contains("cargo test"), "{t}");
    assert!(t.contains("reason: verify the implementation"), "{t}");
    assert!(t.contains("targets: /work/tests"), "{t}");
}

#[test]
fn logs_view_shows_diagnostics_only() {
    let mut a = app();
    let w1 = a.agents()[1].id.clone();
    let snap = a.snap.as_mut().unwrap();
    snap.recent_events.push(event(
        20,
        EventKind::ProviderError,
        Some(&w1),
        json!({ "error": "connection reset by peer" }),
    ));
    snap.recent_events.push(event(
        21,
        EventKind::ProviderRateLimited,
        Some(&w1),
        json!({ "detail": "429 slow down" }),
    ));
    snap.recent_events.push(event(
        22,
        EventKind::TaskProgress,
        Some(&w1),
        json!({ "by_id": w1.0, "percent": 3, "task_key": "T-3" }),
    ));
    snap.agents[1].last_error = Some("connection reset by peer".into());
    let t = at(&mut a, View::Logs, 120, 30);
    assert!(
        t.contains("provider error: connection reset by peer"),
        "{t}"
    );
    assert!(t.contains("rate limited: 429 slow down"), "{t}");
    assert!(
        !t.contains("T-3 3%"),
        "progress chatter is not a diagnostic"
    );
}

#[test]
fn config_view_summarises_settings_and_agents() {
    let mut a = app();
    let t = at(&mut a, View::Config, 120, 40);
    assert!(t.contains("Max concurrency") && t.contains("Task lease") && t.contains("Auto-assign"));
    assert!(t.contains("perm=balanced") && t.contains("codex-backend"));
    assert!(t.contains("/work/myapp"));
}

#[test]
fn agent_grid_cards_adapt_to_space() {
    let mut a = app();
    let t = at(&mut a, View::Agents, 140, 40);
    assert!(t.contains("claude-lead") && t.contains("codex-tests"));
    assert!(t.contains("[REVIEWER]"));
    // compact rows when cards would not fit
    let t = at(&mut a, View::Agents, 80, 12);
    assert!(
        t.contains("1 ") && t.contains("THINKING") && t.contains("WAITING_APPROVAL"),
        "{t}"
    );
}

#[test]
fn agent_detail_shows_transcript_search_highlight_and_status() {
    let mut a = app();
    let id = a.agents()[0].id.clone();
    let mut lines: Vec<TranscriptLine> = (0..80)
        .map(|i| line(LineKind::Assistant, &format!("working on step {i}")))
        .collect();
    lines.push(line(LineKind::Tool, "✓ maw_task_create"));
    lines.push(line(LineKind::Error, "something failed"));
    a.transcripts.insert(id, lines);
    let t = at(&mut a, View::AgentDetail, 120, 40);
    assert!(t.contains("claude-lead") && t.contains("[LEADER]"));
    assert!(
        t.contains("following")
            && t.contains("maw_task_create")
            && t.contains("! something failed"),
        "{t}"
    );
    assert!(
        t.contains("usage: unavailable from provider"),
        "usage is never fabricated\n{t}"
    );
    // scrolled up: shows an earlier line and the scroll status
    a.detail_scroll = 60;
    let t = at(&mut a, View::AgentDetail, 120, 40);
    assert!(t.contains("up") && t.contains("working on step 1"), "{t}");
    // search bar and match counter
    a.detail_scroll = 0;
    a.search = Some(SearchState {
        input: TextInput::with_text("step 7"),
        editing: false,
        current: 0,
    });
    let t = at(&mut a, View::AgentDetail, 120, 40);
    assert!(t.contains("/step 7") && t.contains("matches"), "{t}");
}

#[test]
fn provider_reported_usage_is_labelled() {
    let mut a = app();
    a.snap.as_mut().unwrap().agents[0].usage = UsageMetrics {
        input_tokens: Some(10),
        output_tokens: Some(5),
        total_tokens: Some(15),
        cost_usd: None,
        reported_by_provider: true,
    };
    let t = at(&mut a, View::AgentDetail, 140, 40);
    assert!(
        t.contains("provider-reported") && t.contains("total 15"),
        "{t}"
    );
}

#[test]
fn toast_replaces_footer_hint() {
    let mut a = app();
    a.set_toast("approval approved", maw_tui::app::Level::Ok);
    a.now = a.toast.as_ref().unwrap().at;
    let t = at(&mut a, View::Dashboard, 120, 30);
    assert!(t.contains("approval approved"));
}

#[test]
fn monochrome_theme_emits_no_colors() {
    let mut a = app();
    a.theme = Theme::new(true);
    for v in [
        View::Dashboard,
        View::Tasks,
        View::Goal,
        View::Flow,
        View::Approvals,
        View::Agents,
        View::Config,
    ] {
        a.view = v;
        a.size = (140, 44);
        let buf = render_buffer(&a, 140, 44);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let c = &buf[(x, y)];
                assert!(
                    c.fg == Color::Reset && c.bg == Color::Reset,
                    "{v:?} cell ({x},{y}) '{}' has color {:?}/{:?}",
                    c.symbol(),
                    c.fg,
                    c.bg
                );
            }
        }
    }
    // and state is still readable as text
    let t = at(&mut a, View::Dashboard, 120, 30);
    assert!(t.contains("WORKING"));
}

#[test]
fn color_theme_uses_color() {
    let mut a = app();
    a.size = (140, 44);
    let buf = render_buffer(&a, 140, 44);
    let colored = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .any(|(x, y)| buf[(x, y)].fg != Color::Reset);
    assert!(colored);
}

#[test]
fn key_driven_navigation_renders_consistently() {
    let mut a = app();
    a.size = (100, 30);
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for k in ['t', 'g', 'f', 'o', 'l', 'c', 's', 'd'] {
        handle_key(&mut a, KeyEvent::new(KeyCode::Char(k), KeyModifiers::NONE));
        let t = render_text(&a, 100, 30);
        assert!(
            t.contains(a.view.title()) || a.view == View::AgentDetail,
            "{:?}\n{t}",
            a.view
        );
    }
}

// ---- narrow title bar scrolls sideways instead of hiding the key list ----

fn wheel(kind: crossterm::event::MouseEventKind, row: u16) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: 5,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

#[test]
fn narrow_title_bar_can_be_scrolled_to_reveal_every_view_key() {
    use crossterm::event::MouseEventKind::{ScrollDown, ScrollUp};
    let mut a = app();
    let first = at(&mut a, View::Dashboard, 60, 24);
    let row0 = first.lines().next().unwrap().to_string();
    assert!(
        row0.contains('›'),
        "hidden content to the right is marked: {row0:?}"
    );
    assert!(!row0.contains('‹'), "nothing hidden on the left yet");
    assert!(
        !row0.contains("c:Config"),
        "the last key does not fit at 60 columns"
    );

    // wheel over the bar (row 0) scrolls it; keep going until it stops
    for _ in 0..40 {
        maw_tui::app::handle_mouse(&mut a, wheel(ScrollDown, 0));
    }
    let end = at(&mut a, View::Dashboard, 60, 24);
    let row0 = end.lines().next().unwrap().to_string();
    assert!(
        row0.contains('‹') && !row0.contains('›'),
        "scrolled to the end: {row0:?}"
    );
    assert!(
        row0.contains("Config"),
        "the last key is now readable: {row0:?}"
    );
    assert!(
        row0.contains("dir:"),
        "working folder is shown at the end of the bar: {row0:?}"
    );

    // and back
    for _ in 0..40 {
        maw_tui::app::handle_mouse(&mut a, wheel(ScrollUp, 0));
    }
    let back = at(&mut a, View::Dashboard, 60, 24);
    assert_eq!(back.lines().next().unwrap(), first.lines().next().unwrap());
}

#[test]
fn wheel_in_the_body_still_scrolls_the_content_not_the_bar() {
    use crossterm::event::MouseEventKind::ScrollDown;
    let mut a = app();
    let _ = at(&mut a, View::Dashboard, 60, 24);
    maw_tui::app::handle_mouse(&mut a, wheel(ScrollDown, 10));
    assert_eq!(a.header_scroll, 0);
    assert_eq!(a.footer_scroll, 0);
}

#[test]
fn hint_bar_scrolls_too_and_wide_terminals_need_no_scrolling() {
    use crossterm::event::MouseEventKind::ScrollDown;
    let mut a = app();
    let narrow = at(&mut a, View::Dashboard, 50, 24);
    assert!(narrow.lines().last().unwrap().contains('›'));
    for _ in 0..40 {
        maw_tui::app::handle_mouse(&mut a, wheel(ScrollDown, 23));
    }
    let moved = at(&mut a, View::Dashboard, 50, 24);
    let last = moved.lines().last().unwrap().to_string();
    assert!(
        last.contains("quit"),
        "end of the hint list reached: {last:?}"
    );
    assert_eq!(a.header_scroll, 0, "only the footer moved");
    // plenty of room: no markers at all
    let _ = at(&mut a, View::Dashboard, 260, 40);
    a.footer_scroll = 0;
    a.header_scroll = 0;
    let wide = at(&mut a, View::Dashboard, 260, 40);
    assert!(
        !wide.lines().next().unwrap().contains('›') && !wide.lines().last().unwrap().contains('›')
    );
}

// ---- `!` shell, model & effort, permissions ----

#[test]
fn bang_opens_the_shell_dialog_and_submits_a_run_action() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app();
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    handle_key(&mut a, key(KeyCode::Char('!')));
    assert!(matches!(&a.overlay, Overlay::Dialog(d) if d.kind == maw_tui::app::DialogKind::Shell));
    for c in "git status".chars() {
        handle_key(&mut a, key(KeyCode::Char(c)));
    }
    let actions = handle_key(&mut a, key(KeyCode::Enter));
    assert!(
        matches!(actions.as_slice(), [maw_tui::app::Action::RunShell(c)] if c == "git status"),
        "{actions:?}"
    );
    // an empty command is refused, not run
    handle_key(&mut a, key(KeyCode::Char('!')));
    let actions = handle_key(&mut a, key(KeyCode::Enter));
    assert!(actions.is_empty());
}

#[test]
fn model_dialog_offers_the_catalog_and_efforts_and_a_typed_id_wins() {
    use maw_core::provider::ModelChoice;
    use maw_tui::app::Action;
    let a = app();
    let agent = a
        .agents()
        .iter()
        .find(|x| x.provider == ProviderKind::Codex)
        .unwrap()
        .clone();
    let models = vec![
        ModelChoice {
            id: "gpt-6.1-sol".into(),
            display_name: "Sol".into(),
            description: String::new(),
            is_default: true,
            source: "provider".into(),
            reasoning_efforts: vec!["low".into(), "high".into(), "ultra".into()],
        },
        ModelChoice {
            id: "gpt-6-luna".into(),
            display_name: "Luna".into(),
            description: String::new(),
            is_default: false,
            source: "provider".into(),
            reasoning_efforts: vec!["low".into()],
        },
    ];
    let mut d = Dialog::agent_model(&agent, &models);
    assert_eq!(
        d.fields[0].choices,
        vec!["default", "gpt-6.1-sol", "gpt-6-luna"]
    );
    assert_eq!(
        d.fields[2].choices,
        vec!["default", "low", "high", "ultra"],
        "efforts come from the default model"
    );
    // pick a model and an effort
    d.fields[0].choice = 2;
    d.fields[2].choice = 2;
    match d.submit().unwrap() {
        Action::SetAgentModel { model, effort, .. } => assert_eq!(
            (model.as_deref(), effort.as_deref()),
            (Some("gpt-6-luna"), Some("high"))
        ),
        o => panic!("{o:?}"),
    }
    // a typed id overrides the selector; `default` clears both
    d.fields[1].input.set("my-custom-model");
    match d.submit().unwrap() {
        Action::SetAgentModel { model, .. } => {
            assert_eq!(model.as_deref(), Some("my-custom-model"))
        }
        o => panic!("{o:?}"),
    }
    d.fields[1].input.set("");
    d.fields[0].choice = 0;
    d.fields[2].choice = 0;
    match d.submit().unwrap() {
        Action::SetAgentModel { model, effort, .. } => assert_eq!((model, effort), (None, None)),
        o => panic!("{o:?}"),
    }
    // it renders without panicking at small sizes
    let mut app2 = App::new(Theme::new(false));
    app2.apply_snapshot(sample_snapshot());
    app2.overlay = Overlay::Dialog(d);
    for (w, h) in [(80, 24), (40, 12), (20, 5)] {
        let _ = render_text(&app2, w, h);
    }
}

#[test]
fn permission_dialog_maps_to_an_action() {
    use maw_tui::app::Action;
    let a = app();
    let agent = a.agents()[1].clone();
    let mut d = Dialog::agent_permission(&agent);
    d.fields[0].choice = 2;
    assert!(matches!(
        d.submit().unwrap(),
        Action::SetAgentPermission {
            permission: PermissionProfile::Autonomous,
            ..
        }
    ));
}

#[test]
fn output_overlay_renders_and_scrolls() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use maw_tui::app::OutputState;
    let mut a = app();
    a.overlay = Overlay::Output(OutputState {
        title: "$ ls".into(),
        lines: (0..100).map(|i| format!("line {i}")).collect(),
        scroll: 0,
    });
    let t = render_text(&a, 100, 30);
    assert!(t.contains("line 0") && t.contains("$ ls"));
    for _ in 0..5 {
        handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    let t = render_text(&a, 100, 30);
    assert!(!t.contains("line 0\n") && t.contains("line 5"));
    handle_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(a.overlay, Overlay::None));
}
