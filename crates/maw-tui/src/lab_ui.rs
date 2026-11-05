//! The Lab screen: a live view of a tiny language model learning from the student's own text.
//!
//! Left, a 3D scatter of the real token embeddings that you can rotate (they move while the model
//! trains); right, the loss and learning-rate curves and the attention weights the model used on a
//! sentence; below, a lesson that explains one concept using the numbers on screen right now, the
//! text the model is writing, and what it thinks comes next. Everything is read from the running
//! model (`maw_lab`), nothing is drawn from imagination.

use crate::theme::Theme;
use maw_lab::probe::rope_frequencies;
use maw_lab::{LabSession, Snapshot, Status};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CLine, Points};
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph, Wrap};
use ratatui::Frame;

/// What the screen itself remembers between frames.
#[derive(Clone, Debug)]
pub struct LabView {
    pub yaw: f64,
    pub pitch: f64,
    pub zoom: f64,
    pub auto_rotate: bool,
    pub lesson: usize,
    pub layer: usize,
    pub head: usize,
    /// `Some(text)` while the user is typing a new sentence for the probe.
    pub editing: Option<String>,
}

impl Default for LabView {
    fn default() -> Self {
        LabView {
            yaw: 0.6,
            pitch: 0.35,
            zoom: 1.0,
            auto_rotate: true,
            lesson: 0,
            layer: 0,
            head: 0,
            editing: None,
        }
    }
}

pub const LESSONS: usize = 10;

// ------------------------------------------------------------------ 3D

/// Rotate a point by `yaw` (around the vertical axis) then `pitch` (around the horizontal one) and
/// look at it with a little perspective. Returns screen x, screen y and depth (larger = nearer).
pub fn project(p: [f64; 3], yaw: f64, pitch: f64) -> (f64, f64, f64) {
    let (sy, cy) = yaw.sin_cos();
    let x1 = p[0] * cy + p[2] * sy;
    let z1 = -p[0] * sy + p[2] * cy;
    let (sp, cp) = pitch.sin_cos();
    let y2 = p[1] * cp - z1 * sp;
    let z2 = p[1] * sp + z1 * cp;
    let persp = 1.0 / (1.0 - 0.25 * z2).max(0.3);
    (x1 * persp, y2 * persp, z2)
}

/// Scale a cloud so its largest coordinate is 1 (keeps the shape, fits the box).
pub fn normalise(points: &[[f64; 3]]) -> Vec<[f64; 3]> {
    let m = points
        .iter()
        .flat_map(|p| p.iter())
        .fold(0.0f64, |a, b| a.max(b.abs()));
    if m < 1e-12 {
        return points.to_vec();
    }
    points
        .iter()
        .map(|p| [p[0] / m, p[1] / m, p[2] / m])
        .collect()
}

fn shade(depth: f64) -> Color {
    // near points bright, far points dim
    let t = ((depth + 1.2) / 2.4).clamp(0.0, 1.0);
    let v = (90.0 + 165.0 * t) as u8;
    Color::Rgb(v, v, 255.min(v as u16 + 30) as u8)
}

fn render_cloud(f: &mut Frame, area: Rect, snap: &Snapshot, view: &LabView, t: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border(true))
        .title(Span::styled(
            " EMBEDDINGS · every dot is a token, placed by what the model learned about it ",
            t.bold(),
        ));
    let pts = normalise(&snap.emb3);
    let (yaw, pitch, zoom) = (view.yaw, view.pitch, view.zoom);
    let labels = &snap.labels;
    let show_labels = pts.len() <= 160;
    let canvas = Canvas::default()
        .block(block)
        .marker(Marker::Braille)
        .x_bounds([-1.5 / zoom, 1.5 / zoom])
        .y_bounds([-1.5 / zoom, 1.5 / zoom])
        .paint(move |ctx| {
            // the three axes, each a line through the middle (the principal components)
            for (axis, name, col) in [
                ([1.0, 0.0, 0.0], "PC1", Color::Red),
                ([0.0, 1.0, 0.0], "PC2", Color::Green),
                ([0.0, 0.0, 1.0], "PC3", Color::Blue),
            ] {
                let a = project([-axis[0], -axis[1], -axis[2]], yaw, pitch);
                let b = project(axis, yaw, pitch);
                ctx.draw(&CLine {
                    x1: a.0,
                    y1: a.1,
                    x2: b.0,
                    y2: b.1,
                    color: col,
                });
                ctx.print(b.0, b.1, Line::styled(name, Style::default().fg(col)));
            }
            let mut proj: Vec<(f64, f64, f64, usize)> = pts
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let q = project(*p, yaw, pitch);
                    (q.0, q.1, q.2, i)
                })
                .collect();
            proj.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
            for (x, y, depth, i) in proj {
                let c = shade(depth);
                ctx.draw(&Points {
                    coords: &[(x, y)],
                    color: c,
                });
                if show_labels {
                    if let Some(l) = labels.get(i) {
                        ctx.print(x, y, Line::styled(l.clone(), Style::default().fg(c)));
                    }
                }
            }
        });
    f.render_widget(canvas, area);
}

// ------------------------------------------------------------------ curves

fn render_curves(f: &mut Frame, area: Rect, snap: &Snapshot, t: &Theme) {
    let parts =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);
    let loss: Vec<(f64, f64)> = snap
        .history
        .iter()
        .map(|h| (h.step as f64, h.loss))
        .collect();
    let val: Vec<(f64, f64)> = snap
        .history
        .iter()
        .filter_map(|h| h.val_loss.map(|v| (h.step as f64, v)))
        .collect();
    let max_loss = loss
        .iter()
        .chain(val.iter())
        .map(|p| p.1)
        .fold(0.5f64, f64::max)
        .min(20.0);
    let steps = snap.steps.max(1) as f64;
    let mut sets = vec![Dataset::default()
        .name("training loss")
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&loss)];
    if !val.is_empty() {
        sets.push(
            Dataset::default()
                .name("held-out loss")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Yellow))
                .data(&val),
        );
    }
    let chart = Chart::new(sets)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(t.border(false))
                .title(Span::styled(" LOSS · lower = less surprised ", t.bold())),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, steps])
                .labels(vec!["0".to_string(), format!("{}", snap.steps)]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, max_loss])
                .labels(vec!["0".to_string(), format!("{max_loss:.1}")]),
        );
    f.render_widget(chart, parts[0]);
    let lr: Vec<(f64, f64)> = (0..snap.steps.max(1))
        .map(|s| (s as f64, snap.train_cfg.lr_at(s)))
        .collect();
    let now = [(
        snap.step as f64,
        snap.train_cfg
            .lr_at(snap.step.min(snap.steps.saturating_sub(1))),
    )];
    let lr_chart = Chart::new(vec![
        Dataset::default()
            .name(snap.train_cfg.schedule.name())
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green))
            .data(&lr),
        Dataset::default()
            .name("now")
            .marker(Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::default().fg(Color::White))
            .data(&now),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(t.border(false))
            .title(Span::styled(
                " LEARNING RATE · warm-up, then decay ",
                t.bold(),
            )),
    )
    .x_axis(Axis::default().bounds([0.0, steps]))
    .y_axis(
        Axis::default()
            .bounds([0.0, snap.train_cfg.lr * 1.05])
            .labels(vec!["0".to_string(), format!("{:.4}", snap.train_cfg.lr)]),
    );
    f.render_widget(lr_chart, parts[1]);
}

// ------------------------------------------------------------------ attention

fn render_attention(f: &mut Frame, area: Rect, snap: &Snapshot, view: &LabView, t: &Theme) {
    let cfg = &snap.config;
    let (layer, head) = (view.layer % cfg.layers.max(1), view.head % cfg.heads.max(1));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border(false))
        .title(Span::styled(
            format!(
                " ATTENTION · layer {}/{} head {}/{} (l / h to change) ",
                layer + 1,
                cfg.layers,
                head + 1,
                cfg.heads
            ),
            t.bold(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(map) = snap.attention.get(layer).and_then(|l| l.get(head)) else {
        f.render_widget(
            Paragraph::new("waiting for the first step...").style(t.dim()),
            inner,
        );
        return;
    };
    let n = snap.probe_pieces.len();
    if n == 0 || inner.height < 3 || inner.width < 8 {
        return;
    }
    let shades = [' ', '░', '▒', '▓', '█'];
    let label_w = 3usize;
    let cell_w = if (inner.width as usize) >= label_w + n * 2 {
        2
    } else {
        1
    };
    let mut lines: Vec<Line> = vec![];
    let mut head_line = " ".repeat(label_w);
    for p in &snap.probe_pieces {
        let c: String = p.chars().take(cell_w).collect();
        head_line.push_str(&format!("{c:<w$}", w = cell_w));
    }
    lines.push(Line::styled(head_line, t.dim()));
    for i in 0..n.min(inner.height as usize - 1) {
        let lab: String = snap.probe_pieces[i].chars().take(2).collect();
        let mut spans = vec![Span::styled(format!("{lab:<w$}", w = label_w), t.dim())];
        for j in 0..n {
            let w = map[i * n + j];
            let ch = if j > i {
                '·' // the future: not allowed to look
            } else {
                shades[((w * 4.0).ceil() as usize).min(4)]
            };
            let col = if j > i {
                Color::DarkGray
            } else {
                Color::Rgb(120 + (135.0 * w) as u8, 160 + (95.0 * w) as u8, 255)
            };
            spans.push(Span::styled(
                ch.to_string().repeat(cell_w),
                Style::default().fg(col),
            ));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ------------------------------------------------------------------ lessons

fn bar(frac: f64, w: usize) -> String {
    let n = (frac.clamp(0.0, 1.0) * w as f64).round() as usize;
    format!("{}{}", "█".repeat(n), "░".repeat(w - n))
}

/// The title and text of one lesson, with the live numbers filled in.
pub fn lesson(idx: usize, s: &Snapshot) -> (String, Vec<String>) {
    let c = &s.config;
    let last = s.history.last();
    let loss = last.map(|h| h.loss);
    let fmt_loss = |l: Option<f64>| l.map(|x| format!("{x:.3}")).unwrap_or_else(|| "—".into());
    let random = (c.vocab as f64).ln();
    match idx % LESSONS {
        0 => (
            "1/10 Tokenizer: text becomes numbers".into(),
            vec![
                format!("The model never sees letters, only numbers. Tokenizer: {}, vocabulary of {} tokens.", s.tokenizer_name, c.vocab),
                format!("Your sentence \"{}\" is cut into: {}", s.probe_text, s.probe_pieces.join(" | ")),
                "A BPE tokenizer starts from characters and keeps merging the pair that appears most often (try --tokenizer bpe:100).".into(),
                "Fewer tokens for the same text = more text fits in the context window.".into(),
            ],
        ),
        1 => (
            "2/10 Embedding: every token is a list of numbers".into(),
            vec![
                format!("Each of the {} tokens is a point with {} coordinates, learned by the model. The 3D picture squeezes those {} numbers into 3 directions (PCA).", c.vocab, c.dim, c.dim),
                "At step 0 the points are random. Watch them move: tokens used in similar ways drift together.".into(),
                format!("This table of {} x {} = {} numbers is {:.0}% of the model.", c.vocab, c.dim, c.vocab * c.dim, 100.0 * (c.vocab * c.dim) as f64 / s.n_params as f64),
            ],
        ),
        2 => (
            "3/10 Attention: which earlier tokens matter?".into(),
            vec![
                format!("{} layers x {} heads. Each row of the map says, for one token, how much it looks at every token before it (a row sums to 1).", c.layers, c.heads),
                "The dots above the diagonal are the future: a language model is not allowed to peek at what comes next.".into(),
                "Different heads learn different habits: look at the previous token, at the first token, at the same letter earlier...".into(),
            ],
        ),
        3 => (
            "4/10 Loss: how surprised is the model?".into(),
            vec![
                format!("Loss now {} (held-out {}). A model that guesses at random has loss ln({}) = {:.3}.", fmt_loss(loss), fmt_loss(last.and_then(|h| h.val_loss).or_else(|| s.history.iter().rev().find_map(|h| h.val_loss))), c.vocab, random),
                match loss {
                    Some(l) => format!("Perplexity e^loss = {:.1}: on average the model is as unsure as if it were choosing between {:.1} equally likely tokens.", l.exp(), l.exp()),
                    None => "Perplexity = e^loss: how many equally likely tokens it is choosing between.".into(),
                },
                "Training means nudging every number a little in the direction that lowers this one number (gradient descent).".into(),
            ],
        ),
        4 => {
            let tc = &s.train_cfg;
            (
                "5/10 Learning rate and its decay".into(),
                vec![
                    format!("Schedule {}: rises over {} steps to {:.4}, then falls toward {:.5} (curve at the right).", tc.schedule.name(), tc.warmup, tc.lr, tc.lr * tc.min_lr_frac),
                    format!("Right now (step {}): {:.5}.", s.step, last.map(|h| h.lr).unwrap_or(0.0)),
                    "Big steps early let the model find the right area quickly; small steps later let it settle without jumping out.".into(),
                    "Exponential decay multiplies by the same factor every step; cosine follows half a wave; constant never decays.".into(),
                ],
            )
        }
        5 => {
            let tc = &s.train_cfg;
            (
                "6/10 Weight decay and gradient clipping".into(),
                vec![
                    format!("Weight decay {}: every step the matrices shrink by lr x {} of their size, apart from the gradient. Huge weights are how a model memorises.", tc.weight_decay, tc.weight_decay),
                    format!("Gradient length now {} (clip at {}). Longer than the clip and it is scaled down, so one odd batch cannot throw the model off.", last.map(|h| format!("{:.2}", h.grad_norm)).unwrap_or("—".into()), tc.clip),
                    "AdamW also keeps a running average of each gradient (and of its square), so each number gets a step size of its own.".into(),
                ],
            )
        }
        6 => {
            let val = s.history.iter().rev().find_map(|h| h.val_loss);
            let gap = match (loss, val) {
                (Some(t), Some(v)) => format!("training {t:.3} vs held-out {v:.3}: gap {:+.3}", v - t),
                _ => "no held-out text yet".into(),
            };
            (
                "7/10 Over-fitting: knowing by heart is not learning".into(),
                vec![
                    format!("{} tokens train the model; {} are kept aside and never trained on. Now: {gap}.", s.train_tokens, s.val_tokens),
                    "While both fall, the model is learning something general. When the held-out loss stops falling or rises while training loss keeps falling, it is memorising.".into(),
                    "Fixes: more text, a smaller model, more weight decay, or stopping earlier. Try training on a tiny file and watch the yellow line.".into(),
                ],
            )
        }
        7 => {
            let top: Vec<String> = s.next.iter().map(|(t, p)| format!("'{t}' {:>5.1}% {}", p * 100.0, bar(*p, 12))).collect();
            let mut v = vec![
                format!("After \"{}\" the model gives these likeliest next tokens (entropy {:.2} bits; 0 = certain):", s.probe_text, s.next_entropy_bits),
            ];
            v.extend(top);
            v.push("Temperature divides the scores before choosing: low = always the favourite, high = adventurous. top-k keeps only the k favourites. (maw lab sample --temperature 1.2 --top-k 5)".into());
            ("8/10 Sampling: choosing the next token".into(), v)
        }
        8 => {
            let kv = 2 * c.layers * c.ctx * c.dim * 2;
            let f = rope_frequencies(c.head_dim().max(2), 10000.0);
            (
                "9/10 Context window, KV cache and positional decay".into(),
                vec![
                    format!("The model reads at most {} tokens at once (the context window); older text is simply not seen.", c.ctx),
                    format!("Remembering keys and values of every token for a window this size would take about {} bytes in 16-bit; real models with 100,000s of tokens need gigabytes.", kv),
                    format!("Real models mark position with rotations whose speeds DECAY geometrically (RoPE): for head size {} the first speeds are {}. This tiny model learns its position vectors instead.", c.head_dim(), f.iter().take(4).map(|x| format!("{x:.3}")).collect::<Vec<_>>().join(", ")),
                ],
            )
        }
        _ => {
            let mut v = vec![format!("{} numbers in total ({:.1} KB at 8 bytes each). Where they are:", s.n_params, s.n_params as f64 * 8.0 / 1024.0)];
            let max = s.groups.iter().map(|g| g.1).max().unwrap_or(1).max(1);
            for (name, n) in &s.groups {
                v.push(format!("{:<30} {:>8}  {}", name, n, bar(*n as f64 / max as f64, 16)));
            }
            v.push(format!("A 7-billion-parameter model has {:.0} thousand times more.", 7.0e9 / s.n_params as f64 / 1000.0));
            ("10/10 Parameters: where the knowledge lives".into(), v)
        }
    }
}

// ------------------------------------------------------------------ the whole screen

pub fn render_lab(
    f: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    view: &LabView,
    theme: &Theme,
    paused: bool,
) {
    let t = theme;
    if area.width < 60 || area.height < 22 {
        f.render_widget(
            Paragraph::new("The Lab needs a terminal of at least 60x22. Enlarge the window.")
                .style(t.dim())
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(12),
        Constraint::Length(1),
    ])
    .split(area);
    // header
    let state = match (snap.status, paused) {
        (Status::Done, _) => "DONE",
        (_, true) => "PAUSED",
        _ => "TRAINING",
    };
    let c = &snap.config;
    let head = format!(
        " MAW LAB │ {state} step {}/{} │ {} layers x {} heads x {} dims │ {} parameters │ {} │ {:.1} steps/s ",
        snap.step, snap.steps, c.layers, c.heads, c.dim, snap.n_params, snap.tokenizer_name, snap.steps_per_sec
    );
    f.render_widget(Paragraph::new(Line::styled(head, t.header())), rows[0]);
    // main: cloud | curves + attention
    let cols =
        Layout::horizontal([Constraint::Percentage(54), Constraint::Percentage(46)]).split(rows[1]);
    render_cloud(f, cols[0], snap, view, t);
    let right =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(cols[1]);
    render_curves(f, right[0], snap, t);
    render_attention(f, right[1], snap, view, t);
    // bottom: lesson | writing
    let bottom =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(rows[2]);
    let (title, lines) = lesson(view.lesson, snap);
    let lesson_block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border(true))
        .title(Span::styled(
            format!(" {title} · Tab next lesson "),
            t.bold(),
        ));
    f.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .block(lesson_block)
            .wrap(Wrap { trim: false }),
        bottom[0],
    );
    let write_block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.border(false))
        .title(Span::styled(
            " WHAT THE MODEL WRITES (s = again) ",
            t.bold(),
        ));
    let sample = if snap.sample.is_empty() {
        "(nothing yet)".to_string()
    } else {
        snap.sample.replace('\n', " ⏎ ")
    };
    f.render_widget(
        Paragraph::new(sample)
            .block(write_block)
            .wrap(Wrap { trim: false }),
        bottom[1],
    );
    // footer
    let foot = match &view.editing {
        Some(text) => format!(" sentence to study: {text}█   Enter apply · Esc cancel"),
        None => {
            let note = if snap.notice.is_empty() {
                String::new()
            } else {
                format!(" │ {}", snap.notice)
            };
            format!(" Space pause/resume · ←→↑↓ rotate · +/- zoom · a auto-rotate · Tab lessons · l/h layer/head · p your own sentence · s sample · w save · q quit{note}")
        }
    };
    f.render_widget(Paragraph::new(Line::styled(foot, t.footer())), rows[3]);
}

// ------------------------------------------------------------------ keys

pub enum LabKey {
    None,
    Quit,
}

pub fn handle_key(
    view: &mut LabView,
    session: &LabSession,
    key: crossterm::event::KeyEvent,
    snap: &Snapshot,
    save_path: &std::path::Path,
) -> LabKey {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if key.kind == KeyEventKind::Release {
        return LabKey::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return LabKey::Quit;
    }
    if let Some(text) = &mut view.editing {
        match key.code {
            KeyCode::Esc => view.editing = None,
            KeyCode::Enter => {
                let t = std::mem::take(text);
                view.editing = None;
                if !t.is_empty() {
                    session.set_probe(t);
                }
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c) if text.chars().count() < 60 => text.push(c),
            _ => {}
        }
        return LabKey::None;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return LabKey::Quit,
        KeyCode::Char(' ') => session.toggle_pause(),
        KeyCode::Left => view.yaw -= 0.1,
        KeyCode::Right => view.yaw += 0.1,
        KeyCode::Up => view.pitch = (view.pitch - 0.1).clamp(-1.5, 1.5),
        KeyCode::Down => view.pitch = (view.pitch + 0.1).clamp(-1.5, 1.5),
        KeyCode::Char('+') | KeyCode::Char('=') => view.zoom = (view.zoom * 1.15).min(6.0),
        KeyCode::Char('-') | KeyCode::Char('_') => view.zoom = (view.zoom / 1.15).max(0.4),
        KeyCode::Char('a') => view.auto_rotate = !view.auto_rotate,
        KeyCode::Tab => view.lesson = (view.lesson + 1) % LESSONS,
        KeyCode::BackTab => view.lesson = (view.lesson + LESSONS - 1) % LESSONS,
        KeyCode::Char('l') => view.layer = (view.layer + 1) % snap.config.layers.max(1),
        KeyCode::Char('h') => view.head = (view.head + 1) % snap.config.heads.max(1),
        KeyCode::Char('s') => session.request_sample(),
        KeyCode::Char('w') => session.request_save(save_path.to_path_buf()),
        KeyCode::Char('p') => view.editing = Some(String::new()),
        KeyCode::Char(c @ '1'..='9') => view.lesson = c as usize - '1' as usize,
        KeyCode::Char('0') => view.lesson = 9,
        _ => {}
    }
    LabKey::None
}

/// Run the Lab full screen until the student quits. The training thread stops when this returns.
pub fn run_lab(session: LabSession, save_path: std::path::PathBuf) -> anyhow::Result<()> {
    use crossterm::event::{self, Event};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use std::time::{Duration, Instant};
    let _guard = crate::runtime::TerminalGuard::enter(false)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let theme = Theme::from_env(false);
    let mut view = LabView::default();
    let mut last = Instant::now();
    loop {
        let snap = session.snapshot();
        terminal.draw(|f| render_lab(f, f.area(), &snap, &view, &theme, session.is_paused()))?;
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(k) = event::read()? {
                if let LabKey::Quit = handle_key(&mut view, &session, k, &snap, &save_path) {
                    break;
                }
            }
        }
        let dt = last.elapsed().as_secs_f64();
        last = Instant::now();
        if view.auto_rotate && view.editing.is_none() {
            view.yaw += 0.35 * dt;
        }
    }
    session.stop();
    Ok(())
}
