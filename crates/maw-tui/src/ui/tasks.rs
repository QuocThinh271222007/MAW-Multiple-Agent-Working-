//! Task board (columns), list mode for narrow terminals, detail pane and the dashboard summary.

use super::widgets::*;
use crate::app::App;
use crate::events::describe;
use crate::theme::status_tag;
use maw_core::{Task, TaskStatus};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

fn owner_name(app: &App, t: &Task) -> String {
    t.owner_agent_id
        .as_ref()
        .map(|o| app.agent_name(o.as_str()))
        .unwrap_or_else(|| "—".into())
}

/// Column groups for the board at a given width. Empty = use list mode.
pub fn column_groups(width: u16) -> Vec<(&'static str, Vec<TaskStatus>)> {
    use TaskStatus::*;
    if width >= 150 {
        vec![
            ("BACKLOG", vec![Backlog]),
            ("READY", vec![Ready]),
            ("ASSIGNED", vec![Assigned]),
            ("RUNNING", vec![Running]),
            ("BLOCKED", vec![Blocked]),
            ("IN REVIEW", vec![InReview]),
            ("VERIFYING", vec![Verifying]),
            ("DONE", vec![Done]),
            ("FAILED", vec![Failed, Canceled]),
        ]
    } else if width >= 100 {
        vec![
            ("QUEUED", vec![Backlog, Ready]),
            ("ASSIGNED", vec![Assigned]),
            ("RUNNING", vec![Running]),
            ("BLOCKED", vec![Blocked]),
            ("REVIEW", vec![InReview, Verifying]),
            ("DONE/FAILED", vec![Done, Failed, Canceled]),
        ]
    } else {
        vec![]
    }
}

pub fn render_tasks_view(f: &mut Frame, area: Rect, app: &App) {
    let groups = column_groups(area.width);
    let sel = app.selected_task().cloned();
    if !groups.is_empty() && area.height >= 14 {
        let parts =
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
        render_columns(f, parts[0], app, &groups, sel.as_ref());
        render_task_detail(f, parts[1], app, sel.as_ref(), app.task_detail_focus);
    } else if app.task_detail_focus
        || area.height < 10 && groups.is_empty() && app.task_detail_focus
    {
        render_task_detail(f, area, app, sel.as_ref(), true);
    } else if area.height >= 20 {
        let parts =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        render_task_list(f, parts[0], app, sel.as_ref(), true);
        render_task_detail(f, parts[1], app, sel.as_ref(), false);
    } else {
        render_task_list(f, area, app, sel.as_ref(), true);
    }
}

fn render_columns(
    f: &mut Frame,
    area: Rect,
    app: &App,
    groups: &[(&'static str, Vec<TaskStatus>)],
    sel: Option<&Task>,
) {
    let t = &app.theme;
    let n = groups.len() as u32;
    let cols = Layout::horizontal(vec![Constraint::Ratio(1, n); groups.len()]).split(area);
    for (ci, (name, statuses)) in groups.iter().enumerate() {
        let mut items: Vec<&Task> = app
            .tasks()
            .iter()
            .filter(|x| statuses.contains(&x.status))
            .collect();
        let num = |t: &Task| {
            t.key
                .trim_start_matches("T-")
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        };
        items.sort_by_key(|x| (TaskStatus::ALL.iter().position(|s| *s == x.status), num(x)));
        let has_sel = sel
            .map(|s| items.iter().any(|x| x.id == s.id))
            .unwrap_or(false);
        let block = panel(
            Line::from(Span::styled(
                format!(" {name} ({}) ", items.len()),
                if has_sel { t.accent() } else { t.bold() },
            )),
            has_sel && !app.task_detail_focus,
            t,
        );
        let inner = block.inner(cols[ci]);
        f.render_widget(block, cols[ci]);
        let h = inner.height as usize;
        let sel_pos = sel
            .and_then(|s| items.iter().position(|x| x.id == s.id))
            .unwrap_or(0);
        let start = if sel_pos >= h { sel_pos + 1 - h } else { 0 };
        let w = inner.width as usize;
        let lines: Vec<Line> = items
            .iter()
            .skip(start)
            .take(h)
            .map(|x| {
                let selected = sel.map(|s| s.id == x.id).unwrap_or(false);
                let st = if selected {
                    t.selected()
                } else {
                    t.task_status(x.status)
                };
                let mark = if x.status == TaskStatus::Running && x.progress > 0 {
                    format!(" {}%", x.progress)
                } else {
                    String::new()
                };
                Line::from(Span::styled(
                    truncate(&format!("{} {}{mark}", x.key, x.title), w),
                    st,
                ))
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn render_task_list(f: &mut Frame, area: Rect, app: &App, sel: Option<&Task>, focused: bool) {
    let t = &app.theme;
    let order = app.board_order();
    let block = panel(
        Line::from(Span::styled(
            format!(" TASKS ({}) · ↑↓ select · Enter details ", order.len()),
            t.bold(),
        )),
        focused,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if order.is_empty() {
        f.render_widget(
            Paragraph::new("No tasks yet. The Leader creates them once a goal is set.")
                .style(t.dim()),
            inner,
        );
        return;
    }
    let h = inner.height as usize;
    let start = if app.task_sel >= h {
        app.task_sel + 1 - h
    } else {
        0
    };
    let w = inner.width as usize;
    let lines: Vec<Line> = order
        .iter()
        .skip(start)
        .take(h)
        .map(|&i| {
            let x = &app.tasks()[i];
            let selected = sel.map(|s| s.id == x.id).unwrap_or(false);
            let base = if selected { t.selected() } else { t.base() };
            let head = format!("{} ", status_tag(x.status));
            let rest = format!("{} {}  {}", pad(&x.key, 5), x.title, owner_name(app, x));
            Line::from(vec![
                Span::styled(
                    head.clone(),
                    if selected {
                        base
                    } else {
                        t.task_status(x.status)
                    },
                ),
                Span::styled(truncate(&rest, w.saturating_sub(head.len())), base),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// Detail lines for one task (kept separate so tests can inspect content).
pub fn task_detail_lines(app: &App, task: &Task, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut l: Vec<Line> = vec![];
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<13}"), t.dim()),
            Span::raw(v),
        ])
    };
    l.push(Line::from(vec![
        Span::styled(format!("{} ", task.key), t.accent()),
        Span::styled(task.title.clone(), t.bold()),
        Span::raw("  "),
        Span::styled(format!("[{}]", task.status), t.task_status(task.status)),
    ]));
    if !task.description.is_empty() {
        for row in wrap_text(&task.description, width.saturating_sub(2)) {
            l.push(Line::from(Span::styled(row, t.dim())));
        }
    }
    l.push(kv(
        "Owner",
        format!(
            "{}    priority {}    weight {}",
            owner_name(app, task),
            task.priority.as_str(),
            task.estimated_weight
        ),
    ));
    l.push(Line::from(vec![
        Span::styled(format!("{:<13}", "Progress"), t.dim()),
        Span::raw(format!(
            "{}% agent-reported (unverified)    ",
            task.progress
        )),
        Span::styled(
            if task.status == TaskStatus::Done {
                "verified DONE".to_string()
            } else {
                "not verified".to_string()
            },
            if task.status == TaskStatus::Done {
                t.ok()
            } else {
                t.warn()
            },
        ),
    ]));
    l.push(kv(
        "Review",
        format!(
            "{}{}",
            task.review_status.as_str(),
            task.reviewer_agent_id
                .as_ref()
                .map(|r| format!(" (reviewer {})", app.agent_name(r.as_str())))
                .unwrap_or_default()
        ),
    ));
    l.push(kv("Attempts", format!("{}", task.attempt_count)));
    if let Some(lease) = task.lease_expires_at {
        l.push(kv(
            "Lease",
            format!("expires in {}", fmt_ago((lease - app.now).num_seconds())),
        ));
    }
    if let Some(r) = &task.blocked_reason {
        l.push(Line::from(vec![
            Span::styled(format!("{:<13}", "BLOCKED"), t.err()),
            Span::styled(r.clone(), t.err()),
        ]));
    }
    // dependencies
    let deps: Vec<String> = task
        .dependencies
        .iter()
        .map(|d| match app.tasks().iter().find(|x| &x.id == d) {
            Some(x) => format!(
                "{} {}{}",
                x.key,
                x.status,
                if x.status == TaskStatus::Done {
                    " ✓"
                } else {
                    " …"
                }
            ),
            None => d.to_string(),
        })
        .collect();
    l.push(kv(
        "Depends on",
        if deps.is_empty() {
            "—".into()
        } else {
            deps.join(" · ")
        },
    ));
    let dependents: Vec<String> = app
        .tasks()
        .iter()
        .filter(|x| x.dependencies.contains(&task.id))
        .map(|x| x.key.clone())
        .collect();
    if !dependents.is_empty() {
        l.push(kv("Unblocks", dependents.join(", ")));
    }
    if !task.acceptance_criteria.is_empty() {
        l.push(Line::from(Span::styled("Acceptance criteria", t.bold())));
        for c in &task.acceptance_criteria {
            l.push(Line::from(Span::raw(format!(
                "  • {}",
                truncate(c, width.saturating_sub(4))
            ))));
        }
    }
    l.push(Line::from(Span::styled(
        format!("Evidence ({})", task.evidence.len()),
        t.bold(),
    )));
    if task.evidence.is_empty() {
        l.push(Line::from(Span::styled(
            "  none yet — a task cannot be DONE without passing evidence",
            t.warn(),
        )));
    }
    for e in &task.evidence {
        l.push(Line::from(vec![
            Span::styled(
                if e.passed { "  ✓ " } else { "  ✗ " },
                if e.passed { t.ok() } else { t.err() },
            ),
            Span::styled(format!("{} ", e.kind.as_str()), t.dim()),
            Span::raw(truncate(&e.summary, width.saturating_sub(18))),
        ]));
    }
    if !task.artifacts.is_empty() {
        l.push(Line::from(Span::styled(
            format!("Artifacts ({})", task.artifacts.len()),
            t.bold(),
        )));
        for a in &task.artifacts {
            l.push(Line::from(Span::raw(format!(
                "  {} {}",
                a.kind,
                a.path.clone().unwrap_or_default()
            ))));
        }
    }
    if let Some(d) = app.task_detail.as_ref().filter(|d| d.key == task.key) {
        if !d.attempts.is_empty() {
            l.push(Line::from(Span::styled("Attempts", t.bold())));
            for a in &d.attempts {
                l.push(Line::from(Span::raw(format!(
                    "  #{} {} → {}",
                    a.attempt_no,
                    app.agent_name(a.agent_id.as_str()),
                    a.outcome.clone().unwrap_or_else(|| "in progress".into())
                ))));
            }
        }
        if !d.events.is_empty() {
            l.push(Line::from(Span::styled("Recent events", t.bold())));
            let name_of = |id: &str| app.agent_name(id);
            for e in d.events.iter().rev().take(10).rev() {
                let ev = describe(e, &name_of);
                l.push(Line::from(vec![
                    Span::styled(format!("  {} ", ev.time), t.dim()),
                    Span::raw(truncate(&ev.text, width.saturating_sub(12))),
                ]));
            }
        }
    }
    l
}

pub fn render_task_detail(
    f: &mut Frame,
    area: Rect,
    app: &App,
    task: Option<&Task>,
    focused: bool,
) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(
            " TASK DETAIL · Enter focus · R requeue · e release · x cancel · v verify ",
            t.bold(),
        )),
        focused,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(task) = task else {
        f.render_widget(Paragraph::new("Select a task.").style(t.dim()), inner);
        return;
    };
    let lines = task_detail_lines(app, task, inner.width as usize);
    let scroll = clamp_scroll(app.task_detail_scroll, lines.len(), inner.height as usize);
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
}

/// Compact task summary used on the dashboard.
pub fn render_tasks_summary(f: &mut Frame, area: Rect, app: &App, focused: bool) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(" TASK BOARD ", t.bold())),
        focused,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let counts = super::agents::task_status_counts(app);
    let mut lines: Vec<Line> = vec![];
    let chips: Vec<Span> = counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .flat_map(|(s, n)| {
            vec![
                Span::styled(format!("{}{n}", status_tag(*s).trim()), t.task_status(*s)),
                Span::raw(" "),
            ]
        })
        .collect();
    lines.push(if chips.is_empty() {
        Line::from(Span::styled("no tasks yet", t.dim()))
    } else {
        Line::from(chips)
    });
    let order = app.board_order();
    // active work first, then everything else by board order
    let mut shown: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| {
            !matches!(
                app.tasks()[i].status,
                TaskStatus::Done | TaskStatus::Canceled
            )
        })
        .collect();
    shown.extend(order.iter().copied().filter(|&i| {
        matches!(
            app.tasks()[i].status,
            TaskStatus::Done | TaskStatus::Canceled
        )
    }));
    let sel_id = app.selected_task().map(|x| x.id.clone());
    for i in shown
        .into_iter()
        .take((inner.height as usize).saturating_sub(1))
    {
        let x = &app.tasks()[i];
        let selected = focused && sel_id.as_ref() == Some(&x.id);
        let base = if selected { t.selected() } else { t.base() };
        let head = format!("[{}] ", status_tag(x.status).trim_end());
        let tail = format!("{} {}", pad(&x.key, 5), x.title);
        lines.push(Line::from(vec![
            Span::styled(
                head.clone(),
                if selected {
                    base
                } else {
                    t.task_status(x.status)
                },
            ),
            Span::styled(
                truncate(
                    &tail,
                    (inner.width as usize).saturating_sub(head.chars().count()),
                ),
                base,
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
