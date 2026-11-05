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
use crate::persist;
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

/// Speed of the most recent request: (when, generation tok/s, prompt tok/s).
pub type SpeedCell = Arc<Mutex<Option<(std::time::Instant, f32, f32)>>>;

/// MAW tools that hand the work item back. After one of them succeeds the turn is over: small
/// models otherwise keep calling tools or re-submit, which costs time and confuses the state.
const TERMINAL_TOOLS: &[&str] = &[
    "maw_task_submit_result",
    "maw_task_block",
    "maw_review_submit",
];

/// A call that failed this many times with the same arguments is not run again in the same turn.
const MAX_SAME_FAILURES: u32 = 2;
/// Refused repeats of failing calls in one turn before the turn is ended.
const MAX_BLOCKED: u32 = 3;
/// Reads in a row (no write, command or MAW action between) before the model is told to act, and
/// before the turn is ended.
const READS_NUDGE: u32 = 8;
const READS_STOP: u32 = 14;

const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 16 * 1024;

/// Everything a session needs, resolved by the provider before the session starts.
pub struct SessionParams {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model: String,
    pub think: Option<Value>,
    /// Tokens of reasoning allowed before each answer or tool call (0 = no limit). When used up the
    /// request is stopped and asked again with thinking off.
    pub think_budget: u32,
    pub num_ctx: u32,
    pub keep_alive: String,
    pub max_steps: usize,
    pub idle_timeout: Duration,
    pub system: String,
    pub tools: LocalTools,
    pub mcp: Option<McpConnection>,
    /// Where each request's measured speed is published for the performance view.
    pub speed: SpeedCell,
    /// Directory where conversations are kept so they can be resumed (`None`: not kept).
    pub state_dir: Option<std::path::PathBuf>,
    /// The session id saved by an earlier run, when MAW is reconnecting this agent.
    pub resume_id: Option<String>,
}

#[derive(Default)]
struct Inner {
    /// Read-only calls already made since the last change in this turn (name + arguments).
    turn_calls: HashSet<String>,
    /// Calls that failed in this turn (name + arguments): how often, and the last error.
    failed_calls: HashMap<String, (u32, String)>,
    /// Repeats of failing calls refused in this turn.
    blocked_calls: u32,
    /// Read-only calls in a row without any change in between.
    reads_in_row: u32,
    /// Characters of reasoning spent so far in this turn (the allowance covers the whole turn).
    turn_thought: usize,
    /// The "context is full" warning is given once per session.
    ctx_warned: bool,
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
    native_id: String,
}

impl Shared {
    /// Keep the conversation on disk so the session can be resumed. A failure is logged only.
    fn save_history(&self) {
        let Some(dir) = &self.p.state_dir else {
            return;
        };
        let history = self.lock().history.clone();
        if let Err(e) = persist::save(dir, &self.native_id, &history) {
            tracing::warn!("could not save the conversation: {e}");
        }
    }

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
        // Resume the saved conversation when there is one; otherwise start a fresh session and
        // say so, as the other providers do when an earlier conversation is gone.
        let mut notice = None;
        let (native_id, history) = match (&p.resume_id, &p.state_dir) {
            (Some(id), Some(dir)) => match persist::load(dir, id) {
                Some(h) => (id.clone(), h),
                None => {
                    notice = Some("the earlier conversation of this agent could not be found; starting a new one".to_string());
                    (persist::new_id(), vec![])
                }
            },
            (Some(_), None) => {
                notice = Some("conversations are not being kept; starting a new one".to_string());
                (persist::new_id(), vec![])
            }
            _ => (persist::new_id(), vec![]),
        };
        let shared = Arc::new(Shared {
            p,
            tx,
            inner: Mutex::new(Inner {
                history,
                ..Inner::default()
            }),
            tool_defs: defs,
            native_id: native_id.clone(),
        });
        // The instructions and tool schemas are sent with every request. If they already take
        // close to half of the window there is no room for a thinking model to work.
        let reserved = (shared.p.system.len()
            + serde_json::to_string(&shared.tool_defs)
                .map(|s| s.len())
                .unwrap_or(0))
            / 3;
        if shared.p.num_ctx > 0 && reserved * 100 > shared.p.num_ctx as usize * 45 {
            let size_note = format!(
                "the instructions and tools take about {reserved} of the {} context tokens; that leaves little room for the model to think. Raise ollama.num_ctx (Leader: ollama.num_ctx_leader) or the agent may forget its instructions and repeat itself.",
                shared.p.num_ctx
            );
            notice = Some(match notice.take() {
                Some(n) => format!("{n}. Also: {size_note}"),
                None => size_note,
            });
        }
        let s = shared.clone();
        let id = native_id.clone();
        tokio::spawn(async move {
            s.emit(ProviderEvent::Connected {
                native_session_id: Some(id),
            })
            .await;
            if let Some(message) = notice {
                s.emit(ProviderEvent::Error {
                    message,
                    fatal: false,
                })
                .await;
            }
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
            resume: self.shared.p.state_dir.is_some(),
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
            self.shared.save_history();
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
    {
        let mut g = sh.lock();
        g.turn_calls.clear();
        g.failed_calls.clear();
        g.blocked_calls = 0;
        g.reads_in_row = 0;
        g.turn_thought = 0;
        g.history.push(json!({"role":"user","content":text}));
    }
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
    sh.save_history();
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
    // Thinking has an allowance (`SessionParams::think_budget`). If the model uses it all without
    // having started to answer, that request is dropped (Ollama stops generating) and asked again
    // with thinking off, so it acts on what it has instead of reasoning for minutes.
    // The allowance is for one request, and the whole turn gets twice that: a model that reasons a
    // little at every step of a long turn would otherwise cost more than one that reasons once.
    let per_request: Option<usize> =
        (sh.p.think_budget > 0).then(|| sh.p.think_budget as usize * 7 / 2);
    let left_in_turn = per_request.map(|b| (b * 2).saturating_sub(sh.lock().turn_thought));
    let mut think = sh.p.think.clone();
    if left_in_turn == Some(0) {
        think = Some(Value::Bool(false)); // this turn's thinking is spent
    }
    let mut budget_chars: Option<usize> = match (per_request, left_in_turn) {
        (Some(b), Some(left)) if left > 0 => Some(b.min(left)),
        _ => None,
    };
    let mut thought_total = 0usize;
    let (mut content, mut calls) = (String::new(), Vec::<ToolCall>::new());
    let mut last: Option<ChatChunk>;
    'attempt: loop {
        let mut body = json!({
            "model": sh.p.model,
            "messages": messages,
            "tools": sh.tool_defs,
            "stream": true,
            "keep_alive": sh.p.keep_alive,
            "options": {"num_ctx": sh.p.num_ctx},
        });
        if let Some(t) = &think {
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
        content.clear();
        calls.clear();
        last = None;
        let mut thought = 0usize;
        let mut cut = false;
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
                    if let ProviderEvent::Reasoning { text } = &ev {
                        thought += text.len();
                    }
                    sh.emit(ev).await;
                }
            }
            // still thinking, nothing said or called yet, and the allowance is gone
            if budget_chars.is_some_and(|b| thought > b) && content.is_empty() && calls.is_empty() {
                cut = true;
                break;
            }
        }
        thought_total += thought;
        if cut {
            drop(stream);
            sh.emit(ProviderEvent::Reasoning {
                text: format!(
                    "\n[thinking stopped after about {} tokens: answering now]\n",
                    thought * 2 / 7
                ),
            })
            .await;
            think = Some(Value::Bool(false));
            budget_chars = None;
            continue 'attempt;
        }
        if !buf.is_empty() {
            let line = std::mem::take(&mut buf);
            for ev in handle(&line, &mut content, &mut calls, &mut last)? {
                sh.emit(ev).await;
            }
        }
        break 'attempt;
    }
    sh.lock().turn_thought += thought_total;
    let Some(fin) = last else {
        return Err("the response from Ollama ended before it was complete".into());
    };

    // speed of this request, from the server's own timings
    if fin.eval_ns > 0 && fin.output_tokens > 0 {
        let dec = fin.output_tokens as f32 / (fin.eval_ns as f32 / 1e9);
        let pro = if fin.prompt_eval_ns > 0 {
            fin.prompt_tokens as f32 / (fin.prompt_eval_ns as f32 / 1e9)
        } else {
            0.0
        };
        if let Ok(mut g) = sh.p.speed.lock() {
            *g = Some((std::time::Instant::now(), dec, pro));
        }
    }
    // A full window silently loses the start of the prompt (the instructions and the tools): the
    // model then forgets its role and loops. Say so once instead of leaving it a mystery.
    let used = fin.prompt_tokens + fin.output_tokens;
    let window = sh.p.num_ctx as u64;
    if window > 0 && used * 100 >= window * 92 && !sh.lock().ctx_warned {
        sh.lock().ctx_warned = true;
        sh.emit(ProviderEvent::Error {
            message: format!(
                "the context window is full ({used} of {window} tokens: {} prompt + {} reply). Ollama then drops the start of the conversation, so the model may forget its instructions and repeat itself. Raise ollama.num_ctx (Leader: ollama.num_ctx_leader), or use a model that thinks less.",
                fin.prompt_tokens, fin.output_tokens
            ),
            fatal: false,
        })
        .await;
    }
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
    // a model that wrote its tool call as text still gets it run (known tools only)
    if calls.is_empty() && !content.trim().is_empty() {
        let known = |n: &str| {
            sh.tool_defs
                .iter()
                .any(|d| d["function"]["name"].as_str() == Some(n))
        };
        let parsed = api::text_tool_calls(&content, &known);
        if !parsed.is_empty() {
            tracing::info!(
                calls = parsed.len(),
                "ran tool calls the model wrote as text"
            );
            calls = parsed;
        }
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
    let mut handed_back = false;
    for call in calls {
        let key = format!("{}\u{0}{}", call.name, call.arguments);
        // A call that failed twice with exactly these arguments will fail a third time. Do not run
        // it again; tell the model why, and end the turn if it keeps insisting.
        let prior = sh.lock().failed_calls.get(&key).cloned();
        if let Some((times, why)) = prior.filter(|(n, _)| *n >= MAX_SAME_FAILURES) {
            let blocked = {
                let mut g = sh.lock();
                g.blocked_calls += 1;
                g.blocked_calls
            };
            let hint = format!(
                "This exact call has already failed {times} times in this turn (last error: {}). Do not send it again. Change the arguments, use a different tool, or end your turn with a short status.",
                clip(&why, 300)
            );
            sh.lock()
                .history
                .push(json!({"role":"tool","tool_name":call.name,"content":hint}));
            if blocked >= MAX_BLOCKED {
                return Err(format!(
                    "stopped: the model kept repeating a failing call to {}; send a follow-up to continue",
                    call.name
                ));
            }
            continue;
        }
        // A small model that did not get what it expected tends to ask the same read again and
        // again. Say so instead of repeating it; any change (write, command, MAW action) resets this.
        if is_read_only(&call.name) {
            sh.lock().reads_in_row += 1;
            if !sh.lock().turn_calls.insert(key.clone()) {
                let hint = format!(
                    "You already called {} with exactly these arguments in this turn; its result is above. Do not call it again. Use that result to take the next step now, or answer the user.",
                    call.name
                );
                sh.lock()
                    .history
                    .push(json!({"role":"tool","tool_name":call.name,"content":hint}));
                continue;
            }
        } else {
            let mut g = sh.lock();
            g.turn_calls.clear();
            g.reads_in_row = 0;
        }
        match exec_tool(sh, &call, cancel).await {
            None => return Ok(Step::Interrupted),
            Some(mut text) => {
                if TERMINAL_TOOLS.contains(&call.name.as_str()) && !text.starts_with("error:") {
                    handed_back = true;
                }
                if is_failure(&text) {
                    let mut g = sh.lock();
                    let e = g.failed_calls.entry(key).or_insert((0, String::new()));
                    e.0 += 1;
                    e.1 = clip(&text, 300);
                    drop(g);
                    // a call with the wrong arguments gets the shape it should have had
                    if text.contains("invalid input") || text.contains("invalid_input") {
                        if let Some(h) = args_hint(&sh.tool_defs, &call.name) {
                            text.push('\n');
                            text.push_str(&h);
                        }
                    }
                }
                let text = clip(&text, MAX_TOOL_RESULT_BYTES);
                sh.lock()
                    .history
                    .push(json!({"role":"tool","tool_name":call.name,"content":text}));
            }
        }
    }
    // Only looking, never acting: say so, and stop the turn if it goes on.
    let reads = sh.lock().reads_in_row;
    if !handed_back && reads >= READS_STOP {
        return Err(format!(
            "stopped: {reads} reads in a row without doing anything; send a follow-up to continue"
        ));
    }
    if !handed_back && reads == READS_NUDGE {
        sh.lock().history.push(json!({"role":"user","content":
            "[MAW] You have only been reading for several steps in a row. Stop reading: take the next action now (write the file, submit the result, send the message) or answer."}));
    }
    Ok(if handed_back {
        Step::Done
    } else {
        Step::Continue
    })
}

/// Whether a tool result is a failure of the call (not just an unwelcome answer).
fn is_failure(text: &str) -> bool {
    text.starts_with("error:")
        || text.starts_with("refused:")
        || text.starts_with("unknown tool")
        || text.starts_with("the user denied")
}

/// "Expected arguments for NAME: a (string, required), b (array) ..." from the tool's schema.
fn args_hint(defs: &[Value], name: &str) -> Option<String> {
    let f = defs
        .iter()
        .find(|d| d["function"]["name"].as_str() == Some(name))?;
    let params = &f["function"]["parameters"];
    let props = params["properties"].as_object()?;
    let required: Vec<&str> = params["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let list: Vec<String> = props
        .iter()
        .map(|(k, v)| {
            let ty = v["type"].as_str().unwrap_or("value");
            if required.contains(&k.as_str()) {
                format!("{k} ({ty}, required)")
            } else {
                format!("{k} ({ty})")
            }
        })
        .collect();
    Some(clip(
        &format!("Expected arguments for {name}: {}", list.join(", ")),
        600,
    ))
}

/// Calls that only look at things. Repeating one with the same arguments, with nothing changed in
/// between, cannot give a new answer.
fn is_read_only(name: &str) -> bool {
    matches!(name, "read_file" | "list_dir" | "search_text")
        || (name.starts_with("maw_")
            && ["_get", "_list", "_inbox", "_read", "_history"]
                .iter()
                .any(|s| name.ends_with(s)))
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
