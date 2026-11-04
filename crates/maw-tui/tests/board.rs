//! The board (unbounded node graph): what it is made of, how it is navigated, moved, zoomed, opened
//! and used to create things. Rendered on ratatui's in-memory backend like the other view tests.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use maw_core::*;
use maw_tui::app::{handle_key, handle_mouse, Action, DialogKind, Overlay, View};
use maw_tui::canvas::{build_scene, EdgeKind, NodeKey, ZOOM_LEVELS};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn app() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.now = Utc::now();
    a.size = (170, 44);
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

fn at(a: &mut App, v: View, w: u16, h: u16) -> String {
    a.view = v;
    a.size = (w, h);
    render_text(a, w, h)
}

fn mouse(kind: MouseEventKind, col: i32, row: i32) -> MouseEvent {
    MouseEvent {
        kind,
        column: col as u16,
        row: row as u16,
        modifiers: KeyModifiers::NONE,
    }
}

fn mail(a: &App, from: usize, to: usize, subject: &str, content: &str) -> Message {
    Message {
        id: MessageId::from("msg_board_1"),
        project_id: a.snap.as_ref().unwrap().project.id.clone(),
        sender_agent_id: a.agents()[from].id.clone(),
        recipient_agent_id: a.agents()[to].id.clone(),
        task_id: None,
        message_type: MessageType::Question,
        subject: subject.into(),
        content: content.into(),
        created_at: Utc::now(),
        delivered_at: None,
        acknowledged_at: None,
        correlation_id: None,
    }
}

fn with_mail(a: &mut App) {
    let m = mail(a, 2, 0, "which hash?", "bcrypt or argon2?");
    let mut snap = a.snap.clone().unwrap();
    snap.recent_messages = vec![m];
    a.apply_snapshot(snap);
}

fn open_board(a: &mut App) {
    press(a, KeyCode::Char('b'));
    assert_eq!(a.view, View::Canvas);
    let _ = render_text(a, a.size.0, a.size.1);
}

fn pos_of(a: &App, k: &NodeKey) -> (f32, f32) {
    build_scene(a).node(k).unwrap().at
}

fn screen_of(a: &App, k: &NodeKey) -> (i32, i32) {
    let hits = a.canvas.hits.borrow();
    let ((x, y, _, _), _) = hits.iter().find(|(_, key)| key == k).expect("on screen");
    (*x, *y)
}

#[test]
fn the_board_is_derived_from_the_whole_project() {
    let mut a = app();
    with_mail(&mut a);
    let scene = build_scene(&a);
    let has = |k: NodeKey| scene.nodes.iter().any(|n| n.key == k);
    assert!(has(NodeKey::Goal) && has(NodeKey::Plan) && has(NodeKey::User));
    for t in a.tasks() {
        assert!(has(NodeKey::Task(t.key.clone())), "{}", t.key);
    }
    for ag in a.agents() {
        assert!(has(NodeKey::Agent(ag.id.0.clone())) && has(NodeKey::Chat(ag.id.0.clone())));
    }
    assert!(
        has(NodeKey::Mail("msg_board_1".into())),
        "a recent message is a node"
    );
    // edges follow the data: one per dependency, one per owner, a conversation per agent
    let count = |k: EdgeKind| scene.edges.iter().filter(|e| e.kind == k).count();
    assert_eq!(
        count(EdgeKind::Depends),
        a.tasks()
            .iter()
            .map(|t| t.dependencies.len())
            .sum::<usize>()
    );
    assert_eq!(
        count(EdgeKind::Assigned),
        a.tasks()
            .iter()
            .filter(|t| t.owner_agent_id.is_some())
            .count()
    );
    assert_eq!(count(EdgeKind::Talks), a.agents().len());
    assert_eq!(count(EdgeKind::Mail), 2, "sender -> message -> recipient");
    for e in &scene.edges {
        assert!(has(e.from.clone()) && has(e.to.clone()));
    }
    // no two nodes share a slot
    for (i, x) in scene.nodes.iter().enumerate() {
        for y in &scene.nodes[i + 1..] {
            assert_ne!(x.at, y.at, "{:?} and {:?} overlap", x.key, y.key);
        }
    }
    // a task sits to the right of the tasks it waits for
    for t in a.tasks() {
        for d in &t.dependencies {
            let dep = a.tasks().iter().find(|x| &x.id == d).unwrap();
            let tx = pos_of(&a, &NodeKey::Task(t.key.clone())).0;
            let dx = pos_of(&a, &NodeKey::Task(dep.key.clone())).0;
            assert!(tx > dx, "{} should be right of {}", t.key, dep.key);
        }
    }
}

#[test]
fn b_opens_the_board_with_everything_in_view() {
    let mut a = app();
    open_board(&mut a);
    let t = render_text(&a, 170, 44);
    assert!(
        t.contains("BOARD") && t.contains("T-1") && t.contains("codex-back"),
        "{t}"
    );
    let n = build_scene(&a).nodes.len();
    assert_eq!(
        a.canvas.hits.borrow().len(),
        n,
        "every node is on the first screen\n{t}"
    );
    assert!(t.lines().next().unwrap().contains("[b:Board]"), "{t}");
}

#[test]
fn the_board_never_panics_at_any_size_or_zoom() {
    let mut a = app();
    with_mail(&mut a);
    open_board(&mut a);
    for z in 0..ZOOM_LEVELS {
        a.canvas.zoom = z;
        for (w, h) in [(20, 5), (30, 8), (40, 12), (80, 24), (170, 44), (300, 80)] {
            let _ = at(&mut a, View::Canvas, w, h);
        }
    }
    // far from everything it is just an empty grid
    a.canvas.origin = (10_000.0, -10_000.0);
    assert!(at(&mut a, View::Canvas, 120, 36).contains("BOARD"));
}

#[test]
fn arrows_tab_zoom_and_pan_work() {
    let mut a = app();
    open_board(&mut a);
    press(&mut a, KeyCode::Tab);
    let first = a.canvas.selected.clone().expect("selected");
    press(&mut a, KeyCode::Right);
    let second = a.canvas.selected.clone().unwrap();
    assert_ne!(first, second);
    press(&mut a, KeyCode::Left);
    assert_eq!(a.canvas.selected.as_ref(), Some(&first), "Left goes back");

    // selecting points the rest of the TUI at the same thing, so its commands act on it
    let scene = build_scene(&a);
    maw_tui::canvas_input::select(&mut a, &scene, Some(NodeKey::Task("T-3".into())));
    assert_eq!(a.selected_task().map(|t| t.key.as_str()), Some("T-3"));
    let ag = scene
        .nodes
        .iter()
        .find(|n| matches!(n.key, NodeKey::Agent(_)))
        .unwrap();
    maw_tui::canvas_input::select(&mut a, &scene, Some(ag.key.clone()));
    assert!(matches!(&ag.key, NodeKey::Agent(id) if &a.selected_agent().unwrap().id.0 == id));

    // + and - change the zoom
    let z0 = a.canvas.zoom;
    press(&mut a, KeyCode::Char('+'));
    assert_eq!(a.canvas.zoom, (z0 + 1).min(ZOOM_LEVELS - 1));
    press(&mut a, KeyCode::Char('-'));
    press(&mut a, KeyCode::Char('-'));
    assert!(a.canvas.zoom < z0 + 1);

    // Shift+arrows pan and leave the selection alone
    let sel = a.canvas.selected.clone();
    let o = a.canvas.origin;
    handle_key(&mut a, KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert!(a.canvas.origin.0 > o.0);
    assert_eq!(a.canvas.selected, sel);

    // 0 shows everything again
    a.canvas.origin = (50.0, 50.0);
    press(&mut a, KeyCode::Char('0'));
    let _ = render_text(&a, 170, 44);
    assert_eq!(a.canvas.hits.borrow().len(), scene.nodes.len());
}

#[test]
fn the_mouse_selects_moves_pans_and_opens() {
    let mut a = app();
    open_board(&mut a);
    let t2 = NodeKey::Task("T-2".into());
    let (x, y) = screen_of(&a, &t2);
    // a click selects
    handle_mouse(
        &mut a,
        mouse(MouseEventKind::Down(MouseButton::Left), x + 1, y),
    );
    assert_eq!(a.canvas.selected, Some(t2.clone()));
    // dragging moves it by the distance dragged; letting go saves the layout
    let before = pos_of(&a, &t2);
    handle_mouse(
        &mut a,
        mouse(MouseEventKind::Drag(MouseButton::Left), x + 1, y + 6),
    );
    let acts = handle_mouse(
        &mut a,
        mouse(MouseEventKind::Up(MouseButton::Left), x + 1, y + 6),
    );
    let after = pos_of(&a, &t2);
    assert!(
        after.1 > before.1 && (after.0 - before.0).abs() < 0.3,
        "{before:?} -> {after:?}"
    );
    assert!(
        matches!(acts.as_slice(), [Action::SaveCanvas(j)] if j.contains("task:T-2")),
        "{acts:?}"
    );
    // dragging empty space pans the whole view
    let _ = render_text(&a, 170, 44);
    let o = a.canvas.origin;
    handle_mouse(&mut a, mouse(MouseEventKind::Down(MouseButton::Left), 5, 6));
    handle_mouse(
        &mut a,
        mouse(MouseEventKind::Drag(MouseButton::Left), 15, 9),
    );
    handle_mouse(&mut a, mouse(MouseEventKind::Up(MouseButton::Left), 15, 9));
    assert!(
        a.canvas.origin.0 < o.0 && a.canvas.origin.1 < o.1,
        "{o:?} -> {:?}",
        a.canvas.origin
    );
    // double click opens a task on the task board
    let _ = render_text(&a, 170, 44);
    let (x, y) = screen_of(&a, &NodeKey::Task("T-4".into()));
    for _ in 0..2 {
        handle_mouse(
            &mut a,
            mouse(MouseEventKind::Down(MouseButton::Left), x + 1, y),
        );
        handle_mouse(
            &mut a,
            mouse(MouseEventKind::Up(MouseButton::Left), x + 1, y),
        );
    }
    assert_eq!(a.view, View::Tasks);
    assert_eq!(a.selected_task().map(|t| t.key.as_str()), Some("T-4"));
}

#[test]
fn enter_opens_conversations_messages_and_the_objective() {
    let mut a = app();
    with_mail(&mut a);
    open_board(&mut a);
    let scene = build_scene(&a);
    let select = |a: &mut App, k: NodeKey| maw_tui::canvas_input::select(a, &scene, Some(k));
    // a conversation opens that agent's conversation
    let chat = scene
        .nodes
        .iter()
        .find(|n| matches!(n.key, NodeKey::Chat(_)))
        .unwrap()
        .key
        .clone();
    select(&mut a, chat);
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.view, View::AgentDetail);
    press(&mut a, KeyCode::Esc);
    assert_eq!(a.view, View::Canvas, "Esc comes back to the board");
    // a message shows its text
    select(&mut a, NodeKey::Mail("msg_board_1".into()));
    press(&mut a, KeyCode::Enter);
    match &a.overlay {
        Overlay::Output(o) => {
            assert!(
                o.lines.iter().any(|l| l.contains("bcrypt or argon2?")),
                "{o:?}"
            )
        }
        o => panic!("{o:?}"),
    }
    press(&mut a, KeyCode::Esc);
    // the objective opens the goal view
    select(&mut a, NodeKey::Goal);
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.view, View::Goal);
}

#[test]
fn from_the_board_you_create_tasks_and_plans_and_act_on_a_task() {
    let mut a = app();
    open_board(&mut a);
    let scene = build_scene(&a);
    // n with an agent selected offers that agent as the assignee
    let worker = scene
        .nodes
        .iter()
        .find(|n| matches!(&n.key, NodeKey::Agent(id) if a.agents().iter().any(|g| &g.id.0 == id && g.name == "codex-backend")))
        .unwrap()
        .key
        .clone();
    maw_tui::canvas_input::select(&mut a, &scene, Some(worker));
    press(&mut a, KeyCode::Char('n'));
    match &a.overlay {
        Overlay::Dialog(d) => {
            assert_eq!(d.kind, DialogKind::NewTask);
            assert_eq!(d.fields[3].value(), "codex-backend");
        }
        o => panic!("{o:?}"),
    }
    press(&mut a, KeyCode::Esc);
    // P writes a plan revision; milestones can name the tasks they cover
    press(&mut a, KeyCode::Char('P'));
    assert!(matches!(&a.overlay, Overlay::Dialog(d) if d.kind == DialogKind::NewPlan));
    type_text(&mut a, "Ship login first");
    press(&mut a, KeyCode::Enter);
    type_text(&mut a, "Backend [T-1 T-2]; Docs");
    press(&mut a, KeyCode::Enter);
    type_text(&mut a, "write tests; release");
    press(&mut a, KeyCode::Enter);
    let acts = press(&mut a, KeyCode::Enter);
    match acts.as_slice() {
        [Action::ProposePlan {
            summary,
            milestones,
            next_actions,
            reason,
        }] => {
            assert_eq!(summary, "Ship login first");
            assert_eq!(
                milestones[0],
                (
                    "Backend".to_string(),
                    vec!["T-1".to_string(), "T-2".to_string()]
                )
            );
            assert_eq!(milestones[1], ("Docs".to_string(), vec![]));
            assert_eq!(next_actions.len(), 2);
            assert_eq!(reason, "revised by the user");
        }
        o => panic!("{o:?}"),
    }
    // x on a selected task opens the cancel dialog for it
    let scene = build_scene(&a);
    maw_tui::canvas_input::select(&mut a, &scene, Some(NodeKey::Task("T-4".into())));
    press(&mut a, KeyCode::Char('x'));
    assert!(
        matches!(&a.overlay, Overlay::Dialog(d) if matches!(&d.kind, DialogKind::CancelTask { key } if key == "T-4")),
        "{:?}",
        a.overlay
    );
}

#[test]
fn the_layout_survives_a_restart_and_garbage_is_ignored() {
    use maw_tui::canvas::CanvasState;
    let mut c = CanvasState {
        zoom: 3,
        origin: (2.5, -1.25),
        ..Default::default()
    };
    c.moved
        .insert(NodeKey::Task("T-9".into()).id(), (4.25, 1.5));
    let saved = c.to_json();
    let mut d = CanvasState::default();
    d.load_json(&saved);
    assert_eq!((d.zoom, d.origin), (3, (2.5, -1.25)));
    assert_eq!(d.moved.get("task:T-9"), Some(&(4.25, 1.5)));
    assert!(
        d.framed,
        "a saved view is not re-fitted over the user's choice"
    );
    let mut e = CanvasState::default();
    for junk in [
        "",
        "not json",
        "[]",
        r#"{"zoom":99,"moved":{"nope":[1,2],"task:T-1":["x",1]}}"#,
    ] {
        e.load_json(junk);
    }
    assert!(e.moved.is_empty() && e.zoom < ZOOM_LEVELS);
}

#[test]
fn edges_are_routed_with_corners_and_an_arrow_head() {
    use maw_tui::ui::canvas_view::route;
    // to the right and lower: out of the right side, down in the middle, into the left side
    let r = route((0, 0, 10, 3), (30, 8, 10, 3));
    assert_eq!(r.first().map(|c| (c.0, c.1)), Some((10, 1)));
    assert_eq!(r.last().map(|c| (c.0, c.1, c.2)), Some((29, 9, '▶')));
    assert!(
        r.iter().any(|c| c.2 == '┐')
            && r.iter().any(|c| c.2 == '└')
            && r.iter().any(|c| c.2 == '│')
    );
    // straight across: no corners
    let r = route((0, 0, 10, 3), (30, 0, 10, 3));
    assert!(r.iter().all(|c| c.2 == '─' || c.2 == '▶'), "{r:?}");
    assert_eq!(route((40, 0, 10, 3), (0, 0, 10, 3)).last().unwrap().2, '◀');
    // directly below: leaves the bottom, enters the top
    let r = route((0, 0, 10, 3), (0, 10, 10, 3));
    assert_eq!(r.last().unwrap().2, '▼');
    assert_eq!(r.first().map(|c| c.1), Some(3));
}
