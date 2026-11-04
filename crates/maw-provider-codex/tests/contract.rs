//! Contract tests: drive the real adapter against `maw-fake-codex`, which speaks the Codex
//! App Server protocol. These prove MAW's side of the contract; the live tests and the
//! recorded-fixture tests cover the real server.

use maw_core::provider::*;
use maw_core::{AgentId, PermissionProfile, Role};
use maw_provider_codex::{CodexConfig, CodexProvider, McpInjection};
use std::path::PathBuf;
use std::time::Duration;

const TOKEN: &str = "maw_tok_0123456789abcdef0123456789abcdef";

struct Fx {
    _dir: tempfile::TempDir,
    log: PathBuf,
    provider: CodexProvider,
}

fn fixture(injection: McpInjection) -> Fx {
    fixture_with(injection, true)
}

fn fixture_with(injection: McpInjection, preapprove: bool) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("received.log");
    let provider = CodexProvider::new(CodexConfig {
        binary: env!("CARGO_BIN_EXE_maw-fake-codex").to_string(),
        capture_path: None,
        request_timeout: Duration::from_secs(10),
        mcp_injection: injection,
        preapprove_maw_tools: preapprove,
        reasoning_summary: Some("auto".into()),
    });
    Fx {
        _dir: dir,
        log,
        provider,
    }
}

fn session_cfg(fx: &Fx) -> SessionConfig {
    let mut env = std::collections::HashMap::new();
    env.insert(
        "FAKE_CODEX_LOG".to_string(),
        fx.log.to_string_lossy().to_string(),
    );
    SessionConfig {
        agent_id: AgentId::from("agt_test"),
        name: "codex-1".into(),
        role: Role::Worker,
        cwd: std::env::temp_dir(),
        model: Some("fake-model".into()),
        permission: PermissionProfile::Balanced,
        mcp: Some(McpEndpoint {
            url: "http://127.0.0.1:4321/mcp".into(),
            token: TOKEN.into(),
        }),
        instructions: "You are a test worker.".into(),
        env,
        resume_native_id: None,
        extra_dirs: vec![],
        effort: None,
    }
}

async fn start(fx: &Fx) -> (SessionHandle, EventReceiver) {
    fx.provider
        .start_session(session_cfg(fx))
        .await
        .expect("session starts")
}

/// Collect events until `stop` matches (inclusive) or the timeout hits.
async fn collect(
    rx: &mut EventReceiver,
    stop: impl Fn(&ProviderEvent) -> bool,
) -> Vec<ProviderEvent> {
    let mut out = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ev)) => {
                let done = stop(&ev);
                out.push(ev);
                if done {
                    return out;
                }
            }
            Ok(None) => panic!("event stream closed early; got {out:?}"),
            Err(_) => panic!("timed out waiting for events; got {out:?}"),
        }
    }
}

fn is_turn_done(e: &ProviderEvent) -> bool {
    matches!(e, ProviderEvent::TurnCompleted { .. })
}

fn log_text(fx: &Fx) -> String {
    std::fs::read_to_string(&fx.log).unwrap_or_default()
}

#[tokio::test]
async fn handshake_sends_the_expected_requests_and_never_leaks_the_token() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    let first = rx.recv().await.unwrap();
    assert!(
        matches!(first, ProviderEvent::Connected { native_session_id: Some(ref id) } if id == "thr_fake"),
        "{first:?}"
    );
    assert_eq!(session.native_session_id().as_deref(), Some("thr_fake"));

    let log = log_text(&fx);
    let lines: Vec<serde_json::Value> = log
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let init = lines
        .iter()
        .find(|l| l["method"] == "initialize")
        .expect("initialize sent");
    assert_eq!(init["params"]["clientInfo"]["name"], "maw");
    assert!(
        lines
            .iter()
            .any(|l| l["method"] == "initialized" && l.get("id").is_none()),
        "initialized is a notification"
    );
    let start = lines
        .iter()
        .find(|l| l["method"] == "thread/start")
        .expect("thread/start sent");
    let p = &start["params"];
    assert_eq!(p["approvalPolicy"], "on-request");
    assert_eq!(p["sandbox"], "workspace-write");
    assert_eq!(p["developerInstructions"], "You are a test worker.");
    assert_eq!(p["model"], "fake-model");
    assert_eq!(
        p["config"]["mcp_servers"]["maw"]["url"],
        "http://127.0.0.1:4321/mcp"
    );
    assert_eq!(
        p["config"]["mcp_servers"]["maw"]["bearer_token_env_var"],
        "MAW_MCP_TOKEN"
    );
    assert_eq!(
        p["config"]["mcp_servers"]["maw"]["default_tools_approval_mode"], "approve",
        "MAW's own tools must not prompt"
    );
    // the token is passed through the child's environment only
    assert!(log.contains("env:MAW_MCP_TOKEN=present"));
    assert!(
        !log.contains(TOKEN),
        "bearer token must not appear in anything sent to codex"
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_line_injection_mode_puts_only_non_secrets_in_argv() {
    let fx = fixture(McpInjection::CommandLine);
    let (session, _rx) = start(&fx).await;
    let log = log_text(&fx);
    let argv = log.lines().find(|l| l.starts_with("argv:")).unwrap();
    assert!(
        argv.contains("mcp_servers.maw.url=\"http://127.0.0.1:4321/mcp\""),
        "{argv}"
    );
    assert!(argv.contains("mcp_servers.maw.bearer_token_env_var=\"MAW_MCP_TOKEN\""));
    assert!(
        argv.contains("mcp_servers.maw.default_tools_approval_mode=\"approve\""),
        "{argv}"
    );
    assert!(!log.contains(TOKEN));
    // in this mode thread/start must not repeat the config
    let start: serde_json::Value = log
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .find(|l: &serde_json::Value| l["method"] == "thread/start")
        .unwrap();
    assert!(start["params"].get("config").is_none());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn permission_profiles_map_to_sandbox_and_approval_settings() {
    for (profile, approval, sandbox) in [
        (PermissionProfile::Safe, "untrusted", "read-only"),
        (PermissionProfile::Balanced, "on-request", "workspace-write"),
        (PermissionProfile::Autonomous, "never", "workspace-write"),
    ] {
        let fx = fixture(McpInjection::ThreadConfig);
        let mut cfg = session_cfg(&fx);
        cfg.permission = profile;
        let (session, _rx) = fx.provider.start_session(cfg).await.unwrap();
        let start: serde_json::Value = log_text(&fx)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|l: &serde_json::Value| l["method"] == "thread/start")
            .unwrap();
        assert_eq!(start["params"]["approvalPolicy"], approval);
        assert_eq!(start["params"]["sandbox"], sandbox);
        session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn simple_turn_maps_to_normalized_events() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session
        .send_input(AgentInput::text("OK please"))
        .await
        .unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::TurnStarted { turn_id } if turn_id == "turn_1")));
    let deltas: String = ev
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::MessageDelta { text } = e {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(deltas, "OK", "deltas of other threads must be ignored");
    assert!(ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "OK")));
    let usage = ev
        .iter()
        .find_map(|e| {
            if let ProviderEvent::Usage(u) = e {
                Some(u.clone())
            } else {
                None
            }
        })
        .expect("usage event");
    assert!(usage.reported_by_provider);
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (Some(120), Some(8), Some(128))
    );
    assert!(
        !ev.iter()
            .any(|e| matches!(e, ProviderEvent::RateLimited { .. })),
        "no rate-limit event unless a limit was reached"
    );
    assert!(matches!(
        ev.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            error: None,
            ..
        })
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_and_mcp_tool_items_are_mapped() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session.send_input(AgentInput::text("CMD")).await.unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::CommandStarted { command, cwd: Some(c), .. } if command == "echo hi" && c == "/work")));
    assert!(ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::CommandOutput { chunk, .. } if chunk == "hi\n")));
    assert!(ev.iter().any(|e| matches!(
        e,
        ProviderEvent::CommandCompleted {
            exit_code: Some(0),
            ..
        }
    )));
    assert!(ev.iter().any(
        |e| matches!(e, ProviderEvent::ToolStarted { name, .. } if name == "maw/maw_goal_get")
    ));
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::ToolCompleted { name, ok: true, .. } if name == "maw/maw_goal_get")));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn file_changes_and_failed_turns_are_reported() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session.send_input(AgentInput::text("FILE")).await.unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::FileChanged { paths } if paths == &vec!["src/a.rs".to_string()])));
    assert!(
        matches!(ev.last(), Some(ProviderEvent::TurnCompleted { outcome: TurnOutcome::Failed, error: Some(m), .. }) if m == "model overloaded")
    );
    session.shutdown().await.unwrap();
}

async fn approval_round_trip(decision: ApprovalDecision, expected: &str) {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session
        .send_input(AgentInput::text("APPROVE"))
        .await
        .unwrap();
    let ev = collect(&mut rx, |e| {
        matches!(e, ProviderEvent::ApprovalRequested { .. })
    })
    .await;
    let (request_id, summary, kind) = match ev.last() {
        Some(ProviderEvent::ApprovalRequested {
            request_id,
            summary,
            kind,
            reason,
            paths,
            ..
        }) => {
            assert_eq!(reason.as_deref(), Some("needs to delete build output"));
            assert_eq!(paths, &vec!["/work".to_string()]);
            (request_id.clone(), summary.clone(), *kind)
        }
        other => panic!("expected approval, got {other:?}"),
    };
    assert_eq!(request_id, "900");
    assert_eq!(kind, ApprovalKind::Command);
    assert!(summary.contains("rm -rf build"));
    session
        .resolve_approval(&request_id, decision)
        .await
        .unwrap();
    let rest = collect(&mut rx, is_turn_done).await;
    assert!(matches!(
        rest.last(),
        Some(ProviderEvent::TurnCompleted { .. })
    ));
    assert!(
        log_text(&fx).contains(&format!("approval:{expected}")),
        "log: {}",
        log_text(&fx)
    );
    // an approval can only be answered once
    assert!(session
        .resolve_approval(&request_id, decision)
        .await
        .is_err());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_accept_round_trip() {
    approval_round_trip(ApprovalDecision::Approve, "accept").await;
}

#[tokio::test]
async fn approval_for_session_and_deny_and_abort_map_correctly() {
    approval_round_trip(ApprovalDecision::ApproveForSession, "acceptForSession").await;
    approval_round_trip(ApprovalDecision::Deny, "decline").await;
    approval_round_trip(ApprovalDecision::Abort, "cancel").await;
}

#[tokio::test]
async fn unknown_server_requests_get_an_error_response_instead_of_hanging() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session
        .send_input(AgentInput::text("UNKNOWNREQ"))
        .await
        .unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(matches!(
        ev.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    assert!(log_text(&fx).contains("unknown_request_answered_with_error:true"));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_ends_the_active_turn_and_second_input_is_refused() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session.send_input(AgentInput::text("HANG")).await.unwrap();
    collect(&mut rx, |e| matches!(e, ProviderEvent::TurnStarted { .. })).await;
    // one turn at a time
    assert!(session.send_input(AgentInput::text("OK")).await.is_err());
    session.interrupt().await.unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(matches!(
        ev.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Interrupted,
            ..
        })
    ));
    // a new turn can start afterwards
    session.send_input(AgentInput::text("OK")).await.unwrap();
    collect(&mut rx, is_turn_done).await;
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn steering_an_active_turn_uses_turn_steer() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session.send_input(AgentInput::text("HANG")).await.unwrap();
    collect(&mut rx, |e| matches!(e, ProviderEvent::TurnStarted { .. })).await;
    session
        .send_input(AgentInput {
            text: "also do X".into(),
            steer_if_active: true,
        })
        .await
        .unwrap();
    assert!(log_text(&fx).contains("\"turn/steer\""));
    assert!(log_text(&fx).contains("\"expectedTurnId\":\"turn_1\""));
    session.interrupt().await.unwrap();
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn process_exit_becomes_disconnected_with_diagnostics() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session.send_input(AgentInput::text("EXIT")).await.unwrap();
    let ev = collect(&mut rx, |e| matches!(e, ProviderEvent::Disconnected { .. })).await;
    match ev.last() {
        Some(ProviderEvent::Disconnected { reason }) => {
            assert!(reason.contains("exited"), "{reason}");
            assert!(
                reason.contains("simulated crash"),
                "stderr tail should be included: {reason}"
            );
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    // the session is unusable afterwards, and says so
    assert!(session.send_input(AgentInput::text("OK")).await.is_err());
}

#[tokio::test]
async fn malformed_lines_are_tolerated() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session
        .send_input(AgentInput::text("GARBAGE"))
        .await
        .unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "survived")));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_resume_falls_back_to_a_fresh_thread_with_a_warning() {
    let fx = fixture(McpInjection::ThreadConfig);
    let mut cfg = session_cfg(&fx);
    cfg.resume_native_id = Some("thr_missing".into());
    let (session, mut rx) = fx.provider.start_session(cfg).await.unwrap();
    let ev = collect(&mut rx, |e| matches!(e, ProviderEvent::Connected { .. })).await;
    assert!(ev.iter().any(|e| matches!(e, ProviderEvent::Error { fatal: false, message } if message.contains("could not resume"))), "{ev:?}");
    assert_eq!(session.native_session_id().as_deref(), Some("thr_fake"));
    session.shutdown().await.unwrap();

    // a resume that works keeps the requested thread
    let mut cfg = session_cfg(&fx);
    cfg.resume_native_id = Some("thr_abc".into());
    let (session, _rx) = fx.provider.start_session(cfg).await.unwrap();
    assert_eq!(session.native_session_id().as_deref(), Some("thr_abc"));
    assert!(log_text(&fx).contains("\"thread/resume\""));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_reaps_the_process_and_reports_disconnect() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session.shutdown().await.unwrap();
    let ev = collect(&mut rx, |e| matches!(e, ProviderEvent::Disconnected { .. })).await;
    assert!(
        matches!(ev.last(), Some(ProviderEvent::Disconnected { reason }) if reason == "shutdown")
    );
}

#[tokio::test]
async fn missing_binary_is_reported_cleanly() {
    let provider = CodexProvider::new(CodexConfig {
        binary: "/definitely/not/codex".into(),
        ..CodexConfig::default()
    });
    let info = provider.detect().await.unwrap();
    assert!(!info.installed);
    let err = provider
        .start_session(SessionConfig {
            agent_id: AgentId::from("a"),
            name: "x".into(),
            role: Role::Worker,
            cwd: std::env::temp_dir(),
            model: None,
            permission: PermissionProfile::Safe,
            mcp: None,
            instructions: String::new(),
            env: Default::default(),
            resume_native_id: None,
            extra_dirs: vec![],
            effort: None,
        })
        .await
        .err()
        .expect("must fail");
    assert!(matches!(err, ProviderError::NotInstalled(_)), "{err}");
}

#[tokio::test]
async fn capabilities_are_truthful() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, _rx) = start(&fx).await;
    let c = session.capabilities();
    assert!(
        c.interrupt && c.resume && c.approvals && c.mcp_http && c.usage_reporting && c.streaming
    );
    session.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------------------------
// MCP tool-call approvals (`mcpServer/elicitation/request`)
// ---------------------------------------------------------------------------------------------

async fn elicitation_round_trip(
    decision: ApprovalDecision,
    expect_log: &str,
    expect_tool_ok: bool,
) {
    let fx = fixture_with(McpInjection::ThreadConfig, false);
    let (session, mut rx) = start(&fx).await;
    session
        .send_input(AgentInput::text("ELICIT"))
        .await
        .unwrap();
    let ev = collect(&mut rx, |e| {
        matches!(e, ProviderEvent::ApprovalRequested { .. })
    })
    .await;
    let (request_id, summary, kind, details) = match ev.last() {
        Some(ProviderEvent::ApprovalRequested {
            request_id,
            summary,
            kind,
            details,
            reason,
            paths,
        }) => {
            assert_eq!(reason.as_deref(), Some("Submit a task result"));
            assert!(paths.is_empty());
            (request_id.clone(), summary.clone(), *kind, details.clone())
        }
        other => panic!("expected approval, got {other:?}"),
    };
    assert_eq!(request_id, "910");
    assert_eq!(kind, ApprovalKind::Other);
    assert_eq!(summary, "call tool maw/maw_task_submit_result");
    assert_eq!(
        details["arguments"]["task"], "T-1",
        "the user must see what will be called"
    );
    session
        .resolve_approval(&request_id, decision)
        .await
        .unwrap();
    let rest = collect(&mut rx, is_turn_done).await;
    assert!(
        rest.iter().any(|e| matches!(e, ProviderEvent::ToolCompleted { ok, name, .. } if *ok == expect_tool_ok && name == "maw/maw_task_submit_result")),
        "{rest:?}"
    );
    assert!(log_text(&fx).contains(expect_log), "log: {}", log_text(&fx));
    // answered exactly once
    assert!(session
        .resolve_approval(&request_id, decision)
        .await
        .is_err());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_tool_call_elicitation_is_surfaced_and_accepted() {
    elicitation_round_trip(
        ApprovalDecision::Approve,
        "elicit:accept:persist=none",
        true,
    )
    .await;
}

#[tokio::test]
async fn mcp_tool_call_elicitation_session_approval_sets_persist_session_never_always() {
    elicitation_round_trip(
        ApprovalDecision::ApproveForSession,
        "elicit:accept:persist=session",
        true,
    )
    .await;
}

#[tokio::test]
async fn mcp_tool_call_elicitation_deny_and_abort_map_to_decline_and_cancel() {
    elicitation_round_trip(ApprovalDecision::Deny, "elicit:decline:persist=none", false).await;
    elicitation_round_trip(ApprovalDecision::Abort, "elicit:cancel:persist=none", false).await;
}

#[tokio::test]
async fn form_elicitations_are_declined_with_a_clear_event_instead_of_hanging() {
    let fx = fixture(McpInjection::ThreadConfig);
    let (session, mut rx) = start(&fx).await;
    session
        .send_input(AgentInput::text("FORMELICIT"))
        .await
        .unwrap();
    let ev = collect(&mut rx, is_turn_done).await;
    assert!(
        ev.iter().any(|e| matches!(e, ProviderEvent::Error { fatal: false, message } if message.contains("other-server") && message.contains("declined"))),
        "{ev:?}"
    );
    assert!(!ev
        .iter()
        .any(|e| matches!(e, ProviderEvent::ApprovalRequested { .. })));
    assert!(log_text(&fx).contains("form_elicit_answer:decline"));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn preapproval_can_be_disabled_and_is_then_absent_from_the_config() {
    let fx = fixture_with(McpInjection::ThreadConfig, false);
    let (session, _rx) = start(&fx).await;
    let start_req: serde_json::Value = log_text(&fx)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .find(|l: &serde_json::Value| l["method"] == "thread/start")
        .unwrap();
    let maw = &start_req["params"]["config"]["mcp_servers"]["maw"];
    assert!(maw.get("default_tools_approval_mode").is_none());
    assert!(maw["url"].is_string());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_start_asks_codex_for_reasoning_summaries_without_leaking_the_token() {
    // The only way Codex streams readable reasoning is model_reasoning_summary in the thread config.
    let fx = fixture_with(McpInjection::ThreadConfig, true);
    let cfg = session_cfg(&fx);
    let (h, _rx) = fx.provider.start_session(cfg).await.expect("start");
    let log = std::fs::read_to_string(&fx.log).unwrap_or_default();
    assert!(
        log.contains("\"model_reasoning_summary\":\"auto\""),
        "thread/start config carries the summary setting:
{log}"
    );
    assert!(!log.contains("maw_tok_"), "no token in the log");
    let _ = h.shutdown().await;
}
