//! The map in the corner of the board: when it shows, its own switch, and jumping with a click.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use maw_tui::app::{handle_key, handle_mouse, View};
use maw_tui::canvas::Button;
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn board(w: u16, h: u16) -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.now = Utc::now();
    a.size = (w, h);
    a.goto(View::Canvas);
    let _ = render_text(&a, w, h);
    a
}

fn mouse(kind: MouseEventKind, col: i32, row: i32) -> MouseEvent {
    MouseEvent {
        kind,
        column: col as u16,
        row: row as u16,
        modifiers: KeyModifiers::NONE,
    }
}

fn press(a: &mut App, c: char) {
    handle_key(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

#[test]
fn the_map_shows_by_default_and_has_its_own_switch() {
    let mut a = board(170, 44);
    assert!(a.canvas.map.get().is_some());
    assert!(render_text(&a, 170, 44).contains("─ map"));
    // the button in the top bar hides just the map
    let (x, y) = {
        let b = a.canvas.buttons.borrow();
        let ((x, y, _, _), _) = *b
            .iter()
            .find(|(_, k)| *k == Button::Map)
            .expect("a map button");
        (x, y)
    };
    handle_mouse(
        &mut a,
        mouse(MouseEventKind::Down(MouseButton::Left), x + 1, y),
    );
    let t = render_text(&a, 170, 44);
    assert!(
        a.canvas.map.get().is_none() && !t.contains("─ map") && t.contains("show map"),
        "{t}"
    );
    // M brings it back
    press(&mut a, 'M');
    let _ = render_text(&a, 170, 44);
    assert!(a.canvas.map.get().is_some());
    // h hides everything and takes the map button away
    press(&mut a, 'h');
    let t = render_text(&a, 170, 44);
    assert!(
        a.canvas.map.get().is_none() && !t.contains("hide map"),
        "{t}"
    );
}

#[test]
fn the_map_is_smaller_on_a_small_screen_instead_of_missing() {
    let a = board(100, 20);
    let g = a.canvas.map.get().expect("map on 100x20");
    assert_eq!((g.rect.2, g.rect.3), (24, 8));
    let a = board(44, 14);
    let g = a.canvas.map.get().expect("a small map on 44x14");
    assert_eq!((g.rect.2, g.rect.3), (16, 6));
    // too small to leave any board: none, and no panic
    for (w, h) in [(30, 8), (20, 5)] {
        let a = board(w, h);
        let _ = render_text(&a, w, h);
    }
}

#[test]
fn a_click_on_the_map_puts_that_place_in_the_middle_of_the_view() {
    let mut a = board(170, 44);
    let g = a.canvas.map.get().unwrap();
    let before = a.canvas.origin;
    // the map's top-left corner is the top-left of the board's extent
    handle_mouse(
        &mut a,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            g.rect.0 + 3,
            g.rect.1 + 3,
        ),
    );
    handle_mouse(
        &mut a,
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            g.rect.0 + 3,
            g.rect.1 + 3,
        ),
    );
    assert_ne!(a.canvas.origin, before, "the view moved");
    let (_, _, w, h) = a.canvas.view.get();
    let (pw, ph) = maw_tui::canvas::pitch(a.canvas.zoom);
    let mid = (
        a.canvas.origin.0 + (w as f32 / 2.0) / pw as f32,
        a.canvas.origin.1 + (h as f32 / 2.0) / ph as f32,
    );
    let want = (g.lo.0 + 1.0 / g.scale.0, g.lo.1 + 1.0 / g.scale.1);
    assert!(
        (mid.0 - want.0).abs() < 0.05 && (mid.1 - want.1).abs() < 0.05,
        "{mid:?} vs {want:?}"
    );
    // dragging on the map keeps following the pointer, and nothing underneath was selected
    let o = a.canvas.origin;
    handle_mouse(
        &mut a,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            g.rect.0 + 3,
            g.rect.1 + 3,
        ),
    );
    handle_mouse(
        &mut a,
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            g.rect.0 + 15,
            g.rect.1 + 5,
        ),
    );
    handle_mouse(
        &mut a,
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            g.rect.0 + 15,
            g.rect.1 + 5,
        ),
    );
    assert!(a.canvas.origin.0 > o.0, "{o:?} -> {:?}", a.canvas.origin);
    assert!(a.canvas.selected.is_none());
}
