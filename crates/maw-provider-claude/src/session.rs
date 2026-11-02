//! One long-lived `claude -p --input-format stream-json` process per agent.
//!
//! Tasks per process: a stdin writer, a stderr tail collector and the stdout reader which feeds the
//! [`Mapper`]. A driver task owns the process lifecycle, including the one-shot fallback to a fresh
//! session when `--resume` points at a conversation that no longer exists.

use crate::args::{build_args, SessionArg, MCP_TOKEN_ENV};
use crate::mapper::{success_response, Flags, Mapper, Out, PendingApproval};
use async_trait::async_trait;
use futures::StreamExt;
use maw_core::agent::Capabilities;
use maw_core::provider::*;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

/// Longest accepted stdout line. Larger lines are dropped, never buffered without bound.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
const STDERR_TAIL_LINES: usize = 20;
const GRACEFUL_EXIT: Duration = Duration::from_secs(4);
const TERM_EXIT: Duration = Duration::from_secs(3);

pub(crate) struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
}

pub(crate) fn spawn_process(
    bin: &PathBuf,
    cfg: &SessionConfig,
    arg: &SessionArg,
) -> ProviderResult<Running> {
    let mut cmd = Command::new(bin);
    cmd.args(build_args(cfg, arg))
        .current_dir(&cfg.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    if let Some(mcp) = &cfg.mcp {
        // The bearer token travels in the environment, never on the command line.
        cmd.env(MCP_TOKEN_ENV, &mcp.token);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ProviderError::NotInstalled(format!(
                "{} (is Claude Code installed and on PATH?)",
                bin.display()
            ))
        } else {
            ProviderError::Io(format!("failed to start {}: {e}", bin.display()))
        }
    })?;
    let (stdin, stdout, stderr) =
        match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(i), Some(o), Some(e)) => (i, o, e),
            _ => return Err(ProviderError::Io("child stdio was not piped".into())),
        };
    Ok(Running {
        child,
        stdin,
        stdout,
        stderr,
    })
}

pub(crate) struct Inner {
    bin: PathBuf,
    cfg: SessionConfig,
    events: mpsc::Sender<ProviderEvent>,
    native_id: Mutex<String>,
    /// Lines for the current process's stdin.
    out: Mutex<Option<mpsc::Sender<String>>>,
    flags: Arc<Flags>,
    /// A user message was sent and its `result` has not arrived yet.
    busy: AtomicBool,
    pending: Mutex<HashMap<String, PendingApproval>>,
    closing: AtomicBool,
    kill: Notify,
    last_input: Mutex<Option<String>>,
    /// `--resume` was used and no `system/init` has confirmed the conversation exists yet.
    resume_unverified: AtomicBool,
    req_counter: AtomicU64,
}

pub struct ClaudeSession {
    inner: Arc<Inner>,
    driver: Mutex<Option<JoinHandle<()>>>,
}

impl ClaudeSession {
    pub(crate) fn start(
        bin: PathBuf,
        cfg: SessionConfig,
        arg: SessionArg,
        running: Running,
        events: mpsc::Sender<ProviderEvent>,
    ) -> Arc<ClaudeSession> {
        let resumed = matches!(arg, SessionArg::Resume(_));
        let inner = Arc::new(Inner {
            bin,
            cfg,
            events,
            native_id: Mutex::new(arg.id().to_string()),
            out: Mutex::new(None),
            flags: Arc::new(Flags::default()),
            busy: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
            kill: Notify::new(),
            last_input: Mutex::new(None),
            resume_unverified: AtomicBool::new(resumed),
            req_counter: AtomicU64::new(0),
        });
        // Install the stdin channel before the driver runs so `send_input` works immediately.
        let (tx, rx) = mpsc::channel::<String>(64);
        if let Ok(mut o) = inner.out.lock() {
            *o = Some(tx);
        }
        let driver = tokio::spawn(drive(inner.clone(), running, rx, arg));
        Arc::new(ClaudeSession {
            inner,
            driver: Mutex::new(Some(driver)),
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Inner {
    fn sender(&self) -> Option<mpsc::Sender<String>> {
        lock(&self.out).clone()
    }

    async fn emit(&self, ev: ProviderEvent) -> bool {
        self.events.send(ev).await.is_ok()
    }

    fn next_request_id(&self, prefix: &str) -> String {
        format!(
            "maw-{prefix}-{}",
            self.req_counter.fetch_add(1, Ordering::SeqCst) + 1
        )
    }

    async fn write_line(&self, line: String) -> ProviderResult<()> {
        let tx = self
            .sender()
            .ok_or_else(|| ProviderError::SessionClosed("claude process is not running".into()))?;
        tx.send(line)
            .await
            .map_err(|_| ProviderError::SessionClosed("claude stdin is closed".into()))
    }
}

#[async_trait]
impl AgentSession for ClaudeSession {
    fn native_session_id(&self) -> Option<String> {
        Some(lock(&self.inner.native_id).clone())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            resume: true,
            // Extra input while a turn runs is queued by the CLI and processed afterwards; it does
            // not redirect the in-flight turn.
            steer_active_turn: false,
            approvals: true,
            mcp_http: true,
            usage_reporting: true,
            model_selection: true,
        }
    }

    async fn send_input(&self, input: AgentInput) -> ProviderResult<()> {
        let inner = &self.inner;
        if inner.closing.load(Ordering::SeqCst) {
            return Err(ProviderError::SessionClosed(
                "session is shutting down".into(),
            ));
        }
        if inner.busy.swap(true, Ordering::SeqCst) && !input.steer_if_active {
            return Err(ProviderError::Other(
                "a turn is already active; wait for it to finish or interrupt it".into(),
            ));
        }
        let line = json!({ "type": "user", "message": { "role": "user", "content": input.text } })
            .to_string();
        *lock(&inner.last_input) = Some(line.clone());
        if let Err(e) = inner.write_line(line).await {
            inner.busy.store(false, Ordering::SeqCst);
            return Err(e);
        }
        Ok(())
    }

    async fn interrupt(&self) -> ProviderResult<()> {
        let inner = &self.inner;
        if !inner.busy.load(Ordering::SeqCst) {
            return Ok(());
        }
        inner
            .flags
            .interrupt_requested
            .store(true, Ordering::SeqCst);
        let id = inner.next_request_id("int");
        inner.write_line(json!({ "type": "control_request", "request_id": id, "request": { "subtype": "interrupt" } }).to_string()).await
    }

    async fn resolve_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let pending = lock(&inner.pending).remove(request_id).ok_or_else(|| {
            ProviderError::Protocol(format!("approval {request_id} is no longer pending (the turn ended, was interrupted, or it was already answered)"))
        })?;
        let response = match decision {
            ApprovalDecision::Approve => {
                json!({ "behavior": "allow", "updatedInput": pending.input })
            }
            ApprovalDecision::ApproveForSession => {
                json!({ "behavior": "allow", "updatedInput": pending.input, "updatedPermissions": session_scoped(&pending.suggestions) })
            }
            ApprovalDecision::Deny => {
                json!({ "behavior": "deny", "message": "The MAW operator denied this action." })
            }
            ApprovalDecision::Abort => {
                json!({ "behavior": "deny", "message": "The MAW operator denied this action and stopped the turn.", "interrupt": true })
            }
        };
        inner
            .write_line(success_response(request_id, response))
            .await?;
        if decision == ApprovalDecision::Abort {
            let _ = self.interrupt().await;
        }
        Ok(())
    }

    async fn shutdown(&self) -> ProviderResult<()> {
        let inner = &self.inner;
        inner.closing.store(true, Ordering::SeqCst);
        // 1) stop any in-flight turn, 2) close stdin so the CLI exits when idle.
        if inner.busy.load(Ordering::SeqCst) {
            let id = inner.next_request_id("int");
            let _ = inner.write_line(json!({ "type": "control_request", "request_id": id, "request": { "subtype": "interrupt" } }).to_string()).await;
        }
        // Dropping the sender ends the writer task, which drops (closes) the child's stdin.
        *lock(&inner.out) = None;
        let handle = lock(&self.driver).take();
        if let Some(mut h) = handle {
            if tokio::time::timeout(GRACEFUL_EXIT, &mut h).await.is_err() {
                // 3) SIGTERM lets the CLI clean up its own process tree, 4) then SIGKILL.
                inner.kill.notify_one();
                if tokio::time::timeout(TERM_EXIT + Duration::from_secs(3), &mut h)
                    .await
                    .is_err()
                {
                    h.abort(); // dropping the Child kills it (kill_on_drop)
                }
            }
        }
        Ok(())
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        // Never leak the subprocess if a session handle is dropped without `shutdown`.
        self.inner.closing.store(true, Ordering::SeqCst);
        if let Some(h) = lock(&self.driver).take() {
            h.abort();
        }
    }
}

/// Re-target persistent suggestions (`localSettings`) to the current session only, so approving "for
/// session" never writes to the user's settings files.
fn session_scoped(suggestions: &Value) -> Value {
    let rules: Vec<Value> = suggestions
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|s| s.get("type").and_then(Value::as_str) == Some("addRules"))
                .map(|s| {
                    let mut s = s.clone();
                    s["destination"] = json!("session");
                    s
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Array(rules)
}

enum RunExit {
    ResumeFailed,
    Ended(String),
}

async fn drive(
    inner: Arc<Inner>,
    first: Running,
    first_rx: mpsc::Receiver<String>,
    arg: SessionArg,
) {
    let mut running = first;
    let mut rx = first_rx;
    let mut current = arg;
    let mut initial: Vec<String> = Vec::new();
    loop {
        let exit = run_one(&inner, running, rx, &current, std::mem::take(&mut initial)).await;
        inner.busy.store(false, Ordering::SeqCst);
        inner.flags.turn_open.store(false, Ordering::SeqCst);
        lock(&inner.pending).clear();
        match exit {
            RunExit::ResumeFailed if !inner.closing.load(Ordering::SeqCst) => {
                let old = current.id().to_string();
                let fresh = uuid::Uuid::new_v4().to_string();
                tracing::warn!(
                    "claude session {old} could not be resumed; starting a fresh session"
                );
                let sent = inner
                    .emit(ProviderEvent::Error {
                        message: format!("Claude session {old} could not be resumed (not found); started a fresh session — earlier conversation context is lost"),
                        fatal: false,
                    })
                    .await;
                let next = SessionArg::New(fresh.clone());
                let spawned = spawn_process(&inner.bin, &inner.cfg, &next);
                match spawned {
                    Ok(r) if sent => {
                        *lock(&inner.native_id) = fresh.clone();
                        inner.resume_unverified.store(false, Ordering::SeqCst);
                        let (tx, new_rx) = mpsc::channel::<String>(64);
                        *lock(&inner.out) = Some(tx);
                        // re-deliver the message that triggered the failed resume
                        if let Some(l) = lock(&inner.last_input).clone() {
                            initial.push(l);
                            inner.busy.store(true, Ordering::SeqCst);
                        }
                        let _ = inner
                            .emit(ProviderEvent::Connected {
                                native_session_id: Some(fresh),
                            })
                            .await;
                        running = r;
                        rx = new_rx;
                        current = next;
                        continue;
                    }
                    Ok(_) => break,
                    Err(e) => {
                        let _ = inner
                            .emit(ProviderEvent::Disconnected {
                                reason: format!("fallback session failed to start: {e}"),
                            })
                            .await;
                        return;
                    }
                }
            }
            RunExit::ResumeFailed => {
                let _ = inner
                    .emit(ProviderEvent::Disconnected {
                        reason: "shutdown".into(),
                    })
                    .await;
                return;
            }
            RunExit::Ended(reason) => {
                let _ = inner.emit(ProviderEvent::Disconnected { reason }).await;
                return;
            }
        }
    }
}

async fn run_one(
    inner: &Arc<Inner>,
    mut r: Running,
    mut line_rx: mpsc::Receiver<String>,
    arg: &SessionArg,
    initial: Vec<String>,
) -> RunExit {
    // stdin writer: ends (closing the child's stdin) when every sender is dropped.
    let mut stdin = r.stdin;
    let writer = tokio::spawn(async move {
        for l in initial {
            if write_one(&mut stdin, &l).await.is_err() {
                return;
            }
        }
        while let Some(l) = line_rx.recv().await {
            if write_one(&mut stdin, &l).await.is_err() {
                break;
            }
        }
    });

    // stderr tail (redacted) for diagnostics and resume-failure detection.
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let tail_w = tail.clone();
    let stderr = r.stderr;
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let l: String = maw_core::redact::redact(&l).chars().take(300).collect();
            tracing::debug!(target: "claude.stderr", "{l}");
            let mut t = lock(&tail_w);
            t.push_back(l);
            while t.len() > STDERR_TAIL_LINES {
                t.pop_front();
            }
        }
    });

    let mut mapper = Mapper::new(arg.id().to_string(), inner.flags.clone());
    let mut frames = FramedRead::new(r.stdout, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
    let mut held: Vec<ProviderEvent> = Vec::new();
    let mut malformed = 0u64;
    let mut terminated = false;
    let mut receiver_gone = false;

    'read: loop {
        let frame = tokio::select! {
            f = frames.next() => f,
            _ = inner.kill.notified(), if !terminated => {
                terminated = true;
                terminate(&mut r.child).await;
                continue 'read;
            }
        };
        let line = match frame {
            None => break,
            Some(Ok(l)) => l,
            Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                tracing::warn!("dropped an oversized line from claude");
                continue;
            }
            Some(Err(LinesCodecError::Io(e))) => {
                tracing::debug!("claude stdout read error: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                malformed += 1;
                tracing::debug!(
                    "non-JSON line from claude (#{malformed}): {}",
                    maw_core::redact::redact(&line)
                        .chars()
                        .take(120)
                        .collect::<String>()
                );
                continue;
            }
        };
        if v.get("type").and_then(Value::as_str) == Some("system")
            && v.get("subtype").and_then(Value::as_str) == Some("init")
        {
            inner.resume_unverified.store(false, Ordering::SeqCst);
        }
        for o in mapper.map_line(&v) {
            match o {
                Out::Approval(p) => {
                    lock(&inner.pending).insert(p.request_id.clone(), p);
                }
                Out::Reply(l) => {
                    if let Some(tx) = inner.sender() {
                        let _ = tx.send(l).await;
                    }
                }
                Out::Event(ev) => {
                    match &ev {
                        ProviderEvent::Connected {
                            native_session_id: Some(id),
                        } => *lock(&inner.native_id) = id.clone(),
                        ProviderEvent::TurnCompleted { .. } => {
                            inner.busy.store(false, Ordering::SeqCst);
                            lock(&inner.pending).clear();
                        }
                        _ => {}
                    }
                    // Until the conversation is confirmed (init), keep events back: a failed resume
                    // must not leak a bogus failed turn.
                    if inner.resume_unverified.load(Ordering::SeqCst) {
                        held.push(ev);
                        continue;
                    }
                    for h in std::mem::take(&mut held) {
                        if !inner.emit(h).await {
                            receiver_gone = true;
                        }
                    }
                    if !inner.emit(ev).await {
                        receiver_gone = true;
                    }
                }
            }
            if receiver_gone {
                break 'read;
            }
        }
    }

    // Reap the process: it should be gone once stdout closed; never wait unboundedly.
    if receiver_gone {
        let _ = r.child.start_kill();
    }
    let status = match tokio::time::timeout(Duration::from_secs(5), r.child.wait()).await {
        Ok(s) => s.ok(),
        Err(_) => {
            terminate(&mut r.child).await;
            r.child.wait().await.ok()
        }
    };
    *lock(&inner.out) = None;
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), err_task).await;
    let tail_text = lock(&tail).iter().cloned().collect::<Vec<_>>().join(" | ");

    if inner.resume_unverified.load(Ordering::SeqCst) && tail_text.contains("No conversation found")
    {
        return RunExit::ResumeFailed;
    }
    // Normal path: release anything still held back.
    for h in held {
        if !inner.emit(h).await {
            break;
        }
    }
    if inner.closing.load(Ordering::SeqCst) {
        return RunExit::Ended("shutdown".into());
    }
    let code = status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown status".into());
    let mut reason = format!("claude process exited ({code})");
    if !tail_text.is_empty() {
        reason.push_str(&format!(
            ": {}",
            tail_text.chars().take(400).collect::<String>()
        ));
    }
    if receiver_gone {
        reason = "event consumer went away".into();
    }
    RunExit::Ended(reason)
}

async fn write_one(stdin: &mut ChildStdin, line: &str) -> std::io::Result<()> {
    stdin.write_all(line.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await
}

/// SIGTERM first (Claude Code tears down its own process tree on it), then SIGKILL.
async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let _ = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if tokio::time::timeout(TERM_EXIT, child.wait()).await.is_ok() {
                return;
            }
        }
    }
    let _ = child.start_kill();
}
