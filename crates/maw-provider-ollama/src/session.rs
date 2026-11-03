//! One Ollama-backed agent session: an agent loop MAW runs itself.
//!
//! Ollama only serves a model; it has no agent runtime. So this session owns the loop a coding
//! CLI would otherwise own: send the conversation and tool definitions to `/api/chat`, run the
//! tool calls the model asks for (local file/command tools gated by the permission profile, and
//! MAW's coordination tools through MCP with this agent's own bearer token), feed the results
//! back, and repeat until the model answers without calling a tool.

use crate::api::{self, ChatChunk, ToolCall};
use crate::history;
use crate::mcp::McpConnection;
use crate::tools::{self, Gate, LocalTools};
use async_trait::async_trait;
use futures::StreamExt;
use maw_core::agent::{Capabilities, UsageMetrics};
use maw_core::provider::*;
use maw_core::redact::redact;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 16 * 1024;

/// Everything a session needs, resolved by the provider before the session starts.
pub struct SessionParams {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model: String,
    pub think: Option<Value>,
    pub num_ctx: u32,
    pub keep_alive: String,
    pub max_steps: usize,
    pub idle_timeout: Duration,
    pub system: String,
    pub tools: LocalTools,
    pub mcp: Option<McpConnection>,
}

#[derive(Default)]
struct Inner {
    history: Vec<Value>,
    cancel: Option<CancellationToken>,
    pending: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    allow_files: bool,
    allow_commands: HashSet<String>,
    usage_in: u64,
    usage_out: u64,
    turns: u64,
    closed: bool,
}

struct Shared {
    p: SessionParams,
    tx: mpsc::Sender<ProviderEvent>,
    inner: Mutex<Inner>,
    tool_defs: Vec<Value>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn emit(&self, ev: ProviderEvent) {
        let _ = self.tx.send(ev).await;
    }
}

pub struct OllamaSession {
    shared: Arc<Shared>,
    native_id: String,
}

impl OllamaSession {
    pub fn start(p: SessionParams) -> (SessionHandle, EventReceiver) {
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let mut defs = tools::definitions();
        if let Some(m) = &p.mcp {
            defs.extend(m.definitions.iter().cloned());
        }
        let native_id = format!("ollama-{}", uuid::Uuid::new_v4().simple());
        let shared = Arc::new(Shared {
            p,
            tx,
            inner: Mutex::new(Inner::default()),
            tool_defs: defs,
        });
        let s = shared.clone();
        let id = native_id.clone();
        tokio::spawn(async move {
            s.emit(ProviderEvent::Connected {
                native_session_id: Some(id),
            })
            .await;
        });
        (Arc::new(OllamaSession { shared, native_id }), rx)
    }
}

#[async_trait]
impl AgentSession for OllamaSession {
    fn native_session_id(&self) -> Option<String> {
        Some(self.native_id.clone())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            resume: false,
            steer_active_turn: false,
            approvals: true,
            mcp_http: true,
            usage_reporting: true,
            model_selection: true,
        }
    }

    async fn send_input(&self, input: AgentInput) -> ProviderResult<()> {
        let cancel = CancellationToken::new();
        let turn_id = {
            let mut g = self.shared.lock();
            if g.closed {
                return Err(ProviderError::SessionClosed("session is shut down".into()));
            }
            if g.cancel.is_some() {
                return Err(ProviderError::Protocol(
                    "a turn is already running; wait for it or interrupt it".into(),
                ));
            }
            g.cancel = Some(cancel.clone());
            g.turns += 1;
            format!("turn-{}", g.turns)
        };
        let sh = self.shared.clone();
        tokio::spawn(run_turn(sh, input.text, turn_id, cancel));
        Ok(())
    }

    async fn interrupt(&self) -> ProviderResult<()> {
        if let Some(c) = self.shared.lock().cancel.clone() {
            c.cancel();
        }
        Ok(())
    }

    async fn resolve_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> ProviderResult<()> {
        let tx = self.shared.lock().pending.remove(request_id);
        match tx {
            Some(tx) => {
                let _ = tx.send(decision);
                Ok(())
            }
            None => Err(ProviderError::Protocol(format!(
                "no pending approval '{request_id}'"
            ))),
        }
    }

    async fn shutdown(&self) -> ProviderResult<()> {
        let was_open = {
            let mut g = self.shared.lock();
            let was_open = !g.closed;
            g.closed = true;
            if let Some(c) = g.cancel.take() {
                c.cancel();
            }
            g.pending.clear();
            was_open
        };
        if was_open {
            self.shared
                .emit(ProviderEvent::Disconnected {
                    reason: "session shut down".into(),
                })
                .await;
        }
        Ok(())
    }
}

enum Step {
    /// The model answered without tool calls (or hit its end): the turn is done.
    Done,
    /// The model asked for tools; results were appended to the history.
    Continue,
    Interrupted,
}

async fn run_turn(sh: Arc<Shared>, text: String, turn_id: String, cancel: CancellationToken) {
    sh.emit(ProviderEvent::TurnStarted {
        turn_id: turn_id.clone(),
    })
    .await;
    sh.lock()
        .history
        .push(json!({"role":"user","content":text}));
    let mut outcome = TurnOutcome::Completed;
    let mut error = None;
    let mut finished = false;
    for _ in 0..sh.p.max_steps.max(1) {
        match step(&sh, &cancel).await {
            Ok(Step::Done) => {
                finished = true;
                break;
            }
            Ok(Step::Continue) => {}
            Ok(Step::Interrupted) => {
                outcome = TurnOutcome::Interrupted;
                finished = true;
                break;
            }
            Err(e) => {
                outcome = TurnOutcome::Failed;
                error = Some(e);
                finished = true;
                break;
            }
        }
    }
    if !finished {
        outcome = TurnOutcome::Failed;
        error = Some(format!(
            "stopped after {} tool steps without a final answer; send a follow-up to continue",
            sh.p.max_steps
        ));
    }
    {
        let mut g = sh.lock();
        g.cancel = None;
        g.pending.clear();
    }
    sh.emit(ProviderEvent::TurnCompleted {
        turn_id,
        outcome,
        error: error.map(|e| redact(&e)),
    })
    .await;
}

/// One model request plus the execution of whatever tools it asked for.
async fn step(sh: &Arc<Shared>, cancel: &CancellationToken) -> Result<Step, String> {
    let messages = {
        let mut g = sh.lock();
        let reserved = (sh.p.system.len()
            + serde_json::to_string(&sh.tool_defs)
                .map(|s| s.len())
                .unwrap_or(0))
            / 3;
        let budget = (sh.p.num_ctx as usize) * 7 / 10;
        let n = history::compact(&mut g.history, budget, reserved);
        if n > 0 {
            tracing::info!(
                changed = n,
                "trimmed conversation history to fit the context window"
            );
        }
        let mut m = vec![json!({"role":"system","content":sh.p.system})];
        m.extend(g.history.iter().cloned());
        m
    };
    let mut body = json!({
        "model": sh.p.model,
        "messages": messages,
        "tools": sh.tool_defs,
        "stream": true,
        "keep_alive": sh.p.keep_alive,
        "options": {"num_ctx": sh.p.num_ctx},
    });
    if let Some(t) = &sh.p.think {
        body["think"] = t.clone();
    }
    let url = format!("{}/api/chat", sh.p.base_url);
    let sent = tokio::select! {
        _ = cancel.cancelled() => return Ok(Step::Interrupted),
        r = sh.p.http.post(&url).json(&body).send() => r,
    };
    let resp = sent.map_err(|e| format!("cannot reach Ollama at {}: {e}", sh.p.base_url))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(String::from))
            .unwrap_or(text);
        return Err(format!("Ollama answered {status}: {}", msg.trim()));
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let (mut content, mut calls) = (String::new(), Vec::<ToolCall>::new());
    let mut last: Option<ChatChunk> = None;
    let handle = |line: &[u8],
                  content: &mut String,
                  calls: &mut Vec<ToolCall>,
                  last: &mut Option<ChatChunk>|
     -> Result<Vec<ProviderEvent>, String> {
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        if text.is_empty() {
            return Ok(vec![]);
        }
        let c = match api::parse_chunk(text) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("{e}");
                return Ok(vec![]);
            }
        };
        if let Some(e) = &c.error {
            return Err(format!("Ollama error: {e}"));
        }
        let mut evs = Vec::new();
        if !c.thinking.is_empty() {
            evs.push(ProviderEvent::Reasoning {
                text: c.thinking.clone(),
            });
        }
        if !c.content.is_empty() {
            content.push_str(&c.content);
            evs.push(ProviderEvent::MessageDelta {
                text: c.content.clone(),
            });
        }
        calls.extend(c.tool_calls.iter().cloned());
        if c.done {
            *last = Some(c);
        }
        Ok(evs)
    };
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Ok(Step::Interrupted),
            r = tokio::time::timeout(sh.p.idle_timeout, stream.next()) => r,
        };
        let bytes = match next {
            Err(_) => {
                return Err(format!(
                    "no data from Ollama for {}s (model still loading, or the server is stuck)",
                    sh.p.idle_timeout.as_secs()
                ))
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => return Err(format!("connection to Ollama broke: {e}")),
            Ok(Some(Ok(b))) => b,
        };
        buf.extend_from_slice(&bytes);
        if buf.len() > MAX_LINE_BYTES {
            return Err("Ollama sent an oversized stream line".into());
        }
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            for ev in handle(&line, &mut content, &mut calls, &mut last)? {
                sh.emit(ev).await;
            }
        }
    }
    if !buf.is_empty() {
        let line = std::mem::take(&mut buf);
        for ev in handle(&line, &mut content, &mut calls, &mut last)? {
            sh.emit(ev).await;
        }
    }
    let Some(fin) = last else {
        return Err("the response from Ollama ended before it was complete".into());
    };

    // usage: Ollama reports per-request counts; the session keeps the running total
    {
        let (i, o) = {
            let mut g = sh.lock();
            g.usage_in += fin.prompt_tokens;
            g.usage_out += fin.output_tokens;
            (g.usage_in, g.usage_out)
        };
        sh.emit(ProviderEvent::Usage(UsageMetrics {
            input_tokens: Some(i),
            output_tokens: Some(o),
            total_tokens: Some(i + o),
            cost_usd: None,
            reported_by_provider: true,
        }))
        .await;
    }

    if !content.trim().is_empty() {
        sh.emit(ProviderEvent::Message {
            text: content.clone(),
        })
        .await;
    }
    let mut assistant = json!({"role":"assistant","content":content});
    if !calls.is_empty() {
        assistant["tool_calls"] = Value::Array(
            calls
                .iter()
                .map(|c| json!({"function":{"name":c.name,"arguments":c.arguments}}))
                .collect(),
        );
    }
    sh.lock().history.push(assistant);

    if calls.is_empty() {
        if content.trim().is_empty() {
            sh.emit(ProviderEvent::Error {
                message: "the model returned an empty response".into(),
                fatal: false,
            })
            .await;
        }
        return Ok(Step::Done);
    }
    for call in calls {
        match exec_tool(sh, &call, cancel).await {
            None => return Ok(Step::Interrupted),
            Some(text) => {
                let text = clip(&text, MAX_TOOL_RESULT_BYTES);
                sh.lock()
                    .history
                    .push(json!({"role":"tool","tool_name":call.name,"content":text}));
            }
        }
    }
    Ok(Step::Continue)
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    format!("{}\n[output truncated at {max} bytes]", &s[..i])
}

fn preview(args: &Value) -> String {
    let s = args.to_string();
    redact(&s.chars().take(240).collect::<String>())
}

/// Run one tool call. `None` means the turn was interrupted.
async fn exec_tool(
    sh: &Arc<Shared>,
    call: &ToolCall,
    cancel: &CancellationToken,
) -> Option<String> {
    let call_id = call
        .id
        .clone()
        .unwrap_or_else(|| format!("call-{}", uuid::Uuid::new_v4().simple()));
    let name = call.name.as_str();
    let args = &call.arguments;

    if tools::is_local(name) {
        match sh.p.tools.gate(name, args) {
            Gate::Deny(why) => {
                sh.emit(ProviderEvent::ToolStarted {
                    call_id: call_id.clone(),
                    name: name.into(),
                    detail: Some(preview(args)),
                })
                .await;
                sh.emit(ProviderEvent::ToolCompleted {
                    call_id,
                    name: name.into(),
                    ok: false,
                    detail: Some(why.clone()),
                })
                .await;
                return Some(format!("refused: {why}"));
            }
            Gate::Ask {
                kind,
                summary,
                paths,
                remember,
            } => {
                let pre_ok = {
                    let g = sh.lock();
                    (remember == "files" && g.allow_files) || g.allow_commands.contains(&remember)
                };
                if !pre_ok {
                    let (tx, rx) = oneshot::channel();
                    let rid = format!("appr-{}", uuid::Uuid::new_v4().simple());
                    sh.lock().pending.insert(rid.clone(), tx);
                    sh.emit(ProviderEvent::ApprovalRequested {
                        request_id: rid.clone(),
                        kind,
                        summary: redact(&summary),
                        reason: None,
                        paths,
                        details: json!({"tool": name, "arguments": preview(args)}),
                    })
                    .await;
                    let decision = tokio::select! {
                        _ = cancel.cancelled() => { sh.lock().pending.remove(&rid); return None; }
                        d = rx => d.unwrap_or(ApprovalDecision::Deny),
                    };
                    match decision {
                        ApprovalDecision::Approve => {}
                        ApprovalDecision::ApproveForSession => {
                            let mut g = sh.lock();
                            if remember == "files" {
                                g.allow_files = true
                            } else {
                                g.allow_commands.insert(remember);
                            }
                        }
                        ApprovalDecision::Deny => {
                            return Some("the user denied this action".into())
                        }
                        ApprovalDecision::Abort => {
                            cancel.cancel();
                            return None;
                        }
                    }
                }
            }
            Gate::Allow => {}
        }
        return run_local(sh, name, args, call_id, cancel).await;
    }

    // MAW coordination tools
    let Some(mcp) = sh.p.mcp.as_ref().filter(|m| m.has_tool(name)) else {
        let known: Vec<&str> = sh
            .tool_defs
            .iter()
            .filter_map(|d| d["function"]["name"].as_str())
            .collect();
        sh.emit(ProviderEvent::ToolStarted {
            call_id: call_id.clone(),
            name: name.into(),
            detail: None,
        })
        .await;
        sh.emit(ProviderEvent::ToolCompleted {
            call_id,
            name: name.into(),
            ok: false,
            detail: Some("unknown tool".into()),
        })
        .await;
        return Some(format!(
            "unknown tool '{name}'. Available tools: {}",
            known.join(", ")
        ));
    };
    sh.emit(ProviderEvent::ToolStarted {
        call_id: call_id.clone(),
        name: name.into(),
        detail: Some(preview(args)),
    })
    .await;
    let res = tokio::select! {
        _ = cancel.cancelled() => return None,
        r = mcp.call(name, args) => r,
    };
    match res {
        Ok(o) => {
            sh.emit(ProviderEvent::ToolCompleted {
                call_id,
                name: name.into(),
                ok: !o.is_error,
                detail: Some(redact(&clip(&o.text, 300))),
            })
            .await;
            Some(if o.is_error {
                format!("error: {}", o.text)
            } else {
                o.text
            })
        }
        Err(e) => {
            sh.emit(ProviderEvent::ToolCompleted {
                call_id,
                name: name.into(),
                ok: false,
                detail: Some(redact(&e)),
            })
            .await;
            Some(format!("error: {e}"))
        }
    }
}

async fn run_local(
    sh: &Arc<Shared>,
    name: &str,
    args: &Value,
    call_id: String,
    cancel: &CancellationToken,
) -> Option<String> {
    if name == "run_command" {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        sh.emit(ProviderEvent::CommandStarted {
            call_id: call_id.clone(),
            command: redact(&command),
            cwd: Some(sh.p.tools.root().to_string_lossy().into_owned()),
        })
        .await;
        let (ltx, mut lrx) = mpsc::channel::<String>(256);
        let fwd = {
            let sh = sh.clone();
            let id = call_id.clone();
            tokio::spawn(async move {
                while let Some(chunk) = lrx.recv().await {
                    sh.emit(ProviderEvent::CommandOutput {
                        call_id: id.clone(),
                        chunk: redact(&chunk),
                    })
                    .await;
                }
            })
        };
        let r = sh.p.tools.run_command(&command, cancel, ltx).await;
        let _ = fwd.await;
        sh.emit(ProviderEvent::CommandCompleted {
            call_id,
            exit_code: r.exit_code,
        })
        .await;
        if r.cancelled {
            return None;
        }
        let mut out = String::new();
        if r.timed_out {
            out.push_str("the command was stopped: it ran longer than the time limit\n");
        }
        match r.exit_code {
            Some(c) => out.push_str(&format!("exit code: {c}\n")),
            None => out.push_str("exit code: unknown\n"),
        }
        out.push_str(&r.output);
        return Some(out);
    }
    sh.emit(ProviderEvent::ToolStarted {
        call_id: call_id.clone(),
        name: name.into(),
        detail: Some(preview(args)),
    })
    .await;
    let tools = sh.p.tools.clone();
    let (n, a) = (name.to_string(), args.clone());
    let res = tokio::task::spawn_blocking(move || tools.run_file_tool(&n, &a)).await;
    match res {
        Ok(Ok(o)) => {
            if !o.changed.is_empty() {
                sh.emit(ProviderEvent::FileChanged {
                    paths: o.changed.clone(),
                })
                .await;
            }
            sh.emit(ProviderEvent::ToolCompleted {
                call_id,
                name: name.into(),
                ok: true,
                detail: None,
            })
            .await;
            Some(o.text)
        }
        Ok(Err(e)) => {
            sh.emit(ProviderEvent::ToolCompleted {
                call_id,
                name: name.into(),
                ok: false,
                detail: Some(e.clone()),
            })
            .await;
            Some(format!("error: {e}"))
        }
        Err(e) => Some(format!("error: tool crashed: {e}")),
    }
}
