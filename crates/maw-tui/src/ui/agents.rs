//! Agent grid cards and the full-screen agent detail (transcript) view.

use super::widgets::*;
use crate::app::{App, SidePanel};
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
        LineKind::Thinking => "~ ",
        LineKind::Protocol => "⇄ ",
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
        LineKind::Thinking => t.info().add_modifier(Modifier::ITALIC),
        LineKind::Protocol => t.dim(),
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
            .filter(|l| app.line_visible(l.kind))
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
    let composer_h: u16 = if area.height >= 12 { 3 } else { 0 };
    let parts = Layout::vertical([
        Constraint::Length(info_h.min(area.height)),
        Constraint::Min(1),
        Constraint::Length(search_h),
        Constraint::Length(composer_h),
    ])
    .split(area);
    // a window opened with /performance, /tokens or /status sits to the right of the
    // conversation when there is room, and covers it otherwise
    let side_w = (parts[1].width * 45 / 100).clamp(44, 72);
    let beside = app.side.is_some() && parts[1].width >= side_w + 56;
    let tr = if beside {
        Rect {
            width: parts[1].width - side_w,
            ..parts[1]
        }
    } else {
        parts[1]
    };
    if composer_h > 0 {
        render_composer(f, parts[3], app, a);
    }

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
    let h = tr.height.saturating_sub(2) as usize;
    let total = lines.len();
    let scroll = clamp_scroll(app.detail_scroll, total, h);
    let end = total - scroll;
    let start = end.saturating_sub(h);
    let status = if scroll == 0 {
        "following".to_string()
    } else {
        format!("↑{scroll} lines up — End to follow")
    };
    let mark = |on: bool| if on { "✓" } else { "✗" };
    let hint = format!(
        " Transcript [T thinking {} · L tool logs {} · P MAW protocol {}] · {total} lines · {status} · Tab browse · /cmd ",
        mark(app.show_thinking),
        mark(app.show_logs),
        mark(app.show_protocol)
    );
    let block = panel(
        Line::from(Span::styled(
            truncate(&hint, tr.width.saturating_sub(4) as usize),
            t.dim(),
        )),
        false,
        t,
    );
    let tin = block.inner(tr);
    f.render_widget(block, tr);
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
    if let Some(panel) = app.side {
        let r = if beside {
            Rect {
                x: parts[1].x + tr.width,
                width: side_w,
                ..parts[1]
            }
        } else {
            parts[1]
        };
        f.render_widget(Clear, r);
        match panel {
            SidePanel::Perf => super::perf_view::render_perf(f, r, app),
            SidePanel::Tokens => super::usage_view::render_tokens(f, r, app),
            SidePanel::Status => render_status(f, r, app, a),
        }
    }
    render_composer_menu(f, parts[1], app);
}

/// `/status`: the facts about the open agent in one place, like Claude Code's `/status`.
fn render_status(f: &mut Frame, area: Rect, app: &App, a: &Agent) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(
            format!(" STATUS · {} · Esc closes ", a.name),
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = (inner.width as usize).saturating_sub(12);
    let row = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<11}"), t.dim()),
            Span::raw(truncate(&v, w)),
        ])
    };
    let none = || "—".to_string();
    let (turn, _) = turn_status(app, a);
    let u = &a.usage;
    let tok = |x: Option<u64>| {
        x.map(crate::usage::fmt_tokens)
            .unwrap_or_else(|| "?".into())
    };
    let (project, paused, goal) = app
        .snap
        .as_ref()
        .map(|s| {
            (
                s.project.name.clone(),
                s.paused,
                s.goal
                    .as_ref()
                    .map(|g| format!("{} ({})", truncate(&g.text, 40), g.status.as_str()))
                    .unwrap_or_else(|| "no goal".into()),
            )
        })
        .unwrap_or_default();
    let mut lines = vec![
        row("agent", format!("{} [{}]", a.name, role_tag(a.role))),
        row("provider", a.provider.to_string()),
        row("model", a.model.clone().unwrap_or_else(|| "default".into())),
        row(
            "effort",
            a.effort.clone().unwrap_or_else(|| "default".into()),
        ),
        row("permission", a.permission.as_str().to_string()),
        row(
            "state",
            format!(
                "{}{}",
                a.state,
                state_age(app, a)
                    .map(|s| format!(" for {}", fmt_elapsed(s)))
                    .unwrap_or_default()
            ),
        ),
        row("turn", turn.trim().to_string()),
        row("task", task_label(app, a)),
        row(
            "tokens",
            if u.reported_by_provider {
                format!(
                    "in {} · out {} · total {}",
                    tok(u.input_tokens),
                    tok(u.output_tokens),
                    tok(u.total_tokens)
                )
            } else {
                "not reported by this provider".into()
            },
        ),
        row("session", a.native_session_id.clone().unwrap_or_else(none)),
        row("workspace", a.workspace.clone().unwrap_or_else(none)),
        row(
            "last error",
            a.last_error.clone().unwrap_or_else(|| "none".into()),
        ),
        Line::from(""),
        row("project", project),
        row("goal", goal),
        row(
            "scheduler",
            if paused { "paused" } else { "running" }.into(),
        ),
    ];
    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn task_status_counts(app: &App) -> Vec<(TaskStatus, usize)> {
    TaskStatus::ALL
        .iter()
        .map(|s| (*s, app.tasks().iter().filter(|t| t.status == *s).count()))
        .collect()
}

/// The line on the message box's bottom border: what the agent is doing, for how long and with how
/// many tokens (as the provider reports them; never estimated). `true` while it is working.
fn turn_status(app: &App, a: &Agent) -> (String, bool) {
    use crate::usage::fmt_tokens;
    let total = a
        .usage
        .total_tokens
        .unwrap_or(a.usage.input_tokens.unwrap_or(0) + a.usage.output_tokens.unwrap_or(0));
    let reported = a.usage.reported_by_provider;
    let mark = app.turn_marks.get(&a.id);
    if a.state.is_busy() {
        let secs = mark
            .and_then(|m| m.started)
            .map(|s| (app.now - s).num_seconds().max(0))
            .unwrap_or(0);
        let spin = ["◐", "◓", "◑", "◒"][(secs as usize) % 4];
        let what = match a.state {
            AgentState::WaitingApproval => "waiting for approval",
            AgentState::WaitingDependency => "waiting",
            AgentState::Working => "working",
            _ => "thinking",
        };
        let tokens = if reported {
            format!(
                " · ↓ {} tokens",
                fmt_tokens(mark.map(|m| m.tokens).unwrap_or(0))
            )
        } else {
            String::new()
        };
        return (
            format!(
                " {spin} {what} {}{tokens} · /interrupt to stop ",
                fmt_elapsed(secs)
            ),
            true,
        );
    }
    let mut s = format!(" ● {}", a.state.as_str().to_lowercase());
    if let Some((secs, tokens)) = mark.and_then(|m| m.last) {
        s.push_str(&format!(" · last turn {}", fmt_elapsed(secs)));
        if reported {
            s.push_str(&format!(" · {} tokens", fmt_tokens(tokens)));
        }
    }
    if reported && total > 0 {
        s.push_str(&format!(" · session {} tokens", fmt_tokens(total)));
    }
    s.push(' ');
    (s, false)
}

/// The message box: always ready for typing, `/` opens the command menu above it.
fn render_composer(f: &mut Frame, area: Rect, app: &App, agent: &maw_core::Agent) {
    let t = &app.theme;
    let focus = app.composer_focus && !app.is_text_mode();
    let title = if focus {
        format!(
            " message {} · Enter send · /command · !shell · Tab browse · Esc back ",
            agent.name
        )
    } else {
        " browse: ↑↓ scroll · y copy · / search · T L P filters · Tab or Enter: message "
            .to_string()
    };
    let (status, busy) = turn_status(app, agent);
    let status_style = if busy { t.warn() } else { t.dim() };
    let block = panel(Line::from(Span::styled(title, t.bold())), focus, t).title_bottom(
        Line::from(Span::styled(
            truncate(&status, (area.width as usize).saturating_sub(4)),
            status_style,
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height == 0 {
        return;
    }
    let text = app.composer.value();
    let (vis, col) = app.composer.view((inner.width as usize).saturating_sub(3));
    let line = if text.is_empty() && focus {
        Line::from(vec![
            Span::styled("› ", t.accent()),
            Span::styled("message the agent, or type / for commands", t.dim()),
        ])
    } else {
        Line::from(vec![Span::styled("› ", t.accent()), Span::raw(vis)])
    };
    f.render_widget(Paragraph::new(line), inner);
    if focus {
        f.set_cursor_position((inner.x + 2 + col as u16, inner.y));
    }
}

/// The `/` command menu, drawn over the bottom of the transcript just above the message box.
pub fn render_composer_menu(f: &mut Frame, transcript: Rect, app: &App) {
    let t = &app.theme;
    if !app.composer_focus || app.is_text_mode() {
        return;
    }
    let menu = crate::app::composer_menu(app);
    if menu.is_empty() && !app.composer.value().starts_with('/') {
        return;
    }
    let rows = (menu.len().min(8) as u16).max(1);
    let h = (rows + 2).min(transcript.height);
    if h < 3 {
        return;
    }
    let r = Rect {
        x: transcript.x,
        y: transcript.y + transcript.height - h,
        width: transcript.width,
        height: h,
    };
    f.render_widget(ratatui::widgets::Clear, r);
    let block = panel(
        Line::from(Span::styled(" commands · ↑↓ · Enter run ", t.bold())),
        false,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let room = inner.height as usize;
    let sel = app.composer_sel.min(menu.len().saturating_sub(1));
    let start = if sel >= room { sel + 1 - room } else { 0 };
    let w = inner.width as usize;
    let mut lines: Vec<Line> = vec![];
    for (i, c) in menu.iter().enumerate().skip(start).take(room) {
        let on = i == sel;
        let base = if on { t.selected() } else { t.base() };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<27}", truncate(&format!("/{}", c.name), 26)),
                if on { base } else { t.bold() },
            ),
            Span::styled(
                truncate(c.desc, w.saturating_sub(27)),
                if on { base } else { t.dim() },
            ),
        ]));
    }
    if menu.is_empty() {
        lines.push(Line::styled("no matching command", t.dim()));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
