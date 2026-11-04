//! Keys and mouse on the board: selecting, moving, panning, zooming, opening what is selected and
//! starting the dialogs that create things (objective, plan, task). It only changes `App`; anything
//! that has to reach the engine goes out as an `Action`, as everywhere else in the TUI.

use crate::app::{run_cmd, Action, App, Cmd, DialogKind, Level, OutputState, Overlay, View};
use crate::canvas::{build_scene, card, cycle, neighbor, Button, Drag, NodeKey, Scene};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Make `key` the selection and point the rest of the TUI (agent list, task board) at the same thing,
/// so the usual commands (message, verify, cancel ...) act on it.
pub fn select(app: &mut App, scene: &Scene, key: Option<NodeKey>) {
    if let Some(k) = &key {
        match k {
            NodeKey::Agent(id) | NodeKey::Chat(id) => {
                if let Some(i) = app.agents().iter().position(|a| &a.id.0 == id) {
                    app.selected_agent = i;
                }
            }
            NodeKey::Task(tk) => {
                let order = app.board_order();
                if let Some(i) = order.iter().position(|&i| &app.tasks()[i].key == tk) {
                    app.task_sel = i;
                }
            }
            NodeKey::Mail(mid) => {
                // the recipient is the agent whose conversation the message ends up in
                if let Some(m) = app
                    .snap
                    .as_ref()
                    .and_then(|s| s.recent_messages.iter().find(|m| &m.id.0 == mid))
                {
                    if let Some(i) = app
                        .agents()
                        .iter()
                        .position(|a| a.id == m.recipient_agent_id)
                    {
                        app.selected_agent = i;
                    }
                }
            }
            _ => {}
        }
        if let Some(n) = scene.node(k) {
            app.canvas.reveal(n.at);
        }
    }
    app.canvas.selected = key;
}

/// What a double click, Enter or a click on "open" does for each kind of node.
pub fn open(app: &mut App, key: &NodeKey) {
    match key {
        NodeKey::User | NodeKey::Goal | NodeKey::Plan => app.goto(View::Goal),
        NodeKey::Task(_) => {
            app.goto(View::Tasks);
            app.task_detail_focus = true;
        }
        NodeKey::Agent(_) | NodeKey::Chat(_) => app.goto(View::AgentDetail),
        NodeKey::Mail(id) => {
            let Some(m) = app
                .snap
                .as_ref()
                .and_then(|s| s.recent_messages.iter().find(|m| &m.id.0 == id))
                .cloned()
            else {
                return;
            };
            let name = |a: &maw_core::AgentId| app.agent_name(a.as_str());
            let mut lines = vec![
                format!(
                    "{} → {}",
                    name(&m.sender_agent_id),
                    name(&m.recipient_agent_id)
                ),
                format!(
                    "type: {}   sent: {}",
                    m.message_type,
                    m.created_at.format("%H:%M:%S")
                ),
                format!(
                    "status: {}",
                    if m.acknowledged_at.is_some() {
                        "acknowledged"
                    } else if m.delivered_at.is_some() {
                        "delivered"
                    } else {
                        "waiting to be delivered"
                    }
                ),
                String::new(),
            ];
            if !m.subject.trim().is_empty() {
                lines.push(m.subject.clone());
                lines.push(String::new());
            }
            lines.extend(m.content.lines().map(String::from));
            app.overlay = Overlay::Output(OutputState {
                title: "message".into(),
                lines,
                scroll: 0,
            });
        }
    }
}

fn save(app: &mut App, out: &mut Vec<Action>) {
    if app.canvas.dirty && app.canvas.drag.is_none() {
        app.canvas.dirty = false;
        out.push(Action::SaveCanvas(app.canvas.to_json()));
    }
}

fn center(app: &App) -> (i32, i32) {
    let (_, _, w, h) = app.canvas.view.get();
    (w as i32 / 2, h as i32 / 2)
}

/// The agent a node is about (the node itself, or the one a conversation belongs to).
fn agent_of(sel: &Option<NodeKey>) -> bool {
    matches!(
        sel,
        Some(NodeKey::Agent(_)) | Some(NodeKey::Chat(_)) | Some(NodeKey::Mail(_))
    )
}

/// Keys of the board. `None`: not ours, the usual global keys apply (views, help, quit ...).
pub fn canvas_key(app: &mut App, key: &KeyEvent) -> Option<Vec<Action>> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let scene = build_scene(app);
    let mut out = vec![];
    let sel = app.canvas.selected.clone();
    match key.code {
        // arrows move the selection to the nearest node that way; with Shift they pan
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
            let (dx, dy) = match key.code {
                KeyCode::Left => (-1, 0),
                KeyCode::Right => (1, 0),
                KeyCode::Up => (0, -1),
                _ => (0, 1),
            };
            if shift {
                app.canvas.pan_cells(dx * 8, dy * 4);
            } else if let Some(k) = app.canvas.grabbed.clone() {
                // a picked-up node: the arrows nudge it by a quarter slot
                if let Some(n) = scene.node(&k) {
                    let at = (n.at.0 + dx as f32 * 0.25, n.at.1 + dy as f32 * 0.25);
                    app.canvas.moved.insert(k.id(), at);
                    app.canvas.dirty = true;
                }
            } else {
                let next = match &sel {
                    Some(k) => neighbor(&scene, k, dx, dy),
                    None => cycle(&scene, None, false),
                };
                if next.is_some() {
                    select(app, &scene, next);
                }
            }
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            let (_, _, _, h) = app.canvas.view.get();
            let step = (h as i32 - 2).max(4);
            app.canvas.pan_cells(
                0,
                if key.code == KeyCode::PageUp {
                    -step
                } else {
                    step
                },
            );
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let back = key.code == KeyCode::BackTab || shift;
            let next = cycle(&scene, sel.as_ref(), back);
            select(app, &scene, next);
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            let c = center(app);
            app.canvas.set_zoom(app.canvas.zoom + 1, c);
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            let c = center(app);
            app.canvas.set_zoom(app.canvas.zoom.saturating_sub(1), c);
        }
        KeyCode::Char('0') | KeyCode::Home => {
            let (_, _, w, h) = app.canvas.view.get();
            app.canvas.fit(&scene, w, h);
        }
        KeyCode::Enter => match sel {
            Some(k) => open(app, &k),
            None => {
                let first = cycle(&scene, None, false);
                select(app, &scene, first);
            }
        },
        KeyCode::Char('h') => {
            app.canvas.clean = !app.canvas.clean;
        }
        KeyCode::Char('g') => match (app.canvas.grabbed.take(), sel.clone()) {
            (Some(_), _) => {} // second press puts it down where it is
            (None, Some(k)) => {
                app.canvas.grabbed = Some(k);
                app.set_toast(
                    "node picked up: click where it goes (or nudge with the arrows), Esc cancels",
                    Level::Info,
                );
            }
            (None, None) => app.set_toast("select a node first, then g picks it up", Level::Info),
        },
        KeyCode::Esc if app.canvas.grabbed.is_some() => {
            app.canvas.grabbed = None;
        }
        KeyCode::Esc => {
            if sel.is_some() {
                app.canvas.selected = None;
            } else {
                let back = if app.prev_view == View::Canvas {
                    View::Dashboard
                } else {
                    app.prev_view
                };
                app.goto(back);
            }
        }
        // create
        KeyCode::Char('n') => {
            out.extend(run_cmd(app, Cmd::TaskNew));
            // new task for the selected agent: preselect it
            let wanted = if agent_of(&sel) {
                app.agents().get(app.selected_agent).map(|a| a.name.clone())
            } else {
                None
            };
            if let (Overlay::Dialog(d), Some(name)) = (&mut app.overlay, wanted) {
                if d.kind == DialogKind::NewTask {
                    if let Some(i) = d.fields[3].choices.iter().position(|c| c == &name) {
                        d.fields[3].choice = i;
                    }
                }
            }
        }
        KeyCode::Char('P') => out.extend(run_cmd(app, Cmd::PlanNew)),
        // act on what is selected
        KeyCode::Char(c @ ('v' | 'x' | 'e' | 'R')) if matches!(sel, Some(NodeKey::Task(_))) => {
            let cmd = match c {
                'v' => Cmd::TaskVerify,
                'x' => Cmd::TaskCancel,
                'e' => Cmd::TaskRelease,
                _ => Cmd::TaskRequeue,
            };
            out.extend(run_cmd(app, cmd));
        }
        KeyCode::Char(c @ ('m' | 'C' | 'D' | 'i')) if agent_of(&sel) => {
            let cmd = match c {
                'm' => Cmd::MessageAgent,
                'C' => Cmd::ConnectAgent,
                'D' => Cmd::DisconnectAgent,
                _ => Cmd::InterruptAgent,
            };
            out.extend(run_cmd(app, cmd));
        }
        KeyCode::Char(c @ ('v' | 'x' | 'e' | 'R' | 'm' | 'C' | 'D' | 'i')) => {
            app.set_toast(
                format!(
                    "'{c}' acts on the selected {}: select one first",
                    if "vxeR".contains(c) { "task" } else { "agent" }
                ),
                Level::Info,
            );
        }
        _ => return None,
    }
    save(app, &mut out);
    Some(out)
}

fn hit_at(app: &App, col: u16, row: u16) -> Option<(NodeKey, (i32, i32))> {
    let hits = app.canvas.hits.borrow();
    hits.iter().rev().find_map(|((x, y, w, h), k)| {
        let (c, r) = (col as i32, row as i32);
        (c >= *x && c < x + *w as i32 && r >= *y && r < y + *h as i32)
            .then(|| (k.clone(), (*x, *y)))
    })
}

/// Mouse on the board: click selects, double click opens, dragging a node moves it, dragging the
/// background (or with the middle/right button anywhere) pans, the wheel zooms.
pub fn canvas_mouse(app: &mut App, m: MouseEvent) -> Vec<Action> {
    let (vx, vy, vw, vh) = app.canvas.view.get();
    let inside = m.column >= vx && m.column < vx + vw && m.row >= vy && m.row < vy + vh;
    let rel = (m.column as i32 - vx as i32, m.row as i32 - vy as i32);
    let mut out = vec![];
    let at = |r: &(i32, i32, u16, u16)| {
        let (c, rw) = (m.column as i32, m.row as i32);
        c >= r.0 && c < r.0 + r.2 as i32 && rw >= r.1 && rw < r.1 + r.3 as i32
    };
    // the buttons of the top bar work with a plain click
    if let MouseEventKind::Down(MouseButton::Left) = m.kind {
        let hit = app
            .canvas
            .buttons
            .borrow()
            .iter()
            .find(|(r, _)| at(r))
            .map(|(_, b)| *b);
        if let Some(b) = hit {
            press(app, b);
            save(app, &mut out);
            return out;
        }
    }
    // floating panels (the inspector) are not part of the board underneath
    let shielded = matches!(
        m.kind,
        MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && app.canvas.shield.borrow().iter().any(at);
    if shielded {
        return out;
    }
    // a node picked up with `g` goes down where the click is
    if let (MouseEventKind::Down(MouseButton::Left), Some(key), true) =
        (m.kind, app.canvas.grabbed.clone(), inside)
    {
        let (cw, ch) = card(app.canvas.zoom);
        let g = app
            .canvas
            .to_grid((rel.0 - cw as i32 / 2, rel.1 - ch as i32 / 2));
        let snap = |v: f32| (v * 4.0).round() / 4.0;
        app.canvas.moved.insert(key.id(), (snap(g.0), snap(g.1)));
        app.canvas.grabbed = None;
        app.canvas.dirty = true;
        save(app, &mut out);
        return out;
    }
    match m.kind {
        MouseEventKind::Down(btn) if inside => {
            if btn == MouseButton::Left {
                if let Some((key, (nx, ny))) = hit_at(app, m.column, m.row) {
                    let scene = build_scene(app);
                    let now = chrono::Utc::now();
                    let double = app
                        .canvas
                        .last_click
                        .as_ref()
                        .is_some_and(|(t, k)| k == &key && (now - *t).num_milliseconds() < 450);
                    app.canvas.last_click = Some((now, key.clone()));
                    select(app, &scene, Some(key.clone()));
                    if double {
                        app.canvas.drag = None;
                        open(app, &key);
                    } else {
                        app.canvas.drag = Some(Drag::Node {
                            key,
                            grab: (m.column as i32 - nx, m.row as i32 - ny),
                        });
                    }
                    return out;
                }
            }
            // background (or the other buttons): pan
            app.canvas.drag = Some(Drag::Pan {
                origin: app.canvas.origin,
                from: (m.column, m.row),
            });
            if btn == MouseButton::Left {
                app.canvas.selected = None;
            }
        }
        MouseEventKind::Drag(_) => match app.canvas.drag.clone() {
            Some(Drag::Node { key, grab }) => {
                let cell = (rel.0 - grab.0, rel.1 - grab.1);
                let g = app.canvas.to_grid(cell);
                // snap to a quarter slot so things line up without effort
                let snap = |v: f32| (v * 4.0).round() / 4.0;
                app.canvas.moved.insert(key.id(), (snap(g.0), snap(g.1)));
                app.canvas.dirty = true;
            }
            Some(Drag::Pan { origin, from }) => {
                let (pw, ph) = crate::canvas::pitch(app.canvas.zoom);
                let (dx, dy) = (
                    m.column as f32 - from.0 as f32,
                    m.row as f32 - from.1 as f32,
                );
                app.canvas.origin = (origin.0 - dx / pw as f32, origin.1 - dy / ph as f32);
                app.canvas.dirty = true;
            }
            None => {}
        },
        MouseEventKind::Up(_) => {
            if app.canvas.drag.take().is_some() {
                app.canvas.dirty = true;
                save(app, &mut out);
            }
        }
        // Shift+wheel (and a sideways wheel) pans left and right, Alt+wheel up and down
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if inside
                && m.modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            let s = if m.kind == MouseEventKind::ScrollUp {
                -1
            } else {
                1
            };
            if m.modifiers.contains(KeyModifiers::SHIFT) {
                app.canvas.pan_cells(s * 8, 0);
            } else {
                app.canvas.pan_cells(0, s * 4);
            }
            save(app, &mut out);
        }
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if inside => {
            let s = if m.kind == MouseEventKind::ScrollLeft {
                -1
            } else {
                1
            };
            app.canvas.pan_cells(s * 8, 0);
            save(app, &mut out);
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside => {
            let z = app.canvas.zoom;
            let nz = if m.kind == MouseEventKind::ScrollUp {
                z + 1
            } else {
                z.saturating_sub(1)
            };
            app.canvas.set_zoom(nz, rel);
            save(app, &mut out);
        }
        _ => {}
    }
    out
}

/// What a click on one of the top bar's buttons does.
fn press(app: &mut App, b: Button) {
    let c = center(app);
    match b {
        Button::ZoomIn => app.canvas.set_zoom(app.canvas.zoom + 1, c),
        Button::ZoomOut => app.canvas.set_zoom(app.canvas.zoom.saturating_sub(1), c),
        Button::Fit => {
            let scene = build_scene(app);
            let (_, _, w, h) = app.canvas.view.get();
            app.canvas.fit(&scene, w, h);
        }
        Button::Left => app.canvas.pan_cells(-12, 0),
        Button::Right => app.canvas.pan_cells(12, 0),
        Button::Up => app.canvas.pan_cells(0, -6),
        Button::Down => app.canvas.pan_cells(0, 6),
        Button::Panels => app.canvas.clean = !app.canvas.clean,
    }
}

/// Card size in cells at the current zoom, for tests and callers that lay things out.
pub fn card_size(app: &App) -> (u16, u16) {
    card(app.canvas.zoom)
}
