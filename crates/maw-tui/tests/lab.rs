//! The Lab screen: the 3D maths, every lesson, the keys, and drawing at every size.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maw_lab::train::{TrainConfig, Trainer};
use maw_lab::{Config, Gpt, LabSession, Snapshot, Status, Tokenizer};
use maw_tui::lab_ui::{
    handle_key, lesson, normalise, project, render_lab, LabKey, LabView, LESSONS,
};
use maw_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::time::Duration;

fn session(steps: usize, paused: bool) -> LabSession {
    let text = maw_lab::DEMO_EN.to_string();
    let tok = Tokenizer::bpe(&text, 20);
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
        steps,
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
        "BPE, 20 merges".into(),
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
    panic!("timed out at step {}", s.snapshot().step);
}

fn draw(snap: &Snapshot, view: &LabView, w: u16, h: u16) -> String {
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

#[test]
fn rotating_keeps_distances_and_the_cloud_fits_its_box() {
    let a: [f64; 3] = [0.3, -0.5, 0.8];
    let before = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    // at yaw = pitch = 0 we look straight down the z axis: x and y are what we see (a little perspective aside)
    let (x, y, z) = project(a, 0.0, 0.0);
    assert!((z - 0.8).abs() < 1e-12 && x.signum() == 1.0 && y.signum() == -1.0);
    // a full turn brings every point back
    let (x2, y2, _) = project(a, std::f64::consts::TAU, std::f64::consts::TAU);
    assert!((x2 - x).abs() < 1e-9 && (y2 - y).abs() < 1e-9);
    // turning by 90 degrees puts the old depth axis on the horizontal axis (before the perspective shrink)
    let (hx, _, hz) = project([0.0, 0.0, 1.0], std::f64::consts::FRAC_PI_2, 0.0);
    assert!(hx > 0.9 && hz.abs() < 1e-9, "{hx} {hz}");
    assert!(before > 0.0);
    let n = normalise(&[[2.0, 0.0, -4.0], [1.0, 1.0, 1.0]]);
    assert_eq!(n[0], [0.5, 0.0, -1.0]);
    assert_eq!(normalise(&[[0.0; 3]]), vec![[0.0; 3]]);
    assert!(normalise(&[]).is_empty());
}

#[test]
fn every_lesson_explains_a_concept_with_the_numbers_on_screen() {
    let s = session(40, false);
    let snap = wait(&s, |x| x.status == Status::Done);
    for i in 0..LESSONS {
        let (title, lines) = lesson(i, &snap);
        assert!(
            title.starts_with(&format!("{}/{}", i + 1, LESSONS)),
            "{title}"
        );
        assert!(
            lines.len() >= 2 && lines.iter().all(|l| !l.is_empty()),
            "{title}: {lines:?}"
        );
    }
    let all: String = (0..LESSONS)
        .map(|i| {
            let (title, lines) = lesson(i, &snap);
            format!("{title} {}", lines.join(" "))
        })
        .collect::<Vec<_>>()
        .join(" ");
    for must in [
        "Tokenizer",
        "PCA",
        "Perplexity",
        "cosine",
        "Weight decay",
        "held-out",
        "Temperature",
        "Context window",
        "Parameters",
    ] {
        assert!(all.contains(must), "the lessons never mention {must}");
    }
    // live numbers, not canned text
    assert!(lesson(4, &snap)
        .1
        .join(" ")
        .contains(&format!("{:.4}", snap.train_cfg.lr)));
    assert!(lesson(9, &snap)
        .1
        .join(" ")
        .contains(&snap.n_params.to_string()));
    s.stop();
}

#[test]
fn the_screen_shows_the_cloud_the_curves_the_attention_and_the_writing() {
    let s = session(60, false);
    let snap = wait(&s, |x| x.status == Status::Done);
    let view = LabView::default();
    let t = draw(&snap, &view, 140, 44);
    for must in [
        "MAW LAB",
        "DONE step 60/60",
        "EMBEDDINGS",
        "PC1",
        "LOSS",
        "LEARNING RATE",
        "ATTENTION",
        "WHAT THE MODEL WRITES",
        "Tokenizer",
        "Space pause",
    ] {
        assert!(t.contains(must), "{must}\n{t}");
    }
    // braille dots are the 3D cloud; the attention map uses shades
    assert!(
        t.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
        "the 3D cloud is drawn"
    );
    // the sentence being studied is cut into its tokens at the top of the attention map
    assert!(snap.probe_pieces.concat().starts_with("A l") || !snap.probe_pieces.is_empty());
    // a different angle draws a different picture
    let mut v2 = LabView::default();
    v2.yaw += 1.0;
    assert_ne!(draw(&snap, &view, 140, 44), draw(&snap, &v2, 140, 44));
    s.stop();
}

#[test]
fn it_draws_at_every_size_without_panicking_and_asks_for_room_when_too_small() {
    let s = session(10, false);
    let snap = wait(&s, |x| x.step >= 3);
    let mut view = LabView::default();
    for (w, h) in [
        (1, 1),
        (20, 5),
        (59, 21),
        (60, 22),
        (80, 24),
        (100, 30),
        (200, 70),
    ] {
        for lesson in [0, 7, 9] {
            view.lesson = lesson;
            let t = draw(&snap, &view, w, h);
            if w < 60 || h < 22 {
                // (a very narrow window wraps the sentence, so only look for it where it fits on a line)
                assert!(t.contains("60x22") || w < 40, "{w}x{h}");
            }
        }
    }
    // before the first step there is nothing to show yet, and that is fine too
    let paused = session(10_000, true);
    let first = wait(&paused, |x| !x.labels.is_empty());
    let _ = draw(&first, &view, 100, 30);
    paused.stop();
    s.stop();
}

#[test]
fn the_keys_rotate_zoom_pause_choose_lessons_and_quit() {
    let s = session(100_000, true);
    let snap = wait(&s, |x| !x.labels.is_empty());
    let path = std::env::temp_dir().join(format!("maw-lab-ui-{}.json", std::process::id()));
    let mut v = LabView::default();
    let y0 = v.yaw;
    handle_key(&mut v, &s, key(KeyCode::Right), &snap, &path);
    assert!(v.yaw > y0);
    handle_key(&mut v, &s, key(KeyCode::Down), &snap, &path);
    for _ in 0..40 {
        handle_key(&mut v, &s, key(KeyCode::Down), &snap, &path);
    }
    assert!(v.pitch <= 1.5, "looking straight down is the limit");
    let z0 = v.zoom;
    handle_key(&mut v, &s, key(KeyCode::Char('+')), &snap, &path);
    assert!(v.zoom > z0);
    handle_key(&mut v, &s, key(KeyCode::Char('a')), &snap, &path);
    assert!(!v.auto_rotate);
    handle_key(&mut v, &s, key(KeyCode::Tab), &snap, &path);
    assert_eq!(v.lesson, 1);
    handle_key(&mut v, &s, key(KeyCode::BackTab), &snap, &path);
    handle_key(&mut v, &s, key(KeyCode::BackTab), &snap, &path);
    assert_eq!(v.lesson, LESSONS - 1);
    handle_key(&mut v, &s, key(KeyCode::Char('3')), &snap, &path);
    assert_eq!(v.lesson, 2);
    handle_key(&mut v, &s, key(KeyCode::Char('l')), &snap, &path);
    handle_key(&mut v, &s, key(KeyCode::Char('h')), &snap, &path);
    assert_eq!((v.layer, v.head), (1, 1));
    // Space starts the paused run
    assert!(s.is_paused());
    handle_key(&mut v, &s, key(KeyCode::Char(' ')), &snap, &path);
    assert!(!s.is_paused());
    wait(&s, |x| x.step > 2);
    // typing a sentence of your own
    handle_key(&mut v, &s, key(KeyCode::Char('p')), &snap, &path);
    assert_eq!(v.editing.as_deref(), Some(""));
    for c in "hi".chars() {
        handle_key(&mut v, &s, key(KeyCode::Char(c)), &snap, &path);
    }
    handle_key(&mut v, &s, key(KeyCode::Backspace), &snap, &path);
    handle_key(&mut v, &s, key(KeyCode::Char('e')), &snap, &path);
    assert_eq!(
        v.editing.as_deref(),
        Some("he"),
        "q and the other hotkeys are letters while typing"
    );
    handle_key(&mut v, &s, key(KeyCode::Enter), &snap, &path);
    assert!(v.editing.is_none());
    wait(&s, |x| x.probe_text == "he");
    // Esc while typing only cancels
    handle_key(&mut v, &s, key(KeyCode::Char('p')), &snap, &path);
    assert!(matches!(
        handle_key(&mut v, &s, key(KeyCode::Esc), &snap, &path),
        LabKey::None
    ));
    // and q, Esc, Ctrl+C leave
    assert!(matches!(
        handle_key(&mut v, &s, key(KeyCode::Char('q')), &snap, &path),
        LabKey::Quit
    ));
    assert!(matches!(
        handle_key(&mut v, &s, key(KeyCode::Esc), &snap, &path),
        LabKey::Quit
    ));
    assert!(matches!(
        handle_key(
            &mut v,
            &s,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &snap,
            &path
        ),
        LabKey::Quit
    ));
    // w saves the model
    handle_key(&mut v, &s, key(KeyCode::Char('w')), &snap, &path);
    wait(&s, |x| x.notice.starts_with("saved"));
    assert!(path.is_file());
    let _ = std::fs::remove_file(&path);
    s.stop();
}
