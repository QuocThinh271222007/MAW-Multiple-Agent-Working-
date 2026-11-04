//! Goal panel (dashboard) and the full Goal & Plan view.
//!
//! Verified progress (DONE with evidence) is always shown separately from agent-reported
//! progress, blocked, failed and unknown work. No number here is estimated by the UI.

use super::widgets::*;
use crate::app::App;
use maw_core::{Milestone, TaskStatus};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

pub fn count(app: &App, s: TaskStatus) -> usize {
    app.tasks().iter().filter(|t| t.status == s).count()
}

/// `(done, in_progress)` for a plan milestone computed from real task state.
pub fn milestone_state(app: &App, m: &Milestone) -> char {
    let tasks: Vec<_> = m
        .task_keys
        .iter()
        .filter_map(|k| app.tasks().iter().find(|t| &t.key == k))
        .collect();
    if m.done
        || (!tasks.is_empty()
            && tasks
                .iter()
                .all(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Canceled)))
    {
        'x'
    } else if tasks.iter().any(|t| {
        matches!(
            t.status,
            TaskStatus::Running
                | TaskStatus::Assigned
                | TaskStatus::InReview
                | TaskStatus::Verifying
        )
    }) {
        '>'
    } else {
        ' '
    }
}

pub fn render_goal_panel(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(" LEADER GOAL ", t.bold())),
        focused,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let Some(snap) = &app.snap else {
        f.render_widget(Paragraph::new("loading…").style(t.dim()), inner);
        return;
    };
    let Some(goal) = &snap.goal else {
        f.render_widget(
            Paragraph::new("No objective yet. Press G (or type /goal set in an agent's message box) to set one: the Leader then plans it into tasks.").style(t.warn()),
            inner,
        );
        return;
    };
    let w = inner.width as usize;
    let p = &snap.progress;
    let leader = snap
        .agents
        .iter()
        .find(|a| a.role == maw_core::Role::Leader)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "—".into());
    let mut lines: Vec<Line> = vec![];
    let obj_rows = if inner.height >= 6 { 2 } else { 1 };
    for r in wrap_text(&goal.text, w).into_iter().take(obj_rows) {
        lines.push(Line::from(Span::styled(r, t.bold())));
    }
    let bar_w = (w / 4).clamp(6, 24);
    lines.push(Line::from(vec![
        Span::styled("Verified ", t.dim()),
        Span::styled(bar(bar_w, p.verified_pct / 100.0), t.ok()),
        Span::styled(format!(" {:.0}%", p.verified_pct), t.bold()),
        Span::styled(
            format!("  ({}/{} tasks)", p.done_count, p.task_count),
            t.dim(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Milestone: ", t.dim()),
        Span::raw(truncate(
            goal.current_milestone.as_deref().unwrap_or("—"),
            w / 3,
        )),
        Span::styled("  Leader: ", t.dim()),
        Span::raw(truncate(&leader, 16)),
        Span::styled(
            format!("  rev {} {}", goal.revision, goal.status.as_str()),
            t.dim(),
        ),
    ]));
    let blockers = count(app, TaskStatus::Blocked);
    let review = count(app, TaskStatus::InReview);
    let approvals = app.pending_approvals().len();
    lines.push(Line::from(vec![
        Span::styled(
            format!("Blockers: {blockers}"),
            if blockers > 0 { t.err() } else { t.dim() },
        ),
        Span::styled(
            format!("  Review pending: {review}"),
            if review > 0 { t.warn() } else { t.dim() },
        ),
        Span::styled(
            format!("  Approvals: {approvals}"),
            if approvals > 0 { t.warn() } else { t.dim() },
        ),
        Span::styled(
            format!("  Running: {}", count(app, TaskStatus::Running)),
            t.dim(),
        ),
    ]));
    if inner.height >= 7 {
        if let Some(focus) = &goal.current_focus {
            lines.push(Line::from(vec![
                Span::styled("Focus: ", t.dim()),
                Span::raw(truncate(focus, w.saturating_sub(7))),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn breakdown(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let Some(snap) = &app.snap else { return vec![] };
    let p = &snap.progress;
    let total = p.total_weight;
    let pct = |w: f64| if total > 0.0 { w / total } else { 0.0 };
    let bw = width.saturating_sub(50).clamp(8, 30);
    let row = |label: &str, w: f64, st: ratatui::style::Style, note: &str| {
        Line::from(vec![
            Span::styled(format!("{label:<30}"), st),
            Span::styled(bar(bw, pct(w)), st),
            Span::raw(format!(" {:>3.0}%  w={:<5.1}", pct(w) * 100.0, w)),
            Span::styled(note.to_string(), t.dim()),
        ])
    };
    vec![
        row("Verified complete", p.verified_weight, t.ok(), " counted"),
        row("Agent-reported (unverified)", p.reported_weight, t.warn(), " NOT counted"),
        row("Blocked", p.blocked_weight, t.err(), ""),
        row("Failed", p.failed_weight, t.err(), ""),
        row("Unknown (not started)", p.unknown_weight, t.dim(), ""),
        Line::from(Span::styled(format!("Total planned weight {:.1} over {} tasks (canceled excluded). Scope changes alter the denominator.", total, p.task_count), t.dim())),
    ]
}

pub fn goal_view_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut l: Vec<Line> = vec![];
    let Some(snap) = &app.snap else { return l };
    let Some(goal) = &snap.goal else {
        l.push(Line::from(Span::styled(
            "No objective yet. Press G (or type /goal set in an agent's message box) to set one: the Leader then plans it into tasks.",
            t.warn(),
        )));
        return l;
    };
    l.push(Line::from(Span::styled("OBJECTIVE", t.accent())));
    for r in wrap_text(&goal.text, width) {
        l.push(Line::from(Span::styled(r, t.bold())));
    }
    let leader = snap
        .agents
        .iter()
        .find(|a| a.role == maw_core::Role::Leader)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "—".into());
    l.push(Line::from(vec![
        Span::styled("Status: ", t.dim()),
        Span::raw(goal.status.as_str().to_string()),
        Span::styled("   Revision: ", t.dim()),
        Span::raw(goal.revision.to_string()),
        Span::styled("   Leader: ", t.dim()),
        Span::raw(leader),
    ]));
    l.push(Line::from(vec![
        Span::styled("Milestone: ", t.dim()),
        Span::raw(goal.current_milestone.clone().unwrap_or_else(|| "—".into())),
    ]));
    l.push(Line::from(vec![
        Span::styled("Focus: ", t.dim()),
        Span::raw(goal.current_focus.clone().unwrap_or_else(|| "—".into())),
    ]));
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled("SUCCESS CRITERIA", t.accent())));
    if goal.success_criteria.is_empty() {
        l.push(Line::from(Span::styled("  (none recorded)", t.dim())));
    }
    for c in &goal.success_criteria {
        l.push(Line::from(Span::raw(format!("  • {c}"))));
    }
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled(
        format!("PROGRESS  verified {:.0}%", snap.progress.verified_pct),
        t.accent(),
    )));
    l.extend(breakdown(app, width));
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled("PLAN", t.accent())));
    match &snap.plan {
        None => l.push(Line::from(Span::styled(
            "  The Leader has not published a plan yet.",
            t.dim(),
        ))),
        Some(p) => {
            l.push(Line::from(vec![
                Span::styled(format!("  r{} ", p.revision), t.dim()),
                Span::raw(p.summary.clone()),
            ]));
            for m in &p.milestones {
                let mark = milestone_state(app, m);
                let st = match mark {
                    'x' => t.ok(),
                    '>' => t.warn(),
                    _ => t.base(),
                };
                l.push(Line::from(Span::styled(
                    format!("  [{mark}] {}  {}", m.title, m.task_keys.join(",")),
                    st,
                )));
            }
            if !p.next_actions.is_empty() {
                l.push(Line::from(Span::styled("  Next actions:", t.dim())));
                for a in &p.next_actions {
                    l.push(Line::from(Span::raw(format!("    → {a}"))));
                }
            }
        }
    }
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled("HISTORY", t.accent())));
    if app.goal_history.is_empty() && app.plan_history.is_empty() {
        l.push(Line::from(Span::styled("  (loading or empty)", t.dim())));
    }
    for r in &app.goal_history {
        l.push(Line::from(Span::raw(format!(
            "  goal r{} {} — {} ({})",
            r.revision,
            r.created_at.format("%H:%M:%S"),
            r.reason,
            app.agent_name(r.author.as_str())
        ))));
    }
    for p in &app.plan_history {
        l.push(Line::from(Span::raw(format!(
            "  plan r{} {} — {} ({})",
            p.revision,
            p.created_at.format("%H:%M:%S"),
            p.reason,
            app.agent_name(p.author.as_str())
        ))));
    }
    l.push(Line::raw(""));
    l.push(Line::from(Span::styled(
        "Enter/G: redirect objective — completed tasks are preserved and history is recorded.",
        t.dim(),
    )));
    l
}

pub fn render_goal_view(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let block = panel(Line::from(Span::styled(" GOAL & PLAN ", t.bold())), true, t);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = goal_view_lines(app, inner.width as usize);
    let scroll = clamp_scroll(app.goal_scroll, lines.len(), inner.height as usize);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}
