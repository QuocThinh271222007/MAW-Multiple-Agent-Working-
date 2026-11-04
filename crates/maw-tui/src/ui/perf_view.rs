//! Performance view: machine load while local models run. CPU, RAM, GPU, VRAM and generation
//! speed as line charts over a selectable window, plus the models held in memory and how much of
//! each sits on the GPU. Every figure is a reading; a source that is missing says so.

use super::widgets::*;
use crate::app::App;
use chrono::{DateTime, Utc};
use maw_core::perf::{PerfHistory, PerfSample};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph};
use ratatui::Frame;

/// Selectable time windows in seconds (key `w` cycles).
pub const WINDOWS: [i64; 3] = [60, 300, 1800];

pub fn window_label(secs: i64) -> String {
    if secs < 120 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

fn gb(mb: u64) -> String {
    format!("{:.1}", mb as f64 / 1024.0)
}

fn points<F: Fn(&PerfSample) -> Option<f64>>(
    samples: &[PerfSample],
    now: DateTime<Utc>,
    f: F,
) -> Vec<(f64, f64)> {
    samples
        .iter()
        .filter_map(|s| {
            let v = f(s)?;
            let x = -((now - s.at).num_milliseconds() as f64 / 1000.0);
            Some((x.min(0.0), v))
        })
        .collect()
}

fn pct(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => Some(u as f64 * 100.0 / t as f64),
        _ => None,
    }
}

fn peak(d: &[(f64, f64)]) -> f64 {
    d.iter().map(|p| p.1).fold(0.0, f64::max)
}

struct Spec<'a> {
    title: String,
    data: Vec<(f64, f64)>,
    /// A second series drawn in a different style (prompt speed).
    extra: Vec<(f64, f64)>,
    ymax: f64,
    style: Style,
    empty_note: &'a str,
}

fn chart(f: &mut Frame, area: Rect, app: &App, window: f64, s: &Spec) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(format!(" {} ", s.title), t.bold())),
        false,
        t,
    );
    if s.data.is_empty() && s.extra.is_empty() {
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(s.empty_note.to_string()).style(t.dim()),
            inner,
        );
        return;
    }
    let mut sets = vec![Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(s.style)
        .data(&s.data)];
    if !s.extra.is_empty() {
        sets.push(
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(t.dim())
                .data(&s.extra),
        );
    }
    let ymid = format!("{:.0}", s.ymax / 2.0);
    let chart = Chart::new(sets)
        .block(block)
        .x_axis(
            Axis::default()
                .style(t.dim())
                .bounds([-window, 0.0])
                .labels(vec![
                    Span::styled(format!("-{}", window_label(window as i64)), t.dim()),
                    Span::styled("now", t.dim()),
                ]),
        )
        .y_axis(
            Axis::default()
                .style(t.dim())
                .bounds([0.0, s.ymax])
                .labels(vec![
                    Span::styled("0", t.dim()),
                    Span::styled(ymid, t.dim()),
                    Span::styled(format!("{:.0}", s.ymax), t.dim()),
                ]),
        );
    f.render_widget(chart, area);
}

fn models_panel(f: &mut Frame, area: Rect, app: &App, latest: Option<&PerfSample>) {
    let t = &app.theme;
    let block = panel(
        Line::from(Span::styled(" MODELS IN MEMORY · GPU/RAM split ", t.bold())),
        false,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line> = vec![];
    match latest {
        Some(s) if !s.loaded.is_empty() => {
            for m in &s.loaded {
                let share = m.gpu_share();
                let w = (inner.width as usize).saturating_sub(34).clamp(6, 24);
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<20.20} ", m.name), t.bold()),
                    Span::styled(format!("{:>5.1} GB ", m.size_bytes as f64 / 1e9), t.base()),
                    Span::styled(bar(w, share as f64), t.accent()),
                    Span::styled(
                        format!(" {:.0}% GPU", share * 100.0),
                        if share < 0.99 { t.warn() } else { t.ok() },
                    ),
                ]));
            }
            if s.loaded.iter().any(|m| m.gpu_share() < 0.99) {
                lines.push(Line::styled(
                    "part of a model is in system RAM: it runs slower than a model that fits the GPU",
                    t.dim(),
                ));
            }
        }
        Some(_) => lines.push(Line::styled(
            "no model is loaded right now (it loads when an agent first uses it)",
            t.dim(),
        )),
        None => lines.push(Line::styled("no samples yet", t.dim())),
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_perf(f: &mut Frame, area: Rect, app: &App) {
    let t = &app.theme;
    let win = WINDOWS[app.perf_window % WINDOWS.len()];
    let block = panel(
        Line::from(Span::styled(
            format!(
                " PERFORMANCE · last {} · {} ",
                window_label(win),
                if app.view == crate::app::View::Perf {
                    "w changes the window"
                } else {
                    "/performance 1m|5m|30m changes the window · Esc closes"
                }
            ),
            t.bold(),
        )),
        true,
        t,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let hist: &PerfHistory = &app.perf;
    if hist.is_empty() {
        f.render_widget(
            Paragraph::new("Collecting samples (one every 2 s) ...").style(t.dim()),
            inner,
        );
        return;
    }
    let now = app.now;
    let samples = hist.since(now - chrono::Duration::seconds(win));
    let latest = hist.latest();
    let sys = if hist.sources.system.is_empty() {
        "unknown"
    } else {
        hist.sources.system.as_str()
    };
    let gpu_src = if hist.sources.gpu.is_empty() {
        "no GPU tool found (nvidia-smi); GPU charts are unavailable"
    } else {
        hist.sources.gpu.as_str()
    };

    // summary: current values
    let cur = |label: &str, v: Option<String>| -> Vec<Span<'static>> {
        vec![
            Span::styled(format!("{label} "), t.dim()),
            Span::styled(v.unwrap_or_else(|| "—".into()), t.bold()),
            Span::raw("   "),
        ]
    };
    let mut spans: Vec<Span> = vec![];
    if let Some(l) = latest {
        spans.extend(cur("CPU", l.cpu_pct.map(|v| format!("{v:.0}%"))));
        spans.extend(cur(
            "RAM",
            l.ram_used_mb
                .zip(l.ram_total_mb)
                .map(|(u, tot)| format!("{}/{} GB", gb(u), gb(tot))),
        ));
        spans.extend(cur("GPU", l.gpu_pct.map(|v| format!("{v:.0}%"))));
        spans.extend(cur(
            "VRAM",
            l.vram_used_mb
                .zip(l.vram_total_mb)
                .map(|(u, tot)| format!("{}/{} GB", gb(u), gb(tot))),
        ));
        if let Some(c) = l.gpu_temp_c {
            spans.extend(cur("temp", Some(format!("{c:.0}°C"))));
        }
        if let Some(p) = l.gpu_power_w {
            spans.extend(cur("power", Some(format!("{p:.0}W"))));
        }
        spans.extend(cur("speed", l.decode_tps.map(|v| format!("{v:.1} tok/s"))));
    }
    // one line that always says which models are in memory and how they are split
    let mut mspans: Vec<Span> = vec![Span::styled("Models in memory: ", t.dim())];
    match latest.map(|l| l.loaded.as_slice()) {
        Some(m) if !m.is_empty() => {
            for (i, lm) in m.iter().enumerate() {
                if i > 0 {
                    mspans.push(Span::raw(" · "));
                }
                let share = lm.gpu_share();
                mspans.push(Span::styled(lm.name.clone(), t.bold()));
                mspans.push(Span::styled(
                    format!(
                        " {:.1} GB, {:.0}% on GPU",
                        lm.size_bytes as f64 / 1e9,
                        share * 100.0
                    ),
                    if share < 0.99 { t.warn() } else { t.ok() },
                ));
            }
        }
        _ => mspans.push(Span::styled("none", t.dim())),
    }
    let head = vec![
        Line::from(spans),
        Line::from(mspans),
        Line::styled(format!("CPU/RAM: {sys} · GPU: {gpu_src}"), t.dim()),
    ];
    let [top, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(inner);
    f.render_widget(Paragraph::new(head), top);

    // charts, most useful for local models first
    let window = win as f64;
    let gpu = points(&samples, now, |s| s.gpu_pct.map(f64::from));
    let vram = points(&samples, now, |s| pct(s.vram_used_mb, s.vram_total_mb));
    let ram = points(&samples, now, |s| pct(s.ram_used_mb, s.ram_total_mb));
    let cpu = points(&samples, now, |s| s.cpu_pct.map(f64::from));
    let dec = points(&samples, now, |s| s.decode_tps.map(f64::from));
    let pro = points(&samples, now, |s| s.prompt_tps.map(f64::from));
    let speed_max = (peak(&dec).max(peak(&pro)) * 1.15).max(10.0);
    let danger = |data: &[(f64, f64)]| {
        let p = data.last().map(|p| p.1).unwrap_or(0.0);
        if p >= 95.0 {
            t.err()
        } else if p >= 85.0 {
            t.warn()
        } else {
            t.accent()
        }
    };
    let last = |d: &[(f64, f64)]| d.last().map(|p| p.1);
    let title = |name: &str, d: &[(f64, f64)], unit: &str| match last(d) {
        Some(v) => format!("{name} {v:.0}{unit} (peak {:.0}{unit})", peak(d)),
        None => format!("{name} (no data)"),
    };
    let specs = [
        Spec {
            title: match last(&dec) {
                Some(v) => format!(
                    "GENERATION SPEED {v:.1} tok/s (peak {:.1}) · dim = prompt",
                    peak(&dec)
                ),
                None => "GENERATION SPEED (no recent request)".into(),
            },
            data: dec,
            extra: pro,
            ymax: speed_max,
            style: t.ok(),
            empty_note: "shows tokens per second once a local-model agent has answered",
        },
        Spec {
            title: title("VRAM", &vram, "%"),
            style: danger(&vram),
            data: vram,
            extra: vec![],
            ymax: 100.0,
            empty_note: "no GPU memory reading",
        },
        Spec {
            title: title("GPU LOAD", &gpu, "%"),
            style: danger(&gpu),
            data: gpu,
            extra: vec![],
            ymax: 100.0,
            empty_note: "no GPU load reading",
        },
        Spec {
            title: title("RAM", &ram, "%"),
            style: danger(&ram),
            data: ram,
            extra: vec![],
            ymax: 100.0,
            empty_note: "no RAM reading",
        },
        Spec {
            title: title("CPU", &cpu, "%"),
            style: danger(&cpu),
            data: cpu,
            extra: vec![],
            ymax: 100.0,
            empty_note: "no CPU reading",
        },
    ];

    // a detailed models panel above the charts when the terminal is tall enough
    let mut rest = rest;
    let n_loaded = latest.map(|l| l.loaded.len()).unwrap_or(0);
    if rest.height >= 26 && n_loaded > 0 {
        let h = 3 + n_loaded.min(3) as u16 + 1;
        let [strip, below] =
            Layout::vertical([Constraint::Length(h), Constraint::Min(0)]).areas(rest);
        models_panel(f, strip, app, latest);
        rest = below;
    }
    // charts: two columns when wide, as many rows as fit (each cell needs ~7 lines)
    const CELL_H: u16 = 7;
    let cols: u16 = if rest.width >= 100 { 2 } else { 1 };
    let rows_fit = (rest.height / CELL_H).max(1);
    let cells = ((rows_fit * cols) as usize).min(specs.len());
    if rest.height < 5 || rest.width < 30 {
        f.render_widget(
            Paragraph::new("Enlarge the terminal to see the charts.").style(t.dim()),
            rest,
        );
        return;
    }
    let nrows = (cells as u16).div_ceil(cols);
    let row_areas =
        Layout::vertical(vec![Constraint::Ratio(1, nrows as u32); nrows as usize]).split(rest);
    let mut idx = 0usize;
    for r in row_areas.iter() {
        let col_areas =
            Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols as usize]).split(*r);
        for c in col_areas.iter() {
            if idx >= cells {
                break;
            }
            chart(f, *c, app, window, &specs[idx]);
            idx += 1;
        }
    }
}
