//! Long input and long text are wrapped, not cut: the message box grows with what is typed, and the
//! places that used to end in "…" show the whole text somewhere.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maw_tui::app::{handle_key, View};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn app() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.now = Utc::now();
    a.size = (80, 30);
    a.cmdline_focus = true;
    a
}

fn type_text(a: &mut App, s: &str) {
    for c in s.chars() {
        handle_key(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

const LONG: &str = "please write a bubble sort in python, add unit tests for the empty list and for a list that is already sorted, then explain the complexity";

#[test]
fn a_long_message_wraps_and_every_word_can_be_read() {
    let mut a = app();
    a.selected_agent = 1;
    a.goto(View::AgentDetail);
    a.cmdline_focus = true;
    type_text(&mut a, LONG);
    let t = render_text(&a, 80, 30);
    for w in ["bubble", "python,", "empty", "already", "complexity"] {
        assert!(t.contains(w), "{w} is visible\n{t}");
    }
    // the box is taller than for a short message
    let rows = |s: &str| {
        s.lines()
            .filter(|l| l.contains("complexity") || l.contains("› please"))
            .count()
    };
    assert!(rows(&t) >= 2, "{t}");
}

#[test]
fn the_box_grows_with_the_text_up_to_a_limit() {
    use maw_tui::ui::agents::input_box_height;
    let mut a = app();
    assert_eq!(input_box_height(&a, 60, 30), 3);
    type_text(&mut a, &"word ".repeat(30));
    let h = input_box_height(&a, 60, 30);
    assert!(h > 3 && h <= 8, "{h}");
    type_text(&mut a, &"word ".repeat(200));
    assert!(
        input_box_height(&a, 60, 30) <= 8,
        "never takes over the screen"
    );
    // on a small screen it stays small
    assert!(input_box_height(&a, 60, 12) <= 5);
}

#[test]
fn the_command_line_of_the_main_screen_wraps_too() {
    let mut a = app();
    type_text(
        &mut a,
        "!echo one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen",
    );
    let t = render_text(&a, 60, 30);
    assert!(t.contains("echo one") && t.contains("seventeen"), "{t}");
}

#[test]
fn the_event_stream_shows_long_events_on_several_rows() {
    let mut a = app();
    a.goto(View::Logs);
    // whatever the events are, no row of the stream ends in an ellipsis that hides text
    let t = render_text(&a, 50, 30);
    assert!(!t.lines().any(|l| l.trim_end().ends_with("…│")), "{t}");
}
