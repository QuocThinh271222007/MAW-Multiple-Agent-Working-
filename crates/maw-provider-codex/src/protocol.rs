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

/// Config override registering MAW's streamable-HTTP MCP server. The token itself is NOT
/// included: Codex reads it from [`MCP_TOKEN_ENV`] in its own environment.
pub fn mcp_config(endpoint: &McpEndpoint) -> Value {
    json!({
        "mcp_servers": {
            MCP_SERVER_NAME: {
                "url": endpoint.url,
                "bearer_token_env_var": MCP_TOKEN_ENV,
            }
        }
    })
}

/// Parameters shared by `thread/start` and `thread/resume`.
pub fn thread_params(cfg: &SessionConfig, include_mcp_config: bool) -> Value {
    let mut p = json!({
        "cwd": cfg.cwd.to_string_lossy(),
        "approvalPolicy": approval_policy(cfg.permission),
        "sandbox": sandbox_mode(cfg.permission),
        "developerInstructions": cfg.instructions,
    });
    if let Some(m) = &cfg.model {
        p["model"] = json!(m);
    }
    if include_mcp_config {
        if let Some(e) = &cfg.mcp {
            p["config"] = mcp_config(e);
        }
    }
    p
}

pub fn turn_start_params(thread_id: &str, text: &str) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{ "type": "text", "text": text, "text_elements": [] }],
    })
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
        other => ServerRequestOutcome::Reject {
            code: -32601,
            message: format!("MAW does not support server request '{other}'"),
        },
    }
}

/// Build the JSON-RPC `result` that answers an approval request.
pub fn approval_response(p: &PendingApproval, d: ApprovalDecision) -> Value {
    match p.kind {
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
        let c = mcp_config(&McpEndpoint {
            url: "http://127.0.0.1:1/mcp".into(),
            token: "maw_tok_secret".into(),
        });
        let text = c.to_string();
        assert!(!text.contains("maw_tok_secret"));
        assert_eq!(
            c["mcp_servers"]["maw"]["bearer_token_env_var"],
            MCP_TOKEN_ENV
        );
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
