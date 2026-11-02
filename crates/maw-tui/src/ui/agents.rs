//! Agent grid cards and the full-screen agent detail (transcript) view.

use super::widgets::*;
use crate::app::App;
use maw_core::{Agent, AgentState, EventKind, Role, TaskStatus};
use maw_orchestrator::{LineKind, TranscriptLine};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

/// One visual (already wrapped) row of a transcript.
#[derive(Clone, Debug)]
pub struct DisplayLine {
    pub kind: LineKind,
    pub text: String,
}

fn prefix(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Output => "│ ",
        LineKind::System => "· ",
        LineKind::Error => "! ",
        LineKind::User => "› ",
        LineKind::Tool | LineKind::Command | LineKind::Assistant => "",
    }
}

/// Wrap transcript lines to `width` columns. Continuation rows are indented under the prefix.
pub fn display_lines(lines: &[TranscriptLine], width: usize) -> Vec<DisplayLine> {
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        let pre = prefix(l.kind);
        let pw = pre.chars().count();
        let inner = width.saturating_sub(pw).max(1);
        for (i, row) in wrap_text(&l.text, inner).into_iter().enumerate() {
            let p = if i == 0 {
                pre.to_string()
            } else {
                " ".repeat(pw)
            };
            out.push(DisplayLine {
                kind: l.kind,
                text: format!("{p}{row}"),
            });
        }
    }
    out
}

pub fn line_style(app: &App, kind: LineKind) -> Style {
    let t = &app.theme;
    match kind {
        LineKind::Assistant => t.base(),
        LineKind::Tool => t.info(),
        LineKind::Command => t.warn(),
        LineKind::Output => t.dim(),
        LineKind::System => t.dim().add_modifier(Modifier::ITALIC),
        LineKind::Error => t.err(),
        LineKind::User => t.accent(),
    }
}

/// Seconds the agent has been in its current state (from the latest state_changed event).
pub fn state_age(app: &App, a: &Agent) -> Option<i64> {
    let snap = app.snap.as_ref()?;
    let ev = snap.recent_events.iter().rev().find(|e| {
        e.event_type == EventKind::AgentStateChanged
            && e.agent_id.as_ref() == Some(&a.id)
            && e.payload.get("state").and_then(|s| s.as_str()) == Some(a.state.as_str())
    });
    ev.map(|e| (app.now - e.timestamp).num_seconds())
        .or_else(|| a.last_heartbeat.map(|h| (app.now - h).num_seconds()))
}

fn task_label(app: &App, a: &Agent) -> String {
    match a
        .current_task_id
        .as_ref()
        .and_then(|id| app.tasks().iter().find(|t| &t.id == id))
    {
        Some(t) => format!("{} {}", t.key, t.title),
        None => "—".to_string(),
    }
}

fn role_tag(r: Role) -> &'static str {
    match r {
        Role::Leader => "LEADER",
        Role::Worker => "WORKER",
        Role::Reviewer => "REVIEWER",
        Role::Specialist => "SPECIALIST",
    }
}

fn recent_lines<'a>(app: &'a App, a: &Agent) -> &'a [TranscriptLine] {
    app.transcripts
        .get(&a.id)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// The most recent tool/command line, i.e. what the agent is doing right now.
fn current_activity(app: &App, a: &Agent) -> Option<String> {
    if !a.state.is_busy() {
        return None;
    }
    recent_lines(app, a)
        .iter()
        .rev()
        .find(|l| matches!(l.kind, LineKind::Tool | LineKind::Command))
        .map(|l| l.text.clone())
}

fn card(f: &mut Frame, area: Rect, app: &App, idx: usize, a: &Agent, focused: bool) {
    let t = &app.theme;
    let selected = idx == app.selected_agent;
    let waiting = a.state == AgentState::WaitingApproval;
    let mut title = vec![
        Span::styled(format!(" {} ", idx + 1), t.accent()),
        Span::styled(a.name.clone(), t.bold()),
        Span::styled(format!(" [{}] ", role_tag(a.role)), t.role(a.role)),
    ];
    if waiting {
        title.push(Span::styled(
            "⚠ APPROVAL ",
            t.warn().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ));
    }
    let block = panel(Line::from(title), focused && selected, t).border_style(if selected {
        t.border(true)
    } else {
        t.border(false)
    });
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let age = state_age(app, a)
        .map(|s| format!("  ⏱ {}", fmt_elapsed(s)))
        .unwrap_or_default();
    lines.push(Line::from(vec![
        Span::styled("State: ", t.dim()),
        Span::styled(
            format!("{} {}", crate::theme::state_dot(a.state), a.state),
            t.agent_state(a.state),
        ),
        Span::styled(age, t.dim()),
    ]));
    lines.push(Line::from(vec![Span::styled(
        format!(
            "{}{}",
            a.provider,
            a.model
                .as_ref()
                .map(|m| format!(" · {m}"))
                .unwrap_or_default()
        ),
        t.dim(),
    )]));
    lines.push(Line::from(vec![
        Span::styled("Task: ", t.dim()),
        Span::raw(truncate(&task_label(app, a), w.saturating_sub(6))),
    ]));
    if let Some(act) = current_activity(app, a) {
        lines.push(Line::from(vec![
            Span::styled("▶ ", t.ok()),
            Span::styled(truncate(&act, w.saturating_sub(2)), t.warn()),
        ]));
    } else if let Some(e) = &a.last_error {
        lines.push(Line::from(Span::styled(
            truncate(&format!("! {e}"), w),
            t.err(),
        )));
    }
    let used = lines.len();
    let room = (inner.height as usize).saturating_sub(used);
    if room > 0 {
        let tail: Vec<&TranscriptLine> = recent_lines(app, a)
            .iter()
            .rev()
            .filter(|l| l.kind != LineKind::Output || room > 3)
            .take(room)
            .collect();
        for l in tail.into_iter().rev() {
            let st = line_style(app, l.kind);
            lines.push(Line::from(Span::styled(
                truncate(
                    &format!("{}{}", prefix(l.kind), l.text.replace('\n', " ")),
                    w,
                ),
                st,
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn compact_row<'a>(app: &App, idx: usize, a: &Agent, w: usize, selected: bool) -> Line<'a> {
    let t = &app.theme;
    let base = if selected { t.selected() } else { t.base() };
    let task = a
        .current_task_id
        .as_ref()
        .and_then(|id| app.tasks().iter().find(|t| &t.id == id))
        .map(|t| t.key.clone())
        .unwrap_or_else(|| "—".into());
    let head = format!(
        "{} {} {} ",
        idx + 1,
        crate::theme::state_dot(a.state),
        pad(&a.name, 14)
    );
    let role = format!("{} ", pad(role_tag(a.role), 8));
    let state = format!("{} ", pad(a.state.as_str(), 17));
    let rest = format!(
        "{task}{}",
        if a.state == AgentState::WaitingApproval {
            "  ⚠ approval"
        } else {
            ""
        }
    );
    let used = head.chars().count() + role.chars().count() + state.chars().count();
    Line::from(vec![
        Span::styled(head, base),
        Span::styled(role, if selected { base } else { t.role(a.role) }),
        Span::styled(
            state,
            if selected {
                base
            } else {
                t.agent_state(a.state)
            },
        ),
        Span::styled(truncate(&rest, w.saturating_sub(used)), base),
    ])
}

/// Agent grid (cards when there's room, compact rows otherwise).
pub fn render_agents_panel(f: &mut Frame, area: Rect, app: &App, focused: bool, title: &str) {
    let t = &app.theme;
    let agents = app.agents();
    let block = panel(
        Line::from(Span::styled(
            format!(" {title} ({}) ", agents.len()),
            t.bold(),
        )),
        focused,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if agents.is_empty() {
        f.render_widget(
            Paragraph::new("No agents yet. Press 'a' to add one.").style(t.dim()),
            inner,
        );
        return;
    }
    let n = agents.len();
    let cols = ((inner.width / 34).max(1) as usize).min(n);
    let rows = n.div_ceil(cols);
    let card_h = inner.height as usize / rows;
    if card_h < 6 {
        // compact rows, windowed around the selection
        let h = inner.height as usize;
        let start = if app.selected_agent >= h {
            app.selected_agent + 1 - h
        } else {
            0
        };
        let lines: Vec<Line> = agents
            .iter()
            .enumerate()
            .skip(start)
            .take(h)
            .map(|(i, a)| {
                compact_row(
                    app,
                    i,
                    a,
                    inner.width as usize,
                    i == app.selected_agent && focused,
                )
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(inner);
    for (r, ra) in row_areas.iter().enumerate() {
        let col_areas =
            Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).split(*ra);
        for (c, ca) in col_areas.iter().enumerate() {
            let idx = r * cols + c;
            if let Some(a) = agents.get(idx) {
                card(f, *ca, app, idx, a, focused);
            }
        }
    }
}

/// Spans for `text` with case-insensitive occurrences of `query` highlighted.
fn highlight(text: &str, query: &str, base: Style, hl: Style) -> Vec<Span<'static>> {
    let lower: Vec<char> = text
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let q: Vec<char> = query
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let chars: Vec<char> = text.chars().collect();
    if q.is_empty() || q.len() > lower.len() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans = Vec::new();
    let (mut i, mut seg_start) = (0, 0);
    while i + q.len() <= lower.len() {
        if lower[i..i + q.len()] == q[..] {
            if seg_start < i {
                spans.push(Span::styled(
                    chars[seg_start..i].iter().collect::<String>(),
                    base,
                ));
            }
            spans.push(Span::styled(
                chars[i..i + q.len()].iter().collect::<String>(),
                hl,
            ));
            i += q.len();
            seg_start = i;
        } else {
            i += 1;
        }
    }
    if seg_start < chars.len() {
        spans.push(Span::styled(
            chars[seg_start..].iter().collect::<String>(),
            base,
        ));
    }
    spans
}

pub fn render_agent_detail(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let Some(a) = app.selected_agent() else {
        f.render_widget(Paragraph::new("No agent selected.").style(t.dim()), area);
        return;
    };
    let info_h = if area.height >= 20 { 7 } else { 4 };
    let search_h = if app.search.is_some() { 1 } else { 0 };
    let parts = Layout::vertical([
        Constraint::Length(info_h.min(area.height)),
        Constraint::Min(1),
        Constraint::Length(search_h),
    ])
    .split(area);

    // ---- info panel
    let age = state_age(app, a)
        .map(|s| format!("  ⏱ {}", fmt_elapsed(s)))
        .unwrap_or_default();
    let title = Line::from(vec![
        Span::styled(format!(" {} ", a.name), t.bold()),
        Span::styled(format!("[{}] ", role_tag(a.role)), t.role(a.role)),
        Span::styled(format!("{} ", a.provider), t.dim()),
    ]);
    let block = panel(title, true, t);
    let inner = block.inner(parts[0]);
    f.render_widget(block, parts[0]);
    let w = inner.width as usize;
    let task_status = a
        .current_task_id
        .as_ref()
        .and_then(|id| app.tasks().iter().find(|t| &t.id == id))
        .map(|t| format!(" ({}, {}% reported, unverified)", t.status, t.progress))
        .unwrap_or_default();
    let mut info = vec![
        Line::from(vec![
            Span::styled("State: ", t.dim()),
            Span::styled(
                format!("{} {}", crate::theme::state_dot(a.state), a.state),
                t.agent_state(a.state),
            ),
            Span::styled(age, t.dim()),
            if a.state == AgentState::WaitingApproval {
                Span::styled("   ⚠ waiting for approval (press o)", t.warn())
            } else {
                Span::raw("")
            },
        ]),
        Line::from(vec![
            Span::styled("Task:  ", t.dim()),
            Span::raw(truncate(
                &format!("{}{task_status}", task_label(app, a)),
                w.saturating_sub(7),
            )),
        ]),
    ];
    if inner.height > 2 {
        let u = &a.usage;
        let usage = if u.reported_by_provider {
            format!(
                "tokens (provider-reported): in {} out {} total {}",
                u.input_tokens.map(|x| x.to_string()).unwrap_or("?".into()),
                u.output_tokens.map(|x| x.to_string()).unwrap_or("?".into()),
                u.total_tokens.map(|x| x.to_string()).unwrap_or("?".into())
            )
        } else {
            "usage: unavailable from provider".to_string()
        };
        info.push(Line::from(vec![
            Span::styled("Usage: ", t.dim()),
            Span::raw(truncate(&usage, w.saturating_sub(7))),
        ]));
        info.push(Line::from(vec![
            Span::styled("Space: ", t.dim()),
            Span::raw(truncate(
                a.workspace.as_deref().unwrap_or("—"),
                w.saturating_sub(7),
            )),
        ]));
    }
    if inner.height > 4 {
        let caps = &a.capabilities;
        let mut c = vec![];
        if caps.streaming {
            c.push("stream");
        }
        if caps.interrupt {
            c.push("interrupt");
        }
        if caps.resume {
            c.push("resume");
        }
        if caps.approvals {
            c.push("approvals");
        }
        info.push(Line::from(vec![
            Span::styled("Caps:  ", t.dim()),
            Span::raw(truncate(
                &format!(
                    "{}  session {}",
                    if c.is_empty() {
                        "—".to_string()
                    } else {
                        c.join(",")
                    },
                    a.native_session_id.as_deref().unwrap_or("n/a")
                ),
                w.saturating_sub(7),
            )),
        ]));
        if let Some(e) = &a.last_error {
            info.push(Line::from(Span::styled(
                truncate(&format!("Error: {e}"), w),
                t.err(),
            )));
        }
    }
    f.render_widget(Paragraph::new(info), inner);

    // ---- transcript
    let lines = app.transcript_lines();
    let matches = app.search_matches();
    let cur_match = app
        .search
        .as_ref()
        .and_then(|s| matches.get(s.current))
        .copied();
    let h = parts[1].height.saturating_sub(2) as usize;
    let total = lines.len();
    let scroll = clamp_scroll(app.detail_scroll, total, h);
    let end = total - scroll;
    let start = end.saturating_sub(h);
    let status = if scroll == 0 {
        "following".to_string()
    } else {
        format!("↑{scroll} lines up — End to follow")
    };
    let hint = format!(" Transcript · {total} lines · {status} · y copy · Y copy all · / search · i interrupt · m message ");
    let block = panel(
        Line::from(Span::styled(
            truncate(&hint, parts[1].width.saturating_sub(4) as usize),
            t.dim(),
        )),
        false,
        t,
    );
    let tin = block.inner(parts[1]);
    f.render_widget(block, parts[1]);
    let query = app
        .search
        .as_ref()
        .map(|s| s.input.value().to_string())
        .unwrap_or_default();
    let rows: Vec<Line> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let idx = start + i;
            let st = line_style(app, l.kind);
            if cur_match == Some(idx) {
                Line::from(highlight(
                    &l.text,
                    &query,
                    st.add_modifier(Modifier::BOLD),
                    t.highlight(),
                ))
            } else if !query.is_empty() && matches.binary_search(&idx).is_ok() {
                Line::from(highlight(&l.text, &query, st, t.highlight()))
            } else {
                Line::from(Span::styled(l.text.clone(), st))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(rows), tin);

    // ---- search bar
    if let Some(s) = &app.search {
        let r = parts[2];
        f.render_widget(Clear, r);
        let (vis, col) = s.input.view((r.width as usize).saturating_sub(24));
        let info = format!(
            "  {}/{} matches  n/N next/prev  Esc clear",
            if matches.is_empty() { 0 } else { s.current + 1 },
            matches.len()
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/", t.accent()),
                Span::raw(vis),
                Span::styled(info, t.dim()),
            ])),
            r,
        );
        if s.editing && r.width > 1 {
            f.set_cursor_position((r.x + 1 + col as u16, r.y));
        }
    }
}

pub fn task_status_counts(app: &App) -> Vec<(TaskStatus, usize)> {
    TaskStatus::ALL
        .iter()
        .map(|s| (*s, app.tasks().iter().filter(|t| t.status == *s).count()))
        .collect()
}
