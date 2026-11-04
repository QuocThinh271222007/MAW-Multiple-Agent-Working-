//! Terminal lifecycle, the async event loop and action execution.
//!
//! The loop is event driven: it sleeps until a key, a resize, an engine event, a transcript
//! update or the 1 s clock tick arrives. Refreshes and redraws are coalesced to `max_fps`.
//! Blocking SQLite work runs on `spawn_blocking`; provider I/O only ever happens inside
//! spawned action tasks, never on the render path.

use crate::app::{
    handle_key, handle_mouse, Action, App, Level, OutputState, Overlay, TaskDetail, View,
};
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
use maw_core::provider::{AuthStatus, ModelChoice};
use maw_core::{AgentId, CoreError, ProviderKind};
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
    usage: Vec<maw_core::Event>,
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
        let usage = if view == View::Tokens {
            e.read(|db| db.events_of_kind(e.project_id(), maw_core::EventKind::AgentUsage, 3000))?
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
            usage,
        })
    })
    .await
    .map_err(|e| CoreError::Internal(e.to_string()))?;
    joined
}

fn apply_refresh(app: &mut App, sup: &Arc<Supervisor>, engine: &Engine, d: RefreshData) {
    app.apply_snapshot(d.snap);
    app.extra_events = d.extra;
    app.goal_history = d.goal_history;
    app.plan_history = d.plan_history;
    app.task_detail = d.detail;
    app.usage_events = d.usage;
    // performance sampling starts when it is wanted: the view is open, or a local agent exists
    let wants_perf = app.view == View::Perf || app.side == Some(crate::app::SidePanel::Perf);
    if wants_perf
        || app
            .agents()
            .iter()
            .any(|a| a.provider == ProviderKind::Ollama)
    {
        sup.start_perf();
    }
    if wants_perf {
        app.perf = sup.perf();
    }
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

// ------------------------------------------------------------------ provider login

/// True when the provider says the user is not logged in (an unknown state is not treated as
/// "needs login": the session start will report a real authentication error).
pub fn needs_login(status: &AuthStatus) -> bool {
    matches!(status, AuthStatus::NotAuthenticated { .. })
}

/// Re-enter the alternate screen after handing the terminal to another program.
fn reenter_terminal(mouse: bool) -> Result<()> {
    enable_raw_mode().context("enabling raw mode")?;
    ACTIVE.store(true, Ordering::SeqCst);
    let mut out = stdout();
    if mouse {
        execute!(out, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)
    } else {
        execute!(out, EnterAlternateScreen, cursor::Hide)
    }
    .context("re-entering the alternate screen")?;
    Ok(())
}

/// Leave the TUI, run the provider's official login with the user's terminal, wait for Enter.
/// MAW never sees the credentials; this is the same command as `maw auth <provider>`.
async fn run_login(name: &str, kind: ProviderKind, cmd: &[String]) -> Result<(), String> {
    restore_terminal();
    println!(
        "\nLogging in {name} ({kind}) with the provider's own login: {}",
        cmd.join(" ")
    );
    println!("Follow its prompts. MAW does not read or store your credentials.\n");
    let status = tokio::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .await;
    let result = match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("exit {:?}", s.code())),
        Err(e) => Err(format!("could not run {}: {e}", cmd[0])),
    };
    println!(
        "\n{} Press Enter to return to MAW...",
        if result.is_ok() {
            "Login finished."
        } else {
            "Login did not complete."
        }
    );
    let _ = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    })
    .await;
    result
}

// ------------------------------------------------------------------ action execution

enum UiMsg {
    Toast(String, Level),
    Models(ProviderKind, Vec<ModelChoice>),
    Shell {
        cmd: String,
        lines: Vec<String>,
    },
    /// The provider needs an interactive login: the loop hands the terminal over for it.
    NeedLogin {
        agent: AgentId,
        name: String,
        kind: ProviderKind,
    },
}

/// Run a user-typed shell command in the project folder: 60 s limit, output capped and redacted.
/// This is an explicit user action (`!`), never something an agent can trigger.
async fn run_shell(cwd: String, cmd: String) -> Vec<String> {
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;
    const CAP: usize = 64 * 1024;
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", &cmd]);
        c
    } else {
        let mut c = Command::new("bash");
        c.args(["-lc", &cmd]);
        c
    };
    c.current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return vec![format!("could not start the command: {e}")],
    };
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let work = async {
        let (mut so, mut se) = (Vec::new(), Vec::new());
        let a = async {
            if let Some(o) = out.as_mut() {
                let _ = o.take(CAP as u64).read_to_end(&mut so).await;
            }
        };
        let b = async {
            if let Some(e) = err.as_mut() {
                let _ = e.take(CAP as u64).read_to_end(&mut se).await;
            }
        };
        tokio::join!(a, b);
        let status = child.wait().await;
        (so, se, status)
    };
    let mut lines = vec![format!("$ {cmd}"), format!("(in {cwd})"), String::new()];
    match tokio::time::timeout(Duration::from_secs(60), work).await {
        Ok((so, se, status)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&so),
                String::from_utf8_lossy(&se)
            );
            lines.extend(
                maw_core::redact::redact(&text)
                    .lines()
                    .map(|l| l.replace("	", "    ")),
            );
            lines.push(String::new());
            lines.push(match status {
                Ok(s) => format!(
                    "[exit {}]",
                    s.code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into())
                ),
                Err(e) => format!("[wait failed: {e}]"),
            });
        }
        Err(_) => lines.push("[timed out after 60 s and was killed]".into()),
    }
    lines
}

fn err_text(e: impl std::fmt::Display) -> String {
    maw_core::redact::redact(&e.to_string())
}

/// Execute an action against the engine/supervisor. Returns a success message.
async fn execute(
    engine: Arc<Engine>,
    sup: Arc<Supervisor>,
    action: Action,
    tx: mpsc::Sender<UiMsg>,
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
        Action::Quit | Action::Copy(_) | Action::RunShell(_) => Ok(String::new()),
        Action::PullModel { name } => {
            let (ptx, mut prx) = mpsc::channel::<maw_core::provider::PullProgress>(64);
            let (t2, n2) = (tx.clone(), name.clone());
            let fwd = tokio::spawn(async move {
                let mut last = Instant::now() - Duration::from_secs(2);
                while let Some(p) = prx.recv().await {
                    if p.total == 0 || last.elapsed() < Duration::from_millis(900) {
                        continue;
                    }
                    last = Instant::now();
                    let _ = t2
                        .send(UiMsg::Toast(
                            format!(
                                "downloading {n2}: {}% ({:.1}/{:.1} GB)",
                                p.completed * 100 / p.total,
                                p.completed as f64 / 1e9,
                                p.total as f64 / 1e9
                            ),
                            Level::Info,
                        ))
                        .await;
                }
            });
            let res = sup.pull_model(ProviderKind::Ollama, &name, ptx).await;
            let _ = fwd.await;
            if let Ok(list) = sup.list_models(ProviderKind::Ollama).await {
                let _ = tx.send(UiMsg::Models(ProviderKind::Ollama, list)).await;
            }
            res.map_err(err_text)
        }
        Action::SetAgentModel {
            agent,
            model,
            effort,
        } => {
            if let Some(m) = &model {
                let kind = {
                    let (e1, id) = (engine.clone(), agent.clone());
                    blk(move || e1.get_agent(&id))
                        .await
                        .map_err(err_text)?
                        .map_err(err_text)?
                        .provider
                };
                sup.validate_model(kind, m).await.map_err(err_text)?;
            }
            let (e1, a1, m1, f1) = (engine.clone(), agent.clone(), model.clone(), effort.clone());
            blk(move || -> Result<(), CoreError> {
                e1.set_agent_model(&a1, m1)?;
                e1.set_agent_effort(&a1, f1.as_deref().unwrap_or("default"))?;
                Ok(())
            })
            .await
            .map_err(err_text)?
            .map_err(err_text)?;
            let note = sup.reconfigure_agent(&agent).await.map_err(err_text)?;
            Ok(format!(
                "model = {}, effort = {} — {note}",
                model.as_deref().unwrap_or("default"),
                effort.as_deref().unwrap_or("default")
            ))
        }
        Action::SetAgentPermission { agent, permission } => {
            let e1 = engine.clone();
            let a1 = agent.clone();
            blk(move || e1.set_agent_permission(&a1, permission))
                .await
                .map_err(err_text)?
                .map_err(err_text)?;
            let note = sup.reconfigure_agent(&agent).await.map_err(err_text)?;
            Ok(format!("permissions = {} — {note}", permission.as_str()))
        }
        Action::CreateGoal { text, criteria } => {
            on_engine!(e, e.create_goal(&text, criteria))?;
            Ok("objective set — the Leader will start planning".into())
        }
        Action::CreateTask {
            title,
            description,
            criteria,
            assignee,
        } => {
            let nt = maw_orchestrator::tasks::NewTask {
                title: title.clone(),
                description,
                acceptance_criteria: criteria,
                assignee,
                ..Default::default()
            };
            match on_engine!(e, e.create_task(&maw_orchestrator::Actor::User, nt)) {
                Ok(t) => Ok(format!("task {} created: {}", t.key, t.title)),
                Err(m) if m.contains("no active goal") => {
                    Err("there is no active objective: set one first (G, or /goal set)".into())
                }
                Err(m) => Err(m),
            }
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
            effort,
            permission,
        } => {
            if let Some(m) = &model {
                sup.validate_model(provider, m).await.map_err(err_text)?;
            }
            let n = name.clone();
            let agent = on_engine!(e, e.add_agent(&n, provider, role, model, permission, None))?;
            if let Some(f) = effort {
                let (e2, id) = (engine.clone(), agent.id.clone());
                blk(move || e2.set_agent_effort(&id, &f))
                    .await
                    .map_err(err_text)?
                    .map_err(err_text)?;
            }
            // Not logged in yet? Offer the provider's official login right away.
            if let Ok(st) = sup.auth_status(provider).await {
                if needs_login(&st) {
                    let _ = tx
                        .send(UiMsg::NeedLogin {
                            agent: agent.id.clone(),
                            name: name.clone(),
                            kind: provider,
                        })
                        .await;
                    return Ok(format!("{name} added; {provider} needs a login first"));
                }
            }
            match sup.connect_agent(&agent.id).await {
                Ok(()) => Ok(format!("{name} added and connected")),
                Err(e) => Err(format!(
                    "{name} added but could not connect: {}",
                    err_text(e)
                )),
            }
        }
        Action::ConnectAgent(id) => {
            match sup.connect_agent(&id).await {
                Ok(()) => Ok("agent connected".into()),
                Err(e) => {
                    // an authentication failure is fixed by logging in, so offer that
                    let a = engine.get_agent(&id).map_err(err_text)?;
                    if a.state == maw_core::AgentState::AuthRequired {
                        let _ = tx
                            .send(UiMsg::NeedLogin {
                                agent: id,
                                name: a.name.clone(),
                                kind: a.provider,
                            })
                            .await;
                        return Ok(format!("{} is not logged in; starting the login", a.name));
                    }
                    Err(err_text(e))
                }
            }
        }
        Action::RemoveAgent { agent, force } => {
            let removed = sup.remove_agent(&agent, force).await.map_err(err_text)?;
            let mut msg = format!("{} removed", removed.name);
            if !removed.released_tasks.is_empty() {
                msg.push_str(&format!("; released {}", removed.released_tasks.join(", ")));
            }
            if let Some(w) = removed.worktree {
                msg.push_str(&format!("; its worktree was left at {w}"));
            }
            Ok(msg)
        }
        Action::LoginAgent(id) => {
            let a = engine.get_agent(&id).map_err(err_text)?;
            let _ = tx
                .send(UiMsg::NeedLogin {
                    agent: id,
                    name: a.name.clone(),
                    kind: a.provider,
                })
                .await;
            Ok(String::new())
        }
        Action::ConnectAll => {
            sup.connect_all().await;
            Ok("connect attempted for all agents".into())
        }
        Action::SaveCanvas(json) => {
            // a view preference, not project state: kept next to the other small settings
            let e = engine.clone();
            let _ = blk(move || {
                e.store()
                    .write(|db| db.kv_set(e.project_id(), "board_layout", &json))
            })
            .await;
            Ok(String::new())
        }
        Action::ProposePlan {
            summary,
            milestones,
            next_actions,
            reason,
        } => {
            let ms: Vec<maw_core::Milestone> = milestones
                .into_iter()
                .map(|(title, task_keys)| maw_core::Milestone {
                    title,
                    done: false,
                    task_keys,
                })
                .collect();
            match on_engine!(
                e,
                e.propose_plan(
                    &maw_orchestrator::Actor::User,
                    &summary,
                    ms,
                    next_actions,
                    &reason
                )
            ) {
                Ok(p) => Ok(format!("plan revision {} recorded", p.revision)),
                Err(m) if m.contains("goal") => {
                    Err("there is no objective yet: set one first (G, or /goal set)".into())
                }
                Err(m) => Err(m),
            }
        }
        Action::Refresh => {
            // model lists first (they may have changed: a download, a login), then the agents
            for kind in [
                ProviderKind::Claude,
                ProviderKind::Codex,
                ProviderKind::Ollama,
            ] {
                let (s, tx) = (sup.clone(), tx.clone());
                tokio::spawn(async move {
                    if let Ok(list) = s.list_models(kind).await {
                        let _ = tx.send(UiMsg::Models(kind, list)).await;
                    }
                });
            }
            let report = sup.refresh().await;
            let up = report
                .iter()
                .filter(|(_, _, now)| {
                    matches!(
                        now,
                        maw_core::AgentState::Idle
                            | maw_core::AgentState::Thinking
                            | maw_core::AgentState::Working
                    )
                })
                .count();
            Ok(format!(
                "refreshed: model lists reloaded, {up} of {} agents that were down are up",
                report.len()
            ))
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
    // the main screen starts with the command line ready (Tab gives the keys to the hotkeys)
    app.cmdline_focus = true;
    // the board's layout from the last run (positions the user chose, zoom and place)
    if let Ok(Some(saved)) = engine
        .store()
        .read(|db| db.kv_get(engine.project_id(), "board_layout"))
    {
        app.canvas.load_json(&saved);
    }
    let frame = Duration::from_millis(1000 / opts.max_fps.clamp(1, 60) as u64);
    // `None` while the terminal is handed to a provider's login command.
    let mut keys = Some(EventStream::new());
    let mut engine_rx = engine.subscribe();
    let mut engine_open = true;
    let mut ui_rx = sup.ui_updates();
    let mut ui_open = true;
    // twice a second: the clock, the working line and the moving markers of /flow stay lively
    let mut clock = tokio::time::interval(Duration::from_millis(500));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (msg_tx, mut msg_rx) = mpsc::channel::<UiMsg>(64);
    app.pull_suggestions = sup.pull_suggestions(ProviderKind::Ollama);
    // Model catalogs for every real provider (not only those already in use) so the Add-agent
    // dialog can offer pick-lists on a fresh project. Codex answers live; failures are ignored.
    for kind in [
        ProviderKind::Claude,
        ProviderKind::Codex,
        ProviderKind::Ollama,
    ] {
        let (s, tx) = (sup.clone(), msg_tx.clone());
        tokio::spawn(async move {
            if let Ok(list) = s.list_models(kind).await {
                let _ = tx.send(UiMsg::Models(kind, list)).await;
            }
        });
    }

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
            ev = async {
                match keys.as_mut() {
                    Some(k) => k.next().await,
                    None => std::future::pending().await,
                }
            } => match ev {
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
            Some(msg) = msg_rx.recv() => {
                match msg {
                    UiMsg::Toast(t, l) => app.set_toast(t, l),
                    UiMsg::Models(kind, list) => {
                        app.models.insert(kind, list);
                        // a dialog opened before the catalog arrived gets its lists filled in
                        crate::app::refresh_open_dialog(&mut app);
                    }
                    UiMsg::Shell { cmd, lines } => {
                        app.overlay = Overlay::Output(OutputState { title: format!("$ {cmd}"), lines, scroll: 0 });
                    }
                    UiMsg::NeedLogin { agent, name, kind } => {
                        match sup.login_command(kind) {
                            None => app.set_toast(format!("{name}: {kind} has no login command"), Level::Error),
                            Some(cmd) => {
                                // stop reading keys, give the terminal to the provider's own login
                                drop(keys.take());
                                let outcome = run_login(&name, kind, &cmd).await;
                                reenter_terminal(opts.mouse)?;
                                terminal.clear().ok();
                                keys = Some(EventStream::new());
                                match outcome {
                                    Ok(()) => {
                                        app.set_toast(format!("{name}: logged in, connecting..."), Level::Ok);
                                        let (s, tx) = (sup.clone(), msg_tx.clone());
                                        tokio::spawn(async move {
                                            let m = match s.connect_agent(&agent).await {
                                                Ok(()) => UiMsg::Toast(format!("{name} connected"), Level::Ok),
                                                Err(e) => UiMsg::Toast(format!("{name}: {}", err_text(e)), Level::Error),
                                            };
                                            let _ = tx.send(m).await;
                                        });
                                    }
                                    Err(e) => app.set_toast(format!("{name}: login did not complete ({e})"), Level::Error),
                                }
                                need_refresh = true;
                            }
                        }
                    }
                }
                need_draw = true;
            }
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
                Action::RunShell(cmd) => {
                    let (cwd, tx) = (engine.project().path.clone(), msg_tx.clone());
                    app.set_toast(format!("running: {cmd}"), Level::Info);
                    tokio::spawn(async move {
                        let lines = run_shell(cwd, cmd.clone()).await;
                        let _ = tx.send(UiMsg::Shell { cmd, lines }).await;
                    });
                }
                other => {
                    let (e, s, tx) = (engine.clone(), sup.clone(), msg_tx.clone());
                    tokio::spawn(async move {
                        let msg = match execute(e, s, other, tx.clone()).await {
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

#[cfg(test)]
mod login_tests {
    use super::*;

    #[test]
    fn only_a_definite_not_logged_in_triggers_the_login_flow() {
        assert!(needs_login(&AuthStatus::NotAuthenticated {
            hint: "run codex login".into()
        }));
        assert!(!needs_login(&AuthStatus::Authenticated {
            method: "chatgpt".into(),
            account: None,
            plan: None
        }));
        // unknown must not open a login: a real auth problem will surface when the session starts
        assert!(!needs_login(&AuthStatus::Unknown {
            reason: "timeout".into()
        }));
    }
}
