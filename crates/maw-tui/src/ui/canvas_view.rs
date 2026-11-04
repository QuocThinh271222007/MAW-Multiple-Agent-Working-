//! Drawing the board (see `crate::canvas`): a dotted grid, orthogonal edges with a marker running
//! along the active ones, node cards at three zoom levels, a minimap and an inspector for the
//! selected node. Everything is drawn straight into the frame buffer so nodes can be partly off
//! screen, and every drawn node is recorded so the mouse can find it again.

use super::widgets::*;
use crate::app::App;
use crate::canvas::Button;
use crate::canvas::{build_scene, card, pitch, EdgeKind, Node, NodeKey, Scene, Tone};
use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

/// Draws into a rectangle of the buffer using coordinates relative to it; anything outside is ignored.
struct Painter<'a> {
    buf: &'a mut Buffer,
    r: Rect,
}

impl Painter<'_> {
    fn put(&mut self, x: i32, y: i32, ch: char, st: Style) {
        if x < 0 || y < 0 || x >= self.r.width as i32 || y >= self.r.height as i32 {
            return;
        }
        if let Some(c) = self
            .buf
            .cell_mut((self.r.x + x as u16, self.r.y + y as u16))
        {
            c.set_char(ch).set_style(st);
        }
    }
    /// Text starting at `x`, at most `max` columns wide.
    fn text(&mut self, x: i32, y: i32, s: &str, st: Style, max: i32) {
        let mut cx = x;
        for ch in s.chars() {
            let w = ch.width().unwrap_or(0) as i32;
            if w == 0 {
                continue;
            }
            if cx + w > x + max {
                break;
            }
            self.put(cx, y, ch, st);
            if w == 2 {
                self.put(cx + 1, y, ' ', st);
            }
            cx += w;
        }
    }
    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, st: Style) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, ' ', st);
            }
        }
    }
}

fn tone_style(t: &Theme, tone: Tone) -> Style {
    match tone {
        Tone::Plain => t.base(),
        Tone::Ok => t.ok(),
        Tone::Active => t.accent(),
        Tone::Warn => t.warn(),
        Tone::Bad => t.err(),
        Tone::Dim => t.dim(),
    }
}

/// The cells of an orthogonal route from one card to another, in order from the sender to the
/// receiver, each with the line character that fits its neighbours; the last one is the arrow head.
pub fn route(a: (i32, i32, u16, u16), b: (i32, i32, u16, u16)) -> Vec<(i32, i32, char)> {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let (aw, ah, bw, bh) = (aw as i32, ah as i32, bw as i32, bh as i32);
    // waypoints: leave one side, run to the middle, turn, enter the other side
    let pts: Vec<(i32, i32)> = if ax + aw + 1 < bx {
        let (sx, sy, ex, ey) = (ax + aw, ay + ah / 2, bx - 1, by + bh / 2);
        let mid = (sx + ex) / 2;
        vec![(sx, sy), (mid, sy), (mid, ey), (ex, ey)]
    } else if bx + bw + 1 < ax {
        let (sx, sy, ex, ey) = (ax - 1, ay + ah / 2, bx + bw, by + bh / 2);
        let mid = (sx + ex) / 2;
        vec![(sx, sy), (mid, sy), (mid, ey), (ex, ey)]
    } else {
        let (sx, ex) = (ax + aw / 2, bx + bw / 2);
        if by >= ay + ah {
            let (sy, ey) = (ay + ah, by - 1);
            let mid = (sy + ey) / 2;
            vec![(sx, sy), (sx, mid), (ex, mid), (ex, ey)]
        } else {
            let (sy, ey) = (ay - 1, by + bh);
            let mid = (sy + ey) / 2;
            vec![(sx, sy), (sx, mid), (ex, mid), (ex, ey)]
        }
    };
    // expand the waypoints into single cells
    let mut cells: Vec<(i32, i32)> = vec![pts[0]];
    for w in pts.windows(2) {
        let (mut x, mut y) = w[0];
        let (tx, ty) = w[1];
        while (x, y) != (tx, ty) {
            x += (tx - x).signum();
            y += (ty - y).signum();
            if cells.last() != Some(&(x, y)) {
                cells.push((x, y));
            }
            // never loop on a degenerate pair
            if cells.len() > 4000 {
                break;
            }
        }
    }
    let dir =
        |from: (i32, i32), to: (i32, i32)| ((to.0 - from.0).signum(), (to.1 - from.1).signum());
    let mut out = Vec::with_capacity(cells.len());
    for i in 0..cells.len() {
        let din = if i > 0 {
            dir(cells[i - 1], cells[i])
        } else {
            (0, 0)
        };
        let dout = if i + 1 < cells.len() {
            dir(cells[i], cells[i + 1])
        } else {
            (0, 0)
        };
        let ch = if i + 1 == cells.len() {
            match din {
                (1, _) => '▶',
                (-1, _) => '◀',
                (_, 1) => '▼',
                _ => '▲',
            }
        } else {
            let (i0, o0) = (if i == 0 { dout } else { din }, dout);
            match (i0, o0) {
                ((_, 0), (_, 0)) if i0.0 != 0 => '─',
                ((0, _), (0, _)) => '│',
                ((1, 0), (0, 1)) | ((0, -1), (-1, 0)) => '┐',
                ((1, 0), (0, -1)) | ((0, 1), (-1, 0)) => '┘',
                ((-1, 0), (0, 1)) | ((0, -1), (1, 0)) => '┌',
                ((-1, 0), (0, -1)) | ((0, 1), (1, 0)) => '└',
                _ => '─',
            }
        };
        out.push((cells[i].0, cells[i].1, ch));
    }
    out
}

fn draw_card(
    p: &mut Painter,
    n: &Node,
    (x, y, w, h): (i32, i32, u16, u16),
    selected: bool,
    t: &Theme,
) {
    let tone = tone_style(t, n.tone);
    let w = w as i32;
    let h = h as i32;
    if h <= 1 {
        // one line: icon, short tag, title
        // tasks and the objective keep their title next to the short name; everything else is
        // known by its short name alone (an agent's name, "mail" ...)
        let with_title = matches!(n.key, NodeKey::Task(_) | NodeKey::Goal | NodeKey::Plan);
        let body = if with_title {
            format!("{} {}", n.short, n.title)
        } else {
            n.short.clone()
        };
        let label = if w < 20 {
            format!("{} {}", n.icon, body)
        } else {
            format!(" {} {}", n.icon, body)
        };
        let mut st = tone;
        if selected {
            st = st.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        p.fill(x, y, w, 1, st);
        p.text(x, y, &label, st, w);
        return;
    }
    let (tl, tr, bl, br, hz, vt) = if selected {
        ('┏', '┓', '┗', '┛', '━', '┃')
    } else {
        ('┌', '┐', '└', '┘', '─', '│')
    };
    let border = if selected {
        tone.add_modifier(Modifier::BOLD)
    } else {
        tone
    };
    p.fill(x, y, w, h, t.base());
    for xx in x + 1..x + w - 1 {
        p.put(xx, y, hz, border);
        p.put(xx, y + h - 1, hz, border);
    }
    for yy in y + 1..y + h - 1 {
        p.put(x, yy, vt, border);
        p.put(x + w - 1, yy, vt, border);
    }
    p.put(x, y, tl, border);
    p.put(x + w - 1, y, tr, border);
    p.put(x, y + h - 1, bl, border);
    p.put(x + w - 1, y + h - 1, br, border);
    let tag = format!(" {} {} ", n.icon, n.tag);
    let tag_st = if selected {
        border.add_modifier(Modifier::REVERSED)
    } else {
        border.add_modifier(Modifier::BOLD)
    };
    p.text(x + 2, y, &tag, tag_st, w - 4);

    let inner = (w - 4).max(1) as usize;
    let rows = (h - 2) as usize;
    let mut lines: Vec<(String, Style)> = vec![];
    let title_rows = if h >= 5 { 2 } else { 1 };
    let tw = wrap_text(&n.title, inner);
    for r in tw.iter().take(title_rows) {
        lines.push((r.clone(), t.bold()));
    }
    if tw.len() > title_rows {
        if let Some(last) = lines.last_mut() {
            last.0 = truncate(&format!("{}…", last.0), inner);
        }
    }
    for d in &n.details {
        for r in wrap_text(d, inner) {
            lines.push((r, t.dim()));
        }
        if h < 5 && lines.len() >= rows {
            break;
        }
    }
    let total = lines.len();
    for (i, (s, st)) in lines.into_iter().take(rows).enumerate() {
        let s = if i + 1 == rows && total > rows {
            truncate(&format!("{s} …"), inner)
        } else {
            truncate(&s, inner)
        };
        p.text(x + 2, y + 1 + i as i32, &s, st, w - 4);
    }
}

fn hints_for(sel: Option<&NodeKey>) -> &'static str {
    match sel {
        Some(NodeKey::Task(_)) => {
            "Enter open · v verify · x cancel · e release · R requeue · n new task"
        }
        Some(NodeKey::Agent(_)) => {
            "Enter open conversation · m message · C connect · D stop · i interrupt · n task for it"
        }
        Some(NodeKey::Chat(_)) => "Enter open the conversation · m message",
        Some(NodeKey::Mail(_)) => "Enter read the message",
        Some(NodeKey::Goal) | Some(NodeKey::Plan) => {
            "Enter open · G objective · P new plan revision · n new task"
        }
        Some(NodeKey::User) => "G set the objective · n new task · P plan",
        None => "arrows select a node · Tab next · G objective · P plan · n task · a agent",
    }
}

pub fn render_canvas(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    if area.width < 30 || area.height < 8 {
        f.render_widget(
            Paragraph::new("The board needs a terminal of at least 30x8 cells.").style(t.dim()),
            area,
        );
        return;
    }
    let canvas_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height - 1,
    };
    app.canvas.view.set((
        canvas_area.x,
        canvas_area.y,
        canvas_area.width,
        canvas_area.height,
    ));
    let scene = build_scene(app);
    let c = &app.canvas;
    let zoom = c.zoom;
    let (cw, ch) = card(zoom);

    // ---- the top bar: buttons you can click, then what the mouse is doing
    {
        let mut buttons = c.buttons.borrow_mut();
        buttons.clear();
        let mut spans: Vec<Span> = vec![Span::styled(" BOARD ", t.header())];
        let mut x = area.x as i32 + 7;
        let btn = |label: &str,
                   b: Button,
                   spans: &mut Vec<Span>,
                   x: &mut i32,
                   buttons: &mut Vec<(crate::canvas::Rect4, Button)>| {
            let text = format!(" {label} ");
            let w = text.chars().count() as u16;
            if *x + w as i32 + 1 > area.x as i32 + area.width as i32 {
                return;
            }
            buttons.push(((*x, area.y as i32, w, 1u16), b));
            spans.push(Span::styled(text, t.selected()));
            spans.push(Span::styled(" ", t.header()));
            *x += w as i32 + 1;
        };
        btn("−", Button::ZoomOut, &mut spans, &mut x, &mut buttons);
        btn("+", Button::ZoomIn, &mut spans, &mut x, &mut buttons);
        btn("fit", Button::Fit, &mut spans, &mut x, &mut buttons);
        btn("◀", Button::Left, &mut spans, &mut x, &mut buttons);
        btn("▲", Button::Up, &mut spans, &mut x, &mut buttons);
        btn("▼", Button::Down, &mut spans, &mut x, &mut buttons);
        btn("▶", Button::Right, &mut spans, &mut x, &mut buttons);
        btn(
            if c.clean {
                "show panels"
            } else {
                "hide panels"
            },
            Button::Panels,
            &mut spans,
            &mut x,
            &mut buttons,
        );
        let mouse = if app.mouse_seen == 0 {
            "mouse: nothing received yet".to_string()
        } else {
            format!("mouse ✓ {}", app.mouse_seen)
        };
        let status = format!(
            " zoom {}/{} · {} nodes · {} ",
            zoom + 1,
            crate::canvas::ZOOM_LEVELS,
            scene.nodes.len(),
            mouse
        );
        let used = x - area.x as i32;
        spans.push(Span::styled(
            truncate(&status, (area.width as i32 - used).max(0) as usize),
            t.header(),
        ));
        let line_w: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let pad = (area.width as usize).saturating_sub(line_w);
        spans.push(Span::styled(" ".repeat(pad), t.header()));
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { height: 1, ..area },
        );
    }
    c.shield.borrow_mut().clear();

    // ---- the canvas itself
    c.hits.borrow_mut().clear();
    let tick = (app.now.timestamp_millis().max(0) / 250) as usize;
    {
        let mut p = Painter {
            buf: f.buffer_mut(),
            r: canvas_area,
        };
        p.fill(
            0,
            0,
            canvas_area.width as i32,
            canvas_area.height as i32,
            t.base(),
        );
        // grid dots, fixed to the world so they move when the view is panned
        let (pw, ph) = pitch(zoom);
        let ox = (c.origin.0 * pw as f32).round() as i32;
        let oy = (c.origin.1 * ph as f32).round() as i32;
        let (gx, gy) = (8, 4);
        for y in 0..canvas_area.height as i32 {
            if (y + oy).rem_euclid(gy) != 0 {
                continue;
            }
            for x in 0..canvas_area.width as i32 {
                if (x + ox).rem_euclid(gx) == 0 {
                    p.put(x, y, '·', t.dim());
                }
            }
        }
        if scene.nodes.is_empty() {
            let msg = "Nothing here yet. G sets an objective, n adds a task, a adds an agent.";
            p.text(
                2,
                canvas_area.height as i32 / 2,
                msg,
                t.dim(),
                canvas_area.width as i32 - 4,
            );
        }
        let rect_of = |n: &Node| {
            let (x, y) = c.to_cell(n.at);
            (x, y, cw, ch)
        };
        // edges first, the ones that are happening now on top
        let mut order: Vec<usize> = (0..scene.edges.len()).collect();
        order.sort_by_key(|&i| scene.edges[i].active);
        for i in order {
            let e = &scene.edges[i];
            let (Some(a), Some(b)) = (scene.node(&e.from), scene.node(&e.to)) else {
                continue;
            };
            let cells = route(rect_of(a), rect_of(b));
            let st = if e.active {
                if e.kind == EdgeKind::Mail {
                    t.warn()
                } else {
                    t.accent()
                }
            } else {
                t.dim()
            };
            let marker = if e.active && !cells.is_empty() {
                Some((tick + i * 3) % cells.len())
            } else {
                None
            };
            for (k, (x, y, chr)) in cells.iter().enumerate() {
                let (chr, st) = match marker {
                    Some(m) if k == m => ('●', st.add_modifier(Modifier::BOLD)),
                    Some(m) if k + 1 == m || k == m + 1 && m + 1 < cells.len() => ('•', st),
                    _ => (*chr, st),
                };
                p.put(*x, *y, chr, st);
            }
        }
        // nodes
        let mut hits = c.hits.borrow_mut();
        let selected_at: Option<NodeKey> = c.selected.clone();
        let mut draw_later: Option<(&Node, crate::canvas::Rect4)> = None;
        for n in &scene.nodes {
            let r = rect_of(n);
            let on_screen = r.0 + (r.2 as i32) > 0
                && r.1 + (r.3 as i32) > 0
                && r.0 < canvas_area.width as i32
                && r.1 < canvas_area.height as i32;
            if !on_screen {
                continue;
            }
            if selected_at.as_ref() == Some(&n.key) {
                draw_later = Some((n, r)); // the selected card is drawn last, on top
                continue;
            }
            draw_card(&mut p, n, r, false, t);
            hits.push((
                (
                    canvas_area.x as i32 + r.0,
                    canvas_area.y as i32 + r.1,
                    r.2,
                    r.3,
                ),
                n.key.clone(),
            ));
        }
        if let Some((n, r)) = draw_later {
            draw_card(&mut p, n, r, true, t);
            hits.push((
                (
                    canvas_area.x as i32 + r.0,
                    canvas_area.y as i32 + r.1,
                    r.2,
                    r.3,
                ),
                n.key.clone(),
            ));
        }
        drop(hits);
        if !c.clean {
            minimap(&mut p, &scene, app, canvas_area);
        }
    }

    // ---- the inspector floats over the bottom of the board while something is selected
    if !c.clean && c.selected.is_some() && area.height >= 12 && area.width >= 40 {
        let insp_h: u16 = if area.height >= 24 { 8 } else { 6 };
        let w = area.width.min(78);
        let r = Rect {
            x: area.x,
            y: area.y + area.height - insp_h,
            width: w,
            height: insp_h,
        };
        f.render_widget(ratatui::widgets::Clear, r);
        inspector(f, r, &scene, app);
        c.shield
            .borrow_mut()
            .push((r.x as i32, r.y as i32, r.width, r.height));
    }
}

/// A small map of the whole board in the corner, with the part on screen outlined.
fn minimap(p: &mut Painter, scene: &Scene, app: &App, area: Rect) {
    let t = &app.theme;
    let (mw, mh) = (24i32, 8i32);
    if (area.width as i32) < mw + 30 || (area.height as i32) < mh + 6 || scene.nodes.len() < 2 {
        return;
    }
    let Some((lo, hi)) = crate::canvas::bounds(scene) else {
        return;
    };
    let (x0, y0) = (area.width as i32 - mw - 1, area.height as i32 - mh - 1);
    p.fill(x0, y0, mw, mh, t.base());
    for x in 0..mw {
        p.put(x0 + x, y0, '─', t.dim());
        p.put(x0 + x, y0 + mh - 1, '─', t.dim());
    }
    for y in 0..mh {
        p.put(x0, y0 + y, '│', t.dim());
        p.put(x0 + mw - 1, y0 + y, '│', t.dim());
    }
    p.text(x0 + 2, y0, " map ", t.dim(), mw - 4);
    // the world spans the nodes and the view, so the outline is always on the map
    let (cw, chh) = (area.width as f32, area.height as f32);
    let (pw, ph) = pitch(app.canvas.zoom);
    let view_lo = app.canvas.origin;
    let view_hi = (
        app.canvas.origin.0 + cw / pw as f32,
        app.canvas.origin.1 + chh / ph as f32,
    );
    let wlo = (lo.0.min(view_lo.0), lo.1.min(view_lo.1));
    let whi = (hi.0.max(view_hi.0), hi.1.max(view_hi.1));
    let sx = (mw - 4) as f32 / (whi.0 - wlo.0).max(0.5);
    let sy = (mh - 4) as f32 / (whi.1 - wlo.1).max(0.5);
    let map = |g: (f32, f32)| {
        (
            x0 + 2 + ((g.0 - wlo.0) * sx) as i32,
            y0 + 2 + ((g.1 - wlo.1) * sy) as i32,
        )
    };
    let (vx0, vy0) = map(view_lo);
    let (vx1, vy1) = map(view_hi);
    for x in vx0..=vx1 {
        p.put(x, vy0, '·', t.accent());
        p.put(x, vy1, '·', t.accent());
    }
    for y in vy0..=vy1 {
        p.put(vx0, y, '·', t.accent());
        p.put(vx1, y, '·', t.accent());
    }
    for n in &scene.nodes {
        let (x, y) = map(n.at);
        let sel = app.canvas.selected.as_ref() == Some(&n.key);
        let st = if sel {
            tone_style(t, n.tone).add_modifier(Modifier::REVERSED)
        } else {
            tone_style(t, n.tone)
        };
        p.put(x, y, if sel { '◆' } else { '▪' }, st);
    }
}

fn inspector(f: &mut Frame, r: Rect, scene: &Scene, app: &App) {
    let t = &app.theme;
    let w = r.width as usize;
    let mut lines: Vec<Line> = vec![Line::from(Span::styled("─".repeat(w), t.dim()))];
    match app.canvas.selected.as_ref().and_then(|k| scene.node(k)) {
        Some(n) => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {} ", n.icon, n.tag),
                    tone_style(t, n.tone).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(&n.title, w.saturating_sub(n.tag.chars().count() + 6)),
                    t.bold(),
                ),
            ]));
            let room = (r.height as usize).saturating_sub(3);
            let mut rows: Vec<String> = vec![];
            for d in &n.details {
                rows.extend(wrap_text(d, w.saturating_sub(2).max(10)));
            }
            let total = rows.len();
            for (i, row) in rows.into_iter().take(room).enumerate() {
                let row = if i + 1 == room && total > room {
                    format!("{row} …")
                } else {
                    row
                };
                lines.push(Line::from(Span::styled(
                    format!(" {}", truncate(&row, w - 2)),
                    t.base(),
                )));
            }
            lines.push(Line::from(Span::styled(
                format!(
                    " {}",
                    truncate(hints_for(app.canvas.selected.as_ref()), w - 2)
                ),
                t.accent(),
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                " Nothing selected. Click a node, or use the arrow keys / Tab.",
                t.dim(),
            )));
            lines.push(Line::from(Span::styled(
                format!(" {}", hints_for(None)),
                t.accent(),
            )));
        }
    }
    lines.truncate(r.height as usize);
    f.render_widget(Paragraph::new(lines), r);
}
