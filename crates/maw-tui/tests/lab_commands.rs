//! The Lab's `/commands`, the mouse, and the rule that the view never fights the student's hand.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use maw_lab::train::{TrainConfig, Trainer};
use maw_lab::{Config, Gpt, LabSession, Snapshot, Tokenizer};
use maw_tui::lab_ui::{
    command_matches, complete, handle_key, handle_mouse, render_lab, run_command, LabKey, LabView,
    COMMANDS,
};
use maw_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::time::Duration;

fn session(paused: bool) -> LabSession {
    let text = maw_lab::DEMO_EN.to_string();
    let tok = Tokenizer::char_level(&text);
    let cfg = Config {
        vocab: tok.vocab_size(),
        dim: 16,
        heads: 2,
        layers: 2,
        ctx: 16,
        ff_mult: 2,
    };
    let model = Gpt::new(cfg, 1).unwrap();
    let tc = TrainConfig {
        steps: 100_000,
        batch: 2,
        seq: 16,
        lr: 0.01,
        warmup: 5,
        eval_every: 10,
        ..Default::default()
    };
    let trainer = Trainer::new(model, tok.encode(&text), tc).unwrap();
    LabSession::start(
        trainer,
        tok,
        "characters".into(),
        "A language model".into(),
        paused,
    )
}

fn wait(s: &LabSession, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
    for _ in 0..1200 {
        let snap = s.snapshot();
        if f(&snap) {
            return snap;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out");
}

fn draw(snap: &Snapshot, view: &LabView) -> String {
    let (w, h) = (140u16, 44u16);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let theme = Theme::new(false);
    t.draw(|f| render_lab(f, f.area(), snap, view, &theme, false))
        .unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn type_text(v: &mut LabView, s: &LabSession, snap: &Snapshot, text: &str) {
    let path = std::path::Path::new("unused.json");
    for c in text.chars() {
        handle_key(v, s, key(KeyCode::Char(c)), snap, path);
    }
}

fn run(v: &mut LabView, s: &LabSession, snap: &Snapshot, line: &str) -> LabKey {
    run_command(v, s, snap, line, std::path::Path::new("unused.json"))
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn slash_opens_a_command_line_with_a_menu_and_tab_completes() {
    let s = session(true);
    let snap = wait(&s, |x| !x.labels.is_empty());
    let mut v = LabView::default();
    type_text(&mut v, &s, &snap, "/");
    assert_eq!(v.command.as_deref(), Some("/"));
    let t = draw(&snap, &v);
    assert!(t.contains("COMMANDS"), "{t}");
    for c in COMMANDS {
        assert!(
            t.contains(&format!("/{}", c.0)),
            "/{} is listed while typing /",
            c.0
        );
    }
    assert!(
        t.contains("█"),
        "the typed text shows with a cursor in the footer"
    );
    // the menu covers the whole bottom row: nothing of the panel beside the lesson shows through
    assert!(!t.contains("WHAT THE MODEL WRITES"), "{t}");
    // typing narrows the menu, Tab finishes the name
    type_text(&mut v, &s, &snap, "ro");
    assert_eq!(command_matches("/ro").len(), 1);
    assert!(draw(&snap, &v).contains("turn the 3D view by degrees"));
    handle_key(
        &mut v,
        &s,
        key(KeyCode::Tab),
        &snap,
        std::path::Path::new("x"),
    );
    assert_eq!(v.command.as_deref(), Some("/rotate "));
    type_text(&mut v, &s, &snap, "90 20");
    handle_key(
        &mut v,
        &s,
        key(KeyCode::Enter),
        &snap,
        std::path::Path::new("x"),
    );
    assert!(v.command.is_none());
    assert!((v.yaw.to_degrees() - 90.0).abs() < 1e-9 && (v.pitch.to_degrees() - 20.0).abs() < 1e-9);
    assert!(!v.auto_rotate, "turning by hand switches the spin off");
    assert!(
        draw(&snap, &v).contains("yaw 90"),
        "and the answer shows in the footer / title"
    );
    // Esc cancels, and so does erasing the slash
    type_text(&mut v, &s, &snap, "/ro");
    handle_key(
        &mut v,
        &s,
        key(KeyCode::Esc),
        &snap,
        std::path::Path::new("x"),
    );
    assert!(v.command.is_none());
    type_text(&mut v, &s, &snap, "/");
    handle_key(
        &mut v,
        &s,
        key(KeyCode::Backspace),
        &snap,
        std::path::Path::new("x"),
    );
    assert!(v.command.is_none());
    // q and the other hotkeys are plain letters inside a command
    type_text(&mut v, &s, &snap, "/quit");
    assert_eq!(v.command.as_deref(), Some("/quit"));
    assert_eq!(complete("/zo"), "/zoom ");
    assert_eq!(complete("/zoom 2"), "/zoom 2");
    assert_eq!(complete("/nonsense"), "/nonsense");
    s.stop();
}

#[test]
fn every_command_does_what_it_says_and_bad_input_is_explained() {
    let s = session(false);
    let snap = wait(&s, |x| x.step > 2);
    let mut v = LabView::default();
    run(&mut v, &s, &snap, "/view top");
    assert!(v.pitch.to_degrees() > 80.0 && v.yaw == 0.0);
    run(&mut v, &s, &snap, "/view side");
    assert!((v.yaw.to_degrees() - 90.0).abs() < 1e-9 && v.pitch == 0.0);
    run(&mut v, &s, &snap, "/view sideways");
    assert!(v.message.contains("usage"), "{}", v.message);
    run(&mut v, &s, &snap, "/zoom 2.5");
    assert!((v.zoom - 2.5).abs() < 1e-12);
    run(&mut v, &s, &snap, "/zoom 99");
    assert!(
        (v.zoom - 2.5).abs() < 1e-12 && v.message.contains("usage"),
        "out of range is refused"
    );
    run(&mut v, &s, &snap, "/rotate abc");
    assert!(v.message.contains("usage"));
    run(&mut v, &s, &snap, "/rotate 10");
    assert!(
        (v.yaw.to_degrees() - 10.0).abs() < 1e-9,
        "pitch is optional"
    );
    run(&mut v, &s, &snap, "/spin on");
    assert!(v.auto_rotate);
    run(&mut v, &s, &snap, "/spin off");
    assert!(!v.auto_rotate);
    run(&mut v, &s, &snap, "/lesson 4");
    assert_eq!(v.lesson, 3);
    run(&mut v, &s, &snap, "/lesson 99");
    assert_eq!(v.lesson, 3);
    assert!(v.message.contains("usage"));
    run(&mut v, &s, &snap, "/layer 2");
    assert_eq!(v.layer, 1);
    run(&mut v, &s, &snap, "/layer 3");
    assert!(v.message.contains("2 layer"), "{}", v.message);
    run(&mut v, &s, &snap, "/head 2");
    assert_eq!(v.head, 1);
    run(&mut v, &s, &snap, "/head 0");
    assert!(v.message.contains("2 head"), "{}", v.message);
    // training control
    run(&mut v, &s, &snap, "/pause");
    assert!(s.is_paused());
    run(&mut v, &s, &snap, "/resume");
    assert!(!s.is_paused());
    // your own sentence
    run(&mut v, &s, &snap, "/probe hello there");
    wait(&s, |x| x.probe_text == "hello there");
    run(&mut v, &s, &snap, "/probe");
    assert!(v.message.contains("usage"));
    // saving
    let path = std::env::temp_dir().join(format!("maw-lab-cmd-{}.json", std::process::id()));
    run(&mut v, &s, &snap, &format!("/save {}", path.display()));
    wait(&s, |x| x.notice.starts_with("saved"));
    assert!(path.is_file());
    let _ = std::fs::remove_file(path);
    // the rest
    run(&mut v, &s, &snap, "/sample");
    run(&mut v, &s, &snap, "/help");
    assert!(
        v.message.contains("/rotate") && v.message.contains("mouse"),
        "{}",
        v.message
    );
    run(&mut v, &s, &snap, "/frobnicate");
    assert!(
        v.message.contains("unknown command /frobnicate"),
        "{}",
        v.message
    );
    assert!(matches!(run(&mut v, &s, &snap, "/quit"), LabKey::Quit));
    assert!(matches!(run(&mut v, &s, &snap, "/exit"), LabKey::Quit));
    assert!(matches!(run(&mut v, &s, &snap, "/"), LabKey::None));
    s.stop();
}

#[test]
fn dragging_with_the_mouse_turns_the_view_and_the_wheel_zooms() {
    let mut v = LabView::default();
    let (y0, p0, z0) = (v.yaw, v.pitch, v.zoom);
    handle_mouse(
        &mut v,
        mouse(MouseEventKind::Down(MouseButton::Left), 40, 10),
    );
    handle_mouse(
        &mut v,
        mouse(MouseEventKind::Drag(MouseButton::Left), 50, 12),
    );
    assert!(
        (v.yaw - y0 - 0.6).abs() < 1e-9,
        "10 columns = 0.6 rad: {}",
        v.yaw - y0
    );
    assert!(v.pitch > p0, "dragging down tilts the view");
    // the drag continues from where it is now, not from where it began
    handle_mouse(
        &mut v,
        mouse(MouseEventKind::Drag(MouseButton::Left), 50, 12),
    );
    assert!((v.yaw - y0 - 0.6).abs() < 1e-9);
    handle_mouse(&mut v, mouse(MouseEventKind::Up(MouseButton::Left), 50, 12));
    let held = v.yaw;
    handle_mouse(
        &mut v,
        mouse(MouseEventKind::Drag(MouseButton::Left), 70, 12),
    );
    assert_eq!(v.yaw, held, "no button held, no turning");
    handle_mouse(&mut v, mouse(MouseEventKind::ScrollUp, 10, 10));
    assert!(v.zoom > z0);
    handle_mouse(&mut v, mouse(MouseEventKind::ScrollDown, 10, 10));
    handle_mouse(&mut v, mouse(MouseEventKind::ScrollDown, 10, 10));
    assert!(v.zoom < z0);
    for _ in 0..80 {
        handle_mouse(&mut v, mouse(MouseEventKind::ScrollUp, 1, 1));
    }
    assert!(v.zoom <= 6.0);
    // dragging far down cannot flip the view over the top
    handle_mouse(&mut v, mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    handle_mouse(
        &mut v,
        mouse(MouseEventKind::Drag(MouseButton::Left), 0, 200),
    );
    assert!(v.pitch <= 1.5);
}

#[test]
fn the_view_spins_only_when_left_alone_and_the_keys_always_turn_it() {
    // right after the student touches something it stays put, even for a long moment
    let mut v = LabView {
        since_input: 0.0,
        ..LabView::default()
    };
    let y = v.yaw;
    v.tick(1.0);
    v.tick(1.0);
    assert_eq!(v.yaw, y, "the view does not fight the hand");
    // after a few quiet seconds it turns by itself
    for _ in 0..3 {
        v.tick(1.0);
    }
    assert!(v.yaw > y);
    // and not at all when switched off or while typing a command
    let y = v.yaw;
    v.auto_rotate = false;
    v.since_input = 99.0;
    v.tick(1.0);
    assert_eq!(v.yaw, y);
    v.auto_rotate = true;
    v.command = Some("/".into());
    v.tick(1.0);
    assert_eq!(v.yaw, y);
    // messages fade
    v.say("hello");
    assert!(v.message_ttl > 0);
    for _ in 0..200 {
        v.tick(0.05);
    }
    assert_eq!(v.message_ttl, 0);
    // the alternative keys turn the view too, for terminals that do not pass the arrows on
    let s = session(true);
    let snap = wait(&s, |x| !x.labels.is_empty());
    let mut v = LabView::default();
    let path = std::path::Path::new("x");
    let y0 = v.yaw;
    handle_key(&mut v, &s, key(KeyCode::Char('.')), &snap, path);
    assert!(v.yaw > y0);
    handle_key(&mut v, &s, key(KeyCode::Char(',')), &snap, path);
    handle_key(&mut v, &s, key(KeyCode::Char(',')), &snap, path);
    assert!(v.yaw < y0);
    let p0 = v.pitch;
    handle_key(&mut v, &s, key(KeyCode::Char(']')), &snap, path);
    assert!(v.pitch > p0);
    handle_key(&mut v, &s, key(KeyCode::Char('[')), &snap, path);
    handle_key(&mut v, &s, key(KeyCode::Char('[')), &snap, path);
    assert!(v.pitch < p0);
    assert_eq!(v.since_input, 0.0);
    s.stop();
}

#[test]
fn the_screen_says_how_to_turn_it_and_the_angle_in_use() {
    let s = session(true);
    let snap = wait(&s, |x| !x.labels.is_empty());
    let mut v = LabView::default();
    let t = draw(&snap, &v);
    assert!(
        t.contains("yaw") && t.contains("pitch") && t.contains("zoom"),
        "{t}"
    );
    assert!(
        t.contains("/ commands") && t.contains("mouse drag"),
        "the footer says how"
    );
    v.yaw = std::f64::consts::FRAC_PI_2;
    assert!(draw(&snap, &v).contains("yaw 90"));
    s.stop();
}
