//! One long-lived `codex app-server` process per agent plus a JSON-RPC client over its stdio.

use crate::protocol::{self, PendingApproval, ServerRequestOutcome};
use async_trait::async_trait;
use maw_core::provider::*;
use maw_core::redact::redact;
use maw_core::Capabilities;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Lines longer than this are dropped (protects memory against a misbehaving server).
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
const STDERR_TAIL: usize = 20;

#[derive(Debug)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

type RpcResult = Result<Value, RpcError>;

pub(crate) struct Shared {
    tx: mpsc::Sender<ProviderEvent>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: StdMutex<HashMap<i64, oneshot::Sender<RpcResult>>>,
    next_id: AtomicI64,
    thread_id: StdMutex<Option<String>>,
    turn_id: StdMutex<Option<String>>,
    turn_active: AtomicBool,
    approvals: StdMutex<HashMap<String, PendingApproval>>,
    shutting_down: AtomicBool,
    stderr_tail: StdMutex<VecDeque<String>>,
    stderr_done: AtomicBool,
    stderr_notify: tokio::sync::Notify,
    capture: Option<PathBuf>,
    request_timeout: Duration,
    /// Reasoning effort sent with every `turn/start` (None = Codex default for the model).
    effort: StdMutex<Option<String>>,
}

fn lock<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A poisoned lock only means another thread panicked; the data is still usable here.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub(crate) fn new(
        tx: mpsc::Sender<ProviderEvent>,
        stdin: ChildStdin,
        child: Child,
        capture: Option<PathBuf>,
        request_timeout: Duration,
    ) -> Arc<Shared> {
        Arc::new(Shared {
            tx,
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            thread_id: StdMutex::new(None),
            turn_id: StdMutex::new(None),
            turn_active: AtomicBool::new(false),
            approvals: StdMutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            stderr_tail: StdMutex::new(VecDeque::new()),
            stderr_done: AtomicBool::new(false),
            stderr_notify: tokio::sync::Notify::new(),
            capture,
            request_timeout,
            effort: StdMutex::new(None),
        })
    }

    async fn write_line(&self, v: &Value) -> ProviderResult<()> {
        let mut line = v.to_string();
        line.push('\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| {
            ProviderError::SessionClosed("codex app-server stdin is closed".into())
        })?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ProviderError::SessionClosed(format!("write failed: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| ProviderError::SessionClosed(format!("flush failed: {e}")))
    }

    pub(crate) async fn notify(&self, method: &str, params: Option<Value>) -> ProviderResult<()> {
        let mut v = json!({ "method": method });
        if let Some(p) = params {
            v["params"] = p;
        }
        self.write_line(&v).await
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> ProviderResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        if let Err(e) = self
            .write_line(&json!({ "id": id, "method": method, "params": params }))
            .await
        {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(ProviderError::Protocol(format!(
                "{method} failed ({}): {}",
                e.code, e.message
            ))),
            Ok(Err(_)) => Err(ProviderError::SessionClosed(format!(
                "codex app-server exited while waiting for {method}"
            ))),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(ProviderError::Timeout(format!(
                    "{method} did not respond within {:?}",
                    self.request_timeout
                )))
            }
        }
    }

    async fn respond(&self, id: Value, result: Value) -> ProviderResult<()> {
        self.write_line(&json!({ "id": id, "result": result }))
            .await
    }

    async fn respond_error(&self, id: Value, code: i64, message: &str) -> ProviderResult<()> {
        self.write_line(&json!({ "id": id, "error": { "code": code, "message": message } }))
            .await
    }

    pub(crate) fn set_effort(&self, effort: Option<String>) {
        *lock(&self.effort) = effort;
    }

    pub(crate) fn set_thread(&self, id: String) {
        *lock(&self.thread_id) = Some(id);
    }

    fn thread(&self) -> Option<String> {
        lock(&self.thread_id).clone()
    }

    fn fail_all_pending(&self) {
        let drained: Vec<_> = lock(&self.pending).drain().collect();
        drop(drained); // dropping the senders wakes every waiter with a "closed" error
    }

    fn capture_line(&self, line: &str) {
        if let Some(path) = &self.capture {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "{}", redact(line));
            }
        }
    }

    /// Reader loop: dispatches responses, server requests and notifications until EOF.
    pub(crate) async fn run_reader(self: Arc<Self>, stdout: ChildStdout) {
        let mut reader = BufReader::new(stdout);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("codex stdout read error: {e}");
                    break;
                }
            }
            if buf.len() > MAX_LINE_BYTES {
                tracing::warn!(
                    "dropping oversized line from codex app-server ({} bytes)",
                    buf.len()
                );
                continue;
            }
            let line = buf.trim();
            if line.is_empty() {
                continue;
            }
            self.capture_line(line);
            let msg: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("ignoring malformed line from codex app-server: {e}");
                    continue;
                }
            };
            self.dispatch(msg).await;
        }
        self.fail_all_pending();
        self.finish().await;
    }

    async fn dispatch(&self, msg: Value) {
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").cloned();
        match (method, id) {
            // response to one of our requests
            (None, Some(id)) => {
                let Some(n) = id.as_i64() else { return };
                let waiter = lock(&self.pending).remove(&n);
                let Some(waiter) = waiter else { return };
                let outcome = match msg.get("error") {
                    Some(err) => Err(RpcError {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(-1),
                        message: err
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                    }),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiter.send(outcome);
            }
            // server -> client request: must always be answered
            (Some(method), Some(id)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                self.handle_server_request(id, method, &params).await;
            }
            (Some(method), None) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                self.handle_notification(method, &params).await;
            }
            (None, None) => tracing::debug!("ignoring message without id or method"),
        }
    }

    async fn handle_server_request(&self, id: Value, method: &str, params: &Value) {
        match protocol::translate_server_request(&id, method, params) {
            ServerRequestOutcome::Approval {
                request_id,
                event,
                pending,
            } => {
                lock(&self.approvals).insert(request_id, pending);
                self.emit(event).await;
            }
            ServerRequestOutcome::Respond { result, notice } => {
                tracing::debug!(method, "declining unrepresentable server request");
                if let Err(e) = self.respond(id, result).await {
                    tracing::debug!("failed to answer request: {e}");
                }
                self.emit(ProviderEvent::Error {
                    message: notice,
                    fatal: false,
                })
                .await;
            }
            ServerRequestOutcome::Reject { code, message } => {
                tracing::debug!(method, "rejecting unsupported server request");
                if let Err(e) = self.respond_error(id, code, &message).await {
                    tracing::debug!("failed to answer unsupported request: {e}");
                }
            }
        }
    }

    async fn handle_notification(&self, method: &str, params: &Value) {
        // Only this session's thread is relevant.
        if let (Some(mine), Some(theirs)) = (
            self.thread(),
            params.get("threadId").and_then(Value::as_str),
        ) {
            if mine != theirs {
                return;
            }
        }
        if method == "serverRequest/resolved" {
            if let Some(id) = params.get("requestId") {
                lock(&self.approvals).remove(&protocol::rpc_id_string(id));
            }
            return;
        }
        for ev in protocol::translate_notification(method, params) {
            match &ev {
                ProviderEvent::TurnStarted { turn_id } => {
                    *lock(&self.turn_id) = Some(turn_id.clone());
                    self.turn_active.store(true, Ordering::SeqCst);
                }
                ProviderEvent::TurnCompleted { .. } => {
                    *lock(&self.turn_id) = None;
                    self.turn_active.store(false, Ordering::SeqCst);
                    // Codex cancels outstanding approvals when a turn ends.
                    lock(&self.approvals).clear();
                }
                _ => {}
            }
            self.emit(ev).await;
        }
    }

    pub(crate) async fn emit(&self, ev: ProviderEvent) {
        // Bounded: a slow consumer applies backpressure instead of growing memory.
        if self.tx.send(ev).await.is_err() {
            tracing::debug!("event receiver dropped");
        }
    }

    pub(crate) fn record_stderr(&self, line: &str) {
        let line = redact(line);
        tracing::debug!(target: "codex_stderr", "{line}");
        let mut t = lock(&self.stderr_tail);
        if t.len() >= STDERR_TAIL {
            t.pop_front();
        }
        t.push_back(line);
    }

    /// Called by the stderr drain task when the child's stderr reaches EOF.
    pub(crate) fn mark_stderr_done(&self) {
        self.stderr_done.store(true, Ordering::SeqCst);
        self.stderr_notify.notify_waiters();
    }

    /// Give the stderr drain a moment to consume the child's last words before we report them.
    async fn wait_stderr_done(&self) {
        let waited = async {
            loop {
                // register interest before checking the flag so a wake-up cannot be missed
                let notified = self.stderr_notify.notified();
                if self.stderr_done.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_millis(750), waited).await;
    }

    /// Reap the child and tell the supervisor the session is gone.
    async fn finish(&self) {
        let status = {
            let child = self.child.lock().await.take();
            match child {
                Some(mut c) => match tokio::time::timeout(Duration::from_secs(3), c.wait()).await {
                    Ok(Ok(st)) => Some(st.to_string()),
                    _ => {
                        let _ = c.start_kill();
                        let _ = c.wait().await;
                        Some("killed".to_string())
                    }
                },
                None => None,
            }
        };
        *self.stdin.lock().await = None;
        self.wait_stderr_done().await;
        let reason = if self.shutting_down.load(Ordering::SeqCst) {
            "shutdown".to_string()
        } else {
            let tail: Vec<String> = lock(&self.stderr_tail)
                .iter()
                .rev()
                .take(3)
                .cloned()
                .collect();
            format!(
                "codex app-server exited ({}){}",
                status.unwrap_or_else(|| "unknown".into()),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(
                        ": {}",
                        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                    )
                }
            )
        };
        self.emit(ProviderEvent::Disconnected { reason }).await;
    }

    pub(crate) async fn terminate(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        // Closing stdin asks the server to exit; the reader then reaps the child.
        *self.stdin.lock().await = None;
        let child_done = {
            let mut guard = self.child.lock().await;
            match guard.as_mut() {
                Some(c) => matches!(
                    tokio::time::timeout(Duration::from_secs(3), c.wait()).await,
                    Ok(Ok(_))
                ),
                None => true,
            }
        };
        if !child_done {
            if let Some(c) = self.child.lock().await.as_mut() {
                let _ = c.start_kill();
            }
        }
    }
}

pub struct CodexSession {
    pub(crate) shared: Arc<Shared>,
    pub(crate) native_id: Option<String>,
}

#[async_trait]
impl AgentSession for CodexSession {
    fn native_session_id(&self) -> Option<String> {
        self.native_id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            resume: true,
            steer_active_turn: true,
            approvals: true,
            mcp_http: true,
            usage_reporting: true,
            model_selection: true,
        }
    }

    async fn send_input(&self, input: AgentInput) -> ProviderResult<()> {
        let thread = self
            .shared
            .thread()
            .ok_or_else(|| ProviderError::SessionClosed("no thread".into()))?;
        if self.shared.turn_active.load(Ordering::SeqCst) {
            if !input.steer_if_active {
                return Err(ProviderError::Other(
                    "a turn is already active; Codex runs one turn at a time per thread".into(),
                ));
            }
            let expected = lock(&self.shared.turn_id)
                .clone()
                .ok_or_else(|| ProviderError::Other("active turn id not known yet".into()))?;
            self.shared
                .request("turn/steer", json!({ "threadId": thread, "expectedTurnId": expected, "input": [{ "type": "text", "text": input.text, "text_elements": [] }] }))
                .await?;
            return Ok(());
        }
        self.shared.turn_active.store(true, Ordering::SeqCst);
        // copy out of the lock first: a std MutexGuard must not live across an await
        let effort = lock(&self.shared.effort).clone();
        match self
            .shared
            .request(
                "turn/start",
                protocol::turn_start_params(&thread, &input.text, effort.as_deref()),
            )
            .await
        {
            Ok(resp) => {
                if let Some(id) = resp.pointer("/turn/id").and_then(Value::as_str) {
                    let mut t = lock(&self.shared.turn_id);
                    if t.is_none() {
                        *t = Some(id.to_string());
                    }
                }
                Ok(())
            }
            Err(e) => {
                self.shared.turn_active.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    async fn interrupt(&self) -> ProviderResult<()> {
        let thread = self
            .shared
            .thread()
            .ok_or_else(|| ProviderError::SessionClosed("no thread".into()))?;
        let turn = lock(&self.shared.turn_id)
            .clone()
            .ok_or_else(|| ProviderError::Other("no active turn to interrupt".into()))?;
        self.shared
            .request(
                "turn/interrupt",
                json!({ "threadId": thread, "turnId": turn }),
            )
            .await
            .map(|_| ())
    }

    async fn resolve_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> ProviderResult<()> {
        let pending = lock(&self.shared.approvals).remove(request_id).ok_or_else(|| ProviderError::Protocol(format!("no pending approval '{request_id}' (it may have been resolved or the turn ended)")))?;
        let result = protocol::approval_response(&pending, decision);
        self.shared.respond(pending.rpc_id.clone(), result).await?;
        // Permission requests have no "cancel" decision; honour Abort by interrupting the turn.
        if decision == ApprovalDecision::Abort && pending.kind == ApprovalKind::Permissions {
            let _ = self.interrupt().await;
        }
        Ok(())
    }

    async fn shutdown(&self) -> ProviderResult<()> {
        self.shared.terminate().await;
        Ok(())
    }
}
