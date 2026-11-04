//! Rendering. Pure functions of `&App`: no I/O, no engine calls, deterministic given `app.now`.

pub mod agents;
pub mod canvas_view;
pub mod downloads;
pub mod flow_view;
pub mod goal;
pub mod misc;
pub mod overlays;
pub mod perf_view;
pub mod tasks;
pub mod usage_view;
pub mod widgets;

use crate::app::{App, Level, Overlay, View};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use widgets::truncate;

pub const MIN_WIDTH: u16 = 20;
pub const MIN_HEIGHT: u16 = 5;

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let msg = format!(
            "MAW: terminal too small ({}x{}), need {}x{}",
            area.width, area.height, MIN_WIDTH, MIN_HEIGHT
        );
        f.render_widget(
            Paragraph::new(truncate(
                &msg,
                (area.width as usize).saturating_mul(area.height as usize),
            ))
            .wrap(ratatui::widgets::Wrap { trim: true }),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(gauge_rows(app, area.height)),
        Constraint::Length(footer_rows(app, area.width)),
    ])
    .split(area);
    render_header(f, rows[0], app);
    render_body(f, rows[1], app);
    if rows[2].height > 0 {
        downloads::render_gauge(f, rows[2], app);
    }
    render_footer(f, rows[3], app);
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => overlays::render_help(f, rows[1], app),
        Overlay::Palette(p) => overlays::render_palette(f, rows[1], app, p),
        Overlay::Dialog(d) => overlays::render_dialog(f, rows[1], app, d),
        Overlay::Picker(p) => overlays::render_picker(f, rows[1], app, p),
        Overlay::Output(o) => overlays::render_output(f, rows[1], app, o),
        Overlay::Downloads => downloads::render_downloads(f, rows[1], app),
    }
}

/// One row for the download gauge while a model is downloading (when the screen has room).
fn gauge_rows(app: &App, height: u16) -> u16 {
    u16::from(height >= 10 && app.downloads.active().next().is_some())
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    match app.view {
        View::Dashboard => misc::render_dashboard(f, area, app),
        View::Agents => misc::render_agents_view(f, area, app),
        View::AgentDetail => agents::render_agent_detail(f, area, app),
        View::Tasks => tasks::render_tasks_view(f, area, app),
        View::Goal => goal::render_goal_view(f, area, app),
        View::Flow => flow_view::render_flow_view(f, area, app),
        View::Canvas => canvas_view::render_canvas(f, area, app),
        View::Approvals => misc::render_approvals(f, area, app),
        View::Logs => misc::render_logs(f, area, app),
        View::Config => misc::render_config(f, area, app),
        View::Tokens => usage_view::render_tokens(f, area, app),
        View::Perf => perf_view::render_perf(f, area, app),
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let w = area.width as usize;
    let (project, paused, path) = app
        .snap
        .as_ref()
        .map(|s| (s.project.name.clone(), s.paused, s.project.path.clone()))
        .unwrap_or_else(|| ("—".into(), false, String::new()));
    let mode = if app.config.max_concurrency > 1 {
        "Parallel"
    } else {
        "Serial"
    };
    let sched = if paused { "⏸ PAUSED" } else { "▶ running" };
    let tokens = crate::usage::fmt_tokens(
        app.agents()
            .iter()
            .map(|a| {
                a.usage.total_tokens.unwrap_or(
                    a.usage.input_tokens.unwrap_or(0) + a.usage.output_tokens.unwrap_or(0),
                )
            })
            .sum(),
    );
    // The view keys come FIRST so they are always visible: full names when they fit, otherwise
    // just the keys with the open view named. Status and the working folder follow and may be
    // scrolled (mouse wheel over the bar).
    let status = format!(
        "{project} │ Agents: {} │ Tokens: {tokens} │ {mode}",
        app.agents().len()
    );
    let tabs = |compact: bool| -> String {
        let mut out = String::new();
        for v in View::TABS {
            let active = v == app.view || (v == View::Agents && app.view == View::AgentDetail);
            if active {
                out.push_str(&format!("[{}:{}] ", v.key(), v.title()));
            } else if compact {
                out.push_str(&format!("{} ", v.key()));
            } else {
                out.push_str(&format!("{}:{} ", v.key(), v.title()));
            }
        }
        out.trim_end().to_string()
    };
    let full = tabs(false);
    // full view names whenever they fit on their own; the status after them may scroll
    // running / paused is always right after the name: it must never scroll out of sight
    // full names only when the project name still fits after them
    let mut text = if format!(" MAW {sched} │ {full} │ ").chars().count() + 14 <= w {
        format!(" MAW {sched} │ {full} │ {status} ")
    } else {
        format!(" MAW {sched} │ {} │ {status} ", tabs(true))
    };
    if !path.is_empty() {
        text.push_str(&format!("│ dir: {path} "));
    }
    let line = scrolled(&text, app.header_scroll, w, &app.header_max);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(line, t.header()))),
        area,
    );
}

/// Show a window of `text` starting at `offset` (clamped), exactly `width` columns wide, with
/// `‹` / `›` markers when content is hidden on that side. Records the largest useful offset.
fn scrolled(text: &str, offset: usize, width: usize, max_cell: &std::cell::Cell<usize>) -> String {
    use unicode_width::UnicodeWidthChar;
    let chars: Vec<char> = text.chars().collect();
    let total: usize = chars.iter().map(|c| c.width().unwrap_or(0)).sum();
    let max_off = total.saturating_sub(width);
    max_cell.set(max_off);
    let off = offset.min(max_off);
    // skip `off` columns, then take up to `width` columns
    let (mut skipped, mut used) = (0usize, 0usize);
    let mut out = String::new();
    for c in chars {
        let cw = c.width().unwrap_or(0);
        if skipped < off {
            skipped += cw;
            continue;
        }
        if used + cw > width {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    // markers (replace the first/last visible cell)
    let mut v: Vec<char> = out.chars().collect();
    if off > 0 && !v.is_empty() {
        v[0] = '‹';
    }
    if off < max_off && !v.is_empty() {
        let last = v.len() - 1;
        v[last] = '›';
    }
    v.into_iter().collect()
}

fn footer_hint(app: &App) -> &'static str {
    match app.view {
        View::Dashboard if app.typing() => "type a command: /board · /task new · /goal set · /performance · !git status · Tab = hotkeys · Ctrl+C quit",
        View::Dashboard => "Tab panel · 1-9 agent · Enter open · G goal · a add · $ tokens · % performance · M model · ! shell · / cmds · ? help · q quit",
        View::Agents => "←→ select · Enter detail · C connect · D disconnect · i interrupt · m message · M model/effort · ! shell · ? help",
        View::AgentDetail => "type to message · /command · !shell · Tab browse (y copy · / search · n/N · T L P) · PgUp/PgDn scroll · Esc back",
        View::Tasks => "↑↓←→ select · Enter detail · R requeue · e release · x cancel · v verify · ? help",
        View::Goal => "↑↓ scroll · Enter/G redirect objective · ? help",
        View::Flow => "Tab graph/list · ↑↓ scroll · ? help",
        View::Canvas => "arrows select · drag pans · wheel zoom · Shift+wheel sideways · g pick up · h hide panels · 0 fit · Enter open · n task · P plan · ? help",
        View::Approvals => "↑↓ select · y approve · Y approve for session · n deny · x deny+abort",
        View::Logs => "↑↓ scroll · End latest · ? help",
        View::Config => "+/- concurrency · [ ] lease · u auto-assign · ? help",
        View::Tokens => "token figures are reported by the providers · $ this view · d dashboard · ? help",
        View::Perf => "machine load while models run · w window 1m/5m/30m · % this view · d dashboard · ? help",
    }
}

/// The notice line (toast) may need more than one row; it is wrapped, not cut off.
fn footer_rows(app: &App, width: u16) -> u16 {
    match app
        .toast
        .as_ref()
        .filter(|x| (app.now - x.at).num_seconds() < 8)
    {
        Some(toast) => widgets::wrap_text(&format!(" {}", toast.text), width.max(1) as usize)
            .len()
            .clamp(1, 3) as u16,
        None => 1,
    }
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let w = area.width as usize;
    if let Some(toast) = app
        .toast
        .as_ref()
        .filter(|x| (app.now - x.at).num_seconds() < 8)
    {
        let st: Style = match toast.level {
            Level::Info => t.accent(),
            Level::Ok => t.ok(),
            Level::Error => t.err(),
        };
        let rows: Vec<Line> = widgets::wrap_text(&format!(" {}", toast.text), w.max(1))
            .into_iter()
            .take(area.height.max(1) as usize)
            .map(|r| Line::from(Span::styled(r, st)))
            .collect();
        f.render_widget(Paragraph::new(rows), area);
    } else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                scrolled(
                    &format!(" {}", footer_hint(app)),
                    app.footer_scroll,
                    w,
                    &app.footer_max,
                ),
                t.footer(),
            ))),
            area,
        );
    }
}
