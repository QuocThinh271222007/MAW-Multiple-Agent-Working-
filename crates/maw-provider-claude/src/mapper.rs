//! Pure translation of Claude Code `stream-json` output lines into MAW [`ProviderEvent`]s.
//!
//! No I/O happens here, so the whole mapping is exercised against real captured streams
//! (`fixtures/claude/*.jsonl`). Unknown message types are ignored, never fatal.

use maw_core::agent::UsageMetrics;
use maw_core::provider::{ApprovalKind, ProviderEvent, TurnOutcome};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// State shared between the mapper (reader task) and the session handle.
#[derive(Default)]
pub struct Flags {
    /// A turn is in flight (between `system/init` and `result`).
    pub turn_open: AtomicBool,
    /// The host asked the CLI to interrupt the current turn.
    pub interrupt_requested: AtomicBool,
}

/// A permission prompt waiting for the operator.
#[derive(Clone, Debug)]
pub struct PendingApproval {
    pub request_id: String,
    /// Original tool input; `allow` responses must echo it as `updatedInput`.
    pub input: Value,
    /// `permission_suggestions` offered by the CLI (used for session-scoped approvals).
    pub suggestions: Value,
}

/// Output of mapping one line.
#[derive(Debug)]
pub enum Out {
    Event(ProviderEvent),
    /// Register a pending approval so it can be answered later.
    Approval(PendingApproval),
    /// A line that must be written back to the CLI's stdin (e.g. an error response).
    Reply(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolKind {
    Bash,
    FileEdit,
    Other,
}

struct ToolMeta {
    name: String,
    kind: ToolKind,
    paths: Vec<String>,
}

pub struct Mapper {
    session_id: String,
    flags: Arc<Flags>,
    turn_seq: u64,
    turn_id: String,
    tools: HashMap<String, ToolMeta>,
    reported_mcp: HashSet<String>,
    /// Thinking text already forwarded from streaming deltas for the current message (so the
    /// final assistant block is not shown twice).
    thinking_streamed: bool,
}

const MAX_DETAIL: usize = 160;
const MAX_OUTPUT: usize = 8 * 1024;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

/// Plain text of a `tool_result` content (string or array of text blocks).
fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn classify(name: &str) -> ToolKind {
    match name {
        "Bash" => ToolKind::Bash,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ToolKind::FileEdit,
        _ => ToolKind::Other,
    }
}

fn input_paths(input: &Value) -> Vec<String> {
    ["file_path", "notebook_path", "path"]
        .iter()
        .filter_map(|k| str_field(input, k))
        .map(String::from)
        .collect()
}

fn summarize_input(name: &str, input: &Value) -> String {
    let pick = |k: &str| str_field(input, k).map(String::from);
    let s = match name {
        "Read" | "Edit" | "MultiEdit" | "Write" => pick("file_path"),
        "Grep" | "Glob" => pick("pattern"),
        "Bash" => pick("command"),
        _ => None,
    };
    truncate(&s.unwrap_or_else(|| input.to_string()), MAX_DETAIL)
}

/// Parse "Exit code N" produced by the Bash tool's error text.
fn parse_exit_code(text: &str) -> Option<i64> {
    let idx = text.find("Exit code")?;
    let rest = &text[idx + "Exit code".len()..];
    let digits: String = rest
        .trim_start_matches([':', ' '])
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

fn looks_like_auth_failure(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("/login")
        || t.contains("invalid api key")
        || t.contains("authentication")
        || t.contains("oauth token")
        || t.contains("not logged in")
}

impl Mapper {
    pub fn new(session_id: String, flags: Arc<Flags>) -> Self {
        Mapper {
            session_id,
            flags,
            turn_seq: 0,
            turn_id: String::new(),
            tools: HashMap::new(),
            reported_mcp: HashSet::new(),
            thinking_streamed: false,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn ensure_turn(&mut self, out: &mut Vec<Out>) {
        if !self.flags.turn_open.swap(true, Ordering::SeqCst) {
            self.turn_seq += 1;
            self.turn_id = format!("{}:{}", self.session_id, self.turn_seq);
            out.push(Out::Event(ProviderEvent::TurnStarted {
                turn_id: self.turn_id.clone(),
            }));
        }
    }

    /// Map one parsed line. Never panics on unexpected shapes.
    pub fn map_line(&mut self, v: &Value) -> Vec<Out> {
        let mut out = Vec::new();
        match str_field(v, "type").unwrap_or("") {
            "system" => self.on_system(v, &mut out),
            "stream_event" => self.on_stream_event(v, &mut out),
            "assistant" => self.on_assistant(v, &mut out),
            "user" => self.on_user(v, &mut out),
            "result" => self.on_result(v, &mut out),
            "control_request" => self.on_control_request(v, &mut out),
            "rate_limit_event" => self.on_rate_limit(v, &mut out),
            // answers to our own control requests, cancellations, keep-alives, suggestions...
            other => tracing::debug!(kind = other, "ignoring claude stream message"),
        }
        out
    }

    fn on_system(&mut self, v: &Value, out: &mut Vec<Out>) {
        match str_field(v, "subtype").unwrap_or("") {
            "init" => {
                self.ensure_turn(out);
                if let Some(sid) = str_field(v, "session_id") {
                    if sid != self.session_id {
                        self.session_id = sid.to_string();
                        out.push(Out::Event(ProviderEvent::Connected {
                            native_session_id: Some(sid.to_string()),
                        }));
                    }
                }
                if let Some(servers) = v.get("mcp_servers").and_then(Value::as_array) {
                    for s in servers {
                        let name = str_field(s, "name").unwrap_or("?");
                        let status = str_field(s, "status").unwrap_or("unknown");
                        // "pending" is a normal transient state for remote servers.
                        if matches!(status, "connected" | "pending") {
                            continue;
                        }
                        if self.reported_mcp.insert(format!("{name}:{status}")) {
                            out.push(Out::Event(ProviderEvent::Error {
                                message: format!("MCP server '{name}' is {status}: its coordination tools are unavailable to this agent"),
                                fatal: false,
                            }));
                        }
                    }
                }
            }
            "api_retry" => {
                let category = str_field(v, "error").unwrap_or("unknown");
                let attempt = v.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                let max = v.get("max_retries").and_then(Value::as_u64).unwrap_or(0);
                let delay = v.get("retry_delay_ms").and_then(Value::as_u64);
                if category == "rate_limit" || category == "overloaded" {
                    out.push(Out::Event(ProviderEvent::RateLimited {
                        retry_after_ms: delay,
                        detail: format!("{category} (retry {attempt}/{max})"),
                    }));
                } else {
                    out.push(Out::Event(ProviderEvent::Error {
                        message: format!("API retry {attempt}/{max}: {category}"),
                        fatal: false,
                    }));
                }
            }
            other => tracing::debug!(subtype = other, "ignoring claude system message"),
        }
    }

    fn on_rate_limit(&mut self, v: &Value, out: &mut Vec<Out>) {
        let info = v.get("rate_limit_info").unwrap_or(&Value::Null);
        let status = str_field(info, "status").unwrap_or("allowed");
        if status != "allowed" && status != "allowed_warning" {
            let kind = str_field(info, "rateLimitType").unwrap_or("usage limit");
            let resets = info.get("resetsAt").and_then(Value::as_i64);
            out.push(Out::Event(ProviderEvent::RateLimited {
                retry_after_ms: None,
                detail: format!(
                    "{kind}: {status}{}",
                    resets
                        .map(|r| format!(" (resets at unix {r})"))
                        .unwrap_or_default()
                ),
            }));
        }
    }

    fn on_stream_event(&mut self, v: &Value, out: &mut Vec<Out>) {
        // Sub-agent streams are noise for the transcript; their tool calls arrive as full messages.
        if !v
            .get("parent_tool_use_id")
            .map(Value::is_null)
            .unwrap_or(true)
        {
            return;
        }
        let Some(ev) = v.get("event") else { return };
        if str_field(ev, "type") == Some("content_block_delta") {
            if let Some(d) = ev.get("delta") {
                // Thinking is forwarded only as far as Claude Code itself streams it to clients.
                if str_field(d, "type") == Some("thinking_delta") {
                    if let Some(t) = str_field(d, "thinking").filter(|t| !t.is_empty()) {
                        self.ensure_turn(out);
                        self.thinking_streamed = true;
                        out.push(Out::Event(ProviderEvent::Reasoning {
                            text: t.to_string(),
                        }));
                    }
                }
                if str_field(d, "type") == Some("text_delta") {
                    if let Some(t) = str_field(d, "text") {
                        self.ensure_turn(out);
                        out.push(Out::Event(ProviderEvent::MessageDelta {
                            text: t.to_string(),
                        }));
                    }
                }
            }
        }
    }

    fn on_assistant(&mut self, v: &Value, out: &mut Vec<Out>) {
        self.ensure_turn(out);
        if let Some(err) = str_field(v, "error").filter(|e| !e.is_empty()) {
            if err == "rate_limit" {
                out.push(Out::Event(ProviderEvent::RateLimited {
                    retry_after_ms: None,
                    detail: "rate_limit".into(),
                }));
            } else {
                out.push(Out::Event(ProviderEvent::Error {
                    message: format!("assistant error: {err}"),
                    fatal: err == "authentication_failed",
                }));
            }
        }
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        // streamed deltas already showed this message's thinking; the flag covers one message
        let streamed_thinking = std::mem::take(&mut self.thinking_streamed);
        for b in blocks {
            match str_field(b, "type").unwrap_or("") {
                "thinking" => {
                    // complete block; skip it when the deltas already showed this text
                    if !streamed_thinking {
                        if let Some(t) = str_field(b, "thinking").filter(|t| !t.is_empty()) {
                            out.push(Out::Event(ProviderEvent::Reasoning {
                                text: t.to_string(),
                            }));
                        }
                    }
                }
                "text" => {
                    if let Some(t) = str_field(b, "text").filter(|t| !t.is_empty()) {
                        out.push(Out::Event(ProviderEvent::Message {
                            text: t.to_string(),
                        }));
                    }
                }
                "tool_use" => {
                    let (Some(id), Some(name)) = (str_field(b, "id"), str_field(b, "name")) else {
                        continue;
                    };
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    let kind = classify(name);
                    self.tools.insert(
                        id.to_string(),
                        ToolMeta {
                            name: name.to_string(),
                            kind,
                            paths: input_paths(&input),
                        },
                    );
                    match kind {
                        ToolKind::Bash => out.push(Out::Event(ProviderEvent::CommandStarted {
                            call_id: id.to_string(),
                            command: str_field(&input, "command").unwrap_or_default().to_string(),
                            cwd: None,
                        })),
                        _ => out.push(Out::Event(ProviderEvent::ToolStarted {
                            call_id: id.to_string(),
                            name: name.to_string(),
                            detail: Some(summarize_input(name, &input)),
                        })),
                    }
                }
                _ => {}
            }
        }
    }

    fn on_user(&mut self, v: &Value, out: &mut Vec<Out>) {
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for b in blocks {
            if str_field(b, "type") != Some("tool_result") {
                continue;
            }
            let Some(id) = str_field(b, "tool_use_id") else {
                continue;
            };
            let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            let text = content_text(b.get("content").unwrap_or(&Value::Null));
            let Some(meta) = self.tools.remove(id) else {
                continue;
            };
            match meta.kind {
                ToolKind::Bash => {
                    if !text.trim().is_empty() {
                        out.push(Out::Event(ProviderEvent::CommandOutput {
                            call_id: id.to_string(),
                            chunk: truncate(&text, MAX_OUTPUT),
                        }));
                    }
                    let exit_code = if is_error {
                        parse_exit_code(&text)
                    } else {
                        Some(0)
                    };
                    out.push(Out::Event(ProviderEvent::CommandCompleted {
                        call_id: id.to_string(),
                        exit_code,
                    }));
                }
                ToolKind::FileEdit => {
                    out.push(Out::Event(ProviderEvent::ToolCompleted {
                        call_id: id.to_string(),
                        name: meta.name.clone(),
                        ok: !is_error,
                        detail: is_error.then(|| truncate(&text, MAX_DETAIL)),
                    }));
                    if !is_error && !meta.paths.is_empty() {
                        out.push(Out::Event(ProviderEvent::FileChanged { paths: meta.paths }));
                    }
                }
                ToolKind::Other => out.push(Out::Event(ProviderEvent::ToolCompleted {
                    call_id: id.to_string(),
                    name: meta.name,
                    ok: !is_error,
                    detail: is_error.then(|| truncate(&text, MAX_DETAIL)),
                })),
            }
        }
    }

    fn on_result(&mut self, v: &Value, out: &mut Vec<Out>) {
        self.ensure_turn(out);
        if let Some(u) = usage_from(v) {
            out.push(Out::Event(ProviderEvent::Usage(u)));
        }
        let subtype = str_field(v, "subtype").unwrap_or("");
        let is_error = v
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(subtype != "success");
        let aborted = str_field(v, "terminal_reason") == Some("aborted_streaming")
            || self.flags.interrupt_requested.load(Ordering::SeqCst) && is_error;
        let error_text = || {
            let errors = v
                .get("errors")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|s| !s.is_empty());
            errors
                .or_else(|| str_field(v, "result").map(String::from))
                .unwrap_or_else(|| subtype.to_string())
        };
        let (outcome, error) = if aborted {
            (TurnOutcome::Interrupted, None)
        } else if is_error {
            (TurnOutcome::Failed, Some(truncate(&error_text(), 400)))
        } else {
            (TurnOutcome::Completed, None)
        };
        if let Some(e) = &error {
            if looks_like_auth_failure(e) {
                out.push(Out::Event(ProviderEvent::Error { message: format!("authentication required: {e} (run `claude auth login` or `maw auth claude`)"), fatal: true }));
            } else if e.to_lowercase().contains("rate limit")
                || e.to_lowercase().contains("usage limit")
            {
                out.push(Out::Event(ProviderEvent::RateLimited {
                    retry_after_ms: None,
                    detail: truncate(e, 200),
                }));
            }
        }
        let turn_id = self.turn_id.clone();
        self.flags.turn_open.store(false, Ordering::SeqCst);
        self.flags
            .interrupt_requested
            .store(false, Ordering::SeqCst);
        self.tools.clear();
        out.push(Out::Event(ProviderEvent::TurnCompleted {
            turn_id,
            outcome,
            error,
        }));
    }

    fn on_control_request(&mut self, v: &Value, out: &mut Vec<Out>) {
        let Some(request_id) = str_field(v, "request_id") else {
            return;
        };
        let req = v.get("request").unwrap_or(&Value::Null);
        match str_field(req, "subtype").unwrap_or("") {
            "can_use_tool" => {
                self.ensure_turn(out);
                let tool = str_field(req, "tool_name").unwrap_or("tool");
                let input = req.get("input").cloned().unwrap_or(Value::Null);
                let kind = match classify(tool) {
                    ToolKind::Bash => ApprovalKind::Command,
                    ToolKind::FileEdit => ApprovalKind::FileChange,
                    ToolKind::Other => ApprovalKind::Other,
                };
                let summary = match kind {
                    ApprovalKind::Command => format!(
                        "run `{}`",
                        truncate(str_field(&input, "command").unwrap_or_default(), 200)
                    ),
                    ApprovalKind::FileChange => {
                        format!("{} {}", tool.to_lowercase(), input_paths(&input).join(", "))
                    }
                    _ => format!("use {tool} {}", summarize_input(tool, &input)),
                };
                let mut paths = input_paths(&input);
                if let Some(b) = str_field(req, "blocked_path") {
                    if !paths.iter().any(|p| p == b) {
                        paths.push(b.to_string());
                    }
                }
                out.push(Out::Approval(PendingApproval {
                    request_id: request_id.to_string(),
                    input: input.clone(),
                    suggestions: req
                        .get("permission_suggestions")
                        .cloned()
                        .unwrap_or(Value::Null),
                }));
                out.push(Out::Event(ProviderEvent::ApprovalRequested {
                    request_id: request_id.to_string(),
                    kind,
                    summary,
                    reason: str_field(req, "description").map(String::from),
                    paths,
                    details: json!({ "tool_name": tool, "input": input }),
                }));
            }
            other => {
                // Not something a MAW host implements: answer with an error so the CLI never waits forever.
                tracing::debug!(subtype = other, "unsupported control request from claude");
                out.push(Out::Reply(error_response(
                    request_id,
                    &format!("unsupported control request: {other}"),
                )));
            }
        }
    }
}

/// Usage as reported by the CLI. `modelUsage` is cumulative for the conversation; fall back to the
/// per-turn `usage` object. Input counts include cache reads/writes (everything the model read).
fn usage_from(v: &Value) -> Option<UsageMetrics> {
    let num = |o: &Value, k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (input, output) = if let Some(models) = v
        .get("modelUsage")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
    {
        models.values().fold((0u64, 0u64), |(i, o), m| {
            (
                i + num(m, "inputTokens")
                    + num(m, "cacheReadInputTokens")
                    + num(m, "cacheCreationInputTokens"),
                o + num(m, "outputTokens"),
            )
        })
    } else if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
        (
            num(u, "input_tokens")
                + num(u, "cache_read_input_tokens")
                + num(u, "cache_creation_input_tokens"),
            num(u, "output_tokens"),
        )
    } else {
        return v
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .map(|c| UsageMetrics {
                cost_usd: Some(c),
                reported_by_provider: true,
                ..Default::default()
            });
    };
    Some(UsageMetrics {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
        cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
        reported_by_provider: true,
    })
}

pub fn error_response(request_id: &str, message: &str) -> String {
    json!({ "type": "control_response", "response": { "subtype": "error", "request_id": request_id, "error": message } }).to_string()
}

pub fn success_response(request_id: &str, response: Value) -> String {
    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id, "response": response } }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[Value]) -> Vec<Out> {
        let mut m = Mapper::new("sid".into(), Arc::new(Flags::default()));
        lines.iter().flat_map(|l| m.map_line(l)).collect()
    }
    fn events(outs: Vec<Out>) -> Vec<ProviderEvent> {
        outs.into_iter()
            .filter_map(|o| if let Out::Event(e) = o { Some(e) } else { None })
            .collect()
    }

    #[test]
    fn thinking_is_forwarded_once_whether_streamed_or_only_in_the_final_block() {
        let reasoning = |evs: &[ProviderEvent]| -> String {
            evs.iter()
                .filter_map(|e| match e {
                    ProviderEvent::Reasoning { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        };
        // streamed deltas + the final block that repeats them: shown once
        let streamed = events(run(&[
            json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"First, "}}}),
            json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"read the file."}}}),
            json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"First, read the file."},{"type":"text","text":"Done."}]}}),
        ]));
        assert_eq!(reasoning(&streamed), "First, read the file.");
        // no deltas (partial messages off): the complete block is the only source
        let block_only = events(run(&[
            json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"Plan: edit then test."},{"type":"text","text":"ok"}]}}),
        ]));
        assert_eq!(reasoning(&block_only), "Plan: edit then test.");
        // the visible answer is untouched
        assert!(block_only
            .iter()
            .any(|e| matches!(e, ProviderEvent::Message { text } if text == "ok")));
        // an empty / redacted block yields nothing
        let empty = events(run(&[
            json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":""},{"type":"redacted_thinking","data":"xyz"}]}}),
        ]));
        assert!(reasoning(&empty).is_empty());
    }

    #[test]
    fn exit_code_parsing() {
        assert_eq!(parse_exit_code("Exit code 2\nboom"), Some(2));
        assert_eq!(parse_exit_code("Exit code: 127"), Some(127));
        assert_eq!(parse_exit_code("fine"), None);
    }

    #[test]
    fn unknown_and_garbage_shapes_are_ignored() {
        let evs = events(run(&[
            json!({"type": "wat"}),
            json!({"nope": 1}),
            json!(42),
            json!({"type": "assistant"}),
            json!({"type":"user","message":{"content":"text"}}),
        ]));
        // an assistant message with no content only opens a turn
        assert!(
            evs.iter()
                .all(|e| matches!(e, ProviderEvent::TurnStarted { .. })),
            "{evs:?}"
        );
    }

    #[test]
    fn unsupported_control_requests_get_an_error_reply() {
        let outs = run(&[
            json!({"type":"control_request","request_id":"r1","request":{"subtype":"hook_callback"}}),
        ]);
        match &outs[0] {
            Out::Reply(l) => {
                let v: Value = serde_json::from_str(l).unwrap();
                assert_eq!(v["response"]["subtype"], "error");
                assert_eq!(v["response"]["request_id"], "r1");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn failed_mcp_server_is_surfaced_once() {
        let init = json!({"type":"system","subtype":"init","session_id":"sid","mcp_servers":[{"name":"maw","status":"failed"}]});
        let evs = events(run(&[
            init.clone(),
            json!({"type":"result","subtype":"success","is_error":false,"result":"x"}),
            init,
        ]));
        let errors = evs
            .iter()
            .filter(|e| matches!(e, ProviderEvent::Error { .. }))
            .count();
        assert_eq!(errors, 1, "{evs:?}");
    }

    #[test]
    fn auth_failure_is_fatal_and_actionable() {
        let evs = events(run(&[
            json!({"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}),
        ]));
        assert!(evs.iter().any(|e| matches!(e, ProviderEvent::Error { fatal: true, message } if message.contains("auth"))));
        assert!(evs.iter().any(|e| matches!(
            e,
            ProviderEvent::TurnCompleted {
                outcome: TurnOutcome::Failed,
                ..
            }
        )));
    }

    #[test]
    fn rate_limit_paths() {
        let evs = events(run(&[
            json!({"type":"system","subtype":"api_retry","error":"rate_limit","attempt":1,"max_retries":3,"retry_delay_ms":2000}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1}}),
        ]));
        let rl: Vec<_> = evs
            .iter()
            .filter(|e| matches!(e, ProviderEvent::RateLimited { .. }))
            .collect();
        assert_eq!(rl.len(), 2, "{evs:?}");
        assert!(matches!(
            rl[0],
            ProviderEvent::RateLimited {
                retry_after_ms: Some(2000),
                ..
            }
        ));
    }

    #[test]
    fn usage_prefers_cumulative_model_usage_and_marks_reported() {
        let v = json!({"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.5,
            "usage":{"input_tokens":1,"output_tokens":1},
            "modelUsage":{"m":{"inputTokens":10,"cacheReadInputTokens":100,"cacheCreationInputTokens":5,"outputTokens":7}}});
        let u = usage_from(&v).unwrap();
        assert_eq!(
            (u.input_tokens, u.output_tokens, u.total_tokens),
            (Some(115), Some(7), Some(122))
        );
        assert_eq!(u.cost_usd, Some(0.5));
        assert!(u.reported_by_provider);
    }

    #[test]
    fn bash_failure_reports_exit_code_and_edit_reports_files() {
        let evs = events(run(&[
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"false"}},{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"src/a.rs"}}]}}),
            json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"b1","is_error":true,"content":"Exit code 1\nnope"},
                {"type":"tool_result","tool_use_id":"e1","is_error":false,"content":"ok"}]}}),
        ]));
        assert!(evs.iter().any(|e| matches!(
            e,
            ProviderEvent::CommandCompleted {
                exit_code: Some(1),
                ..
            }
        )));
        assert!(evs.iter().any(|e| matches!(e, ProviderEvent::FileChanged { paths } if paths == &vec!["src/a.rs".to_string()])));
    }
}
