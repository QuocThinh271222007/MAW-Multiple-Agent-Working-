//! Rendering. Pure functions of `&App`: no I/O, no engine calls, deterministic given `app.now`.

pub mod agents;
pub mod flow_view;
pub mod goal;
pub mod misc;
pub mod overlays;
pub mod tasks;
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
        Constraint::Length(1),
    ])
    .split(area);
    render_header(f, rows[0], app);
    render_body(f, rows[1], app);
    render_footer(f, rows[2], app);
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => overlays::render_help(f, rows[1], app),
        Overlay::Palette(p) => overlays::render_palette(f, rows[1], app, p),
        Overlay::Dialog(d) => overlays::render_dialog(f, rows[1], app, d),
    }
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    match app.view {
        View::Dashboard => misc::render_dashboard(f, area, app),
        View::Agents => misc::render_agents_view(f, area, app),
        View::AgentDetail => agents::render_agent_detail(f, area, app),
        View::Tasks => tasks::render_tasks_view(f, area, app),
        View::Goal => goal::render_goal_view(f, area, app),
        View::Flow => flow_view::render_flow_view(f, area, app),
        View::Approvals => misc::render_approvals(f, area, app),
        View::Logs => misc::render_logs(f, area, app),
        View::Config => misc::render_config(f, area, app),
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let w = area.width as usize;
    let (project, paused) = app
        .snap
        .as_ref()
        .map(|s| (s.project.name.clone(), s.paused))
        .unwrap_or_else(|| ("—".into(), false));
    let mode = if app.config.max_concurrency > 1 {
        "Parallel"
    } else {
        "Serial"
    };
    let sched = if paused { "⏸ PAUSED" } else { "▶ running" };
    let mut text = format!(
        " MAW ─ Multiple Agent Working │ {project} │ Agents: {} │ {mode} │ {sched} ",
        app.agents().len()
    );
    if w >= 110 {
        text.push('│');
        for v in View::TABS {
            if v == app.view || (v == View::Agents && app.view == View::AgentDetail) {
                text.push_str(&format!(" [{}:{}]", v.key(), v.title()));
            } else {
                text.push_str(&format!("  {}:{}", v.key(), v.title()));
            }
        }
    } else if w >= 70 {
        text.push_str(&format!("│ {}", app.view.title()));
    }
    let mut line = truncate(&text, w);
    let pad = w.saturating_sub(unicode_width::UnicodeWidthStr::width(line.as_str()));
    line.push_str(&" ".repeat(pad));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(line, t.header()))),
        area,
    );
}

fn footer_hint(app: &App) -> &'static str {
    match app.view {
        View::Dashboard => "Tab panel · 1-9 agent · Enter open · G goal · a add · p/r pause/resume · / cmds · ? help · q quit",
        View::Agents => "←→ select · Enter detail · C connect · D disconnect · i interrupt · m message · ? help",
        View::AgentDetail => "↑↓ PgUp/PgDn scroll · / search n/N · y copy · Y copy all · i interrupt · m message · Esc back",
        View::Tasks => "↑↓←→ select · Enter detail · R requeue · e release · x cancel · v verify · ? help",
        View::Goal => "↑↓ scroll · Enter/G redirect objective · ? help",
        View::Flow => "Tab graph/list · ↑↓ scroll · ? help",
        View::Approvals => "↑↓ select · y approve · Y approve for session · n deny · x deny+abort",
        View::Logs => "↑↓ scroll · End latest · ? help",
        View::Config => "+/- concurrency · [ ] lease · u auto-assign · ? help",
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
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate(&format!(" {}", toast.text), w),
                st,
            ))),
            area,
        );
    } else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate(&format!(" {}", footer_hint(app)), w),
                t.footer(),
            ))),
            area,
        );
    }
}
