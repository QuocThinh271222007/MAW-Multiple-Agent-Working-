//! Dashboard composition, event stream, approval center, logs & diagnostics, configuration.

use super::agents::render_agents_panel;
use super::flow_view::flow_events;
use super::goal::render_goal_panel;
use super::tasks::render_tasks_summary;
use super::widgets::*;
use crate::app::{App, Panel};
use crate::events::{describe, is_diagnostic, is_stream_worthy, EventClass, EventLine};
use maw_core::{AgentState, ApprovalStatus};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

fn class_style(app: &App, c: EventClass) -> Style {
    match c {
        EventClass::Normal => app.theme.base(),
        EventClass::Good => app.theme.ok(),
        EventClass::Warn => app.theme.warn(),
        EventClass::Bad => app.theme.err(),
        EventClass::Dim => app.theme.dim(),
    }
}

pub fn stream_lines(app: &App) -> Vec<EventLine> {
    let name_of = |id: &str| app.agent_name(id);
    flow_events(app)
        .iter()
        .filter(|e| is_stream_worthy(e.event_type))
        .map(|e| describe(e, &name_of))
        .collect()
}

/// Rows for a list of events: the time, then the text wrapped under itself so none of it is cut.
fn event_rows(app: &App, evs: &[EventLine], w: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let text_w = w.saturating_sub(9).max(8);
    let mut out = vec![];
    for e in evs {
        for (i, row) in wrap_text(&e.text, text_w).into_iter().enumerate() {
            let lead = if i == 0 {
                format!("{} ", e.time)
            } else {
                " ".repeat(e.time.chars().count() + 1)
            };
            out.push(Line::from(vec![
                Span::styled(lead, t.dim()),
                Span::styled(row, class_style(app, e.class)),
            ]));
        }
    }
    out
}

pub fn render_events_panel(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(" EVENT STREAM ", t.bold())),
        focused,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let evs = stream_lines(app);
    let h = inner.height as usize;
    let rows = event_rows(app, &evs, inner.width as usize);
    let scroll = clamp_scroll(app.events_scroll, rows.len(), h);
    let end = rows.len() - scroll;
    let start = end.saturating_sub(h);
    let lines: Vec<Line> = rows[start..end].to_vec();
    if lines.is_empty() {
        f.render_widget(
            Paragraph::new("waiting for activity…").style(t.dim()),
            inner,
        );
    } else {
        f.render_widget(Paragraph::new(lines), inner);
    }
}

pub fn render_dashboard(f: &mut Frame, area: Rect, app: &App) {
    // the command line takes the bottom of the main screen when there is room
    let (area, cmd) = if app.has_cmdline() && area.height >= 14 {
        let box_h = super::agents::input_box_height(app, area.width, area.height);
        let parts = Layout::vertical([Constraint::Min(8), Constraint::Length(box_h)]).split(area);
        (parts[0], Some(parts[1]))
    } else {
        (area, None)
    };
    let body = area;
    let h = area.height;
    let w = area.width;
    let goal_h: u16 = if h >= 26 {
        7
    } else if h >= 18 {
        6
    } else if h >= 12 {
        5
    } else {
        3
    };
    let ev_h: u16 = if h >= 32 {
        9
    } else if h >= 20 {
        5
    } else {
        0
    };
    let parts = Layout::vertical([
        Constraint::Length(goal_h.min(h)),
        Constraint::Min(3),
        Constraint::Length(ev_h),
    ])
    .split(area);
    render_goal_panel(f, parts[0], app, app.panel == Panel::Goal);
    let mid = parts[1];
    if w >= 70 {
        let pct = if w >= 100 { 60 } else { 55 };
        let cols = Layout::horizontal([
            Constraint::Percentage(pct),
            Constraint::Percentage(100 - pct),
        ])
        .split(mid);
        render_agents_panel(f, cols[0], app, app.panel == Panel::Agents, "AGENTS");
        render_tasks_summary(f, cols[1], app, app.panel == Panel::Tasks);
    } else if mid.height >= 16 {
        let rows =
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(mid);
        render_agents_panel(f, rows[0], app, app.panel == Panel::Agents, "AGENTS");
        render_tasks_summary(f, rows[1], app, app.panel == Panel::Tasks);
    } else {
        render_agents_panel(f, mid, app, app.panel == Panel::Agents, "AGENTS");
    }
    if ev_h > 0 && parts[2].height > 0 {
        render_events_panel(f, parts[2], app, app.panel == Panel::Events);
    }
    if let Some(c) = cmd {
        super::agents::render_cmdline(f, c, app);
        super::agents::render_composer_menu(f, body, app);
    }
}

pub fn render_agents_view(f: &mut Frame, area: Rect, app: &App) {
    render_agents_panel(f, area, app, true, "AGENT GRID · ←→ select · Enter detail · C connect · D disconnect · i interrupt · m message");
}

// ------------------------------------------------------------------ approvals

pub fn render_approvals(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let Some(snap) = &app.snap else { return };
    let pending: Vec<_> = snap
        .approvals
        .iter()
        .filter(|a| a.status == ApprovalStatus::Pending)
        .collect();
    let history: Vec<_> = snap
        .approvals
        .iter()
        .filter(|a| a.status != ApprovalStatus::Pending)
        .collect();
    let parts = if area.height >= 14 {
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area)
    } else {
        Layout::vertical([Constraint::Min(1), Constraint::Length(0)]).split(area)
    };
    let block = panel(Line::from(Span::styled(format!(" APPROVAL CENTER · {} pending · y approve · Y approve for session · n deny · x deny+abort ", pending.len()), t.bold())), true, t);
    let inner = block.inner(parts[0]);
    f.render_widget(block, parts[0]);
    let w = inner.width as usize;
    let mut lines: Vec<Line> = vec![];
    if pending.is_empty() {
        lines.push(Line::from(Span::styled("No pending approvals.", t.dim())));
    }
    for (i, a) in pending.iter().enumerate() {
        let selected = i == app.approval_sel;
        let base = if selected { t.selected() } else { t.base() };
        let who = app.agent_name(a.agent_id.as_str());
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} {who}  ", if selected { "▶" } else { " " }),
                base,
            ),
            Span::styled(
                format!("[{}] ", a.kind),
                if selected { base } else { t.warn() },
            ),
            Span::styled(
                truncate(&a.summary, w.saturating_sub(who.len() + a.kind.len() + 8)),
                base,
            ),
        ]));
        if selected {
            if let Some(r) = &a.reason {
                lines.push(Line::from(vec![
                    Span::styled("    reason: ", t.dim()),
                    Span::raw(truncate(r, w.saturating_sub(12))),
                ]));
            }
            let paths = a
                .details
                .get("paths")
                .and_then(|p| p.as_array())
                .map(|p| {
                    p.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if !paths.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("    targets: ", t.dim()),
                    Span::raw(truncate(&paths, w.saturating_sub(13))),
                ]));
            }
            if let Some(cmd) = a.details.get("command").and_then(|c| c.as_str()) {
                lines.push(Line::from(vec![
                    Span::styled("    command: ", t.dim()),
                    Span::styled(truncate(cmd, w.saturating_sub(13)), t.warn()),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("    asked: ", t.dim()),
                Span::raw(format!(
                    "{} ago · grants: this action only (Y = same kind for the session)",
                    fmt_ago((app.now - a.requested_at).num_seconds())
                )),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    if parts[1].height > 0 {
        let block = panel(Line::from(Span::styled(" HISTORY ", t.bold())), false, t);
        let inner = block.inner(parts[1]);
        f.render_widget(block, parts[1]);
        let lines: Vec<Line> = history
            .iter()
            .take(inner.height as usize)
            .map(|a| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<20} ", a.status.as_str()),
                        if a.status == ApprovalStatus::Denied {
                            t.err()
                        } else {
                            t.ok()
                        },
                    ),
                    Span::raw(truncate(
                        &format!("{}: {}", app.agent_name(a.agent_id.as_str()), a.summary),
                        (inner.width as usize).saturating_sub(21),
                    )),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    }
}

// ------------------------------------------------------------------ logs

pub fn log_lines(app: &App) -> Vec<EventLine> {
    let name_of = |id: &str| app.agent_name(id);
    flow_events(app)
        .iter()
        .filter(|e| is_diagnostic(e.event_type))
        .map(|e| describe(e, &name_of))
        .collect()
}

pub fn render_logs(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(
            " LOGS & DIAGNOSTICS · ↑↓ scroll · End latest ",
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut head: Vec<Line> = vec![];
    for a in app.agents() {
        if a.last_error.is_some()
            || matches!(
                a.state,
                AgentState::Failed
                    | AgentState::AuthRequired
                    | AgentState::RateLimited
                    | AgentState::Disconnected
            )
        {
            head.push(Line::from(vec![
                Span::styled(format!("{} ", a.name), t.bold()),
                Span::styled(format!("{} ", a.state), t.agent_state(a.state)),
                Span::styled(a.last_error.clone().unwrap_or_default(), t.err()),
            ]));
        }
    }
    let evs = log_lines(app);
    let mut body: Vec<Line> = event_rows(app, &evs, inner.width as usize);
    if body.is_empty() {
        body.push(Line::from(Span::styled("no diagnostic events", t.dim())));
    }
    let head_h = head.len().min(inner.height as usize / 3);
    let parts =
        Layout::vertical([Constraint::Length(head_h as u16), Constraint::Min(1)]).split(inner);
    if head_h > 0 {
        f.render_widget(
            Paragraph::new(head.into_iter().take(head_h).collect::<Vec<_>>()),
            parts[0],
        );
    }
    let h = parts[1].height as usize;
    let scroll = clamp_scroll(app.logs_scroll, body.len(), h);
    let end = body.len() - scroll;
    let start = end.saturating_sub(h);
    f.render_widget(Paragraph::new(body[start..end].to_vec()), parts[1]);
}

// ------------------------------------------------------------------ config

pub fn config_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let c = &app.config;
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<26}"), t.dim()),
            Span::raw(v),
        ])
    };
    let mut l = vec![Line::from(Span::styled(
        "ORCHESTRATOR (adjustable keys in brackets)",
        t.accent(),
    ))];
    l.push(kv("Max concurrency  [+/-]", c.max_concurrency.to_string()));
    l.push(kv("Task lease  [ [ / ] ]", format!("{} s", c.lease_secs)));
    l.push(kv(
        "Auto-assign  [u]",
        if c.auto_assign {
            "on".into()
        } else {
            "off".into()
        },
    ));
    l.push(kv("Max attempts per task", c.max_attempts.to_string()));
    l.push(kv("Max tasks per goal", c.max_tasks_per_goal.to_string()));
    l.push(kv("Max task depth", c.max_task_depth.to_string()));
    l.push(kv(
        "Leader wake debounce",
        format!("{} ms", c.leader_wake_debounce_ms),
    ));
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled("PROJECT & STORAGE", t.accent())));
    if let Some(s) = &app.snap {
        l.push(kv("Project", s.project.name.clone()));
        l.push(kv("Path", s.project.path.clone()));
        l.push(kv("Run", s.run_id.to_string()));
        l.push(kv(
            "Scheduling",
            if s.paused {
                "PAUSED".into()
            } else {
                "running".into()
            },
        ));
        l.push(kv(
            "Workspaces",
            format!(
                "{} tracked ({} worktrees)",
                s.workspaces.len(),
                s.workspaces.iter().filter(|w| w.kind == "worktree").count()
            ),
        ));
    }
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled(
        "PROVIDERS & PERMISSIONS",
        t.accent(),
    )));
    for a in app.agents() {
        l.push(Line::from(vec![
            Span::styled(format!("{:<26}", a.name), t.bold()),
            Span::raw(format!(
                "{:<8} {:<10} perm={:<10}",
                a.provider,
                a.role,
                a.permission.as_str()
            )),
            Span::styled(a.state.to_string(), t.agent_state(a.state)),
        ]));
    }
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled("UI", t.accent())));
    l.push(kv(
        "Theme",
        if t.mono {
            "monochrome (NO_COLOR or --mono)".into()
        } else {
            "dark / color".into()
        },
    ));
    l
}

pub fn render_config(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(" CONFIGURATION ", t.bold())),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(config_lines(app)).wrap(Wrap { trim: false }),
        inner,
    );
}
