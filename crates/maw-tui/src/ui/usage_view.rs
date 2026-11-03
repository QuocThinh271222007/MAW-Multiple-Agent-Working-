//! Token-usage view: per-agent table with share bars, rate and a 30-minute sparkline, plus the
//! tasks that consumed the most tokens. All figures are what the providers report.

use super::widgets::*;
use crate::app::App;
use crate::usage::{derive_usage, fmt_cost, fmt_tokens, sparkline, SPARK_MINUTES};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render_tokens(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(
            " TOKEN USAGE · reported by the providers, never estimated ",
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(snap) = &app.snap else {
        f.render_widget(Paragraph::new("No data yet.").style(t.dim()), inner);
        return;
    };
    let w = inner.width as usize;
    let r = derive_usage(&snap.agents, &snap.tasks, &app.usage_events, app.now);

    let mut lines: Vec<Line> = vec![];
    lines.push(Line::from(vec![
        Span::styled("Total ", t.dim()),
        Span::styled(format!("{} tokens", fmt_tokens(r.total_tokens)), t.bold()),
        Span::styled(
            format!(
                "   in {} · out {}",
                fmt_tokens(r.total_input),
                fmt_tokens(r.total_output)
            ),
            t.base(),
        ),
        Span::styled(
            match r.total_cost_usd {
                Some(c) => format!("   cost (Claude's own estimate) {}", fmt_cost(c)),
                None => "   cost: not reported".to_string(),
            },
            t.base(),
        ),
        Span::styled(
            format!(
                "   {}/{} agents reporting",
                r.agents_reporting, r.agents_total
            ),
            t.dim(),
        ),
    ]));
    lines.push(Line::raw(""));

    // columns by available width
    let show_model = w >= 108;
    let show_share = w >= 82;
    let show_rate = w >= 94;
    let show_spark = w >= 138;
    let mut head = format!("{} {} ", pad("AGENT", 15), pad("PROVIDER", 8));
    if show_model {
        head.push_str(&format!("{} ", pad("MODEL / EFFORT", 22)));
    }
    head.push_str(&format!(
        "{} {} {} ",
        pad("INPUT", 8),
        pad("OUTPUT", 8),
        pad("TOTAL", 8)
    ));
    if show_share {
        head.push_str(&format!("{} ", pad("SHARE", 16)));
    }
    if show_rate {
        head.push_str(&format!("{} ", pad("TOK/MIN", 8)));
    }
    head.push_str(&pad("COST", 8));
    if show_spark {
        head.push_str(&format!(" LAST {SPARK_MINUTES} MIN"));
    }
    lines.push(Line::from(Span::styled(truncate(&head, w), t.bold())));

    for row in &r.agents {
        let n = |v: Option<u64>| v.map(fmt_tokens).unwrap_or_else(|| "—".into());
        let mut s = format!("{} {} ", pad(&row.name, 15), pad(row.provider.as_str(), 8));
        if show_model {
            let m = format!(
                "{}{}",
                row.model.as_deref().unwrap_or("default"),
                row.effort
                    .as_ref()
                    .map(|e| format!(" · {e}"))
                    .unwrap_or_default()
            );
            s.push_str(&format!("{} ", pad(&m, 22)));
        }
        s.push_str(&format!(
            "{} {} {} ",
            pad(&n(row.input), 8),
            pad(&n(row.output), 8),
            pad(&n(row.total), 8)
        ));
        if show_share {
            s.push_str(&format!(
                "{} {:>3.0}% ",
                bar(10, row.share),
                row.share * 100.0
            ));
        }
        if show_rate {
            let rate = row
                .rate_per_min
                .map(|r| fmt_tokens(r.round() as u64))
                .unwrap_or_else(|| "—".into());
            s.push_str(&format!("{} ", pad(&rate, 8)));
        }
        s.push_str(&pad(
            &row.cost_usd.map(fmt_cost).unwrap_or_else(|| "—".into()),
            8,
        ));
        if show_spark {
            s.push(' ');
            s.push_str(&sparkline(&row.spark));
        }
        let st = if row.reported { t.base() } else { t.dim() };
        lines.push(Line::from(Span::styled(truncate(&s, w), st)));
        if !row.reported {
            lines.push(Line::from(Span::styled(
                "  no usage reported yet (the provider sends it after a turn)",
                t.dim(),
            )));
        }
    }
    if r.agents.is_empty() {
        lines.push(Line::from(Span::styled("No agents yet.", t.dim())));
    }

    // tasks that used the most tokens
    let used = lines.len();
    let notes_h = 4usize;
    let room = (inner.height as usize).saturating_sub(used + notes_h + 2);
    if room >= 2 {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "TOKENS BY TASK (increments recorded while the task was the agent's current one)",
            t.bold(),
        )));
        let max = r.tasks.first().map(|x| x.tokens).unwrap_or(0).max(1);
        for tk in r.tasks.iter().take(room.saturating_sub(1).min(8)) {
            let line = format!(
                "{} {} {} {}",
                pad(&tk.key, 6),
                pad(&fmt_tokens(tk.tokens), 8),
                bar(12, tk.tokens as f64 / max as f64),
                tk.title
            );
            lines.push(Line::from(Span::styled(truncate(&line, w), t.base())));
        }
        if r.tasks.is_empty() {
            lines.push(Line::from(Span::styled(
                "  nothing attributed to a task yet",
                t.dim(),
            )));
        }
        if r.unattributed_tokens > 0 {
            lines.push(Line::from(Span::styled(
                truncate(
                    &format!(
                        "  + {} tokens outside any task (planning, Leader overhead, idle turns)",
                        fmt_tokens(r.unattributed_tokens)
                    ),
                    w,
                ),
                t.dim(),
            )));
        }
    }

    // footnotes at the bottom
    let notes = [
        "Totals are each provider's cumulative figure for its current session; they restart when a session is not resumed.",
        "TOK/MIN = tokens added in the last 5 minutes. Cost is Claude Code's client-side estimate; Codex reports tokens only.",
        "Provider quota or plan limits are not exposed by the providers, so none are shown.",
    ];
    let height = inner.height as usize;
    let mut shown = lines.len();
    if shown + 1 < height {
        lines.push(Line::raw(""));
        shown += 1;
    }
    for n in notes {
        for row in wrap_text(n, w.max(10)) {
            if shown < height {
                lines.push(Line::from(Span::styled(row, t.dim())));
                shown += 1;
            }
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}
