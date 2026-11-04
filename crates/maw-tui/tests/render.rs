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
        row0.contains("[d:Dashboard]") && row0.contains('%'),
        "at 60 columns the keys are compact but all there: {row0:?}"
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
fn alt_arrows_scroll_the_title_bar_and_help_says_whether_the_mouse_reaches_maw() {
    use crossterm::event::MouseEventKind::ScrollDown;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail); // the message box has focus: letters are text
    let _ = at(&mut a, View::AgentDetail, 60, 24);
    let alt = |c| KeyEvent::new(c, KeyModifiers::ALT);
    for _ in 0..40 {
        handle_key(&mut a, alt(KeyCode::Right));
    }
    assert!(a.header_scroll > 0, "Alt+Right scrolled the bar");
    let end = a.header_scroll;
    handle_key(&mut a, alt(KeyCode::Left));
    assert!(a.header_scroll < end);
    assert_eq!(a.composer.value(), "", "nothing was typed");

    // the help overlay reports whether any mouse event arrived
    handle_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    a.overlay = Overlay::Help;
    assert!(render_text(&a, 120, 40).contains("none received"));
    maw_tui::app::handle_mouse(&mut a, wheel(ScrollDown, 10));
    assert!(render_text(&a, 120, 40).contains("1 mouse events received"));
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

// ---- add-agent: model and effort are pick-lists that follow provider and model ----

fn catalog() -> std::collections::HashMap<ProviderKind, Vec<maw_core::provider::ModelChoice>> {
    use maw_core::provider::ModelChoice;
    let m = |id: &str, def: bool, eff: &[&str], src: &str| ModelChoice {
        id: id.into(),
        display_name: id.into(),
        description: String::new(),
        is_default: def,
        source: src.into(),
        reasoning_efforts: eff.iter().map(|s| s.to_string()).collect(),
    };
    std::collections::HashMap::from([
        (
            ProviderKind::Codex,
            vec![
                m("gpt-6.1-sol", true, &["low", "high", "ultra"], "provider"),
                m("gpt-6-luna", false, &["low"], "provider"),
            ],
        ),
        (
            ProviderKind::Claude,
            vec![
                m("sonnet", false, &[], "catalog"),
                m("opus", false, &[], "catalog"),
            ],
        ),
    ])
}

#[test]
fn add_agent_dialog_offers_only_models_of_the_chosen_provider() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use maw_tui::app::Action;
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut a = app();
    a.models = catalog();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    // starts on codex: its models and the efforts of its default model
    match &a.overlay {
        Overlay::Dialog(d) => {
            assert_eq!(
                d.fields[3].choices,
                vec!["default", "gpt-6.1-sol", "gpt-6-luna"]
            );
            assert_eq!(d.fields[4].choices, vec!["default", "low", "high", "ultra"]);
        }
        _ => unreachable!(),
    }
    for c in "w1".chars() {
        handle_key(&mut a, key(KeyCode::Char(c)));
    }
    handle_key(&mut a, key(KeyCode::Tab)); // provider
    handle_key(&mut a, key(KeyCode::Left)); // codex -> claude
    match &a.overlay {
        Overlay::Dialog(d) => {
            assert_eq!(
                d.fields[3].choices,
                vec!["default", "sonnet", "opus"],
                "Codex ids are gone"
            );
            assert_eq!(d.fields[4].choices[0], "default");
            assert!(d.fields[4].choices.contains(&"max".to_string()));
        }
        _ => unreachable!(),
    }
    handle_key(&mut a, key(KeyCode::Tab)); // role
    handle_key(&mut a, key(KeyCode::Tab)); // model
    handle_key(&mut a, key(KeyCode::Right)); // default -> sonnet
    handle_key(&mut a, key(KeyCode::Tab)); // effort
    handle_key(&mut a, key(KeyCode::Right)); // default -> low
    handle_key(&mut a, key(KeyCode::Tab)); // permissions
    let acts = handle_key(&mut a, key(KeyCode::Enter));
    match acts.as_slice() {
        [Action::AddAgent {
            provider,
            model,
            effort,
            ..
        }] => {
            assert_eq!(*provider, ProviderKind::Claude);
            assert_eq!(model.as_deref(), Some("sonnet"));
            assert_eq!(effort.as_deref(), Some("low"));
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn effort_choices_follow_the_selected_model() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut a = app();
    a.models = catalog();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    handle_key(&mut a, key(KeyCode::Tab)); // provider (codex)
    handle_key(&mut a, key(KeyCode::Tab)); // role
    handle_key(&mut a, key(KeyCode::Tab)); // model
    handle_key(&mut a, key(KeyCode::Right)); // gpt-6.1-sol
    handle_key(&mut a, key(KeyCode::Right)); // gpt-6-luna: only supports low
    match &a.overlay {
        Overlay::Dialog(d) => assert_eq!(d.fields[4].choices, vec!["default", "low"]),
        _ => unreachable!(),
    }
}

#[test]
fn add_agent_dialog_renders_with_and_without_a_catalog() {
    let mut a = app();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    let t = render_text(&a, 100, 30);
    assert!(t.contains("Model") && t.contains("Effort"), "{t}");
    a.models = catalog();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    for (w, h) in [(100, 30), (60, 20), (30, 8)] {
        let _ = render_text(&a, w, h);
    }
}

// ---- long model lists are a scrollable vertical list, nothing is clipped away ----

fn many_models() -> std::collections::HashMap<ProviderKind, Vec<maw_core::provider::ModelChoice>> {
    use maw_core::provider::ModelChoice;
    let ids = [
        "gpt-6.1-sol",
        "gpt-6-astra",
        "gpt-6.1-terra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ];
    let list = ids
        .iter()
        .enumerate()
        .map(|(i, id)| ModelChoice {
            id: id.to_string(),
            display_name: id.to_uppercase(),
            description: format!("description of model number {i}"),
            is_default: i == 0,
            source: "provider".into(),
            reasoning_efforts: vec!["low".into(), "high".into()],
        })
        .collect();
    std::collections::HashMap::from([(ProviderKind::Codex, list)])
}

#[test]
fn every_codex_model_is_visible_in_a_tall_terminal() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut a = app();
    a.models = many_models();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    for _ in 0..3 {
        handle_key(&mut a, key(KeyCode::Tab)); // name -> provider -> role -> model
    }
    let t = render_text(&a, 110, 44);
    for id in [
        "gpt-6.1-sol",
        "gpt-6-astra",
        "gpt-6.1-terra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        assert!(t.contains(id), "{id} should be listed:\n{t}");
    }
    assert!(
        t.contains("description of model number 3"),
        "descriptions are shown beside the ids"
    );
    assert!(t.contains("(1/9)"), "position counter: {t}");
}

#[test]
fn a_short_terminal_scrolls_the_list_and_arrows_pick_the_option() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use maw_tui::app::Action;
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut a = app();
    a.models = many_models();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    for c in "w".chars() {
        handle_key(&mut a, key(KeyCode::Char(c)));
    }
    for _ in 0..3 {
        handle_key(&mut a, key(KeyCode::Tab));
    }
    let first = render_text(&a, 90, 22);
    assert!(
        !first.contains("gpt-5.6-luna"),
        "the last model does not fit yet at 22 rows"
    );
    // End jumps to the last option; the window follows it and shows a ▲ marker
    handle_key(&mut a, key(KeyCode::End));
    let last = render_text(&a, 90, 22);
    assert!(
        last.contains("gpt-5.6-luna") && last.contains("(9/9)"),
        "{last}"
    );
    assert!(last.contains('▲'), "more above: {last}");
    // Up/Down move through options (not between fields)
    handle_key(&mut a, key(KeyCode::Up));
    match &a.overlay {
        Overlay::Dialog(d) => {
            assert_eq!(d.focus, 3, "still on the Model field");
            assert_eq!(d.fields[3].value(), "gpt-5.6-terra");
        }
        _ => unreachable!(),
    }
    // Tab leaves the list; Enter through to submit gives a typed action with that model
    handle_key(&mut a, key(KeyCode::Tab)); // effort
    handle_key(&mut a, key(KeyCode::Tab)); // permissions
    let acts = handle_key(&mut a, key(KeyCode::Enter));
    assert!(
        matches!(acts.as_slice(), [Action::AddAgent { model: Some(m), .. }] if m == "gpt-5.6-terra"),
        "{acts:?}"
    );
}

#[test]
fn a_dialog_opened_before_the_catalog_arrives_gets_filled_in() {
    let mut a = app();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    match &a.overlay {
        Overlay::Dialog(d) => assert_eq!(d.fields[3].choices, vec!["default"], "nothing yet"),
        _ => unreachable!(),
    }
    a.models = many_models();
    maw_tui::app::refresh_open_dialog(&mut a);
    match &a.overlay {
        Overlay::Dialog(d) => assert_eq!(d.fields[3].choices.len(), 9, "default + 8 Codex models"),
        _ => unreachable!(),
    }
}

// ---- long pick-lists are vertical and scrollable, not one clipped line ----

fn codex_models() -> Vec<maw_core::provider::ModelChoice> {
    use maw_core::provider::ModelChoice;
    [
        "gpt-6.1-sol",
        "gpt-6-astra",
        "gpt-6.1-terra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ]
    .iter()
    .enumerate()
    .map(|(i, id)| ModelChoice {
        id: id.to_string(),
        display_name: id.to_uppercase(),
        description: format!("description of model number {i}"),
        is_default: i == 0,
        source: "provider".into(),
        reasoning_efforts: vec!["low".into(), "high".into()],
    })
    .collect()
}

fn app_with_codex_catalog() -> App {
    let mut a = app();
    a.models.insert(ProviderKind::Codex, codex_models());
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    a
}

fn focus_model_field(a: &mut App) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for _ in 0..3 {
        // name -> provider -> role -> model
        handle_key(a, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
}

#[test]
fn every_model_is_visible_in_the_vertical_list_with_its_description() {
    let mut a = app_with_codex_catalog();
    focus_model_field(&mut a);
    let t = render_text(&a, 110, 40);
    for id in [
        "default",
        "gpt-6.1-sol",
        "gpt-6-astra",
        "gpt-6.1-terra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        assert!(
            t.contains(id),
            "{id} must be visible (not clipped off the right edge)\n{t}"
        );
    }
    assert!(
        t.contains("description of model number 3"),
        "descriptions are shown beside the ids\n{t}"
    );
    assert!(t.contains("1/9"), "position counter\n{t}");
    assert!(t.contains("▶"), "the selected row is marked");
}

#[test]
fn arrows_move_through_the_list_and_tab_moves_on() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut a = app_with_codex_catalog();
    focus_model_field(&mut a);
    let model = |a: &App| match &a.overlay {
        Overlay::Dialog(d) => (d.focus, d.fields[3].value()),
        _ => unreachable!(),
    };
    assert_eq!(model(&a), (3, "default".to_string()));
    handle_key(&mut a, key(KeyCode::Down));
    handle_key(&mut a, key(KeyCode::Down));
    assert_eq!(
        model(&a),
        (3, "gpt-6-astra".to_string()),
        "↓ changes the choice, focus stays"
    );
    handle_key(&mut a, key(KeyCode::Up));
    assert_eq!(model(&a).1, "gpt-6.1-sol");
    handle_key(&mut a, key(KeyCode::End));
    assert_eq!(model(&a).1, "gpt-5.6-luna");
    handle_key(&mut a, key(KeyCode::Home));
    assert_eq!(model(&a).1, "default");
    handle_key(&mut a, key(KeyCode::PageDown));
    assert_eq!(
        model(&a).1,
        "gpt-6-luna",
        "PgDn jumps 5 options (index 0 -> 5)"
    );
    // the effort list followed the model while scrolling
    match &a.overlay {
        Overlay::Dialog(d) => assert_eq!(d.fields[4].choices, vec!["default", "low", "high"]),
        _ => unreachable!(),
    }
    handle_key(&mut a, key(KeyCode::Tab));
    assert_eq!(model(&a).0, 4, "Tab leaves the list for the next field");
}

#[test]
fn a_short_terminal_scrolls_the_window_and_keeps_the_selection_visible() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app_with_codex_catalog();
    focus_model_field(&mut a);
    handle_key(&mut a, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    for (w, h) in [(100, 24), (80, 20), (60, 16)] {
        let t = render_text(&a, w, h);
        assert!(
            t.contains("gpt-5.6-luna"),
            "{w}x{h}: the selected (last) model is visible\n{t}"
        );
        assert!(t.contains("9/9"), "{w}x{h}: counter\n{t}");
    }
    // and nothing panics on absurd sizes
    for (w, h) in [(20, 5), (30, 8), (200, 60)] {
        let _ = render_text(&a, w, h);
    }
}

#[test]
fn a_dialog_opened_before_the_catalog_arrives_fills_in_when_it_does() {
    let mut a = app();
    a.overlay = Overlay::Dialog(Dialog::add_agent_with(&a.models));
    match &a.overlay {
        Overlay::Dialog(d) => {
            assert_eq!(d.fields[3].choices, vec!["default"], "nothing loaded yet")
        }
        _ => unreachable!(),
    }
    a.models.insert(ProviderKind::Codex, codex_models());
    maw_tui::app::refresh_open_dialog(&mut a);
    match &a.overlay {
        Overlay::Dialog(d) => assert_eq!(d.fields[3].choices.len(), 9),
        _ => unreachable!(),
    }
}

// ---- token usage view ------------------------------------------------------------------------

fn usage_event(
    app: &App,
    agent_idx: usize,
    task_idx: Option<usize>,
    delta: u64,
    age_secs: i64,
) -> maw_core::Event {
    let snap = app.snap.as_ref().unwrap();
    let mut e = maw_core::Event::new(
        snap.project.id.clone(),
        EventKind::AgentUsage,
        json!({ "delta": { "total": delta, "input": delta, "output": 0, "cost_usd": 0.0 } }),
    );
    e.agent_id = Some(snap.agents[agent_idx].id.clone());
    e.task_id = task_idx.map(|i| snap.tasks[i].id.clone());
    e.timestamp = app.now - chrono::Duration::seconds(age_secs);
    e
}

fn app_with_usage() -> App {
    let mut a = app();
    {
        let s = a.snap.as_mut().unwrap();
        s.agents[0].usage = UsageMetrics {
            input_tokens: Some(9_000),
            output_tokens: Some(3_345),
            total_tokens: Some(12_345),
            cost_usd: Some(0.0421),
            reported_by_provider: true,
        };
        s.agents[1].usage = UsageMetrics {
            input_tokens: Some(40_000),
            output_tokens: Some(2_000),
            total_tokens: Some(42_000),
            cost_usd: None,
            reported_by_provider: true,
        };
        // agents[2], agents[3]: never reported
    }
    a.usage_events = vec![
        usage_event(&a, 1, Some(2), 800, 60),
        usage_event(&a, 1, Some(2), 1200, 400),
        usage_event(&a, 0, None, 300, 90),
    ];
    a
}

#[test]
fn tokens_view_shows_reported_usage_shares_rates_and_honest_notes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app_with_usage();
    // the `$` key opens it
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE),
    );
    assert_eq!(a.view, View::Tokens);
    let name0 = a.snap.as_ref().unwrap().agents[0].name.clone();
    let name1 = a.snap.as_ref().unwrap().agents[1].name.clone();
    let t = render_text(&a, 150, 40);
    assert!(t.contains("TOKEN USAGE"), "{t}");
    assert!(t.contains(&name0) && t.contains(&name1));
    assert!(t.contains("54.3k"), "project total 12,345 + 42,000: {t}");
    assert!(t.contains("12.3k") && t.contains("42.0k"), "{t}");
    assert!(
        t.contains("$0.0421"),
        "Claude's own cost estimate is shown with its label: {t}"
    );
    assert!(t.contains("never estimated"), "{t}");
    assert!(t.contains("2/4 agents reporting"), "{t}");
    assert!(
        t.contains("no usage reported yet"),
        "agents that never reported are not shown as zero: {t}"
    );
    assert!(
        t.contains("TOKENS BY TASK") && t.contains("2.0k"),
        "800 + 1200 attributed to one task: {t}"
    );
    assert!(
        t.contains("tokens outside any task") || t.contains("outside any task"),
        "{t}"
    );
    assert!(t.contains("█") || t.contains("░"), "share bars");
    assert!(t.contains("TOK/MIN"), "{t}");
    // header carries the running total on every view
    a.view = View::Dashboard;
    assert!(render_text(&a, 150, 40).contains("Tokens: 54.3k"));
    // palette entry exists and works
    assert!(maw_tui::app::COMMANDS
        .iter()
        .any(|c| c.name == "view tokens"));
}

#[test]
fn tokens_view_adapts_and_never_panics() {
    let mut a = app_with_usage();
    a.view = View::Tokens;
    for (w, h) in [
        (200, 60),
        (150, 40),
        (110, 30),
        (90, 24),
        (80, 24),
        (60, 16),
        (40, 10),
        (20, 5),
    ] {
        a.size = (w, h);
        let t = render_text(&a, w, h);
        if w >= 80 && h >= 20 {
            assert!(t.contains("TOKEN USAGE"), "{w}x{h}\n{t}");
        }
    }
    // no data at all
    let mut empty = App::new(Theme::new(false));
    empty.view = View::Tokens;
    let _ = render_text(&empty, 100, 30);
}

// ---- conversation filters --------------------------------------------------------------------

fn app_with_mixed_transcript() -> App {
    let mut a = app();
    let id = a.agents()[1].id.clone();
    a.transcripts.insert(
        id,
        vec![
            line(LineKind::User, "please add login"),
            line(LineKind::Thinking, "THINK-TEXT plan the change"),
            line(LineKind::Protocol, "▶ mcp__maw__maw_message_ack {id}"),
            line(LineKind::Tool, "▶ Read src/auth.rs"),
            line(LineKind::Command, "$ cargo test"),
            line(LineKind::Output, "OUTPUT-TEXT 18 passed"),
            line(LineKind::System, "SYSTEM-TEXT connected"),
            line(LineKind::Assistant, "ASSISTANT-TEXT done, tests pass"),
            line(LineKind::Error, "ERROR-TEXT something odd"),
        ],
    );
    a.selected_agent = 1;
    a.view = View::AgentDetail;
    a.composer_focus = false; // browse mode: the T / L / P filter keys
    a
}

#[test]
fn filters_hide_thinking_logs_and_protocol_independently_and_keep_the_conversation() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
    let mut a = app_with_mixed_transcript();
    let all = render_text(&a, 130, 40);
    for needle in [
        "please add login",
        "THINK-TEXT",
        "maw_message_ack",
        "Read src/auth.rs",
        "cargo test",
        "OUTPUT-TEXT",
        "SYSTEM-TEXT",
        "ASSISTANT-TEXT",
        "ERROR-TEXT",
    ] {
        assert!(
            all.contains(needle),
            "default shows everything: missing {needle}\n{all}"
        );
    }
    assert!(
        all.contains("T thinking ✓")
            && all.contains("L tool logs ✓")
            && all.contains("P MAW protocol ✓"),
        "{all}"
    );

    // P hides only the MAW protocol lines
    handle_key(&mut a, key('P'));
    let t = render_text(&a, 130, 40);
    assert!(
        !t.contains("maw_message_ack")
            && t.contains("Read src/auth.rs")
            && t.contains("THINK-TEXT"),
        "{t}"
    );
    assert!(
        t.contains("P MAW protocol ✗"),
        "the bar says what is hidden: {t}"
    );

    // T hides thinking
    handle_key(&mut a, key('T'));
    assert!(!render_text(&a, 130, 40).contains("THINK-TEXT"));

    // L hides tool calls, commands, output and system lines, but never messages or errors
    handle_key(&mut a, key('L'));
    let t = render_text(&a, 130, 40);
    for hidden in [
        "Read src/auth.rs",
        "cargo test",
        "OUTPUT-TEXT",
        "SYSTEM-TEXT",
    ] {
        assert!(!t.contains(hidden), "{hidden} should be hidden\n{t}");
    }
    for kept in ["please add login", "ASSISTANT-TEXT", "ERROR-TEXT"] {
        assert!(t.contains(kept), "{kept} must stay\n{t}");
    }

    // toggling back restores everything
    for c in ['P', 'T', 'L'] {
        handle_key(&mut a, key(c));
    }
    assert!(render_text(&a, 130, 40).contains("THINK-TEXT"));
}

#[test]
fn conversation_only_preset_keeps_thinking_and_show_all_restores_it() {
    use maw_tui::app::{run_cmd, Cmd};
    let mut a = app_with_mixed_transcript();
    run_cmd(&mut a, Cmd::ConversationOnly);
    let t = render_text(&a, 130, 40);
    assert!(
        t.contains("THINK-TEXT") && t.contains("ASSISTANT-TEXT") && t.contains("please add login")
    );
    assert!(!t.contains("maw_message_ack") && !t.contains("cargo test"));
    run_cmd(&mut a, Cmd::ShowAllLines);
    assert!(render_text(&a, 130, 40).contains("maw_message_ack"));
    for name in [
        "transcript thinking",
        "transcript tool logs",
        "transcript protocol",
        "transcript conversation only",
        "transcript show all",
    ] {
        assert!(
            maw_tui::app::COMMANDS.iter().any(|c| c.name == name),
            "{name}"
        );
    }
}

#[test]
fn filters_also_apply_to_search_and_copy_so_what_you_see_is_what_you_search() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app_with_mixed_transcript();
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
    );
    assert!(
        !a.full_transcript_text().contains("maw_message_ack"),
        "copy honours the filter"
    );
    assert!(a.full_transcript_text().contains("ASSISTANT-TEXT"));
    a.search = Some(maw_tui::app::SearchState {
        input: {
            let mut i = TextInput::new();
            i.set("maw_message_ack");
            i
        },
        editing: false,
        current: 0,
    });
    assert!(
        a.search_matches().is_empty(),
        "hidden lines are not search hits"
    );
}

#[test]
fn grid_cards_respect_the_filters_too() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app_with_mixed_transcript();
    a.view = View::Agents;
    assert!(render_text(&a, 150, 40).contains("maw_message_ack"));
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
    );
    assert!(!render_text(&a, 150, 40).contains("maw_message_ack"));
}

// ---- removing an agent ------------------------------------------------------------------------

#[test]
fn removing_an_agent_needs_explicit_confirmation_and_can_force() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use maw_tui::app::Action;
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut a = app();
    a.selected_agent = 1;
    let target = a.agents()[1].clone();
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT),
    );
    assert!(
        matches!(&a.overlay, Overlay::Dialog(d) if matches!(&d.kind, maw_tui::app::DialogKind::RemoveAgent { agent } if *agent == target.id))
    );
    let t = render_text(&a, 100, 30);
    assert!(
        t.contains(&target.name) && t.contains("Really remove it?"),
        "{t}"
    );

    // default answer is "no": Enter through the fields must NOT remove anything
    handle_key(&mut a, key(KeyCode::Enter));
    let acts = handle_key(&mut a, key(KeyCode::Enter));
    assert!(acts.is_empty(), "{acts:?}");
    match &a.overlay {
        Overlay::Dialog(d) => assert!(
            d.error.as_deref().unwrap_or("").contains("confirm"),
            "{:?}",
            d.error
        ),
        _ => panic!("dialog should stay open with an explanation"),
    }
    // say yes (focus back on the first field), safe mode: no force
    handle_key(&mut a, KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    handle_key(&mut a, key(KeyCode::Right));
    let acts = handle_key(&mut a, key(KeyCode::Enter));
    assert!(acts.is_empty(), "moves to the second field first");
    let acts = handle_key(&mut a, key(KeyCode::Enter));
    assert!(
        matches!(acts.as_slice(), [Action::RemoveAgent { agent, force: false }] if *agent == target.id),
        "{acts:?}"
    );
    // with "release its tasks": force
    let mut b = app();
    b.selected_agent = 1;
    handle_key(
        &mut b,
        KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT),
    );
    handle_key(&mut b, key(KeyCode::Right)); // yes
    handle_key(&mut b, key(KeyCode::Tab));
    handle_key(&mut b, key(KeyCode::Right)); // release
    let acts = handle_key(&mut b, key(KeyCode::Enter));
    assert!(
        matches!(acts.as_slice(), [Action::RemoveAgent { force: true, .. }]),
        "{acts:?}"
    );
    assert!(maw_tui::app::COMMANDS
        .iter()
        .any(|c| c.name == "agent remove"));
}

#[test]
fn the_mock_provider_is_offered_only_in_demo_projects() {
    use maw_tui::app::Dialog;
    let none = std::collections::HashMap::new();
    assert_eq!(
        Dialog::add_agent_with(&none).fields[1].choices,
        ["claude", "codex", "ollama"]
    );
    let demo = Dialog::add_agent_for(&none, true);
    assert!(demo.fields[1].choices.contains(&"mock".to_string()));
}

#[test]
fn arrows_leave_a_long_list_at_either_end_so_you_can_go_back_up_a_form() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use maw_tui::app::run_cmd;
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let focus = |a: &App| match &a.overlay {
        Overlay::Dialog(d) => d.focus,
        _ => panic!("no dialog"),
    };
    let mut a = app();
    run_cmd(&mut a, maw_tui::app::Cmd::AddAgent);
    for _ in 0..2 {
        handle_key(&mut a, key(KeyCode::Tab)); // Name -> Provider -> Role (a 4-option list)
    }
    assert_eq!(focus(&a), 2);
    handle_key(&mut a, key(KeyCode::Down)); // inside the list: picks the next role, stays on the field
    assert_eq!(focus(&a), 2);
    handle_key(&mut a, key(KeyCode::Up)); // back to the first role
    assert_eq!(focus(&a), 2);
    handle_key(&mut a, key(KeyCode::Up)); // at the top: leaves to the previous field
    assert_eq!(focus(&a), 1, "Up from the first item goes back up the form");
    handle_key(&mut a, key(KeyCode::Down)); // Provider is a short selector: Down moves on
    assert_eq!(focus(&a), 2);
    for _ in 0..3 {
        handle_key(&mut a, key(KeyCode::Down)); // to the last role
    }
    assert_eq!(focus(&a), 2);
    handle_key(&mut a, key(KeyCode::Down)); // past the last item: on to Model
    assert_eq!(focus(&a), 3);
    handle_key(&mut a, key(KeyCode::BackTab)); // Shift+Tab still works everywhere
    assert_eq!(focus(&a), 2);
}

// ---- performance view -------------------------------------------------------------------------

fn perf_history(with_gpu: bool) -> maw_core::perf::PerfHistory {
    use maw_core::perf::{LoadedModel, PerfHistory, PerfSample};
    let now = chrono::Utc::now();
    let mut h = PerfHistory::new(900);
    h.sources.system = "Windows host (read through PowerShell from WSL)".into();
    h.sources.gpu = if with_gpu {
        "nvidia-smi (whole GPU, including other programs)".into()
    } else {
        String::new()
    };
    for i in 0..120i64 {
        let k = i as f32;
        h.push(PerfSample {
            at: now - chrono::Duration::seconds((119 - i) * 2),
            cpu_pct: Some(20.0 + (k % 30.0)),
            ram_used_mb: Some(9000 + (i as u64) * 10),
            ram_total_mb: Some(12000),
            gpu_pct: with_gpu.then_some((k * 0.8).min(97.0)),
            vram_used_mb: with_gpu.then_some(2000 + (i as u64) * 30),
            vram_total_mb: with_gpu.then_some(6144),
            gpu_temp_c: with_gpu.then_some(61.0),
            gpu_power_w: with_gpu.then_some(55.0),
            decode_tps: (i > 40).then_some(18.0 + (k % 5.0)),
            prompt_tps: (i > 40).then_some(120.0),
            loaded: vec![LoadedModel {
                name: "qwen3:8b".into(),
                size_bytes: 5_200_000_000,
                vram_bytes: 3_900_000_000,
            }],
        });
    }
    h
}

#[test]
fn performance_view_charts_every_source_and_shows_the_gpu_ram_split() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app();
    a.perf = perf_history(true);
    a.now = chrono::Utc::now();
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('%'), KeyModifiers::SHIFT),
    );
    assert_eq!(a.view, View::Perf);
    let t = render_text(&a, 170, 50);
    for needle in [
        "PERFORMANCE",
        "last 5m",
        "GENERATION SPEED",
        "VRAM",
        "GPU LOAD",
        "RAM",
        "CPU",
        "MODELS IN MEMORY",
        "qwen3:8b",
        "75% GPU",
    ] {
        assert!(t.contains(needle), "missing {needle}\n{t}");
    }
    assert!(t.contains("part of a model is in system RAM"), "{t}");
    assert!(
        t.contains("Windows host") && t.contains("nvidia-smi"),
        "sources are named: {t}"
    );
    assert!(t.contains("tok/s") && t.contains("61°C"), "{t}");
    // w cycles the window
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
    );
    assert!(render_text(&a, 170, 50).contains("last 30m"));
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
    );
    assert!(render_text(&a, 170, 50).contains("last 60s"));
    assert!(maw_tui::app::COMMANDS
        .iter()
        .any(|c| c.name == "view performance"));
}

#[test]
fn performance_view_says_so_when_there_is_no_gpu_tool_or_no_data() {
    let mut a = app();
    a.view = View::Perf;
    a.now = chrono::Utc::now();
    assert!(render_text(&a, 120, 40).contains("Collecting samples"));
    a.perf = perf_history(false);
    let t = render_text(&a, 170, 50);
    assert!(t.contains("no GPU tool found"), "{t}");
    assert!(
        t.contains("no GPU load reading") || t.contains("(no data)"),
        "{t}"
    );
}

#[test]
fn performance_view_adapts_to_any_terminal_size_without_panicking() {
    let mut a = app();
    a.view = View::Perf;
    a.perf = perf_history(true);
    a.now = chrono::Utc::now();
    for (w, h) in [
        (200, 60),
        (150, 40),
        (110, 30),
        (100, 24),
        (80, 24),
        (60, 16),
        (40, 10),
        (20, 5),
        (10, 3),
    ] {
        a.size = (w, h);
        let t = render_text(&a, w, h);
        if w >= 100 && h >= 24 {
            assert!(t.contains("PERFORMANCE"), "{w}x{h}\n{t}");
        }
    }
    // the models line is always on screen, even in a short terminal
    let short = render_text(&a, 100, 24);
    assert!(
        short.contains("Models in memory")
            && short.contains("qwen3:8b")
            && short.contains("75% on GPU"),
        "{short}"
    );
}

#[test]
fn ollama_models_offer_effort_only_when_the_model_really_has_levels() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use maw_tui::app::{run_cmd, Cmd};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mk = |id: &str, e: &[&str]| maw_core::provider::ModelChoice {
        id: id.into(),
        display_name: id.into(),
        description: String::new(),
        is_default: false,
        source: "provider".into(),
        reasoning_efforts: e.iter().map(|s| s.to_string()).collect(),
    };
    let mut a = app();
    a.models.insert(
        ProviderKind::Ollama,
        vec![
            mk("qwen3:4b", &["none"]),
            mk("gpt-oss:20b", &["low", "medium", "high"]),
        ],
    );
    run_cmd(&mut a, Cmd::AddAgent);
    handle_key(&mut a, key(KeyCode::Tab)); // Provider
    handle_key(&mut a, key(KeyCode::Right)); // codex -> ollama
    let efforts = |a: &App| match &a.overlay {
        Overlay::Dialog(d) => (d.fields[4].choices.clone(), d.fields[4].hint.clone()),
        _ => panic!("no dialog"),
    };
    // no model chosen and two installed: nothing to choose yet
    let (e, hint) = efforts(&a);
    assert_eq!(e, ["default"], "no effort list for an undecided model");
    assert!(hint.contains("no effort setting"), "{hint}");
    handle_key(&mut a, key(KeyCode::Tab)); // Role
    handle_key(&mut a, key(KeyCode::Tab)); // Model (3 choices: a horizontal selector)
    handle_key(&mut a, key(KeyCode::Right)); // qwen3:4b
    let (e, hint) = efforts(&a);
    assert_eq!(e, ["default"], "qwen3 has no effort setting");
    assert!(hint.contains("no effort setting"));
    assert!(
        !render_text(&a, 120, 30).contains("none"),
        "the internal marker is never shown"
    );
    handle_key(&mut a, key(KeyCode::Right)); // gpt-oss:20b
    let (e, hint) = efforts(&a);
    assert_eq!(e, ["default", "low", "medium", "high"]);
    assert!(hint.is_empty(), "{hint}");
}

// ---- pickers (model / effort / permissions / download) -----------------------------------------

fn mk_model(
    id: &str,
    desc: &str,
    efforts: &[&str],
    default: bool,
) -> maw_core::provider::ModelChoice {
    maw_core::provider::ModelChoice {
        id: id.into(),
        display_name: id.into(),
        description: desc.into(),
        is_default: default,
        source: "provider".into(),
        reasoning_efforts: efforts.iter().map(|s| s.to_string()).collect(),
    }
}

fn press(a: &mut App, code: crossterm::event::KeyCode) -> Vec<maw_tui::app::Action> {
    handle_key(
        a,
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
    )
}

fn type_text(a: &mut App, text: &str) {
    for c in text.chars() {
        press(a, crossterm::event::KeyCode::Char(c));
    }
}

#[test]
fn model_picker_chooses_a_model_then_asks_for_the_effort_only_when_it_has_levels() {
    use crossterm::event::KeyCode;
    use maw_tui::app::{run_cmd, Action, Cmd};
    let mut a = app();
    let agent = a
        .agents()
        .iter()
        .find(|x| x.provider == ProviderKind::Codex)
        .unwrap()
        .clone();
    a.selected_agent = a.agents().iter().position(|x| x.id == agent.id).unwrap();
    a.models.insert(
        ProviderKind::Codex,
        vec![
            mk_model("gpt-6.1-sol", "flagship", &["low", "high"], true),
            mk_model("gpt-6-luna", "fast", &["none"], false),
        ],
    );
    run_cmd(&mut a, Cmd::AgentModel);
    let t = render_text(&a, 110, 30);
    assert!(
        t.contains("Model for")
            && t.contains("gpt-6.1-sol")
            && t.contains("flagship")
            && t.contains("❯"),
        "{t}"
    );
    // filter by typing, then choose: the model has levels, so the effort picker follows
    type_text(&mut a, "sol");
    assert!(
        !render_text(&a, 110, 30).contains("gpt-6-luna"),
        "the filter hides other models"
    );
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    let t = render_text(&a, 110, 30);
    assert!(t.contains("Effort for") && t.contains("high"), "{t}");
    press(&mut a, KeyCode::Down); // low
    press(&mut a, KeyCode::Down); // high
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::SetAgentModel { model, effort, .. }] => {
            assert_eq!(
                (model.as_deref(), effort.as_deref()),
                (Some("gpt-6.1-sol"), Some("high"))
            )
        }
        o => panic!("{o:?}"),
    }
    // a model without levels applies at once, clearing the effort
    run_cmd(&mut a, Cmd::AgentModel);
    type_text(&mut a, "luna");
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::SetAgentModel { model, effort, .. }] => assert_eq!(
            (model.as_deref(), effort.as_deref()),
            (Some("gpt-6-luna"), None)
        ),
        o => panic!("{o:?}"),
    }
    // a typed id that is not in the list is taken as typed (a model the provider does not
    // describe gets the common effort levels, which the next picker offers)
    run_cmd(&mut a, Cmd::AgentModel);
    type_text(&mut a, "my-custom-model");
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(render_text(&a, 110, 30).contains("Effort for"));
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::SetAgentModel { model, effort, .. }] => {
            assert_eq!(
                (model.as_deref(), effort.as_deref()),
                (Some("my-custom-model"), None)
            )
        }
        o => panic!("{o:?}"),
    }
    run_cmd(&mut a, Cmd::AgentModel);
    press(&mut a, KeyCode::Home);
    // `default` is the provider's default model, which has levels here: the effort follows
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(render_text(&a, 110, 30).contains("Effort for"));
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::SetAgentModel { model, effort, .. }] => {
            assert_eq!((model.clone(), effort.clone()), (None, None))
        }
        o => panic!("the first entry is default: {o:?}"),
    }
    // Esc cancels
    run_cmd(&mut a, Cmd::AgentModel);
    press(&mut a, KeyCode::Esc);
    assert!(matches!(a.overlay, Overlay::None));
    // small terminals do not panic
    run_cmd(&mut a, Cmd::AgentModel);
    for (w, h) in [(80, 24), (40, 12), (20, 5)] {
        let _ = render_text(&a, w, h);
    }
}

#[test]
fn permission_picker_maps_to_an_action_and_marks_the_current_profile() {
    use crossterm::event::KeyCode;
    use maw_tui::app::{run_cmd, Action, Cmd};
    let mut a = app();
    a.selected_agent = 1;
    run_cmd(&mut a, Cmd::AgentPermission);
    let t = render_text(&a, 110, 30);
    assert!(
        t.contains("safe") && t.contains("balanced") && t.contains("autonomous") && t.contains("●"),
        "{t}"
    );
    type_text(&mut a, "auto");
    assert!(matches!(
        press(&mut a, KeyCode::Enter).as_slice(),
        [Action::SetAgentPermission {
            permission: PermissionProfile::Autonomous,
            ..
        }]
    ));
}

#[test]
fn download_picker_lists_suggestions_marks_installed_and_accepts_a_typed_name() {
    use crossterm::event::KeyCode;
    use maw_tui::app::{run_cmd, Action, Cmd};
    let mut a = app();
    // without the Ollama provider there is nothing to offer, and the user is told
    run_cmd(&mut a, Cmd::PullModel);
    assert!(a
        .toast
        .as_ref()
        .is_some_and(|t| t.text.contains("not available")));
    let mk = |id: &str| mk_model(id, &format!("{id} 5.2 GB · tools"), &[], false);
    a.pull_suggestions = vec![mk("qwen3:8b"), mk("llama3.1:8b"), mk("mistral:7b")];
    a.models
        .insert(ProviderKind::Ollama, vec![mk("llama3.1:8b")]);
    run_cmd(&mut a, Cmd::PullModel);
    let t = render_text(&a, 120, 30);
    assert!(
        t.contains("Download a model") && t.contains("qwen3:8b") && t.contains("✓ installed"),
        "{t}"
    );
    assert!(
        matches!(press(&mut a, KeyCode::Enter).as_slice(), [Action::PullModel { name }] if name == "qwen3:8b")
    );
    run_cmd(&mut a, Cmd::PullModel);
    type_text(&mut a, "phi4-mini");
    assert!(
        matches!(press(&mut a, KeyCode::Enter).as_slice(), [Action::PullModel { name }] if name == "phi4-mini")
    );
    assert!(maw_tui::app::COMMANDS
        .iter()
        .any(|c| c.name == "download model"));
}

// ---- the message box in the agent view ---------------------------------------------------------

/// T / L / P are letters in the message box, so the same switches are slash commands with an
/// explicit on/off.
#[test]
fn transcript_filters_can_be_set_from_the_message_box_with_on_and_off() {
    use crossterm::event::KeyCode;
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    assert!(a.show_thinking && a.show_logs && a.show_protocol);

    type_text(&mut a, "/thinking off");
    // the menu still lists the command while the argument is being typed
    assert!(render_text(&a, 120, 34).contains("/transcript thinking"));
    press(&mut a, KeyCode::Enter);
    assert!(!a.show_thinking && a.show_logs && a.show_protocol);
    assert!(a.composer.value().is_empty());

    type_text(&mut a, "/thinking on");
    press(&mut a, KeyCode::Enter);
    assert!(a.show_thinking);

    type_text(&mut a, "/tool logs off");
    press(&mut a, KeyCode::Enter);
    assert!(!a.show_logs);
    type_text(&mut a, "/protocol hide");
    press(&mut a, KeyCode::Enter);
    assert!(!a.show_protocol);

    // without an argument it still toggles, as the T key does
    type_text(&mut a, "/thinking");
    press(&mut a, KeyCode::Enter);
    assert!(!a.show_thinking);
    assert_eq!(a.view, View::AgentDetail, "the box keeps working");
}

#[test]
fn the_agent_view_has_a_message_box_that_sends_commands_and_runs_shell() {
    use crossterm::event::KeyCode;
    use maw_tui::app::Action;
    let mut a = app();
    a.selected_agent = 1;
    let name = a.agents()[1].name.clone();
    a.goto(View::AgentDetail);
    assert!(a.composer_focus, "typing works straight away");
    let t = render_text(&a, 120, 34);
    assert!(
        t.contains("message the agent") && t.contains(&format!("message {name}")),
        "{t}"
    );

    // letters are text, not hotkeys (d / s / m / M would otherwise change view or open dialogs)
    type_text(&mut a, "dsmM please add login");
    assert_eq!(a.view, View::AgentDetail);
    assert!(matches!(a.overlay, Overlay::None));
    assert!(render_text(&a, 120, 34).contains("dsmM please add login"));
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::MessageAgent {
            agent,
            text,
            interrupt: false,
        }] => {
            assert_eq!(
                (agent.as_str(), text.as_str()),
                (name.as_str(), "dsmM please add login")
            )
        }
        o => panic!("{o:?}"),
    }
    assert!(
        a.composer.value().is_empty(),
        "the box is emptied after sending"
    );

    // !cmd runs a shell command in the project folder
    type_text(&mut a, "!git status");
    assert!(
        matches!(press(&mut a, KeyCode::Enter).as_slice(), [Action::RunShell(c)] if c == "git status")
    );

    // /command opens the menu; typing narrows it; Enter runs the highlighted command
    type_text(&mut a, "/mod");
    let t = render_text(&a, 120, 34);
    assert!(t.contains("commands") && t.contains("/model"), "{t}");
    type_text(&mut a, "el");
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(
        matches!(a.overlay, Overlay::Picker(_)),
        "/model opens the model picker"
    );
    press(&mut a, KeyCode::Esc);
    assert!(a.composer.value().is_empty());

    // Esc clears typed text first, and only then leaves the view
    type_text(&mut a, "draft");
    press(&mut a, KeyCode::Esc);
    assert_eq!((a.view, a.composer.value()), (View::AgentDetail, ""));
    press(&mut a, KeyCode::Esc);
    assert_ne!(a.view, View::AgentDetail);
}

#[test]
fn tab_switches_between_the_message_box_and_browse_keys() {
    use crossterm::event::KeyCode;
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    press(&mut a, KeyCode::Tab);
    assert!(!a.composer_focus);
    assert!(render_text(&a, 120, 34).contains("browse:"));
    // in browse mode the old hotkeys work again: T toggles thinking, d goes to the dashboard
    let before = a.show_thinking;
    press(&mut a, KeyCode::Char('T'));
    assert_ne!(a.show_thinking, before);
    press(&mut a, KeyCode::Enter);
    assert!(a.composer_focus, "Enter returns to the message box");
    press(&mut a, KeyCode::Tab);
    press(&mut a, KeyCode::Char('d'));
    assert_eq!(a.view, View::Dashboard);
    // coming back lands in the message box again
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    assert!(a.composer_focus);
}

#[test]
fn arrows_scroll_the_transcript_even_while_typing() {
    use crossterm::event::KeyCode;
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    let before = a.detail_scroll;
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::PageUp);
    assert!(a.detail_scroll > before);
}

// ---- the top bar ----------------------------------------------------------------------------

#[test]
fn the_top_bar_shows_every_view_key_first_including_the_performance_key() {
    let mut a = app();
    for w in [200usize, 120, 100, 80, 60] {
        a.size = (w as u16, 30);
        let t = render_text(&a, w as u16, 30);
        let bar = t.lines().next().unwrap().to_string();
        assert!(bar.trim_start().starts_with("MAW"), "{w}: {bar}");
        // the keys of all views are visible without scrolling
        for k in ["d", "s", "t", "g", "f", "o", "l", "$", "%", "c"] {
            assert!(bar.contains(k), "{w}: key {k} missing in {bar}");
        }
    }
    a.size = (200, 30);
    let wide = render_text(&a, 200, 30);
    let bar = wide.lines().next().unwrap();
    assert!(
        bar.contains("%:Perf") && bar.contains("$:Tokens"),
        "full names when they fit: {bar}"
    );
}

/// The bottom border of the message box says what the agent is doing, for how long and with how
/// many provider-reported tokens, and keeps the last turn's figures once it is idle again.
#[test]
fn the_message_box_shows_the_turn_clock_and_tokens() {
    use maw_core::AgentState;
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    let mut snap = a.snap.clone().unwrap();
    let id = snap.agents[1].id.clone();
    a.now = chrono::Utc::now();
    // the fixture's agent is already busy: let it finish first, then start a clean turn
    snap.agents[1].state = AgentState::Idle;
    a.apply_snapshot(snap.clone());
    {
        let ag = &mut snap.agents[1];
        ag.state = AgentState::Working;
        ag.usage.reported_by_provider = true;
        ag.usage.total_tokens = Some(10_000);
    }
    a.apply_snapshot(snap.clone());
    // 12 s later the provider has reported 2,300 more tokens
    a.now += chrono::Duration::seconds(12);
    snap.agents[1].usage.total_tokens = Some(12_300);
    a.apply_snapshot(snap.clone());
    let t = render_text(&a, 120, 34);
    assert!(
        t.contains("working 00:12") && t.contains("↓ 2.3k tokens") && t.contains("/interrupt"),
        "{t}"
    );

    // idle again: the last turn is kept next to the session total
    a.now += chrono::Duration::seconds(3);
    snap.agents[1].state = AgentState::Idle;
    a.apply_snapshot(snap.clone());
    let t = render_text(&a, 120, 34);
    assert!(
        t.contains("idle")
            && t.contains("last turn 00:15")
            && t.contains("2.3k tokens")
            && t.contains("session 12.3k"),
        "{t}"
    );
    assert!(a.turn_marks[&id].started.is_none());

    // a provider that reports nothing gets no invented figures
    snap.agents[1].usage.reported_by_provider = false;
    snap.agents[1].state = AgentState::Thinking;
    a.apply_snapshot(snap);
    let t = render_text(&a, 120, 34);
    assert!(t.contains("thinking") && !t.contains("tokens ·"), "{t}");
}

/// `/performance`, `/status` and `/tokens` open a window beside the conversation without leaving
/// the agent view or the message box (the `%` and `$` keys are only letters while typing).
#[test]
fn slash_commands_open_side_windows_beside_the_conversation() {
    use crossterm::event::KeyCode;
    use maw_tui::app::SidePanel;
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);

    type_text(&mut a, "/performance");
    assert!(
        render_text(&a, 160, 36).contains("/performance"),
        "listed in the menu"
    );
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.side, Some(SidePanel::Perf));
    assert_eq!(a.view, View::AgentDetail, "still in the agent view");
    let wide = render_text(&a, 160, 36);
    assert!(
        wide.contains("PERFORMANCE")
            && wide.contains("Transcript")
            && wide.contains("message codex-backend"),
        "chart window, conversation and message box on one screen: {wide}"
    );
    // a narrow terminal has no room beside: the window covers the conversation, the box stays
    let narrow = render_text(&a, 100, 30);
    assert!(
        narrow.contains("PERFORMANCE") && narrow.contains("message codex-backend"),
        "{narrow}"
    );

    // the message box keeps working while it is open
    type_text(&mut a, "hello");
    assert_eq!(a.composer.value(), "hello");
    press(&mut a, KeyCode::Esc); // first Esc clears the text
    assert_eq!(a.side, Some(SidePanel::Perf));
    press(&mut a, KeyCode::Esc); // the next closes the window
    assert_eq!((a.side, a.view), (None, View::AgentDetail));

    // a window length can be given
    type_text(&mut a, "/performance 30m");
    press(&mut a, KeyCode::Enter);
    assert_eq!((a.side, a.perf_window), (Some(SidePanel::Perf), 2));
    assert!(render_text(&a, 160, 36).contains("last 30m"));

    // the same command again closes it (toggle); status and tokens replace one another
    type_text(&mut a, "/performance");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.side, None);
    type_text(&mut a, "/status");
    press(&mut a, KeyCode::Enter);
    let t = render_text(&a, 160, 36);
    assert!(
        t.contains("STATUS")
            && t.contains("permission")
            && t.contains("provider")
            && t.contains("scheduler"),
        "{t}"
    );
    type_text(&mut a, "/tokens");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.side, Some(SidePanel::Tokens));
    assert!(render_text(&a, 160, 36).contains("Transcript"));

    // outside the agent view the same commands open the full view
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Esc);
    assert_ne!(a.view, View::AgentDetail);
    let mut b = app();
    b.overlay = Overlay::Palette(Default::default());
    for c in "performance".chars() {
        press(&mut b, KeyCode::Char(c));
    }
    press(&mut b, KeyCode::Enter);
    assert_eq!(b.view, View::Perf);
}

/// `/workspace wsl|windows|project` switches where worktrees go while MAW runs; without an
/// argument it only reports the current choice.
#[test]
fn workspace_command_switches_the_worktree_location() {
    use crossterm::event::KeyCode;
    use maw_tui::app::Action;
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    assert_eq!(a.config.worktree_location, "project");

    type_text(&mut a, "/workspace wsl");
    assert!(
        render_text(&a, 140, 34).contains("/workspace"),
        "listed in the menu"
    );
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::SetConfig(c)] => assert_eq!(c.worktree_location, "wsl"),
        o => panic!("{o:?}"),
    }
    assert_eq!(a.config.worktree_location, "wsl");

    type_text(&mut a, "/workspace windows");
    match press(&mut a, KeyCode::Enter).as_slice() {
        [Action::SetConfig(c)] => assert_eq!(c.worktree_location, "windows"),
        o => panic!("{o:?}"),
    }
    // no argument: nothing changes, the current choice is reported
    type_text(&mut a, "/workspace");
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert_eq!(a.config.worktree_location, "windows");
    assert_eq!(a.view, View::AgentDetail);
}
