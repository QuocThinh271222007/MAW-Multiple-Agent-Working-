//! Drawing the downloads: a one-row gauge above the footer while anything is downloading, and the
//! full list (`/downloads`) with a bar per download and the whole reason when one failed.

use super::widgets::*;
use crate::app::App;
use crate::downloads::{fmt_bytes, fmt_eta, fmt_speed, DlState, Download};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

fn state_style(app: &App, s: DlState) -> Style {
    match s {
        DlState::Downloading => app.theme.accent(),
        DlState::Done => app.theme.ok(),
        DlState::Failed => app.theme.err(),
        DlState::Stopped => app.theme.warn(),
    }
}

fn numbers(d: &Download, now: chrono::DateTime<chrono::Utc>) -> String {
    let mut s = String::new();
    match d.fraction() {
        Some(_) => s.push_str(&format!(
            "{:>3}%  {} / {}",
            d.percent().unwrap_or(0),
            fmt_bytes(d.completed),
            fmt_bytes(d.total)
        )),
        None => s.push_str(&d.status),
    }
    if d.state == DlState::Downloading {
        if let Some(v) = d.speed() {
            s.push_str(&format!("  {}", fmt_speed(v)));
        }
        if let Some(e) = d.eta_secs() {
            s.push_str(&format!("  ETA {}", fmt_eta(e)));
        }
    } else {
        s.push_str(&format!("  {}", fmt_elapsed(d.elapsed_secs(now))));
    }
    s
}

/// The row above the footer: the download that is running, with its bar, speed and time left.
pub fn render_gauge(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let active: Vec<&Download> = app.downloads.active().collect();
    let Some(d) = active.first() else { return };
    let more = if active.len() > 1 {
        format!(" +{}", active.len() - 1)
    } else {
        String::new()
    };
    let tail = format!("  /downloads{more}");
    let nums = numbers(d, app.now);
    let name = truncate(&d.name, 28);
    let fixed = 2 + name.chars().count() + 1 + 2 + nums.chars().count() + tail.chars().count();
    let bar_w = (area.width as usize).saturating_sub(fixed).clamp(0, 34);
    let mut spans = vec![
        Span::styled("⬇ ", t.accent()),
        Span::styled(name, t.bold()),
        Span::raw(" "),
    ];
    if bar_w >= 6 {
        spans.push(Span::styled(
            bar(bar_w, d.fraction().unwrap_or(0.0)),
            t.accent(),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(nums, t.base()));
    spans.push(Span::styled(tail, t.dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The list of downloads with the keys to control them.
pub fn render_downloads(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let list = &app.downloads.list;
    let h = (list.len() as u16 * 3 + 5).clamp(8, 28).min(area.height);
    let r = centered(area, 100, h);
    f.render_widget(Clear, r);
    let block = panel(
        Line::from(Span::styled(
            " DOWNLOADS · ↑↓ select · x stop · r resume/retry · d remove · c clear finished · Esc close ",
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 2 || inner.width < 20 {
        return;
    }
    if list.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::styled("No downloads yet.", t.dim()),
                Line::styled(
                    "Open /shop, pick a model and press Enter: it appears here with its progress.",
                    t.dim(),
                ),
            ]),
            inner,
        );
        return;
    }
    let w = inner.width as usize;
    // every entry: a title row, a bar row, and the reason/result wrapped under it
    let mut lines: Vec<Line> = vec![];
    let mut first_line_of: Vec<usize> = vec![];
    for (i, d) in list.iter().enumerate() {
        first_line_of.push(lines.len());
        let on = i == app.downloads.selected;
        let st = state_style(app, d.state);
        let icon = match d.state {
            DlState::Downloading => "⬇",
            DlState::Done => "✓",
            DlState::Failed => "✖",
            DlState::Stopped => "⏸",
        };
        let label = match d.state {
            DlState::Downloading => d.status.clone(),
            DlState::Done => "downloaded".into(),
            DlState::Failed => "failed".into(),
            DlState::Stopped => "stopped".into(),
        };
        let head = format!("{} {icon} {}", if on { "❯" } else { " " }, d.name);
        let head_style = if on { t.selected() } else { t.bold() };
        lines.push(Line::from(vec![
            Span::styled(
                truncate(&head, w.saturating_sub(label.chars().count() + 3)),
                head_style,
            ),
            Span::raw("  "),
            Span::styled(label, st),
        ]));
        let bar_w = w.saturating_sub(4).clamp(10, 40);
        let frac = match d.state {
            DlState::Done => 1.0,
            _ => d.fraction().unwrap_or(0.0),
        };
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(bar(bar_w, frac), st),
            Span::raw("  "),
            Span::styled(
                truncate(&numbers(d, app.now), w.saturating_sub(bar_w + 6)),
                t.base(),
            ),
        ]));
        if !d.message.is_empty() {
            let ms = if d.state == DlState::Failed {
                t.err()
            } else {
                t.dim()
            };
            for row in wrap_text(&d.message, w.saturating_sub(4))
                .into_iter()
                .take(4)
            {
                lines.push(Line::from(vec![Span::raw("    "), Span::styled(row, ms)]));
            }
        }
        lines.push(Line::raw(""));
    }
    // keep the highlighted entry in view
    let sel_start = first_line_of
        .get(app.downloads.selected)
        .copied()
        .unwrap_or(0);
    let sel_end = first_line_of
        .get(app.downloads.selected + 1)
        .copied()
        .unwrap_or(lines.len());
    let room = inner.height as usize;
    let offset = if sel_end > room {
        (sel_end - room).min(sel_start)
    } else {
        0
    };
    let lines: Vec<Line> = lines.into_iter().skip(offset).take(room).collect();
    f.render_widget(Paragraph::new(lines), inner);
}
