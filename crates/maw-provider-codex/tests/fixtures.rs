//! Contract tests over sanitized captures from a REAL `codex app-server` (see fixtures/codex/README.md).
//! They pin the tested Codex version: if the wire format drifts, these fail with an explicit message.

use maw_core::provider::*;
use maw_provider_codex::protocol::{
    translate_notification, translate_server_request, ServerRequestOutcome,
};
use serde_json::Value;
use std::path::PathBuf;

const PINNED_VERSION: &str = "0.166.0";

fn load(name: &str) -> Vec<Value> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "fixtures",
        "codex",
        name,
    ]
    .iter()
    .collect();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()))
        .lines()
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("fixture line is not JSON ({e}): {l}"))
        })
        .collect()
}

/// Run a capture through the adapter's translation exactly like the reader loop does.
fn translate_all(lines: &[Value]) -> Vec<ProviderEvent> {
    let mut out = vec![];
    for m in lines {
        if let Some(method) = m.get("method").and_then(Value::as_str) {
            let params = m.get("params").cloned().unwrap_or(Value::Null);
            if let Some(id) = m.get("id") {
                // a server request must never be silently dropped
                assert!(matches!(
                    translate_server_request(id, method, &params),
                    ServerRequestOutcome::Approval { .. } | ServerRequestOutcome::Reject { .. }
                ));
            } else {
                out.extend(translate_notification(method, &params));
            }
        }
    }
    out
}

#[test]
fn fixtures_were_captured_from_the_pinned_codex_version() {
    for f in ["turn_ok_0.166.0.jsonl", "mcp_tool_call_0.166.0.jsonl"] {
        let lines = load(f);
        let ua = lines[0]
            .pointer("/result/userAgent")
            .and_then(Value::as_str)
            .expect("first line is the initialize response");
        assert!(
            ua.contains(PINNED_VERSION),
            "fixture {f} was captured with {ua}; re-capture and update PINNED_VERSION deliberately"
        );
        assert!(f.contains(PINNED_VERSION));
    }
}

#[test]
fn real_plain_turn_maps_to_start_deltas_message_usage_completion() {
    let ev = translate_all(&load("turn_ok_0.166.0.jsonl"));
    assert!(
        matches!(ev.first(), Some(ProviderEvent::TurnStarted { .. })),
        "{ev:?}"
    );
    let text: String = ev
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::MessageDelta { text } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(text, "OK");
    assert!(ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "OK")));
    let usage = ev
        .iter()
        .find_map(|e| {
            if let ProviderEvent::Usage(u) = e {
                Some(u)
            } else {
                None
            }
        })
        .expect("token usage");
    assert!(usage.reported_by_provider && usage.total_tokens.unwrap_or(0) > 0);
    assert!(matches!(
        ev.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            error: None,
            ..
        })
    ));
    // The real server's rate-limit snapshot (no limit reached) must not become a RateLimited event.
    assert!(!ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::RateLimited { .. })));
    assert!(
        !ev.iter().any(|e| matches!(e, ProviderEvent::Error { .. })),
        "no spurious errors: {ev:?}"
    );
}

#[test]
fn real_mcp_session_maps_tool_items_and_reports_no_startup_failure() {
    let ev = translate_all(&load("mcp_tool_call_0.166.0.jsonl"));
    assert!(ev.iter().any(
        |e| matches!(e, ProviderEvent::ToolStarted { name, .. } if name == "maw/maw_goal_get")
    ));
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::ToolCompleted { name, ok: true, .. } if name == "maw/maw_goal_get")));
    assert!(ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "STANDIN-GOAL-7731")));
    assert!(
        !ev.iter().any(|e| matches!(e, ProviderEvent::Error { .. })),
        "the maw MCP server reached `ready`: {ev:?}"
    );
    assert!(matches!(
        ev.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
}

#[test]
fn every_real_notification_is_either_mapped_or_known_to_be_ignorable() {
    // If a new Codex version starts emitting something that carries state MAW cares about, add it here consciously.
    const IGNORABLE: &[&str] = &[
        "remoteControl/status/changed",
        "account/updated",
        "thread/started",
        "thread/status/changed",
        "thread/readState/changed",
        "mcpServer/startupStatus/updated",
        "turn/moderationMetadata",
        "item/started",
        "item/completed",
        "item/agentMessage/delta",
        "turn/started",
        "turn/completed",
        "thread/tokenUsage/updated",
        "account/rateLimits/updated",
    ];
    for f in ["turn_ok_0.166.0.jsonl", "mcp_tool_call_0.166.0.jsonl"] {
        for m in load(f) {
            if let (Some(method), None) = (m.get("method").and_then(Value::as_str), m.get("id")) {
                assert!(
                    IGNORABLE.contains(&method),
                    "unreviewed notification '{method}' in {f}"
                );
            }
        }
    }
}
