//! Terminal lifecycle, the async event loop and action execution.
//!
//! The loop is event driven: it sleeps until a key, a resize, an engine event, a transcript
//! update or the 1 s clock tick arrives. Refreshes and redraws are coalesced to `max_fps`.
//! Blocking SQLite work runs on `spawn_blocking`; provider I/O only ever happens inside
//! spawned action tasks, never on the render path.

use crate::app::{handle_key, handle_mouse, Action, App, Level, TaskDetail, View};
use crate::theme::Theme;
use crate::ui;
use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as CtEvent, EventStream};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use futures::StreamExt;
use maw_core::CoreError;
use maw_orchestrator::{Actor, Engine, Snapshot, Supervisor, TaskPatch};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

#[derive(Clone)]
pub struct TuiOptions {
    /// Force the monochrome theme (also enabled by a non-empty `NO_COLOR`).
    pub monochrome: bool,
    /// Capture the mouse (wheel scrolling).
    pub mouse: bool,
    /// Maximum redraw/refresh rate.
    pub max_fps: u32,
    /// Call `Supervisor::shutdown()` when the user quits (graceful: sessions are closed and
    /// the run is marked clean). Disable if the caller does its own shutdown.
    pub shutdown_on_quit: bool,
    /// Extra hook invoked after the UI loop ends and the terminal is restored.
    pub on_quit: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for TuiOptions {
    fn default() -> Self {
        TuiOptions {
            monochrome: false,
            mouse: true,
            max_fps: 15,
            shutdown_on_quit: true,
            on_quit: None,
        }
    }
}

// ------------------------------------------------------------------ terminal guard

static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

/// Restore the terminal. Idempotent and safe to call from a panic hook.
pub fn restore_terminal() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let mut out = stdout();
        let _ = disable_raw_mode();
        let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
        let _ = out.flush();
    }
}

fn install_panic_hook() {
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
    });
}

/// RAII guard: the terminal is restored when this is dropped, including during unwinding.
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter(mouse: bool) -> Result<TerminalGuard> {
        install_panic_hook();
        enable_raw_mode().context("enabling raw mode")?;
        ACTIVE.store(true, Ordering::SeqCst);
        let mut out = stdout();
        let r = if mouse {
            execute!(out, EnterAlternateScreen, EnableMouseCapture)
        } else {
            execute!(out, EnterAlternateScreen)
        };
        if let Err(e) = r {
            restore_terminal();
            return Err(e).context("entering alternate screen");
        }
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

// ------------------------------------------------------------------ clipboard (OSC 52)

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Escape sequence asking the terminal to place `text` on the system clipboard.
pub fn osc52(text: &str) -> String {
    // cap payload: many terminals refuse very large OSC 52 sequences
    let bytes = text.as_bytes();
    let bytes = &bytes[..bytes.len().min(100_000)];
    format!("\x1b]52;c;{}\x07", base64(bytes))
}

// ------------------------------------------------------------------ data loading

struct RefreshData {
    snap: Snapshot,
    extra: Vec<maw_core::Event>,
    goal_history: Vec<maw_core::GoalRevision>,
    plan_history: Vec<maw_core::PlanRevision>,
    detail: Option<TaskDetail>,
}

async fn blk<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("background task failed")
}

async fn load(
    engine: &Arc<Engine>,
    view: View,
    task_key: Option<String>,
) -> Result<RefreshData, CoreError> {
    let e = engine.clone();
    let joined = blk(move || -> Result<RefreshData, CoreError> {
        let snap = e.snapshot()?;
        let extra = if matches!(view, View::Flow | View::Logs) {
            e.read(|db| db.recent_events(e.project_id(), 4000))?
        } else {
            vec![]
        };
        let (goal_history, plan_history) = if view == View::Goal {
            (e.goal_history()?, e.plan_history()?)
        } else {
            (vec![], vec![])
        };
        let detail = match (view, task_key) {
            (View::Tasks, Some(key)) => {
                let task = e.get_task(&key)?;
                let attempts = e.read(|db| db.list_attempts(&task.id))?;
                let events = e.task_events(&key, 200)?;
                Some(TaskDetail {
                    key,
                    attempts,
                    events,
                })
            }
            _ => None,
        };
        Ok(RefreshData {
            snap,
            extra,
            goal_history,
            plan_history,
            detail,
        })
    })
    .await
    .map_err(|e| CoreError::Internal(e.to_string()))?;
    joined
}

fn apply_refresh(app: &mut App, sup: &Supervisor, engine: &Engine, d: RefreshData) {
    app.apply_snapshot(d.snap);
    app.extra_events = d.extra;
    app.goal_history = d.goal_history;
    app.plan_history = d.plan_history;
    app.task_detail = d.detail;
    app.config = engine.config();
    // transcripts: full for the open agent, a short tail for grid cards
    let sel = app.selected_agent;
    let full = app.view == View::AgentDetail;
    let agents: Vec<_> = app.agents().iter().map(|a| a.id.clone()).collect();
    for (i, id) in agents.into_iter().enumerate() {
        let mut lines = sup.transcript(&id);
        if !(full && i == sel) && lines.len() > 30 {
            lines = lines.split_off(lines.len() - 30);
        }
        app.transcripts.insert(id, lines);
    }
    app.detail_dirty = false;
}

// ------------------------------------------------------------------ action execution

enum UiMsg {
    Toast(String, Level),
}

fn err_text(e: impl std::fmt::Display) -> String {
    maw_core::redact::redact(&e.to_string())
}

/// Execute an action against the engine/supervisor. Returns a success message.
async fn execute(
    engine: Arc<Engine>,
    sup: Arc<Supervisor>,
    action: Action,
) -> Result<String, String> {
    macro_rules! on_engine {
        ($e:ident, $body:expr) => {{
            let $e = engine.clone();
            blk(move || $body)
                .await
                .map_err(err_text)?
                .map_err(err_text)
        }};
    }
    match action {
        Action::Quit | Action::Copy(_) => Ok(String::new()),
        Action::CreateGoal { text, criteria } => {
            on_engine!(e, e.create_goal(&text, criteria))?;
            Ok("objective set — the Leader will start planning".into())
        }
        Action::RedirectGoal {
            text,
            reason,
            interrupt,
        } => {
            sup.redirect_goal(&text, &reason, interrupt)
                .await
                .map_err(err_text)?;
            Ok("objective redirected; completed tasks are preserved".into())
        }
        Action::AddAgent {
            name,
            provider,
            role,
            model,
            permission,
        } => {
            let n = name.clone();
            let agent = on_engine!(e, e.add_agent(&n, provider, role, model, permission, None))?;
            match sup.connect_agent(&agent.id).await {
                Ok(()) => Ok(format!("{name} added and connected")),
                Err(e) => Err(format!(
                    "{name} added but could not connect: {}",
                    err_text(e)
                )),
            }
        }
        Action::ConnectAgent(id) => {
            sup.connect_agent(&id).await.map_err(err_text)?;
            Ok("agent connected".into())
        }
        Action::ConnectAll => {
            sup.connect_all().await;
            Ok("connect attempted for all agents".into())
        }
        Action::DisconnectAgent(id) => {
            sup.disconnect_agent(&id, true).await;
            Ok("agent stopped".into())
        }
        Action::InterruptAgent(id) => {
            sup.interrupt_agent(&id).await.map_err(err_text)?;
            Ok("interrupt requested".into())
        }
        Action::MessageAgent {
            agent,
            text,
            interrupt,
        } => {
            sup.message_agent(&agent, &text, interrupt)
                .await
                .map_err(err_text)?;
            Ok(format!("message queued for {agent}"))
        }
        Action::Pause => {
            on_engine!(e, e.pause())?;
            Ok("scheduling paused".into())
        }
        Action::Resume => {
            on_engine!(e, e.resume())?;
            Ok("scheduling resumed".into())
        }
        Action::RequeueTask(key) => {
            let k = key.clone();
            on_engine!(e, e.requeue_task(&Actor::User, &k))?;
            Ok(format!("{key} re-queued"))
        }
        Action::ReleaseTask { key, reason } => {
            let k = key.clone();
            on_engine!(e, e.release_task(&Actor::User, &k, &reason))?;
            Ok(format!("{key} released"))
        }
        Action::CancelTask { key, reason } => {
            let k = key.clone();
            on_engine!(
                e,
                e.update_task(
                    &Actor::User,
                    &k,
                    TaskPatch {
                        cancel: Some(true),
                        reason: Some(reason),
                        ..Default::default()
                    }
                )
            )?;
            Ok(format!("{key} canceled"))
        }
        Action::VerifyTask { key, pass, notes } => {
            let k = key.clone();
            on_engine!(e, e.verify_task(&Actor::User, &k, pass, &notes))?;
            Ok(format!(
                "{key} {}",
                if pass {
                    "verified DONE"
                } else {
                    "sent back for correction"
                }
            ))
        }
        Action::ResolveApproval { id, decision } => {
            sup.resolve_approval(&id, decision)
                .await
                .map_err(err_text)?;
            Ok(format!("approval {:?}", decision).to_lowercase())
        }
        Action::Integrate => {
            let results = sup.integrate_done_work().await.map_err(err_text)?;
            if results.is_empty() {
                Ok("nothing to integrate".into())
            } else {
                Ok(format!(
                    "integrated {} branch(es): {}",
                    results.len(),
                    results
                        .iter()
                        .map(|(b, o)| format!("{b}={o:?}").chars().take(60).collect::<String>())
                        .collect::<Vec<_>>()
                        .join("; ")
                ))
            }
        }
        Action::SetConfig(cfg) => {
            engine.set_config(cfg);
            Ok("configuration updated".into())
        }
    }
}

// ------------------------------------------------------------------ main loop

/// Run the TUI until the user quits. Owns only the terminal; all state comes from `engine`
/// and all effects go through `engine`/`supervisor`.
pub async fn run(engine: Arc<Engine>, supervisor: Arc<Supervisor>, opts: TuiOptions) -> Result<()> {
    let guard = TerminalGuard::enter(opts.mouse)?;
    let result = event_loop(&engine, &supervisor, &opts).await;
    drop(guard); // terminal restored before any shutdown output
    if opts.shutdown_on_quit {
        supervisor.shutdown().await;
    }
    if let Some(h) = &opts.on_quit {
        h();
    }
    result
}

async fn event_loop(engine: &Arc<Engine>, sup: &Arc<Supervisor>, opts: &TuiOptions) -> Result<()> {
    let mut terminal =
        Terminal::new(CrosstermBackend::new(stdout())).context("creating terminal")?;
    let mut app = App::new(Theme::from_env(opts.monochrome));
    if let Ok(sz) = terminal.size() {
        app.size = (sz.width, sz.height);
    }
    let frame = Duration::from_millis(1000 / opts.max_fps.clamp(1, 60) as u64);
    let mut keys = EventStream::new();
    let mut engine_rx = engine.subscribe();
    let mut engine_open = true;
    let mut ui_rx = sup.ui_updates();
    let mut ui_open = true;
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (msg_tx, mut msg_rx) = mpsc::channel::<UiMsg>(64);

    let (mut need_refresh, mut need_draw) = (true, true);
    let mut last_draw = Instant::now()
        .checked_sub(frame)
        .unwrap_or_else(Instant::now);
    let mut last_refresh = last_draw;

    loop {
        if need_refresh && last_refresh.elapsed() >= frame {
            let key = app.selected_task().map(|t| t.key.clone());
            match load(engine, app.view, key).await {
                Ok(d) => apply_refresh(&mut app, sup, engine, d),
                Err(e) => app.set_toast(format!("refresh failed: {}", err_text(e)), Level::Error),
            }
            last_refresh = Instant::now();
            need_refresh = false;
            need_draw = true;
        }
        if need_draw && last_draw.elapsed() >= frame {
            app.now = Utc::now();
            if let Ok(sz) = terminal.size() {
                app.size = (sz.width, sz.height);
            }
            terminal
                .draw(|f| ui::render(f, &app))
                .context("drawing frame")?;
            last_draw = Instant::now();
            need_draw = false;
        }
        if app.quit {
            break;
        }
        let pending = need_refresh || need_draw;
        let wait = if pending {
            frame
                .saturating_sub(last_draw.elapsed().min(last_refresh.elapsed()))
                .max(Duration::from_millis(2))
        } else {
            Duration::from_secs(3600)
        };

        let mut actions: Vec<Action> = vec![];
        tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(CtEvent::Key(k))) => { actions = handle_key(&mut app, k); need_draw = true; }
                Some(Ok(CtEvent::Mouse(m))) => { actions = handle_mouse(&mut app, m); need_draw = true; }
                Some(Ok(CtEvent::Resize(w, h))) => { app.size = (w, h); need_draw = true; }
                Some(Ok(_)) => {}
                Some(Err(e)) => { tracing::warn!("terminal event error: {e}"); }
                None => break, // stdin closed
            },
            r = engine_rx.recv(), if engine_open => match r {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => need_refresh = true,
                Err(broadcast::error::RecvError::Closed) => engine_open = false,
            },
            r = ui_rx.changed(), if ui_open => match r {
                Ok(()) => need_refresh = true,
                Err(_) => ui_open = false,
            },
            _ = clock.tick() => { need_draw = true; app.now = Utc::now(); }
            Some(UiMsg::Toast(t, l)) = msg_rx.recv() => { app.set_toast(t, l); need_draw = true; }
            _ = tokio::time::sleep(wait), if pending => {}
        }

        for a in actions {
            match a {
                Action::Quit => app.quit = true,
                Action::Copy(text) => {
                    let mut out = stdout();
                    let _ = out.write_all(osc52(&text).as_bytes());
                    let _ = out.flush();
                }
                other => {
                    let (e, s, tx) = (engine.clone(), sup.clone(), msg_tx.clone());
                    tokio::spawn(async move {
                        let msg = match execute(e, s, other).await {
                            Ok(m) if m.is_empty() => return,
                            Ok(m) => UiMsg::Toast(m, Level::Ok),
                            Err(m) => UiMsg::Toast(m, Level::Error),
                        };
                        let _ = tx.send(msg).await;
                    });
                    need_refresh = true;
                }
            }
        }
        if app.detail_dirty {
            need_refresh = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_payload() {
        assert_eq!(osc52("foo"), "\x1b]52;c;Zm9v\x07");
    }

    #[test]
    fn restore_is_idempotent_when_inactive() {
        restore_terminal();
        restore_terminal();
    }
}
