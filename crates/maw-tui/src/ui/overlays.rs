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
        "o / l / c",
        "Approval center / logs & diagnostics / configuration",
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
    ("G", "Set the objective (or redirect it when one is active)"),
    ("a", "Add an agent (guided dialog)"),
    (
        "M  or  / agent model",
        "Choose the selected agent's model and reasoning effort (idle agents reconnect now)",
    ),
    (
        "/ agent permission",
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
    let r = centered(area, 90, (HELP.len() as u16 + 4).min(area.height));
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(" HELP · Esc to close ", t.bold())),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width as usize;
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{:<24}", k), t.accent()),
                Span::raw(truncate(d, w.saturating_sub(24))),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_palette(f: &mut Frame, area: Rect, app: &App, p: &PaletteState) {
    let t = &app.theme;
    let matches = palette_matches(p.input.value());
    let h = (matches.len() as u16 + 3).clamp(4, 16).min(area.height);
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
    let room = (inner.height as usize).saturating_sub(1);
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
    f.render_widget(Paragraph::new(lines), inner);
    f.set_cursor_position((inner.x + 2 + col as u16, inner.y));
}

pub fn render_dialog(f: &mut Frame, area: Rect, app: &App, d: &Dialog) {
    let t = &app.theme;
    let h = (d.fields.len() as u16 * 2 + 4).min(area.height);
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
        let value = if fld.is_select() {
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
        lines.push(Line::raw(""));
    }
    if let Some(e) = &d.error {
        lines.push(Line::from(Span::styled(format!("! {e}"), t.err())));
    } else {
        lines.push(Line::from(Span::styled(
            "Tab/↓ next field · Enter on last field submits · Esc cancels",
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
    let w = inner.width as usize;
    let lines: Vec<Line> = o
        .lines
        .iter()
        .skip(o.scroll)
        .take(inner.height as usize)
        .map(|l| Line::from(Span::raw(truncate(l, w))))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
