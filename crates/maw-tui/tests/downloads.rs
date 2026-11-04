//! Model downloads on screen: the gauge, the list with its keys, and what happens when you pick a
//! model in the shop.

use chrono::{DateTime, Duration, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maw_core::provider::ModelChoice;
use maw_tui::app::{handle_key, run_cmd, Action, Cmd, Overlay, View};
use maw_tui::downloads::{DlState, Outcome};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn app() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.now = Utc::now();
    a.size = (130, 36);
    a
}

fn key(a: &mut App, code: KeyCode) -> Vec<Action> {
    handle_key(a, KeyEvent::new(code, KeyModifiers::NONE))
}

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

/// A download that has been going for 4 seconds at 10 MB/s.
fn running(a: &mut App, name: &str) {
    a.now = t0() + Duration::seconds(4);
    a.downloads.begin(name, t0());
    for s in 0..=4 {
        a.downloads.progress(
            name,
            "pulling",
            s as u64 * 10_000_000,
            100_000_000,
            t0() + Duration::seconds(s),
        );
    }
}

#[test]
fn picking_a_model_in_the_shop_opens_the_downloads_and_starts_it_once() {
    let mut a = app();
    a.pull_suggestions = vec![ModelChoice {
        id: "qwen3:8b".into(),
        display_name: "qwen3:8b".into(),
        description: "5.2 GB · 8B · by Alibaba (Qwen)".into(),
        is_default: false,
        source: "catalog".into(),
        reasoning_efforts: vec![],
    }];
    run_cmd(&mut a, Cmd::PullModel);
    let acts = key(&mut a, KeyCode::Enter);
    assert!(
        matches!(acts.as_slice(), [Action::PullModel { name }] if name == "qwen3:8b"),
        "{acts:?}"
    );
    assert!(
        matches!(a.overlay, Overlay::Downloads),
        "the list opens so the progress is seen"
    );
    assert!(a.downloads.is_active("qwen3:8b"));
    // choosing it again while it runs does not start a second download
    key(&mut a, KeyCode::Esc);
    run_cmd(&mut a, Cmd::PullModel);
    let acts = key(&mut a, KeyCode::Enter);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(a.downloads.list.len(), 1);
}

#[test]
fn the_list_shows_a_bar_the_numbers_the_speed_and_the_time_left() {
    let mut a = app();
    running(&mut a, "qwen3:8b");
    run_cmd(&mut a, Cmd::Downloads);
    let t = render_text(&a, 130, 36);
    assert!(t.contains("DOWNLOADS"), "{t}");
    assert!(t.contains("qwen3:8b") && t.contains("40%"), "{t}");
    assert!(t.contains('█') && t.contains('░'), "a bar\n{t}");
    assert!(t.contains("40 MB / 100 MB"), "{t}");
    assert!(t.contains("10.0 MB/s") && t.contains("ETA 6s"), "{t}");
    assert!(t.contains("x stop") && t.contains("r resume"), "{t}");
}

#[test]
fn a_gauge_stays_on_every_screen_while_something_downloads() {
    let mut a = app();
    running(&mut a, "qwen3:8b");
    for v in [View::Dashboard, View::Tasks, View::Canvas, View::Goal] {
        a.view = v;
        let t = render_text(&a, 130, 36);
        assert!(
            t.contains("⬇ qwen3:8b") && t.contains("40%") && t.contains("/downloads"),
            "{v:?}\n{t}"
        );
    }
    // a second one is counted, a finished one is not shown
    running(&mut a, "gemma4:e2b");
    a.view = View::Tasks;
    assert!(render_text(&a, 130, 36).contains("+1"));
    a.downloads
        .finish("qwen3:8b", Outcome::Done("ok".into()), t0());
    a.downloads
        .finish("gemma4:e2b", Outcome::Done("ok".into()), t0());
    assert!(
        !render_text(&a, 130, 36).contains("⬇ "),
        "no gauge when nothing is running"
    );
    // and on a tiny screen it gives way
    running(&mut a, "x");
    let _ = render_text(&a, 30, 8);
}

#[test]
fn x_stops_r_resumes_d_removes_c_clears() {
    let mut a = app();
    running(&mut a, "qwen3:8b");
    run_cmd(&mut a, Cmd::Downloads);
    let acts = key(&mut a, KeyCode::Char('x'));
    assert!(
        matches!(acts.as_slice(), [Action::StopPull { name }] if name == "qwen3:8b"),
        "{acts:?}"
    );
    assert!(
        matches!(a.overlay, Overlay::Downloads),
        "the list stays open"
    );
    // r does nothing while it is still running
    assert!(key(&mut a, KeyCode::Char('r')).is_empty());
    // the runtime reports it stopped
    a.downloads.finish("qwen3:8b", Outcome::Stopped, t0());
    let t = render_text(&a, 130, 36);
    assert!(
        t.contains("stopped") && t.contains("continues from there"),
        "{t}"
    );
    let acts = key(&mut a, KeyCode::Char('r'));
    assert!(
        matches!(acts.as_slice(), [Action::PullModel { name }] if name == "qwen3:8b"),
        "{acts:?}"
    );
    assert_eq!(
        a.downloads.list[0].state,
        DlState::Downloading,
        "it shows as running at once"
    );
    // d refuses to remove a running one, then removes a finished one
    key(&mut a, KeyCode::Char('d'));
    assert_eq!(a.downloads.list.len(), 1);
    a.downloads
        .finish("qwen3:8b", Outcome::Done("done".into()), t0());
    key(&mut a, KeyCode::Char('d'));
    assert!(a.downloads.list.is_empty());
    assert!(render_text(&a, 130, 36).contains("No downloads yet"));
    // c clears every finished entry
    running(&mut a, "a");
    a.downloads.begin("b", t0());
    a.downloads.finish("b", Outcome::Failed("x".into()), t0());
    key(&mut a, KeyCode::Char('c'));
    assert_eq!(a.downloads.list.len(), 1);
    // Esc closes it
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.overlay, Overlay::None));
}

#[test]
fn a_failure_is_written_out_in_full_not_flashed_for_a_few_seconds() {
    let mut a = app();
    a.downloads.begin("nemotron-3.5-bolt:e4b", t0());
    let why = "'nemotron-3.5-bolt:e4b' needs a download of 7.9 GB, above the 6.5 GB limit (change `max_pull_gb` under [ollama] to allow more); the download was cancelled";
    a.downloads
        .finish("nemotron-3.5-bolt:e4b", Outcome::Failed(why.into()), t0());
    run_cmd(&mut a, Cmd::Downloads);
    let t = render_text(&a, 100, 30);
    assert!(t.contains("failed"), "{t}");
    for part in ["7.9 GB", "6.5 GB limit", "max_pull_gb", "was cancelled"] {
        assert!(t.contains(part), "{part} must be readable\n{t}");
    }
    // it can be retried from there
    let acts = key(&mut a, KeyCode::Char('r'));
    assert!(matches!(acts.as_slice(), [Action::PullModel { .. }]));
}

#[test]
fn the_command_is_listed_and_everything_draws_at_any_size() {
    assert!(maw_tui::app::COMMANDS.iter().any(|c| c.name == "downloads"));
    let mut a = app();
    running(&mut a, "qwen3:8b");
    a.downloads.begin("b", t0());
    a.downloads
        .finish("b", Outcome::Failed("boom ".repeat(60)), t0());
    run_cmd(&mut a, Cmd::Downloads);
    for (w, h) in [(20, 5), (40, 10), (80, 24), (130, 36), (300, 80)] {
        let _ = render_text(&a, w, h);
    }
}
