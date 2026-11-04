//! Data Flow view: a layered Leader → MCP/Orchestrator → Workers diagram drawn with box
//! characters, plus an edge table and timeline. Everything is derived from stored events
//! (see `flow::derive_flow`); when the area is too small only the edge list/timeline is shown.

use super::widgets::*;
use crate::app::App;
use crate::events::{describe, is_stream_worthy};
use crate::flow::{dependency_edges, derive_flow, FlowGraph, FlowNode, ORCHESTRATOR, USER};
use maw_core::{Event, Role};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// Seconds an edge counts as "active" after its last event.
pub const ACTIVE_SECS: i64 = 45;

pub fn flow_events(app: &App) -> &[Event] {
    if !app.extra_events.is_empty() {
        &app.extra_events
    } else {
        app.snap
            .as_ref()
            .map(|s| s.recent_events.as_slice())
            .unwrap_or(&[])
    }
}

pub fn graph_for(app: &App) -> FlowGraph {
    derive_flow(app.agents(), flow_events(app), app.now, ACTIVE_SECS)
}

// ------------------------------------------------------------------ char canvas

struct Canvas {
    w: usize,
    cells: Vec<Vec<(char, Style)>>,
}

impl Canvas {
    fn new(w: usize, h: usize, base: Style) -> Canvas {
        Canvas {
            w,
            cells: vec![vec![(' ', base); w]; h],
        }
    }
    fn put(&mut self, x: usize, y: usize, c: char, st: Style) {
        if let Some(row) = self.cells.get_mut(y) {
            if x < self.w {
                row[x] = (c, st);
            }
        }
    }
    fn text(&mut self, x: usize, y: usize, s: &str, st: Style) {
        let mut cx = x;
        for c in s.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if cw == 0 {
                continue;
            }
            self.put(cx, y, c, st);
            if cw == 2 {
                self.put(cx + 1, y, ' ', st);
            }
            cx += cw;
        }
    }
    fn hline(&mut self, x1: usize, x2: usize, y: usize, st: Style) {
        for x in x1.min(x2)..=x1.max(x2) {
            self.put(x, y, '─', st);
        }
    }
    fn bx(&mut self, x: usize, y: usize, w: usize, h: usize, st: Style) {
        if w < 2 || h < 2 {
            return;
        }
        self.put(x, y, '┌', st);
        self.put(x + w - 1, y, '┐', st);
        self.put(x, y + h - 1, '└', st);
        self.put(x + w - 1, y + h - 1, '┘', st);
        for i in 1..w - 1 {
            self.put(x + i, y, '─', st);
            self.put(x + i, y + h - 1, '─', st);
        }
        for j in 1..h - 1 {
            self.put(x, y + j, '│', st);
            self.put(x + w - 1, y + j, '│', st);
            for i in 1..w - 1 {
                self.put(x + i, y + j, ' ', st);
            }
        }
    }
    fn centered(&mut self, x: usize, w: usize, y: usize, s: &str, st: Style) {
        let tw = s.width().min(w);
        let s = truncate(s, w);
        self.text(x + (w - tw) / 2, y, &s, st);
    }
    fn into_lines(self) -> Vec<Line<'static>> {
        self.cells
            .into_iter()
            .map(|row| {
                let mut spans: Vec<Span<'static>> = vec![];
                let mut cur = String::new();
                let mut cur_st: Option<Style> = None;
                for (c, st) in row {
                    if cur_st == Some(st) {
                        cur.push(c);
                    } else {
                        if let Some(s) = cur_st {
                            spans.push(Span::styled(std::mem::take(&mut cur), s));
                        }
                        cur.push(c);
                        cur_st = Some(st);
                    }
                }
                if let Some(s) = cur_st {
                    spans.push(Span::styled(cur, s));
                }
                Line::from(spans)
            })
            .collect()
    }
}

pub const GRAPH_HEIGHT: usize = 17;

fn role_rank(r: Option<Role>) -> u8 {
    match r {
        Some(Role::Worker) => 0,
        Some(Role::Specialist) => 1,
        Some(Role::Reviewer) => 2,
        _ => 3,
    }
}

/// Draw the layered diagram. Returns `None` if `width` cannot fit all boxes.
pub fn draw_graph(app: &App, g: &FlowGraph, width: usize) -> Option<Vec<Line<'static>>> {
    let t = &app.theme;
    let top: Vec<&FlowNode> = g
        .nodes
        .iter()
        .filter(|n| n.role == Some(Role::Leader) || n.id == USER)
        .collect();
    let mut bottom: Vec<&FlowNode> = g
        .nodes
        .iter()
        .filter(|n| n.id != ORCHESTRATOR && n.id != USER && n.role != Some(Role::Leader))
        .collect();
    bottom.sort_by_key(|n| role_rank(n.role));
    let nb = bottom.len().max(1);
    let nt = top.len().max(1);
    let min_box = 14;
    if nb * min_box + (nb - 1) > width || nt * min_box + (nt - 1) > width {
        return None;
    }
    let bw_b = ((width - (nb - 1)) / nb).clamp(min_box, 28);
    let bw_t = ((width - (nt - 1)) / nt).clamp(min_box, 28);
    let total_b = nb * bw_b + (nb - 1);
    let total_t = nt * bw_t + (nt - 1);
    let off_b = (width - total_b) / 2;
    let off_t = (width - total_t) / 2;
    let cx = width / 2;
    let mut cv = Canvas::new(width, GRAPH_HEIGHT, t.base());

    let active_nodes: std::collections::HashSet<&str> = g
        .edges
        .iter()
        .filter(|e| e.active)
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();
    let draw_node = |cv: &mut Canvas, n: &FlowNode, x: usize, y: usize, w: usize| {
        let active = active_nodes.contains(n.id.as_str());
        let border = if active { t.accent() } else { t.dim() };
        cv.bx(x, y, w, 4, border);
        let label = match n.role {
            Some(r) => format!(
                "{} [{}]",
                n.label,
                r.as_str().to_uppercase().chars().next().unwrap_or('?')
            ),
            None => n.label.clone(),
        };
        cv.centered(x + 1, w - 2, y + 1, &label, t.bold());
        let (up, down) = g.up_down(&n.id);
        let line2 = match n.state {
            Some(s) => format!("{} ↑{up}↓{down}", s.as_str()),
            None => String::new(),
        };
        let st = n.state.map(|s| t.agent_state(s)).unwrap_or(t.dim());
        cv.centered(x + 1, w - 2, y + 2, &line2, st);
    };

    // top row (leader[s] / user)
    let mut top_centers = vec![];
    for (i, n) in top.iter().enumerate() {
        let x = off_t + i * (bw_t + 1);
        draw_node(&mut cv, n, x, 0, bw_t);
        top_centers.push(x + bw_t / 2);
    }
    // orchestrator
    let ow = 30.min(width.saturating_sub(2)).max(14);
    let ox = cx.saturating_sub(ow / 2);
    let oy = 6;
    let total_edges: usize = g.edges.iter().map(|e| e.count).sum();
    let orch_active = g.edges.iter().any(|e| e.active);
    cv.bx(
        ox,
        oy,
        ow,
        4,
        if orch_active { t.accent() } else { t.dim() },
    );
    cv.centered(ox + 1, ow - 2, oy + 1, "MAW MCP / Orchestrator", t.bold());
    cv.centered(
        ox + 1,
        ow - 2,
        oy + 2,
        &format!(
            "{total_edges} flows · {} active",
            g.edges.iter().filter(|e| e.active).count()
        ),
        t.dim(),
    );

    // connectors top <-> orchestrator
    if !top_centers.is_empty() {
        let (mn, mx) = (
            top_centers.iter().copied().min().unwrap_or(cx).min(cx),
            top_centers.iter().copied().max().unwrap_or(cx).max(cx),
        );
        for &xc in &top_centers {
            cv.put(xc, 4, '│', t.dim());
        }
        cv.hline(mn, mx, 5, t.dim());
        for &xc in &top_centers {
            cv.put(xc, 5, if xc == cx { '┼' } else { '┴' }, t.dim());
        }
        if !top_centers.contains(&cx) {
            cv.put(cx, 5, '┬', t.dim());
        }
        // label what flows down/up on the leader link
        let creates: usize = top
            .iter()
            .map(|n| g.count(&n.id, ORCHESTRATOR, crate::flow::EdgeKind::Create))
            .sum();
        let assigns_back: usize = g
            .edges
            .iter()
            .filter(|e| e.from == ORCHESTRATOR && top.iter().any(|n| n.id == e.to))
            .map(|e| e.count)
            .sum();
        let lbl = format!("CREATE TASKS ×{creates}  ·  REPORTS ×{assigns_back}");
        cv.centered(
            0,
            width,
            4,
            &truncate(&lbl, width.saturating_sub(2)),
            t.info(),
        );
        // fix the vertical stubs overwritten by label
        for &xc in &top_centers {
            cv.put(xc, 4, '│', t.dim());
        }
    }

    // connectors orchestrator <-> bottom
    let mut bot_centers = vec![];
    for (i, n) in bottom.iter().enumerate() {
        let x = off_b + i * (bw_b + 1);
        draw_node(&mut cv, n, x, 12, bw_b);
        bot_centers.push(x + bw_b / 2);
    }
    if !bottom.is_empty() {
        let (mn, mx) = (
            bot_centers.iter().copied().min().unwrap_or(cx).min(cx),
            bot_centers.iter().copied().max().unwrap_or(cx).max(cx),
        );
        cv.put(cx, 10, '│', t.dim());
        cv.hline(mn, mx, 11, t.dim());
        for &xc in &bot_centers {
            cv.put(xc, 11, if xc == cx { '┼' } else { '┬' }, t.dim());
        }
        if !bot_centers.contains(&cx) {
            cv.put(cx, 11, '┴', t.dim());
        }
        let assigns: usize = g
            .edges
            .iter()
            .filter(|e| {
                e.from == ORCHESTRATOR
                    && bottom.iter().any(|n| n.id == e.to)
                    && e.kind == crate::flow::EdgeKind::Assign
            })
            .map(|e| e.count)
            .sum();
        let results: usize = g
            .edges
            .iter()
            .filter(|e| {
                e.to == ORCHESTRATOR
                    && bottom.iter().any(|n| n.id == e.from)
                    && matches!(
                        e.kind,
                        crate::flow::EdgeKind::Submit | crate::flow::EdgeKind::Review
                    )
            })
            .map(|e| e.count)
            .sum();
        let lbl = format!("ASSIGN ×{assigns}  ↓   ↑  RESULTS/REVIEW ×{results}");
        let lx = cx
            .saturating_sub(lbl.width() / 2 + 2)
            .min(width.saturating_sub(lbl.width() + 1));
        cv.text(lx, 10, &truncate(&lbl, width.saturating_sub(lx)), t.info());
        cv.put(cx, 10, '│', t.dim());
    }
    // direct links summary under the bottom row
    let direct = g.direct_edges();
    if !direct.is_empty() {
        let mut parts: Vec<String> = vec![];
        for e in direct.iter().take(3) {
            parts.push(format!(
                "{}⇒{} {}×{}",
                g.label(&e.from),
                g.label(&e.to),
                e.kind.label(),
                e.count
            ));
        }
        cv.text(
            0,
            GRAPH_HEIGHT - 1,
            &truncate(&format!("direct: {}", parts.join("  ")), width),
            t.magenta(),
        );
    }
    Some(cv.into_lines())
}

pub fn edge_table(app: &App, g: &FlowGraph, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut l: Vec<Line> = vec![Line::from(Span::styled(
        "EDGES (derived from stored events; ● active, ○ historical)",
        t.accent(),
    ))];
    if g.edges.is_empty() {
        l.push(Line::from(Span::styled(
            "  no coordination events yet",
            t.dim(),
        )));
    }
    for e in &g.edges {
        let ago = fmt_ago((app.now - e.last_at).num_seconds());
        let st = if e.active { t.ok() } else { t.dim() };
        let head = format!(
            "  {} {:<14} → {:<14} {:<12} ×{:<3} {:>4} ",
            if e.active { "●" } else { "○" },
            truncate(&g.label(&e.from), 14),
            truncate(&g.label(&e.to), 14),
            e.kind.label(),
            e.count,
            ago
        );
        let rest = truncate(&e.detail, width.saturating_sub(head.width()));
        l.push(Line::from(vec![
            Span::styled(head, st),
            Span::styled(rest, t.dim()),
        ]));
    }
    l
}

pub fn dependency_section(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let deps = dependency_edges(app.tasks());
    let mut l: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "TASK DEPENDENCIES (blocked edges in red)",
            t.accent(),
        )),
    ];
    if deps.is_empty() {
        l.push(Line::from(Span::styled("  none", t.dim())));
    }
    for (task, dep, ok) in deps {
        let s = format!(
            "  {task} ⇠ {dep}  {}",
            if ok { "satisfied" } else { "WAITING" }
        );
        l.push(Line::from(Span::styled(
            truncate(&s, width),
            if ok { t.dim() } else { t.err() },
        )));
    }
    l
}

pub fn timeline(app: &App, width: usize, max: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let name_of = |id: &str| app.agent_name(id);
    let mut l: Vec<Line> = vec![
        Line::raw(""),
        Line::from(Span::styled("TIMELINE", t.accent())),
    ];
    let evs: Vec<_> = flow_events(app)
        .iter()
        .filter(|e| is_stream_worthy(e.event_type))
        .collect();
    for e in evs.iter().rev().take(max).rev() {
        let d = describe(e, &name_of);
        l.push(Line::from(vec![
            Span::styled(format!("  {} ", d.time), t.dim()),
            Span::raw(truncate(&d.text, width.saturating_sub(12))),
        ]));
    }
    l
}

pub fn render_flow_view(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let mode = if app.flow_list_mode { "list" } else { "graph" };
    let block = panel(
        Line::from(Span::styled(
            format!(" DATA FLOW · {mode} · Tab toggles · ↑↓ scroll "),
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let g = graph_for(app);
    let w = inner.width as usize;
    let mut lines: Vec<Line> = vec![];
    let graph = if !app.flow_list_mode && inner.height as usize >= GRAPH_HEIGHT + 3 {
        draw_graph(app, &g, w)
    } else {
        None
    };
    match graph {
        Some(gl) => lines.extend(gl),
        None => {
            lines.push(Line::from(Span::styled(
                if app.flow_list_mode {
                    "(list mode)"
                } else {
                    "(narrow terminal: edge list view)"
                },
                t.dim(),
            )));
            let nodes: Vec<String> = g
                .nodes
                .iter()
                .map(|n| {
                    format!(
                        "{}{}",
                        n.label,
                        n.state
                            .map(|s| format!("[{}]", s.as_str()))
                            .unwrap_or_default()
                    )
                })
                .collect();
            lines.push(Line::from(Span::raw(truncate(
                &format!("nodes: {}", nodes.join(", ")),
                w,
            ))));
        }
    }
    lines.extend(edge_table(app, &g, w));
    lines.extend(dependency_section(app, w));
    lines.extend(timeline(app, w, 12));
    let scroll = clamp_scroll(app.flow_scroll, lines.len(), inner.height as usize);
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
}

/// A short track with a moving marker: the "packet" travelling from sender to receiver. `phase`
/// advances with the clock, and each flow starts at its own place so they do not move in step.
pub fn track(phase: usize, len: usize) -> String {
    let len = len.max(4);
    let pos = phase % len;
    (0..len)
        .map(|i| {
            if i == pos {
                '●'
            } else if (i + 1) % len == pos || (i + 2) % len == pos {
                '▸'
            } else {
                '─'
            }
        })
        .chain(std::iter::once('▶'))
        .collect()
}

/// `/flow`: who is passing work to whom right now, as a live list for the side window. Flows seen
/// in the last `ACTIVE_SECS` get a moving marker; below them the recent events in order.
pub fn render_flow_panel(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let g = graph_for(app);
    let active: Vec<&crate::flow::FlowEdge> = g.edges.iter().filter(|e| e.active).collect();
    let block = panel(
        Line::from(Span::styled(
            format!(
                " FLOW · {} moving now · ↑↓ scroll · Esc closes ",
                active.len()
            ),
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 12 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    // moves twice a second (the screen refreshes at that rate while this window is open)
    let tick = (app.now.timestamp_millis().max(0) / 500) as usize;
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled("AGENTS", t.bold()))];
    for n in g.nodes.iter().filter(|n| n.id != ORCHESTRATOR) {
        let (dot, st) = match n.state {
            Some(s) => (crate::theme::state_dot(s), t.agent_state(s)),
            None => ("●", t.dim()),
        };
        let what = match (n.role, n.state) {
            (Some(r), Some(s)) => format!("  {} {}", r.as_str(), s),
            (Some(r), None) => format!("  {}", r.as_str()),
            (None, Some(s)) => format!("  {s}"),
            (None, None) => String::new(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{dot} "), st),
            Span::styled(n.label.clone(), t.bold()),
            Span::styled(
                truncate(&what, w.saturating_sub(n.label.chars().count() + 3)),
                t.dim(),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("MOVING NOW", t.bold())));
    if active.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  nothing in the last {ACTIVE_SECS} s"),
            t.dim(),
        )));
    }
    for (i, e) in active.iter().take(6).enumerate() {
        lines.push(Line::from(vec![
            Span::styled(g.label(&e.from), t.bold()),
            Span::raw(" "),
            Span::styled(
                track(tick + i * 3 + (e.last_seq.max(0) as usize), 8),
                t.accent(),
            ),
            Span::raw(" "),
            Span::styled(g.label(&e.to), t.bold()),
        ]));
        let age = (app.now - e.last_at).num_seconds().max(0);
        let detail = format!(
            "  {} {} · ×{} · {age}s ago",
            e.kind.label(),
            e.detail,
            e.count
        );
        for part in wrap_text(&detail, w.saturating_sub(1).max(8)) {
            lines.push(Line::from(Span::styled(part, t.dim())));
        }
    }
    if active.len() > 6 {
        lines.push(Line::from(Span::styled(
            format!("  + {} more flows (see /view flow)", active.len() - 6),
            t.dim(),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("RECENT", t.bold())));
    let name_of = |id: &str| app.agent_name(id);
    let evs: Vec<_> = flow_events(app)
        .iter()
        .filter(|e| is_stream_worthy(e.event_type))
        .collect();
    for e in evs.iter().rev().take(14) {
        let d = describe(e, &name_of);
        let text = format!("{} {}", d.time, d.text);
        let parts = wrap_text(&text, w.saturating_sub(1).max(8));
        for (i, part) in parts.into_iter().enumerate() {
            lines.push(Line::from(Span::styled(
                if i == 0 { part } else { format!("   {part}") },
                t.base(),
            )));
        }
    }
    super::agents::render_side_lines(f, inner, app, lines);
}
