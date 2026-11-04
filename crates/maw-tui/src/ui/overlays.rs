//! Modal overlays: help, command palette, input dialogs.

use super::widgets::*;
use crate::app::{palette_matches, App, Dialog, OutputState, PaletteState};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

pub const HELP: &[(&str, &str)] = &[
    (
        "d / s / t / g / f",
        "Dashboard / agent grid / task board / goal & plan / data flow",
    ),
    (
        "o / l / c / $ / %",
        "Approval center / logs & diagnostics / configuration / token usage / PERFORMANCE charts (w: 1m 5m 30m)",
    ),
    (
        "Agent view: type",
        "The message box is always ready: Enter sends; /command opens the command menu; !cmd runs a shell command",
    ),
    (
        "Agent view: Tab",
        "Switch between the message box and browse mode (scroll, y copy, / search, T L P filters)",
    ),
    (
        "Tab, Shift+Tab",
        "Switch panel (dashboard) · toggle sub-pane (other views)",
    ),
    ("1 … 9", "Select agent N (Enter opens its full transcript)"),
    (
        "↑ ↓ ← → / j k",
        "Move selection or scroll; PgUp/PgDn page; Home/End",
    ),
    (
        "/  or  :  or Ctrl+P",
        "Command palette (in agent detail '/' searches the transcript)",
    ),
    (
        "Main screen: type",
        "The command line at the bottom is ready when MAW opens: /command (type / for the list, Tab completes), !shell command, or plain text to message the selected agent. Tab gives the keys to the one-key hotkeys; / or ! brings the line back",
    ),
    (
        "b  or  /board",
        "The board: an unbounded node graph of objective, plan, tasks, agents, conversations and messages. Click or arrows select, Enter or double click opens, drag moves a node or pans (the buttons in the top bar also zoom and pan, for terminals without drag), wheel zooms, Shift+wheel pans sideways, g picks a node up and a click puts it down, h hides the panels, 0 fits all, n task, P plan, G objective",
    ),
    (
        "G  or  /goal set",
        "Set the objective: the Leader plans it into tasks (redirects it when one is active)",
    ),
    (
        "n (Tasks)  or  /task new",
        "Add a task by hand to the current objective; leave 'Assign to' on any idle worker or pick one",
    ),
    (
        "/performance /flow ...",
        "Windows beside the conversation: /performance /flow /status /tokens (↑↓ scroll, Esc closes)",
    ),
    ("a", "Add an agent (guided dialog)"),
    (
        "X",
        "Remove the selected agent (asks for confirmation; unfinished work blocks it unless released)",
    ),
    (
        "T / L / P",
        "Show/hide thinking / tool logs / MAW protocol lines. In the message box type /thinking off, /tool logs on, /protocol off",
    ),
    (
        "Alt+← / Alt+→",
        "Scroll the title bar sideways (the mouse wheel does it too, where the terminal passes the mouse on)",
    ),
    (
        "/model  or  M",
        "Pick the selected agent's model (then its effort, when the model has levels)",
    ),
    (
        "/permissions",
        "Set safe / balanced / autonomous for the selected agent",
    ),
    (
        "!",
        "Run a shell command in the project folder (result in a scrollable panel)",
    ),
    ("p / r", "Pause / resume scheduling"),
    (
        "C / D / i / m",
        "Connect / disconnect / interrupt / message selected agent",
    ),
    (
        "Agent detail: y Y n N",
        "Copy last message / whole transcript · next / previous search match",
    ),
    (
        "Tasks: R e x v",
        "Requeue (FAILED/BLOCKED) / release / cancel / verify (VERIFYING)",
    ),
    (
        "Approvals: y Y n x",
        "Approve / approve for session / deny / deny and abort the turn",
    ),
    (
        "Config: + - [ ] u",
        "Concurrency / lease seconds / auto-assign",
    ),
    ("?", "This help"),
    ("Esc", "Close overlay / go back"),
    (
        "q, Ctrl+C",
        "Quit gracefully (agents are shut down, state is saved)",
    ),
];

pub fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let r = centered(area, 90, (HELP.len() as u16 + 6).min(area.height));
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(" HELP · Esc to close ", t.bold())),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width as usize;
    let mut lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{:<24}", k), t.accent()),
                Span::raw(truncate(d, w.saturating_sub(24))),
            ])
        })
        .collect();
    let mouse = if app.mouse_seen > 0 {
        format!(
            "{} mouse events received: the terminal passes the mouse on",
            app.mouse_seen
        )
    } else {
        "none received (scroll once, reopen ?): the terminal may turn the wheel into ↑↓".to_string()
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{:<24}", "Mouse status"), t.accent()),
        Span::raw(truncate(&mouse, w.saturating_sub(24))),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_palette(f: &mut Frame, area: Rect, app: &App, p: &PaletteState) {
    let t = &app.theme;
    let matches = palette_matches(p.input.value());
    let h = (matches.len() as u16 + 5).clamp(6, 18).min(area.height);
    let r = centered(area, 80, h);
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(
            " COMMAND PALETTE · type to filter · Enter run · Esc cancel ",
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height == 0 || inner.width < 3 {
        return;
    }
    let (vis, col) = p.input.view((inner.width as usize).saturating_sub(3));
    let mut lines = vec![Line::from(vec![
        Span::styled("› ", t.accent()),
        Span::raw(vis),
    ])];
    let room = (inner.height as usize).saturating_sub(3);
    let start = if p.selected >= room {
        p.selected + 1 - room
    } else {
        0
    };
    for (i, c) in matches.iter().enumerate().skip(start).take(room) {
        let sel = i == p.selected;
        let base = if sel { t.selected() } else { t.base() };
        let name = format!("{:<26}", c.name);
        lines.push(Line::from(vec![
            Span::styled(name, if sel { base } else { t.bold() }),
            Span::styled(
                truncate(c.desc, (inner.width as usize).saturating_sub(26)),
                if sel { base } else { t.dim() },
            ),
        ]));
    }
    if matches.is_empty() {
        lines.push(Line::from(Span::styled("no matching command", t.dim())));
    }
    if let Some(c) = matches.get(p.selected) {
        while lines.len() < 1 + room {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled("─".repeat(inner.width as usize), t.dim()));
        for row in wrap_text(&format!("/{} — {}", c.name, c.desc), inner.width as usize)
            .into_iter()
            .take(2)
        {
            lines.push(Line::styled(row, t.base()));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    f.set_cursor_position((inner.x + 2 + col as u16, inner.y));
}

pub fn render_picker(f: &mut Frame, area: Rect, app: &App, p: &crate::picker::PickerState) {
    let t = &app.theme;
    let vis = p.visible();
    let tabbed = !p.tabs.is_empty();
    // search line, hint/tabs, then the list, then the full text of the highlighted entry
    let head_rows = if tabbed { 3 } else { 2 };
    let detail_rows: usize = 3;
    let h = (vis.len() as u16 + (head_rows + detail_rows) as u16 + 2)
        .clamp(10, 26)
        .min(area.height);
    let r = centered(area, 100, h);
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(
            format!(
                " {} · ↑↓ choose · Enter select{} · Esc cancel ",
                p.title,
                if tabbed {
                    " · Tab section · F2 sort"
                } else {
                    ""
                }
            ),
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 3 || inner.width < 8 {
        return;
    }
    let w = inner.width as usize;
    let label = if tabbed { "search › " } else { "› " };
    let lw = label.chars().count();
    let (vis_text, col) = p.filter.view(w.saturating_sub(lw + 1));
    let search = if p.filter.value().is_empty() && tabbed {
        Line::from(vec![
            Span::styled(label, t.accent()),
            Span::styled("name, maker, size ... (e.g. google, 3b, tools)", t.dim()),
        ])
    } else {
        Line::from(vec![Span::styled(label, t.accent()), Span::raw(vis_text)])
    };
    let mut lines = vec![search];
    if tabbed {
        let mut spans: Vec<Span> = vec![];
        for (i, name) in p.tabs.iter().enumerate() {
            let text = format!(" {} ({}) ", name, p.tab_count(name));
            spans.push(Span::styled(
                text,
                if i == p.tab { t.selected() } else { t.dim() },
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("  sorted by {}", p.sort.label()),
            t.dim(),
        ));
        lines.push(Line::from(spans));
        lines.push(Line::styled(truncate(&p.hint, w), t.dim()));
    } else {
        lines.push(Line::styled(truncate(&p.hint, w), t.dim()));
    }
    let room = (inner.height as usize).saturating_sub(head_rows + detail_rows);
    let sel = p.selected.min(vis.len().saturating_sub(1));
    let start = if sel >= room { sel + 1 - room } else { 0 };
    let name_w = vis
        .iter()
        .map(|i| i.label.chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, if tabbed { 46 } else { 28 });
    for (i, it) in vis.iter().enumerate().skip(start).take(room) {
        let on = i == sel;
        let base = if on { t.selected() } else { t.base() };
        let mark = if on { "❯ " } else { "  " };
        let cur = if it.current { " ●" } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(mark, if on { base } else { t.accent() }),
            Span::styled(
                format!("{:<name_w$}", truncate(&it.label, name_w)),
                if on { base } else { t.bold() },
            ),
            Span::styled(cur, if on { base } else { t.ok() }),
            Span::styled(
                truncate(&it.detail, w.saturating_sub(name_w + 5)),
                if on { base } else { t.dim() },
            ),
        ]));
    }
    if vis.is_empty() {
        let typed = p.filter.value().trim().to_string();
        lines.push(Line::styled(
            if p.allow_custom && !typed.is_empty() {
                format!("Enter uses '{typed}'")
            } else {
                "nothing matches".to_string()
            },
            t.dim(),
        ));
    }
    // the whole description of the highlighted entry, wrapped, so nothing stays hidden behind "…"
    if let Some(it) = vis.get(sel) {
        while lines.len() < head_rows + room {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled("─".repeat(w), t.dim()));
        let text = if it.detail.is_empty() {
            it.label.clone()
        } else {
            format!("{} — {}", it.label, it.detail)
        };
        for row in wrap_text(&text, w).into_iter().take(detail_rows - 1) {
            lines.push(Line::styled(row, t.base()));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    f.set_cursor_position((inner.x + lw as u16 + col as u16, inner.y));
}

pub fn render_dialog(f: &mut Frame, area: Rect, app: &App, d: &Dialog) {
    let t = &app.theme;
    // A focused long pick-list is drawn as a vertical, scrolling list under its label.
    let in_list = d.fields.get(d.focus).map(|f| f.is_list()).unwrap_or(false);
    // With plenty of height the fields keep a blank spacer row and the list may show 10 rows. On a
    // short terminal the spacers go away and the list becomes a 6-row window that scrolls, so the
    // selected option is always visible.
    let roomy = (area.height as usize) >= d.fields.len() * 2 + 4 + 10;
    let compact = in_list && !roomy;
    let per_field = if compact { 1 } else { 2 };
    // three rows under a focused list show the whole text of the highlighted option
    let spare_all = (area.height as usize).saturating_sub(d.fields.len() * per_field + 4);
    let detail_rows: usize = if in_list && spare_all >= 7 { 3 } else { 0 };
    let spare = spare_all - detail_rows;
    let cap = if compact { 6 } else { 10 };
    let list_rows = if in_list {
        d.fields[d.focus].choices.len().min(cap).min(spare)
    } else {
        0
    };
    let list_rows = if list_rows >= 3 { list_rows } else { 0 };
    let detail_rows = if list_rows > 0 { detail_rows } else { 0 };
    let h = (d.fields.len() as u16 * per_field as u16 + 4 + (list_rows + detail_rows) as u16)
        .min(area.height);
    let r = centered(area, 78, h);
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(format!(" {} ", d.title), t.bold())),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines: Vec<Line> = vec![];
    let mut cursor: Option<(u16, u16)> = None;
    let label_w = d
        .fields
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(8)
        + 2;
    for (i, fld) in d.fields.iter().enumerate() {
        let focused = i == d.focus;
        let y = lines.len() as u16;
        let label = format!(
            "{:<w$}",
            format!("{}{}", fld.label, if fld.required { "*" } else { "" }),
            w = label_w
        );
        let field_w = (inner.width as usize).saturating_sub(label_w + 1);
        let mut extra: Vec<Line> = vec![];
        let value = if fld.is_list() {
            let cur = fld.choices.get(fld.choice).cloned().unwrap_or_default();
            if focused && list_rows > 0 {
                let len = fld.choices.len();
                let start = fld
                    .choice
                    .saturating_sub(list_rows / 2)
                    .min(len.saturating_sub(list_rows));
                let id_w = fld
                    .choices
                    .iter()
                    .map(|c| c.chars().count())
                    .max()
                    .unwrap_or(8)
                    .min(24);
                for ci in start..(start + list_rows).min(len) {
                    let sel = ci == fld.choice;
                    let base = if sel { t.selected() } else { t.base() };
                    let marker = if sel { "▶ " } else { "  " };
                    let detail = fld.details.get(ci).cloned().unwrap_or_default();
                    let used = label_w + 2 + id_w + 2;
                    extra.push(Line::from(vec![
                        Span::styled(" ".repeat(label_w), t.base()),
                        Span::styled(format!("{marker}{:<w$}  ", fld.choices[ci], w = id_w), base),
                        Span::styled(
                            truncate(&detail, (inner.width as usize).saturating_sub(used)),
                            if sel { base } else { t.dim() },
                        ),
                    ]));
                }
                if detail_rows > 0 {
                    let full = fld.details.get(fld.choice).cloned().unwrap_or_default();
                    let text = if full.is_empty() {
                        cur.clone()
                    } else {
                        format!("{cur} — {full}")
                    };
                    let room_w = (inner.width as usize).saturating_sub(label_w + 1).max(8);
                    let mut rows = wrap_text(&text, room_w);
                    let more = rows.len() > detail_rows;
                    rows.truncate(detail_rows);
                    if more {
                        if let Some(l) = rows.last_mut() {
                            *l = truncate(&format!("{l} …"), room_w);
                        }
                    }
                    for row in rows {
                        extra.push(Line::from(vec![
                            Span::raw(" ".repeat(label_w)),
                            Span::styled(row, t.base()),
                        ]));
                    }
                }
                Line::from(vec![
                    Span::styled(label, t.accent()),
                    Span::styled(cur, t.bold()),
                    Span::styled(
                        format!(
                            "   ({}/{}){}",
                            fld.choice + 1,
                            len,
                            if start > 0 { " ▲" } else { "" }
                        ),
                        t.dim(),
                    ),
                    Span::styled(if start + list_rows < len { " ▼" } else { "" }, t.dim()),
                ])
            } else {
                Line::from(vec![
                    Span::styled(label, if focused { t.accent() } else { t.dim() }),
                    Span::styled(cur, if focused { t.selected() } else { t.bold() }),
                    Span::styled(format!("   ({} options)", fld.choices.len()), t.dim()),
                ])
            }
        } else if fld.is_select() {
            let opts: Vec<Span> = fld
                .choices
                .iter()
                .enumerate()
                .flat_map(|(ci, c)| {
                    vec![
                        Span::styled(
                            format!(" {c} "),
                            if ci == fld.choice {
                                if focused {
                                    t.selected()
                                } else {
                                    t.bold()
                                }
                            } else {
                                t.dim()
                            },
                        ),
                        Span::raw(" "),
                    ]
                })
                .collect();
            Line::from(
                [
                    vec![Span::styled(
                        label,
                        if focused { t.accent() } else { t.dim() },
                    )],
                    opts,
                ]
                .concat(),
            )
        } else {
            let (vis, col) = fld.input.view(field_w);
            if focused {
                cursor = Some((inner.x + label_w as u16 + col as u16, inner.y + y));
            }
            let shown = if vis.is_empty() && !focused {
                Span::styled(truncate(&fld.hint, field_w), t.dim())
            } else {
                Span::raw(vis)
            };
            Line::from(vec![
                Span::styled(label, if focused { t.accent() } else { t.dim() }),
                shown,
            ])
        };
        lines.push(value);
        lines.extend(extra);
        if !compact {
            lines.push(Line::raw(""));
        }
    }
    if let Some(e) = &d.error {
        lines.push(Line::from(Span::styled(format!("! {e}"), t.err())));
    } else {
        lines.push(Line::from(Span::styled(
            if in_list {
                "↑↓ choose (PgUp/PgDn/Home/End) · Tab or Enter next field · Esc cancels"
            } else {
                "Tab/↓ next field · Enter on last field submits · Esc cancels"
            },
            t.dim(),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
    if let Some((x, y)) = cursor {
        if x < inner.x + inner.width && y < inner.y + inner.height {
            f.set_cursor_position((x, y));
        }
    }
}

/// Scrollable text panel (shell command results).
pub fn render_output(f: &mut Frame, area: Rect, app: &App, o: &OutputState) {
    let t = &app.theme;
    let r = centered(area, 96, area.height.saturating_sub(2).max(5));
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(
            format!(
                " {} · ↑↓ scroll · y copy · Esc close ",
                truncate(&o.title, 60)
            ),
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = (inner.width as usize).max(1);
    let rows: Vec<String> = o.lines.iter().flat_map(|l| wrap_text(l, w)).collect();
    app.output_rows.set(rows.len());
    let scroll = o
        .scroll
        .min(rows.len().saturating_sub(inner.height as usize));
    let lines: Vec<Line> = rows
        .iter()
        .skip(scroll)
        .take(inner.height as usize)
        .map(|l| Line::from(Span::raw(l.clone())))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
