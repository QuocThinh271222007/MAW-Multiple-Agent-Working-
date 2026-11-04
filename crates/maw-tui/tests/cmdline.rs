//! The command line on the main screen: ready for typing, `/` lists commands, `!` runs a shell
//! command, plain text goes to the selected agent, and Tab hands the keys back to the hotkeys.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maw_tui::app::{handle_key, Action, DialogKind, Overlay, View};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn app() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.now = Utc::now();
    a.size = (140, 40);
    a.cmdline_focus = true; // what the runtime does on start
    a
}

fn press(a: &mut App, code: KeyCode) -> Vec<Action> {
    handle_key(a, KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_text(a: &mut App, s: &str) {
    for c in s.chars() {
        press(a, KeyCode::Char(c));
    }
}

#[test]
fn the_main_screen_shows_a_command_line_that_is_ready_for_typing() {
    let a = app();
    let t = render_text(&a, 140, 40);
    assert!(
        t.contains("command · Enter run") && t.contains("type / for the list of commands"),
        "{t}"
    );
    assert!(
        t.contains("selected agent:"),
        "it says who plain text would go to\n{t}"
    );
    // the panels above it are still there
    assert!(t.contains("AGENTS") && t.contains("claude-lead"), "{t}");
}

#[test]
fn slash_commands_run_from_the_main_screen_and_the_menu_lists_them() {
    let mut a = app();
    type_text(&mut a, "/boa");
    let t = render_text(&a, 140, 40);
    assert!(t.contains("/board"), "the menu shows the match\n{t}");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.view, View::Canvas);
    // back on the main screen the line is ready again, and empty
    press(&mut a, KeyCode::Char('d')); // the board's own keys let the global hotkey through
    assert_eq!(a.view, View::Dashboard);
    assert!(a.typing() && a.composer.value().is_empty());
    // a command with an argument
    type_text(&mut a, "/workspace wsl");
    let acts = press(&mut a, KeyCode::Enter);
    assert!(
        matches!(acts.as_slice(), [Action::SetConfig(c)] if c.worktree_location == "wsl"),
        "{acts:?}"
    );
}

#[test]
fn letters_are_text_until_tab_and_then_they_are_hotkeys() {
    let mut a = app();
    type_text(&mut a, "dstq");
    assert_eq!(a.view, View::Dashboard, "typing, not switching views");
    assert_eq!(a.composer.value(), "dstq");
    assert!(!a.quit);
    // Esc clears the text, a second Esc gives the keys to the hotkeys
    press(&mut a, KeyCode::Esc);
    assert!(a.composer.value().is_empty() && a.typing());
    press(&mut a, KeyCode::Esc);
    assert!(!a.typing());
    press(&mut a, KeyCode::Char('t'));
    assert_eq!(a.view, View::Tasks, "hotkey mode");
    // `/` and `!` bring the command line back, with the character already typed
    press(&mut a, KeyCode::Char('d'));
    assert!(a.typing(), "arriving on the main screen focuses the line");
    press(&mut a, KeyCode::Tab);
    assert!(!a.typing());
    press(&mut a, KeyCode::Char('/'));
    assert!(a.typing() && a.composer.value() == "/");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('!'));
    assert!(a.typing() && a.composer.value() == "!");
}

#[test]
fn tab_after_a_slash_completes_the_command_before_it_leaves() {
    let mut a = app();
    type_text(&mut a, "/perf");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.composer.value(), "/performance ");
    assert!(a.typing());
    press(&mut a, KeyCode::Tab);
    assert!(!a.typing());
}

#[test]
fn shell_commands_and_plain_text_have_somewhere_to_go() {
    let mut a = app();
    type_text(&mut a, "!git status");
    let acts = press(&mut a, KeyCode::Enter);
    assert!(
        matches!(acts.as_slice(), [Action::RunShell(c)] if c == "git status"),
        "{acts:?}"
    );
    // plain text is a message for the selected agent
    let name = a.selected_agent().unwrap().name.clone();
    type_text(&mut a, "please run the tests");
    let acts = press(&mut a, KeyCode::Enter);
    assert!(
        matches!(acts.as_slice(), [Action::MessageAgent { agent, text, .. }] if *agent == name && text == "please run the tests"),
        "{acts:?}"
    );
    // a command that opens a dialog still does
    type_text(&mut a, "/task new");
    press(&mut a, KeyCode::Enter);
    assert!(
        matches!(&a.overlay, Overlay::Dialog(d) if d.kind == DialogKind::NewTask),
        "{:?}",
        a.overlay
    );
}

#[test]
fn a_terminal_too_short_has_no_command_line_and_keeps_the_old_keys() {
    let mut a = app();
    a.size = (100, 12);
    assert!(!a.has_cmdline() && !a.typing());
    let t = render_text(&a, 100, 12);
    assert!(!t.contains("command · Enter run"), "{t}");
    // so the hotkeys work as before and ':' still opens the palette
    press(&mut a, KeyCode::Char('t'));
    assert_eq!(a.view, View::Tasks);
    press(&mut a, KeyCode::Char(':'));
    assert!(matches!(a.overlay, Overlay::Palette(_)));
}

#[test]
fn the_command_line_renders_at_every_size_without_panicking() {
    let mut a = app();
    type_text(&mut a, "/");
    for (w, h) in [(20, 5), (40, 16), (60, 18), (80, 24), (140, 40), (300, 80)] {
        a.size = (w, h);
        let _ = render_text(&a, w, h);
    }
}
