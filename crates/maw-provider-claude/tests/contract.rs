//! Contract tests: sanitized REAL captures (fixtures/claude/*.jsonl) replayed through the mapper,
//! and through the full process layer via a fake `claude` that replays the same captures.
//! The fake is a test double; real-CLI behaviour is covered by the opt-in live test.

use maw_core::agent::{PermissionProfile, Role};
use maw_core::ids::AgentId;
use maw_core::provider::*;
use maw_provider_claude::mapper::{Flags, Mapper, Out};
use maw_provider_claude::ClaudeProvider;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude")
}

fn read_fixture(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixtures_dir().join(name))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line is JSON"))
        .collect()
}

fn map_all(name: &str) -> (Vec<ProviderEvent>, Vec<Out>) {
    let mut m = Mapper::new(
        "00000000-0000-4000-8000-000000000000".into(),
        Arc::new(Flags::default()),
    );
    let mut events = vec![];
    let mut others = vec![];
    for line in read_fixture(name) {
        for o in m.map_line(&line) {
            match o {
                Out::Event(e) => events.push(e),
                other => others.push(other),
            }
        }
    }
    (events, others)
}

// ---------------------------------------------------------------- mapper vs real captures

#[test]
fn two_turn_capture_maps_to_the_expected_event_sequence() {
    let (evs, _) = map_all("two_turns_mcp_tool.jsonl");
    let starts = evs
        .iter()
        .filter(|e| matches!(e, ProviderEvent::TurnStarted { .. }))
        .count();
    let completed: Vec<_> = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ProviderEvent::TurnCompleted {
                    outcome: TurnOutcome::Completed,
                    ..
                }
            )
        })
        .collect();
    assert_eq!((starts, completed.len()), (2, 2), "{evs:#?}");
    // streaming text and the final assistant message
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::MessageDelta { text } if text == "OK.")));
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "OK.")));
    // MCP tool call is visible with the full mcp__<server>__<tool> name and completes OK
    assert!(evs.iter().any(
        |e| matches!(e, ProviderEvent::ToolStarted { name, .. } if name == "mcp__maw__maw_goal_get")
    ));
    assert!(evs.iter().any(|e| matches!(e, ProviderEvent::ToolCompleted { name, ok: true, .. } if name == "mcp__maw__maw_goal_get")));
    // provider-reported usage, never estimated
    let usage: Vec<_> = evs
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::Usage(u) = e {
                Some(u)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(usage.len(), 2);
    assert!(usage
        .iter()
        .all(|u| u.reported_by_provider && u.cost_usd.is_some() && u.output_tokens.is_some()));
    // the MCP server connected in the capture, so no error is raised
    assert!(
        !evs.iter().any(|e| matches!(e, ProviderEvent::Error { .. })),
        "{evs:#?}"
    );
    // turn ids are unique
    let ids: Vec<_> = evs
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::TurnStarted { turn_id } = e {
                Some(turn_id.clone())
            } else {
                None
            }
        })
        .collect();
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn permission_capture_yields_a_command_approval_with_suggestions() {
    let (evs, others) = map_all("permission_request_denied.jsonl");
    let req = evs.iter().find_map(|e| {
        if let ProviderEvent::ApprovalRequested {
            request_id,
            kind,
            summary,
            paths,
            details,
            ..
        } = e
        {
            Some((request_id, kind, summary, paths, details))
        } else {
            None
        }
    });
    let (rid, kind, summary, paths, details) = req.expect("approval requested");
    assert_eq!(*kind, ApprovalKind::Command);
    assert!(summary.contains("touch maw_probe_file.txt"), "{summary}");
    assert!(paths.iter().any(|p| p.ends_with("maw_probe_file.txt")));
    assert_eq!(details["tool_name"], "Bash");
    // the pending approval carries the original input and the CLI's suggestions
    let pending = others
        .iter()
        .find_map(|o| {
            if let Out::Approval(p) = o {
                Some(p)
            } else {
                None
            }
        })
        .expect("pending approval registered");
    assert_eq!(&pending.request_id, rid);
    assert_eq!(pending.input["command"], "touch maw_probe_file.txt");
    assert!(pending
        .suggestions
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false));
    // the capture's turn still finishes normally after the (denied) tool
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::TurnCompleted { .. })));
}

#[test]
fn interrupt_capture_maps_to_interrupted_then_completed() {
    let (evs, _) = map_all("allow_interrupt_reuse.jsonl");
    let outcomes: Vec<_> = evs
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::TurnCompleted { outcome, .. } = e {
                Some(*outcome)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![
            TurnOutcome::Completed,
            TurnOutcome::Interrupted,
            TurnOutcome::Completed
        ]
    );
    // an interrupted turn carries no error text
    assert!(evs.iter().any(|e| matches!(
        e,
        ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Interrupted,
            error: None,
            ..
        }
    )));
    // the allowed Bash command shows up as a command lifecycle with exit code 0
    assert!(evs.iter().any(|e| matches!(e, ProviderEvent::CommandStarted { command, .. } if command.contains("touch allowed.txt"))));
    assert!(evs.iter().any(|e| matches!(
        e,
        ProviderEvent::CommandCompleted {
            exit_code: Some(0),
            ..
        }
    )));
}

// ---------------------------------------------------------------- full process layer (fake claude)

struct Fixture {
    provider: ClaudeProvider,
    dir: tempfile::TempDir,
    record: PathBuf,
}

/// The fake executable is written exactly once per test process. Writing a fresh executable per
/// test and exec'ing it while other tests fork races with `ETXTBSY` ("Text file busy").
fn shared_fake_binary() -> &'static Path {
    static BIN: std::sync::OnceLock<(tempfile::TempDir, PathBuf)> = std::sync::OnceLock::new();
    &BIN.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        // copy the script so it has a clean exec bit and LF endings regardless of the checkout
        let script = std::fs::read_to_string(fixtures_dir().join("fake_claude.py"))
            .expect("fake script")
            .replace("\r\n", "\n");
        let bin = dir.path().join("claude");
        std::fs::write(&bin, script).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        (dir, bin)
    })
    .1
}

fn fake(scenario: &str) -> (Fixture, SessionConfig) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = shared_fake_binary().to_path_buf();
    let record = dir.path().join("record.jsonl");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");
    let mut env = HashMap::new();
    env.insert("FAKE_CLAUDE_SCENARIO".to_string(), scenario.to_string());
    env.insert(
        "FAKE_CLAUDE_FIXTURES".to_string(),
        fixtures_dir().to_string_lossy().to_string(),
    );
    env.insert(
        "FAKE_CLAUDE_RECORD".to_string(),
        record.to_string_lossy().to_string(),
    );
    let cfg = SessionConfig {
        agent_id: AgentId::new(),
        name: "claude-test".into(),
        role: Role::Worker,
        cwd: work,
        model: Some("haiku".into()),
        permission: PermissionProfile::Balanced,
        mcp: Some(McpEndpoint {
            url: "http://127.0.0.1:9/mcp".into(),
            token: "maw_tok_abcdef0123456789".into(),
        }),
        instructions: "You are a test agent.".into(),
        env,
        resume_native_id: None,
        extra_dirs: vec![],
        effort: None,
    };
    (
        Fixture {
            provider: ClaudeProvider::with_binary(bin),
            dir,
            record,
        },
        cfg,
    )
}

fn recorded(f: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(&f.record)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

async fn next_event(rx: &mut EventReceiver) -> ProviderEvent {
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event stream closed")
}

/// Collect events until `pred` matches one (inclusive).
async fn until(
    rx: &mut EventReceiver,
    pred: impl Fn(&ProviderEvent) -> bool,
) -> Vec<ProviderEvent> {
    let mut out = vec![];
    loop {
        let e = next_event(rx).await;
        let stop = pred(&e);
        out.push(e);
        if stop {
            return out;
        }
    }
}

fn is_turn_done(e: &ProviderEvent) -> bool {
    matches!(e, ProviderEvent::TurnCompleted { .. })
}

#[tokio::test]
async fn handshake_two_turns_and_launch_arguments() {
    let (f, cfg) = fake("two_turns");
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    let native = session
        .native_session_id()
        .expect("session id known before the first event");
    assert!(
        matches!(next_event(&mut rx).await, ProviderEvent::Connected { native_session_id: Some(id) } if id == native)
    );
    assert!(
        session.capabilities().interrupt
            && session.capabilities().approvals
            && !session.capabilities().steer_active_turn
    );

    session
        .send_input(AgentInput::text("Reply with exactly: OK"))
        .await
        .expect("send 1");
    let t1 = until(&mut rx, is_turn_done).await;
    assert!(t1
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "OK.")));
    // the same process serves a second turn
    session
        .send_input(AgentInput::text("call the tool"))
        .await
        .expect("send 2");
    let t2 = until(&mut rx, is_turn_done).await;
    assert!(t2.iter().any(
        |e| matches!(e, ProviderEvent::ToolStarted { name, .. } if name == "mcp__maw__maw_goal_get")
    ));
    assert!(matches!(
        t2.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));

    // launch arguments: isolation flags present, secrets only in the environment
    let start = recorded(&f)
        .into_iter()
        .find(|r| r["kind"] == "start")
        .expect("start record");
    let argv: Vec<String> = start["argv"]
        .as_array()
        .expect("argv")
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert!(argv
        .windows(2)
        .any(|w| w[0] == "--session-id" && w[1] == native));
    assert!(argv
        .windows(2)
        .any(|w| w[0] == "--setting-sources" && w[1].is_empty()));
    assert!(argv
        .windows(2)
        .any(|w| w[0] == "--permission-prompt-tool" && w[1] == "stdio"));
    assert!(argv
        .windows(2)
        .any(|w| w[0] == "--model" && w[1] == "haiku"));
    assert!(argv
        .windows(2)
        .any(|w| w[0] == "--append-system-prompt" && w[1] == "You are a test agent."));
    assert!(
        argv.iter().all(|a| !a.contains("maw_tok_")),
        "token leaked into argv: {argv:?}"
    );
    assert!(argv.iter().all(|a| !a.contains("dangerously")));
    assert_eq!(start["token_env"], "maw_tok_abcdef0123456789");
    assert!(start["cwd"].as_str().expect("cwd").ends_with("work"));
    session.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_second_input_during_an_active_turn_is_rejected() {
    let (f, cfg) = fake("permission"); // the turn blocks waiting for our approval answer
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    session
        .send_input(AgentInput::text("go"))
        .await
        .expect("send");
    let err = session
        .send_input(AgentInput::text("again"))
        .await
        .expect_err("busy");
    assert!(err.to_string().contains("already active"), "{err}");
    // steering is queued rather than rejected, and capabilities say so honestly
    assert!(session
        .send_input(AgentInput {
            text: "later".into(),
            steer_if_active: true
        })
        .await
        .is_ok());
    let evs = until(&mut rx, |e| {
        matches!(e, ProviderEvent::ApprovalRequested { .. })
    })
    .await;
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::ApprovalRequested { .. })));
    session.shutdown().await.expect("shutdown");
}

async fn approval_response(decision: ApprovalDecision) -> (Value, Result<(), String>) {
    let (f, cfg) = fake("permission");
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    session
        .send_input(AgentInput::text("touch a file"))
        .await
        .expect("send");
    let evs = until(&mut rx, |e| {
        matches!(e, ProviderEvent::ApprovalRequested { .. })
    })
    .await;
    let rid = match evs.last() {
        Some(ProviderEvent::ApprovalRequested { request_id, .. }) => request_id.clone(),
        other => panic!("{other:?}"),
    };
    session
        .resolve_approval(&rid, decision)
        .await
        .expect("resolve");
    // the CLI continues and the turn finishes
    until(&mut rx, is_turn_done).await;
    // the same approval cannot be answered twice
    let second = session
        .resolve_approval(&rid, ApprovalDecision::Approve)
        .await
        .map_err(|e| e.to_string());
    let resp = recorded(&f)
        .into_iter()
        .filter(|r| r["kind"] == "stdin" && r["msg"]["type"] == "control_response")
        .map(|r| r["msg"].clone())
        .next_back()
        .expect("control_response written to the CLI");
    session.shutdown().await.expect("shutdown");
    drop(f.dir);
    (resp, second)
}

#[tokio::test]
async fn approve_echoes_the_original_input() {
    let (resp, second) = approval_response(ApprovalDecision::Approve).await;
    assert_eq!(resp["response"]["subtype"], "success");
    assert_eq!(resp["response"]["response"]["behavior"], "allow");
    assert_eq!(
        resp["response"]["response"]["updatedInput"]["command"],
        "touch maw_probe_file.txt"
    );
    assert!(resp["response"]["response"]
        .get("updatedPermissions")
        .is_none());
    assert!(second
        .expect_err("double answer rejected")
        .contains("no longer pending"));
}

#[tokio::test]
async fn approve_for_session_never_persists_to_settings() {
    let (resp, _) = approval_response(ApprovalDecision::ApproveForSession).await;
    let r = &resp["response"]["response"];
    assert_eq!(r["behavior"], "allow");
    let perms = r["updatedPermissions"]
        .as_array()
        .expect("updatedPermissions");
    assert!(!perms.is_empty());
    assert!(
        perms
            .iter()
            .all(|p| p["destination"] == "session" && p["type"] == "addRules"),
        "{perms:?}"
    );
    assert!(
        !resp.to_string().contains("localSettings"),
        "must not write to the user's settings files"
    );
}

#[tokio::test]
async fn deny_and_abort_are_denials() {
    let (resp, _) = approval_response(ApprovalDecision::Deny).await;
    assert_eq!(resp["response"]["response"]["behavior"], "deny");
    assert!(resp["response"]["response"].get("interrupt").is_none());
    let (resp, _) = approval_response(ApprovalDecision::Abort).await;
    assert_eq!(resp["response"]["response"]["behavior"], "deny");
    assert_eq!(resp["response"]["response"]["interrupt"], true);
}

#[tokio::test]
async fn interrupt_ends_the_turn_and_the_process_survives() {
    let (f, cfg) = fake("interrupt");
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    session
        .send_input(AgentInput::text("count to 300"))
        .await
        .expect("send");
    until(&mut rx, |e| matches!(e, ProviderEvent::MessageDelta { .. })).await;
    session.interrupt().await.expect("interrupt");
    let evs = until(&mut rx, is_turn_done).await;
    assert!(
        matches!(
            evs.last(),
            Some(ProviderEvent::TurnCompleted {
                outcome: TurnOutcome::Interrupted,
                ..
            })
        ),
        "{evs:#?}"
    );
    // the interrupt control request really reached the CLI
    assert!(recorded(&f)
        .iter()
        .any(|r| r["msg"]["request"]["subtype"] == "interrupt"));
    // same process, next turn works
    session
        .send_input(AgentInput::text("are you alive"))
        .await
        .expect("send 2");
    let evs = until(&mut rx, is_turn_done).await;
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    // interrupting while idle is a harmless no-op
    session.interrupt().await.expect("idle interrupt");
    session.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn process_exit_becomes_a_redacted_disconnect() {
    let (f, cfg) = fake("exit");
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    session
        .send_input(AgentInput::text("hello"))
        .await
        .expect("send");
    let evs = until(&mut rx, |e| matches!(e, ProviderEvent::Disconnected { .. })).await;
    match evs.last() {
        Some(ProviderEvent::Disconnected { reason }) => {
            assert!(reason.contains("exited"), "{reason}");
            assert!(
                reason.contains("something broke"),
                "stderr tail is surfaced: {reason}"
            );
            assert!(
                !reason.contains("SECRETSECRET"),
                "secrets must be redacted: {reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    // the session is unusable afterwards, with a clear error
    assert!(session.send_input(AgentInput::text("again")).await.is_err());
}

#[tokio::test]
async fn malformed_and_unknown_lines_never_break_the_stream() {
    let (f, cfg) = fake("malformed");
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    session
        .send_input(AgentInput::text("hi"))
        .await
        .expect("send");
    let evs = until(&mut rx, is_turn_done).await;
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "OK.")));
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    session.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn failed_resume_falls_back_to_a_fresh_session_without_a_bogus_failure() {
    let (f, mut cfg) = fake("resume_fail");
    let old = "11111111-2222-4333-8444-555555555555".to_string();
    cfg.resume_native_id = Some(old.clone());
    let (session, mut rx) = f.provider.start_session(cfg).await.expect("start");
    assert_eq!(session.native_session_id().as_deref(), Some(old.as_str()));
    assert!(
        matches!(next_event(&mut rx).await, ProviderEvent::Connected { native_session_id: Some(id) } if id == old)
    );
    session
        .send_input(AgentInput::text("continue the work"))
        .await
        .expect("send");
    let evs = until(&mut rx, is_turn_done).await;
    // no failed turn leaked; the user is told context was lost; a new id is announced
    assert!(
        !evs.iter().any(|e| matches!(
            e,
            ProviderEvent::TurnCompleted {
                outcome: TurnOutcome::Failed,
                ..
            }
        )),
        "{evs:#?}"
    );
    assert!(evs.iter().any(|e| matches!(e, ProviderEvent::Error { message, fatal: false } if message.contains("could not be resumed"))));
    let new_id = evs
        .iter()
        .find_map(|e| {
            if let ProviderEvent::Connected {
                native_session_id: Some(id),
            } = e
            {
                Some(id.clone())
            } else {
                None
            }
        })
        .expect("new session announced");
    assert_ne!(new_id, old);
    assert_eq!(
        session.native_session_id().as_deref(),
        Some(new_id.as_str())
    );
    assert!(
        matches!(
            evs.last(),
            Some(ProviderEvent::TurnCompleted {
                outcome: TurnOutcome::Completed,
                ..
            })
        ),
        "the original input was re-delivered and answered"
    );
    let starts: Vec<_> = recorded(&f)
        .into_iter()
        .filter(|r| r["kind"] == "start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert!(
        starts[0]["argv"].to_string().contains("--resume")
            && starts[1]["argv"].to_string().contains("--session-id")
    );
    session.shutdown().await.expect("shutdown");
}

fn pid_alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_terminates_a_process_that_ignores_stdin_close() {
    let (f, cfg) = fake("stay_alive");
    let (session, _rx) = f.provider.start_session(cfg).await.expect("start");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let pid = recorded(&f)
        .iter()
        .find_map(|r| r["pid"].as_u64())
        .expect("pid recorded");
    assert!(pid_alive(pid));
    let started = std::time::Instant::now();
    session.shutdown().await.expect("shutdown");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "shutdown must be bounded"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!pid_alive(pid), "no leaked subprocess");
}

#[tokio::test]
async fn missing_executable_is_a_clear_start_error() {
    let (_f, cfg) = fake("two_turns");
    let p = ClaudeProvider::with_binary("/no/such/claude");
    match p.start_session(cfg).await {
        Err(ProviderError::NotInstalled(m)) => assert!(m.contains("/no/such/claude")),
        Ok(_) => panic!("must not start"),
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test]
async fn detect_and_auth_status_use_the_cli_interface() {
    let (f, _) = fake("two_turns");
    let info = f.provider.detect().await.expect("detect");
    assert!(info.installed);
    assert_eq!(info.version.as_deref(), Some("2.1.309"));
    assert!(!info.notes.iter().any(|n| n.contains("differs")));
    match f.provider.auth_status().await.expect("auth") {
        AuthStatus::Authenticated { method, plan, .. } => {
            assert_eq!(method, "claude.ai");
            assert_eq!(plan.as_deref(), Some("pro"));
        }
        other => panic!("{other:?}"),
    }
}
