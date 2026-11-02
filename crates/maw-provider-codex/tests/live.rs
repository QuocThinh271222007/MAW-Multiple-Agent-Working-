//! Opt-in live tests against the real, authenticated `codex` CLI.
//!
//! These spend a small amount of the user's Codex quota, so they never run by default:
//!   MAW_LIVE_CODEX=1 cargo test -p maw-provider-codex --test live -- --ignored --nocapture
//! Set MAW_CODEX_CAPTURE=<file> to append the raw (redacted) server lines for fixtures.

use maw_core::provider::*;
use maw_core::{AgentId, PermissionProfile, Role};
use maw_provider_codex::{CodexConfig, CodexProvider};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn enabled() -> bool {
    std::env::var("MAW_LIVE_CODEX")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn provider() -> CodexProvider {
    CodexProvider::new(CodexConfig {
        capture_path: std::env::var("MAW_CODEX_CAPTURE").ok().map(PathBuf::from),
        request_timeout: Duration::from_secs(60),
        ..CodexConfig::default()
    })
}

fn cfg(cwd: PathBuf, mcp: Option<McpEndpoint>, permission: PermissionProfile) -> SessionConfig {
    SessionConfig {
        agent_id: AgentId::from("agt_live"),
        name: "codex-live".into(),
        role: Role::Worker,
        cwd,
        model: None,
        permission,
        mcp,
        instructions:
            "You are a terse test agent inside MAW. Follow the user's instruction literally.".into(),
        env: Default::default(),
        resume_native_id: None,
    }
}

async fn run_turn(
    rx: &mut EventReceiver,
    session: &SessionHandle,
    prompt: &str,
) -> Vec<ProviderEvent> {
    session
        .send_input(AgentInput::text(prompt))
        .await
        .expect("turn starts");
    let mut out = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ev)) => {
                if let ProviderEvent::ApprovalRequested {
                    request_id,
                    summary,
                    ..
                } = &ev
                {
                    // Live runs must never block on a human: deny and record.
                    eprintln!("[live] approval requested ({summary}); denying");
                    let _ = session
                        .resolve_approval(request_id, ApprovalDecision::Deny)
                        .await;
                }
                let done = matches!(ev, ProviderEvent::TurnCompleted { .. });
                out.push(ev);
                if done {
                    return out;
                }
            }
            other => panic!("turn did not finish: {other:?}; events so far: {out:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "live: needs an authenticated codex CLI and spends quota; set MAW_LIVE_CODEX=1"]
async fn live_minimal_turn() {
    if !enabled() {
        eprintln!("MAW_LIVE_CODEX!=1; skipping");
        return;
    }
    let p = provider();
    let info = p.detect().await.unwrap();
    assert!(info.installed, "{info:?}");
    eprintln!("[live] codex version: {:?}", info.version);
    let auth = p.auth_status().await.unwrap();
    assert!(
        auth.is_authenticated(),
        "log in first with `codex login`: {auth:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let (session, mut rx) = p
        .start_session(cfg(dir.path().to_path_buf(), None, PermissionProfile::Safe))
        .await
        .expect("session starts");
    assert!(matches!(
        rx.recv().await,
        Some(ProviderEvent::Connected {
            native_session_id: Some(_)
        })
    ));
    let events = run_turn(
        &mut rx,
        &session,
        "Reply with exactly: OK. Do not use tools.",
    )
    .await;

    eprintln!("[live] events: {events:#?}");
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderEvent::TurnStarted { .. })));
    let reply: String = events
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::Message { text } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(reply.contains("OK"), "reply was {reply:?}");
    assert!(
        matches!(
            events.last(),
            Some(ProviderEvent::TurnCompleted {
                outcome: TurnOutcome::Completed,
                ..
            })
        ),
        "{:?}",
        events.last()
    );
    if let Some(ProviderEvent::Usage(u)) =
        events.iter().find(|e| matches!(e, ProviderEvent::Usage(_)))
    {
        assert!(u.reported_by_provider && u.total_tokens.unwrap_or(0) > 0);
    }
    session.shutdown().await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(ProviderEvent::Disconnected { .. }) = rx.recv().await {
                    break true;
                }
            }
        })
        .await,
        Ok(true)
    ));
}

/// Minimal streamable-HTTP MCP server (JSON responses) that records bearer tokens and calls.
async fn standin_mcp() -> (String, Arc<Mutex<Vec<(String, String)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let (head_end, content_len, auth, method) = loop {
                    let n = match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let header = |name: &str| {
                            head.lines().find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix(&format!("{name}:"))
                                    .map(|v| v.trim().to_string())
                            })
                        };
                        let len = header("content-length")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        // keep original casing for the token
                        let auth = head
                            .lines()
                            .find(|l| l.to_lowercase().starts_with("authorization:"))
                            .map(|l| l[14..].trim().to_string())
                            .unwrap_or_default();
                        break (
                            pos + 4,
                            len,
                            auth,
                            head.split_whitespace().next().unwrap_or("").to_string(),
                        );
                    }
                };
                while buf.len() < head_end + content_len {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let respond = |status: &str, body: &str| {
                    format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                };
                if method != "POST" {
                    let _ = sock
                        .write_all(respond("405 Method Not Allowed", "").as_bytes())
                        .await;
                    return;
                }
                let req: serde_json::Value =
                    serde_json::from_slice(&buf[head_end..head_end + content_len])
                        .unwrap_or_default();
                let m = req["method"].as_str().unwrap_or("").to_string();
                if let Ok(mut l) = log.lock() {
                    l.push((m.clone(), auth));
                }
                let Some(id) = req.get("id").cloned() else {
                    let _ = sock.write_all(respond("202 Accepted", "").as_bytes()).await;
                    return;
                };
                let result = match m.as_str() {
                    "initialize" => {
                        serde_json::json!({ "protocolVersion": req["params"]["protocolVersion"], "capabilities": { "tools": {} }, "serverInfo": { "name": "maw-standin", "version": "0" } })
                    }
                    "tools/list" => {
                        serde_json::json!({ "tools": [{ "name": "maw_goal_get", "description": "Get the current MAW goal", "inputSchema": { "type": "object", "properties": {} }, "annotations": { "readOnlyHint": true } }] })
                    }
                    "tools/call" => {
                        serde_json::json!({ "content": [{ "type": "text", "text": "{\"goal\":\"STANDIN-GOAL-7731\"}" }] })
                    }
                    _ => serde_json::json!({}),
                };
                let body =
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                let _ = sock.write_all(respond("200 OK", &body).as_bytes()).await;
            });
        }
    });
    (url, seen)
}

#[tokio::test]
#[ignore = "live: needs an authenticated codex CLI and spends quota; set MAW_LIVE_CODEX=1"]
async fn live_codex_calls_an_authenticated_mcp_tool() {
    if !enabled() {
        eprintln!("MAW_LIVE_CODEX!=1; skipping");
        return;
    }
    let (url, seen) = standin_mcp().await;
    let token = "maw_tok_live_0123456789abcdef";
    let p = provider();
    let dir = tempfile::tempdir().unwrap();
    let (session, mut rx) = p
        .start_session(cfg(
            dir.path().to_path_buf(),
            Some(McpEndpoint {
                url,
                token: token.into(),
            }),
            PermissionProfile::Balanced,
        ))
        .await
        .expect("session starts");
    let events = run_turn(&mut rx, &session, "Call the MCP tool maw_goal_get (server: maw) exactly once, then reply with the goal text it returned and nothing else.").await;
    eprintln!("[live] events: {events:#?}");

    assert!(
        events.iter().any(
            |e| matches!(e, ProviderEvent::ToolStarted { name, .. } if name == "maw/maw_goal_get")
        ),
        "codex should call the MAW tool"
    );
    assert!(events.iter().any(|e| matches!(e, ProviderEvent::ToolCompleted { name, ok: true, .. } if name == "maw/maw_goal_get")));
    let reply: String = events
        .iter()
        .filter_map(|e| {
            if let ProviderEvent::Message { text } = e {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(reply.contains("STANDIN-GOAL-7731"), "reply was {reply:?}");
    let seen = seen.lock().unwrap().clone();
    assert!(seen.iter().any(|(m, _)| m == "tools/call"), "{seen:?}");
    assert!(
        seen.iter().all(|(_, a)| a == &format!("Bearer {token}")),
        "every request must carry the per-agent bearer token: {seen:?}"
    );
    session.shutdown().await.unwrap();
}
