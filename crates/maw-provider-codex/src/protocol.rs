//! Pure translation between the Codex App Server wire protocol and MAW's provider-neutral
//! events. No I/O lives here so every mapping can be tested against recorded JSONL.

use maw_core::provider::*;
use maw_core::{PermissionProfile, UsageMetrics};
use serde_json::{json, Value};

/// Name under which the MAW coordination server is registered in Codex's MCP configuration.
pub const MCP_SERVER_NAME: &str = "maw";
/// Environment variable (set on the child process only) carrying the per-agent bearer token.
pub const MCP_TOKEN_ENV: &str = "MAW_MCP_TOKEN";

/// Codex `AskForApproval` for a MAW permission profile.
pub fn approval_policy(p: PermissionProfile) -> &'static str {
    match p {
        PermissionProfile::Safe => "untrusted",
        PermissionProfile::Balanced => "on-request",
        PermissionProfile::Autonomous => "never",
    }
}

/// Codex `SandboxMode` for a MAW permission profile. `danger-full-access` is never selected.
pub fn sandbox_mode(p: PermissionProfile) -> &'static str {
    match p {
        PermissionProfile::Safe => "read-only",
        PermissionProfile::Balanced | PermissionProfile::Autonomous => "workspace-write",
    }
}

/// Per-server key that makes Codex run every tool of that MCP server without asking.
/// Without it, Codex (policy `on-request`/`untrusted`) sends an `mcpServer/elicitation/request`
/// before each non-`readOnlyHint` tool call. Verified live against codex 0.166.0.
pub const PREAPPROVE_KEY: &str = "default_tools_approval_mode";
pub const PREAPPROVE_VALUE: &str = "approve";

/// Config override registering MAW's streamable-HTTP MCP server. The token itself is NOT
/// included: Codex reads it from [`MCP_TOKEN_ENV`] in its own environment.
///
/// `preapprove` sets [`PREAPPROVE_KEY`] on MAW's *own* loopback coordination server only; the
/// coordination tools are authorized by MAW's role checks, and other MCP servers still ask.
pub fn mcp_config(endpoint: &McpEndpoint, preapprove: bool) -> Value {
    let mut server = json!({
        "url": endpoint.url,
        "bearer_token_env_var": MCP_TOKEN_ENV,
    });
    if preapprove {
        server[PREAPPROVE_KEY] = json!(PREAPPROVE_VALUE);
    }
    json!({ "mcp_servers": { MCP_SERVER_NAME: server } })
}

/// Parameters shared by `thread/start` and `thread/resume`.
pub fn thread_params(
    cfg: &SessionConfig,
    include_mcp_config: bool,
    preapprove: bool,
    reasoning_summary: Option<&str>,
) -> Value {
    let mut p = json!({
        "cwd": cfg.cwd.to_string_lossy(),
        "approvalPolicy": approval_policy(cfg.permission),
        "sandbox": sandbox_mode(cfg.permission),
        "developerInstructions": cfg.instructions,
    });
    if let Some(m) = &cfg.model {
        p["model"] = json!(m);
    }
    // Per-thread Codex config. `model_reasoning_summary` is what makes Codex stream the reasoning
    // summaries it is willing to give (without it the reasoning item comes back empty).
    let mut config = serde_json::Map::new();
    if include_mcp_config {
        if let Some(e) = &cfg.mcp {
            if let Value::Object(m) = mcp_config(e, preapprove) {
                config.extend(m);
            }
        }
    }
    if let Some(s) = reasoning_summary.filter(|s| !s.is_empty()) {
        config.insert("model_reasoning_summary".into(), json!(s));
    }
    // folders the user allowed besides the worktree: the sandbox may write there too. Not for the
    // read-only profile, which writes nowhere.
    if !cfg.extra_dirs.is_empty() && sandbox_mode(cfg.permission) == "workspace-write" {
        config.insert(
            "sandbox_workspace_write".into(),
            json!({ "writable_roots": cfg.extra_dirs.iter().map(|d| d.to_string_lossy()).collect::<Vec<_>>() }),
        );
    }
    if !config.is_empty() {
        p["config"] = Value::Object(config);
    }
    p
}

pub fn turn_start_params(thread_id: &str, text: &str, effort: Option<&str>) -> Value {
    let mut p = json!({
        "threadId": thread_id,
        "input": [{ "type": "text", "text": text, "text_elements": [] }],
    });
    if let Some(e) = effort.filter(|e| !e.is_empty()) {
        p["effort"] = json!(e);
    }
    p
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn snippet(v: &Value, max: usize) -> String {
    let t = match v {
        Value::String(x) => x.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    t.chars().take(max).collect()
}

/// Translate one server notification into zero or more MAW events.
pub fn translate_notification(method: &str, params: &Value) -> Vec<ProviderEvent> {
    match method {
        "turn/started" => {
            let id = params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![ProviderEvent::TurnStarted {
                turn_id: id.to_string(),
            }]
        }
        "turn/completed" => {
            let turn = params.get("turn").unwrap_or(&Value::Null);
            let outcome = match s(turn, "status") {
                Some("interrupted") => TurnOutcome::Interrupted,
                Some("failed") => TurnOutcome::Failed,
                _ => TurnOutcome::Completed,
            };
            let error = turn
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(String::from);
            vec![ProviderEvent::TurnCompleted {
                turn_id: s(turn, "id").unwrap_or_default().to_string(),
                outcome,
                error,
            }]
        }
        "item/started" => params
            .get("item")
            .map(translate_item_started)
            .unwrap_or_default(),
        "item/completed" => params
            .get("item")
            .map(translate_item_completed)
            .unwrap_or_default(),
        // Reasoning the model/Codex chooses to stream (summaries; raw text only if enabled).
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => s(params, "delta")
            .map(|d| {
                vec![ProviderEvent::Reasoning {
                    text: d.to_string(),
                }]
            })
            .unwrap_or_default(),
        "item/reasoning/summaryPartAdded" => vec![ProviderEvent::Reasoning {
            text: "\n\n".into(),
        }],
        "item/agentMessage/delta" => s(params, "delta")
            .map(|d| {
                vec![ProviderEvent::MessageDelta {
                    text: d.to_string(),
                }]
            })
            .unwrap_or_default(),
        "item/commandExecution/outputDelta" => match (s(params, "itemId"), s(params, "delta")) {
            (Some(id), Some(d)) => vec![ProviderEvent::CommandOutput {
                call_id: id.to_string(),
                chunk: d.to_string(),
            }],
            _ => vec![],
        },
        "thread/tokenUsage/updated" => {
            let t = params.pointer("/tokenUsage/total").unwrap_or(&Value::Null);
            let n = |k: &str| t.get(k).and_then(Value::as_u64);
            vec![ProviderEvent::Usage(UsageMetrics {
                input_tokens: n("inputTokens"),
                output_tokens: n("outputTokens"),
                total_tokens: n("totalTokens"),
                cost_usd: None,
                reported_by_provider: true,
            })]
        }
        "account/rateLimits/updated" => {
            let rl = params.get("rateLimits").unwrap_or(&Value::Null);
            // Only an actually-reached limit is an event; usage percentages alone are not.
            match rl.get("rateLimitReachedType").and_then(Value::as_str) {
                Some(kind) => {
                    let resets = rl.pointer("/primary/resetsAt").and_then(Value::as_i64);
                    vec![ProviderEvent::RateLimited {
                        retry_after_ms: resets.and_then(retry_ms_from_epoch),
                        detail: format!("rate limit reached ({kind})"),
                    }]
                }
                None => vec![],
            }
        }
        "error" => {
            let msg = params
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            let will_retry = params
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // The turn outcome is reported by turn/completed; an error notification is never fatal to the session.
            vec![ProviderEvent::Error {
                message: if will_retry {
                    format!("{msg} (retrying)")
                } else {
                    msg.to_string()
                },
                fatal: false,
            }]
        }
        "mcpServer/startupStatus/updated" => {
            if s(params, "status") == Some("failed") {
                let name = s(params, "name").unwrap_or("?");
                let err = s(params, "error").unwrap_or("startup failed");
                vec![ProviderEvent::Error {
                    message: format!("MCP server '{name}' failed to start: {err}"),
                    fatal: false,
                }]
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// Reset timestamps are unix seconds; convert to a bounded relative delay.
fn retry_ms_from_epoch(resets_at: i64) -> Option<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let secs = resets_at - now;
    (secs > 0 && secs < 7 * 24 * 3600).then(|| secs as u64 * 1000)
}

fn translate_item_started(item: &Value) -> Vec<ProviderEvent> {
    let id = s(item, "id").unwrap_or_default().to_string();
    match s(item, "type") {
        Some("commandExecution") => vec![ProviderEvent::CommandStarted {
            call_id: id,
            command: s(item, "command").unwrap_or_default().to_string(),
            cwd: item.get("cwd").and_then(Value::as_str).map(String::from),
        }],
        Some("fileChange") => vec![ProviderEvent::ToolStarted {
            call_id: id,
            name: "fileChange".into(),
            detail: Some(file_change_paths(item).join(", ")),
        }],
        Some("mcpToolCall") => vec![ProviderEvent::ToolStarted {
            call_id: id,
            name: format!(
                "{}/{}",
                s(item, "server").unwrap_or("?"),
                s(item, "tool").unwrap_or("?")
            ),
            detail: item.get("arguments").map(|a| snippet(a, 160)),
        }],
        Some("dynamicToolCall") => vec![ProviderEvent::ToolStarted {
            call_id: id,
            name: s(item, "tool").unwrap_or("tool").to_string(),
            detail: item.get("arguments").map(|a| snippet(a, 160)),
        }],
        Some("webSearch") => vec![ProviderEvent::ToolStarted {
            call_id: id,
            name: "webSearch".into(),
            detail: s(item, "query").map(String::from),
        }],
        _ => vec![],
    }
}

fn translate_item_completed(item: &Value) -> Vec<ProviderEvent> {
    let id = s(item, "id").unwrap_or_default().to_string();
    match s(item, "type") {
        Some("agentMessage") => match s(item, "text") {
            Some(t) if !t.is_empty() => vec![ProviderEvent::Message {
                text: t.to_string(),
            }],
            _ => vec![],
        },
        // A finished reasoning item: use whatever readable text it carries (summary first, then
        // content). Models that only reason internally return both empty, which shows nothing.
        Some("reasoning") => {
            let texts = |key: &str| -> Vec<String> {
                item.get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                e.as_str().map(String::from).or_else(|| {
                                    e.get("text").and_then(Value::as_str).map(String::from)
                                })
                            })
                            .filter(|t| !t.trim().is_empty())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let mut t = texts("summary");
            if t.is_empty() {
                t = texts("content");
            }
            if t.is_empty() {
                vec![]
            } else {
                vec![ProviderEvent::Reasoning {
                    text: t.join("\n\n"),
                }]
            }
        }
        Some("commandExecution") => vec![ProviderEvent::CommandCompleted {
            call_id: id,
            exit_code: item.get("exitCode").and_then(Value::as_i64),
        }],
        Some("fileChange") => {
            let paths = file_change_paths(item);
            let ok = s(item, "status") == Some("completed");
            let mut ev = vec![ProviderEvent::ToolCompleted {
                call_id: id,
                name: "fileChange".into(),
                ok,
                detail: Some(paths.join(", ")),
            }];
            if ok && !paths.is_empty() {
                ev.push(ProviderEvent::FileChanged { paths });
            }
            ev
        }
        Some("mcpToolCall") => {
            let err = item
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(String::from);
            let ok = s(item, "status") == Some("completed") && err.is_none();
            vec![ProviderEvent::ToolCompleted {
                call_id: id,
                name: format!(
                    "{}/{}",
                    s(item, "server").unwrap_or("?"),
                    s(item, "tool").unwrap_or("?")
                ),
                ok,
                detail: err,
            }]
        }
        Some("dynamicToolCall") => vec![ProviderEvent::ToolCompleted {
            call_id: id,
            name: s(item, "tool").unwrap_or("tool").to_string(),
            ok: item
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            detail: None,
        }],
        Some("webSearch") => vec![ProviderEvent::ToolCompleted {
            call_id: id,
            name: "webSearch".into(),
            ok: true,
            detail: None,
        }],
        _ => vec![],
    }
}

fn file_change_paths(item: &Value) -> Vec<String> {
    item.get("changes")
        .and_then(Value::as_array)
        .map(|c| {
            c.iter()
                .filter_map(|x| x.get("path").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// What MAW must remember to answer an approval request later.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingApproval {
    /// The original JSON-RPC request id (string or integer) to answer.
    pub rpc_id: Value,
    pub kind: ApprovalKind,
    /// For permission requests: the permission profile Codex asked for (echoed back on approval).
    pub requested_permissions: Option<Value>,
}

#[derive(Debug)]
pub enum ServerRequestOutcome {
    Approval {
        request_id: String,
        event: ProviderEvent,
        pending: PendingApproval,
    },
    /// Respond with a JSON-RPC error so the server never waits on us.
    Reject { code: i64, message: String },
    /// Answer immediately with a valid (declining) result and tell the supervisor why.
    Respond { result: Value, notice: String },
}

/// `mcpServer/elicitation/request` that Codex uses to ask "may this MCP tool run?".
/// Identified by `_meta.codex_approval_kind == "mcp_tool_call"` (observed on codex 0.166.0).
fn is_tool_call_approval(params: &Value) -> bool {
    params
        .pointer("/_meta/codex_approval_kind")
        .and_then(Value::as_str)
        == Some("mcp_tool_call")
}

/// The tool name is only present in the human-readable message: `Allow the maw MCP server to
/// run tool "maw_task_block"?`.
fn tool_name_from_message(msg: &str) -> Option<&str> {
    let start = msg.find('"')? + 1;
    let len = msg[start..].find('"')?;
    Some(&msg[start..start + len])
}

fn translate_elicitation(id: &Value, params: &Value) -> ServerRequestOutcome {
    let server = s(params, "serverName").unwrap_or("?");
    if !is_tool_call_approval(params) {
        // A genuine form / URL elicitation: MAW has no UI to fill it in. Decline explicitly (a valid
        // answer, so the tool call fails fast instead of hanging) and say so.
        let mode = s(params, "mode").unwrap_or("?");
        return ServerRequestOutcome::Respond {
            result: json!({ "action": "decline" }),
            notice: format!(
                "MCP server '{server}' asked for interactive input (mode '{mode}': {}); MAW cannot present it and declined",
                snippet(&json!(s(params, "message").unwrap_or("")), 120)
            ),
        };
    }
    let message = s(params, "message").unwrap_or_default();
    let tool = tool_name_from_message(message).unwrap_or("(unknown tool)");
    let request_id = rpc_id_string(id);
    let meta = params.get("_meta").cloned().unwrap_or(Value::Null);
    ServerRequestOutcome::Approval {
        request_id: request_id.clone(),
        event: ProviderEvent::ApprovalRequested {
            request_id,
            kind: ApprovalKind::Other,
            summary: format!("call tool {server}/{tool}"),
            reason: meta
                .get("tool_description")
                .and_then(Value::as_str)
                .map(String::from),
            paths: vec![],
            details: json!({
                "kind": "mcp_tool_call",
                "server": server,
                "tool": tool,
                "arguments": meta.get("tool_params"),
                "message": message,
                "turnId": s(params, "turnId"),
            }),
        },
        pending: PendingApproval {
            rpc_id: id.clone(),
            kind: ApprovalKind::Other,
            requested_permissions: None,
        },
    }
}

pub fn rpc_id_string(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn translate_server_request(id: &Value, method: &str, params: &Value) -> ServerRequestOutcome {
    let request_id = rpc_id_string(id);
    match method {
        "item/commandExecution/requestApproval" => {
            let command = s(params, "command").unwrap_or("(unknown command)");
            let cwd = params.get("cwd").and_then(Value::as_str);
            ServerRequestOutcome::Approval {
                request_id: request_id.clone(),
                event: ProviderEvent::ApprovalRequested {
                    request_id,
                    kind: ApprovalKind::Command,
                    summary: format!("run `{command}`"),
                    reason: s(params, "reason").map(String::from),
                    paths: cwd.map(|c| vec![c.to_string()]).unwrap_or_default(),
                    details: json!({ "command": command, "cwd": cwd, "itemId": s(params, "itemId"), "turnId": s(params, "turnId") }),
                },
                pending: PendingApproval {
                    rpc_id: id.clone(),
                    kind: ApprovalKind::Command,
                    requested_permissions: None,
                },
            }
        }
        "item/fileChange/requestApproval" => {
            let root = s(params, "grantRoot");
            ServerRequestOutcome::Approval {
                request_id: request_id.clone(),
                event: ProviderEvent::ApprovalRequested {
                    request_id,
                    kind: ApprovalKind::FileChange,
                    summary: match root {
                        Some(r) => format!("write files under {r}"),
                        None => "apply file changes".to_string(),
                    },
                    reason: s(params, "reason").map(String::from),
                    paths: root.map(|r| vec![r.to_string()]).unwrap_or_default(),
                    details: json!({ "grantRoot": root, "itemId": s(params, "itemId"), "turnId": s(params, "turnId") }),
                },
                pending: PendingApproval {
                    rpc_id: id.clone(),
                    kind: ApprovalKind::FileChange,
                    requested_permissions: None,
                },
            }
        }
        "item/permissions/requestApproval" => {
            let perms = params.get("permissions").cloned().unwrap_or(Value::Null);
            ServerRequestOutcome::Approval {
                request_id: request_id.clone(),
                event: ProviderEvent::ApprovalRequested {
                    request_id,
                    kind: ApprovalKind::Permissions,
                    summary: format!("grant additional permissions: {}", snippet(&perms, 200)),
                    reason: s(params, "reason").map(String::from),
                    paths: params
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(|c| vec![c.to_string()])
                        .unwrap_or_default(),
                    details: json!({ "permissions": perms, "itemId": s(params, "itemId") }),
                },
                pending: PendingApproval {
                    rpc_id: id.clone(),
                    kind: ApprovalKind::Permissions,
                    requested_permissions: Some(perms),
                },
            }
        }
        "mcpServer/elicitation/request" => translate_elicitation(id, params),
        other => ServerRequestOutcome::Reject {
            code: -32601,
            message: format!("MAW does not support server request '{other}'"),
        },
    }
}

/// Build the JSON-RPC `result` that answers an approval request.
pub fn approval_response(p: &PendingApproval, d: ApprovalDecision) -> Value {
    match p.kind {
        // `Other` is only produced for MCP tool-call elicitations (`mcpServer/elicitation/request`).
        // `_meta.persist = "session"` remembers the approval for the session (verified live);
        // MAW never sends "always" because that would persist the grant in the user's Codex config.
        ApprovalKind::Other => match d {
            ApprovalDecision::Approve => json!({ "action": "accept" }),
            ApprovalDecision::ApproveForSession => {
                json!({ "action": "accept", "_meta": { "persist": "session" } })
            }
            ApprovalDecision::Deny => json!({ "action": "decline" }),
            ApprovalDecision::Abort => json!({ "action": "cancel" }),
        },
        ApprovalKind::Permissions => match d {
            ApprovalDecision::Approve | ApprovalDecision::ApproveForSession => json!({
                "permissions": p.requested_permissions.clone().unwrap_or_else(|| json!({})),
                "scope": if d == ApprovalDecision::ApproveForSession { "session" } else { "turn" },
            }),
            ApprovalDecision::Deny | ApprovalDecision::Abort => {
                json!({ "permissions": {}, "scope": "turn" })
            }
        },
        _ => json!({
            "decision": match d {
                ApprovalDecision::Approve => "accept",
                ApprovalDecision::ApproveForSession => "acceptForSession",
                ApprovalDecision::Deny => "decline",
                ApprovalDecision::Abort => "cancel",
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_mapping_never_uses_full_access() {
        for p in [
            PermissionProfile::Safe,
            PermissionProfile::Balanced,
            PermissionProfile::Autonomous,
        ] {
            assert_ne!(sandbox_mode(p), "danger-full-access");
        }
        assert_eq!(sandbox_mode(PermissionProfile::Safe), "read-only");
        assert_eq!(approval_policy(PermissionProfile::Balanced), "on-request");
    }

    #[test]
    fn mcp_config_never_embeds_the_token() {
        let ep = McpEndpoint {
            url: "http://127.0.0.1:1/mcp".into(),
            token: "maw_tok_secret".into(),
        };
        let c = mcp_config(&ep, true);
        let text = c.to_string();
        assert!(!text.contains("maw_tok_secret"));
        assert_eq!(
            c["mcp_servers"]["maw"]["bearer_token_env_var"],
            MCP_TOKEN_ENV
        );
    }

    #[test]
    fn maw_server_is_preapproved_only_when_asked_and_only_for_maw() {
        let ep = McpEndpoint {
            url: "http://127.0.0.1:1/mcp".into(),
            token: "t".into(),
        };
        let on = mcp_config(&ep, true);
        assert_eq!(
            on["mcp_servers"]["maw"]["default_tools_approval_mode"],
            "approve"
        );
        assert_eq!(
            on["mcp_servers"].as_object().map(|m| m.len()),
            Some(1),
            "no other server is touched"
        );
        assert!(mcp_config(&ep, false)["mcp_servers"]["maw"]
            .get("default_tools_approval_mode")
            .is_none());
    }

    /// Verbatim shape of the request codex 0.166.0 sent for a non-read-only MCP tool.
    fn tool_call_elicitation() -> Value {
        json!({
            "threadId": "t", "turnId": "u", "serverName": "maw", "mode": "form",
            "_meta": { "codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"],
                       "tool_description": "Record a write", "tool_params": { "message": "hi" } },
            "message": "Allow the maw MCP server to run tool \"maw_probe_write\"?",
            "requestedSchema": { "type": "object", "properties": {} }
        })
    }

    #[test]
    fn mcp_tool_call_elicitation_becomes_an_approval_request() {
        match translate_server_request(
            &json!(0),
            "mcpServer/elicitation/request",
            &tool_call_elicitation(),
        ) {
            ServerRequestOutcome::Approval {
                request_id,
                event,
                pending,
            } => {
                assert_eq!(request_id, "0");
                assert_eq!(pending.kind, ApprovalKind::Other);
                match event {
                    ProviderEvent::ApprovalRequested {
                        summary,
                        kind,
                        details,
                        reason,
                        ..
                    } => {
                        assert_eq!(kind, ApprovalKind::Other);
                        assert_eq!(summary, "call tool maw/maw_probe_write");
                        assert_eq!(reason.as_deref(), Some("Record a write"));
                        assert_eq!(details["arguments"]["message"], "hi");
                        assert_eq!(details["server"], "maw");
                    }
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("expected approval, got {other:?}"),
        }
    }

    #[test]
    fn elicitation_answers_use_the_mcp_action_shape() {
        let p = PendingApproval {
            rpc_id: json!(0),
            kind: ApprovalKind::Other,
            requested_permissions: None,
        };
        assert_eq!(
            approval_response(&p, ApprovalDecision::Approve),
            json!({ "action": "accept" })
        );
        assert_eq!(
            approval_response(&p, ApprovalDecision::ApproveForSession),
            json!({ "action": "accept", "_meta": { "persist": "session" } })
        );
        assert_eq!(
            approval_response(&p, ApprovalDecision::Deny),
            json!({ "action": "decline" })
        );
        assert_eq!(
            approval_response(&p, ApprovalDecision::Abort),
            json!({ "action": "cancel" })
        );
        // never persists a permanent grant
        for d in [
            ApprovalDecision::Approve,
            ApprovalDecision::ApproveForSession,
        ] {
            assert!(!approval_response(&p, d).to_string().contains("always"));
        }
    }

    #[test]
    fn real_form_and_url_elicitations_are_declined_with_a_notice() {
        let form = json!({ "threadId": "t", "serverName": "other", "mode": "form", "message": "Enter your API key",
            "requestedSchema": { "type": "object", "properties": { "key": { "type": "string" } } } });
        let url = json!({ "threadId": "t", "serverName": "other", "mode": "url", "message": "Sign in", "url": "https://x", "elicitationId": "e" });
        for p in [form, url] {
            match translate_server_request(&json!(3), "mcpServer/elicitation/request", &p) {
                ServerRequestOutcome::Respond { result, notice } => {
                    assert_eq!(result["action"], "decline");
                    assert!(
                        notice.contains("'other'") && notice.contains("declined"),
                        "{notice}"
                    );
                }
                other => panic!("expected decline, got {other:?}"),
            }
        }
    }

    #[test]
    fn tool_name_parsing_tolerates_odd_messages() {
        assert_eq!(
            tool_name_from_message("Allow the x MCP server to run tool \"a_b\"?"),
            Some("a_b")
        );
        assert_eq!(tool_name_from_message("no quotes"), None);
        assert_eq!(tool_name_from_message("one \" only"), None);
    }

    #[test]
    fn approval_decisions_map_to_codex_values() {
        let cmd = PendingApproval {
            rpc_id: json!(7),
            kind: ApprovalKind::Command,
            requested_permissions: None,
        };
        assert_eq!(
            approval_response(&cmd, ApprovalDecision::Approve)["decision"],
            "accept"
        );
        assert_eq!(
            approval_response(&cmd, ApprovalDecision::ApproveForSession)["decision"],
            "acceptForSession"
        );
        assert_eq!(
            approval_response(&cmd, ApprovalDecision::Deny)["decision"],
            "decline"
        );
        assert_eq!(
            approval_response(&cmd, ApprovalDecision::Abort)["decision"],
            "cancel"
        );
        let perm = PendingApproval {
            rpc_id: json!("a"),
            kind: ApprovalKind::Permissions,
            requested_permissions: Some(json!({ "network": { "enabled": true } })),
        };
        assert_eq!(
            approval_response(&perm, ApprovalDecision::Approve)["scope"],
            "turn"
        );
        assert_eq!(
            approval_response(&perm, ApprovalDecision::ApproveForSession)["scope"],
            "session"
        );
        assert_eq!(
            approval_response(&perm, ApprovalDecision::Deny)["permissions"],
            json!({})
        );
    }

    #[test]
    fn unknown_server_requests_are_rejected_not_ignored() {
        match translate_server_request(&json!(5), "item/tool/requestUserInput", &json!({})) {
            ServerRequestOutcome::Reject { code, .. } => assert_eq!(code, -32601),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_events_only_when_reached() {
        assert!(translate_notification(
            "account/rateLimits/updated",
            &json!({ "rateLimits": { "primary": { "usedPercent": 40 } } })
        )
        .is_empty());
        let ev = translate_notification(
            "account/rateLimits/updated",
            &json!({ "rateLimits": { "rateLimitReachedType": "rate_limit_reached" } }),
        );
        assert!(matches!(ev[0], ProviderEvent::RateLimited { .. }));
    }
}

#[cfg(test)]
mod effort_tests {
    use super::*;

    #[test]
    fn turn_start_carries_effort_only_when_set() {
        let p = turn_start_params("thr", "hi", Some("high"));
        assert_eq!(p["effort"], "high");
        assert!(turn_start_params("thr", "hi", None).get("effort").is_none());
        assert!(turn_start_params("thr", "hi", Some(""))
            .get("effort")
            .is_none());
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_summaries_become_reasoning_events() {
        let ev = translate_notification(
            "item/reasoning/summaryTextDelta",
            &json!({"delta":"Checking the repo","itemId":"r1","summaryIndex":0}),
        );
        assert!(
            matches!(ev.as_slice(), [ProviderEvent::Reasoning { text }] if text == "Checking the repo")
        );
        let ev = translate_notification(
            "item/reasoning/textDelta",
            &json!({"delta":"raw","itemId":"r1"}),
        );
        assert!(matches!(ev.as_slice(), [ProviderEvent::Reasoning { text }] if text == "raw"));
        let ev = translate_notification(
            "item/reasoning/summaryPartAdded",
            &json!({"itemId":"r1","summaryIndex":1}),
        );
        assert!(
            matches!(ev.as_slice(), [ProviderEvent::Reasoning { text }] if text == "

")
        );
        // malformed: no delta
        assert!(translate_notification("item/reasoning/summaryTextDelta", &json!({})).is_empty());
    }

    #[test]
    fn completed_reasoning_items_use_readable_text_and_ignore_empty_ones() {
        let done = |item: serde_json::Value| {
            translate_notification("item/completed", &json!({ "item": item }))
        };
        let ev = done(
            json!({"type":"reasoning","id":"r1","summary":[{"text":"Look at the tests."},"Then edit."],"content":[]}),
        );
        assert!(
            matches!(ev.as_slice(), [ProviderEvent::Reasoning { text }] if text == "Look at the tests.\n\nThen edit."),
            "{ev:?}"
        );
        let ev = done(json!({"type":"reasoning","id":"r2","summary":[],"content":["raw thought"]}));
        assert!(
            matches!(ev.as_slice(), [ProviderEvent::Reasoning { text }] if text == "raw thought")
        );
        // what the live probe showed for the tested models: nothing readable
        assert!(done(json!({"type":"reasoning","id":"r3","summary":[],"content":[]})).is_empty());
    }

    #[test]
    fn allowed_folders_become_writable_roots_of_the_workspace_sandbox_only() {
        let mk = |p: PermissionProfile, dirs: Vec<std::path::PathBuf>| SessionConfig {
            agent_id: maw_core::ids::AgentId::from("agt_x"),
            name: "c".into(),
            role: maw_core::agent::Role::Worker,
            cwd: std::path::PathBuf::from("/work/wt"),
            model: None,
            effort: None,
            permission: p,
            mcp: None,
            instructions: String::new(),
            env: Default::default(),
            resume_native_id: None,
            extra_dirs: dirs,
        };
        let dirs = vec![std::path::PathBuf::from("/mnt/c/Users/me/download")];
        let v = thread_params(
            &mk(PermissionProfile::Balanced, dirs.clone()),
            false,
            false,
            None,
        );
        assert_eq!(
            v["config"]["sandbox_workspace_write"]["writable_roots"],
            json!(["/mnt/c/Users/me/download"])
        );
        assert_eq!(v["sandbox"], "workspace-write", "still not full access");
        // read-only writes nowhere; no folders, no setting
        assert!(
            thread_params(&mk(PermissionProfile::Safe, dirs), false, false, None)
                .get("config")
                .is_none()
        );
        assert!(
            thread_params(&mk(PermissionProfile::Balanced, vec![]), false, false, None)
                .get("config")
                .is_none()
        );
    }
}
