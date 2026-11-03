//! Opt-in live test against the REAL installed Claude Code. It spends a few cheap `haiku` turns on
//! the logged-in account, so it is `#[ignore]`d AND requires `MAW_LIVE_CLAUDE=1`:
//!
//!     MAW_LIVE_CLAUDE=1 cargo test -p maw-provider-claude --test live -- --ignored --nocapture
//!
//! A tiny in-process HTTP MCP stand-in plays the role of the MAW coordination server so the real
//! MCP wiring (config shape, bearer-token header via env expansion, tool allowlist) is verified.

use maw_core::agent::{PermissionProfile, Role};
use maw_core::ids::AgentId;
use maw_core::provider::*;
use maw_provider_claude::ClaudeProvider;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Default)]
struct Seen {
    methods: Vec<String>,
    auth: Vec<String>,
}

/// Minimal streamable-HTTP MCP server (JSON responses) with one tool.
async fn start_mcp() -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let seen = seen2.clone();
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    // read one request (headers + content-length body); keep-alive loop
                    let header_end = loop {
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break p + 4;
                        }
                        let mut tmp = [0u8; 4096];
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < header_end + len {
                        let mut tmp = [0u8; 4096];
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let body: Vec<u8> = buf[header_end..header_end + len].to_vec();
                    buf.drain(..header_end + len);
                    let first = head.lines().next().unwrap_or("").to_string();
                    let resp = if first.starts_with("POST") {
                        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        let method = req["method"].as_str().unwrap_or("").to_string();
                        if let Ok(mut s) = seen.lock() {
                            s.methods.push(method.clone());
                            if let Some(a) = head
                                .lines()
                                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                            {
                                s.auth.push(
                                    a.split_once(':')
                                        .map(|x| x.1)
                                        .unwrap_or("")
                                        .trim()
                                        .to_string(),
                                );
                            }
                        }
                        match (req.get("id"), method.as_str()) {
                            (None, _) => {
                                "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n".to_string()
                            }
                            (Some(id), m) => {
                                let result = match m {
                                    "initialize" => {
                                        json!({"protocolVersion": req["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "maw-standin", "version": "0"}})
                                    }
                                    "tools/list" => {
                                        json!({"tools": [{"name": "maw_goal_get", "description": "Get the current goal", "inputSchema": {"type": "object", "properties": {}}}]})
                                    }
                                    "tools/call" => {
                                        json!({"content": [{"type": "text", "text": "{\"goal\":\"ship the auth module\"}"}]})
                                    }
                                    _ => Value::Null,
                                };
                                let payload = if m == "initialize" || m == "tools/list" || m == "tools/call" || m == "ping" {
                                    json!({"jsonrpc": "2.0", "id": id, "result": result})
                                } else {
                                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}})
                                }
                                .to_string();
                                format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}", payload.len(), payload)
                            }
                        }
                    } else {
                        "HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\n\r\n".to_string()
                    };
                    if sock.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (format!("http://127.0.0.1:{port}/mcp"), seen)
}

async fn next(rx: &mut EventReceiver) -> ProviderEvent {
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .expect("timed out")
        .expect("stream closed")
}

async fn until(
    rx: &mut EventReceiver,
    pred: impl Fn(&ProviderEvent) -> bool,
) -> Vec<ProviderEvent> {
    let mut out = vec![];
    loop {
        let e = next(rx).await;
        let stop = pred(&e);
        out.push(e);
        if stop {
            return out;
        }
    }
}

#[tokio::test]
#[ignore = "spends real (cheap) Claude turns; run with MAW_LIVE_CLAUDE=1 -- --ignored"]
async fn real_claude_end_to_end() {
    if std::env::var("MAW_LIVE_CLAUDE").ok().as_deref() != Some("1") {
        eprintln!("MAW_LIVE_CLAUDE != 1: skipping live test");
        return;
    }
    let provider = ClaudeProvider::new();
    let info = provider.detect().await.expect("detect");
    assert!(info.installed, "{info:?}");
    println!("claude version: {:?}", info.version);
    let auth = provider.auth_status().await.expect("auth_status");
    assert!(auth.is_authenticated(), "log in first: {auth:?}");

    let (url, seen) = start_mcp().await;
    let work = tempfile::tempdir().expect("tempdir");
    let token = "maw_tok_live_0123456789abcdef";
    let cfg = SessionConfig {
        agent_id: AgentId::new(),
        name: "live".into(),
        role: Role::Worker,
        cwd: work.path().to_path_buf(),
        model: Some("haiku".into()),
        permission: PermissionProfile::Balanced,
        mcp: Some(McpEndpoint {
            url,
            token: token.into(),
        }),
        instructions: "You are a terse test agent. Follow instructions literally.".into(),
        env: HashMap::new(),
        resume_native_id: None,
        effort: None,
    };
    let (session, mut rx) = provider.start_session(cfg).await.expect("start");
    assert!(matches!(
        next(&mut rx).await,
        ProviderEvent::Connected { .. }
    ));

    // Turn 1: plain reply. Confirms init, streaming, usage.
    session
        .send_input(AgentInput::text(
            "Reply with exactly: OK. Do not use any tools.",
        ))
        .await
        .expect("send");
    let t1 = until(&mut rx, |e| {
        matches!(e, ProviderEvent::TurnCompleted { .. })
    })
    .await;
    println!("turn1: {t1:#?}");
    assert!(t1
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text.contains("OK"))));
    assert!(t1.iter().any(
        |e| matches!(e, ProviderEvent::Usage(u) if u.reported_by_provider && u.cost_usd.is_some())
    ));
    assert!(matches!(
        t1.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    assert!(
        !t1.iter().any(|e| matches!(e, ProviderEvent::Error { .. })),
        "MCP server must have connected: {t1:#?}"
    );

    // Turn 2: MCP tool via the allowlist: no approval prompt expected.
    session
        .send_input(AgentInput::text(
            "Call the maw_goal_get tool, then tell me the goal in at most 6 words.",
        ))
        .await
        .expect("send");
    let t2 = until(&mut rx, |e| {
        matches!(e, ProviderEvent::TurnCompleted { .. })
    })
    .await;
    println!("turn2: {t2:#?}");
    assert!(t2.iter().any(|e| matches!(e, ProviderEvent::ToolStarted { name, .. } if name.ends_with("maw_goal_get"))), "tool was called");
    assert!(
        !t2.iter()
            .any(|e| matches!(e, ProviderEvent::ApprovalRequested { .. })),
        "MAW tools must be pre-approved"
    );
    {
        let s = seen.lock().expect("seen");
        assert!(
            s.methods.contains(&"tools/call".to_string()),
            "{:?}",
            s.methods
        );
        assert!(
            s.auth.iter().all(|a| a == &format!("Bearer {token}")),
            "bearer token delivered via env expansion: {:?}",
            s.auth
        );
        assert!(!s.auth.is_empty());
    }

    // Turn 3: a command outside the allowlist must surface as an approval and run once approved.
    session.send_input(AgentInput::text("Run exactly this shell command and nothing else: python3 -c 'print(40+2)'. Then report its output.")).await.expect("send");
    let pre = until(&mut rx, |e| {
        matches!(
            e,
            ProviderEvent::ApprovalRequested { .. } | ProviderEvent::TurnCompleted { .. }
        )
    })
    .await;
    println!("turn3 (to approval): {pre:#?}");
    let rid = match pre.last() {
        Some(ProviderEvent::ApprovalRequested {
            request_id,
            kind,
            summary,
            ..
        }) => {
            assert_eq!(*kind, ApprovalKind::Command);
            assert!(summary.contains("python3"), "{summary}");
            request_id.clone()
        }
        other => panic!("expected an approval request, got {other:?}"),
    };
    session
        .resolve_approval(&rid, ApprovalDecision::ApproveForSession)
        .await
        .expect("resolve");
    let post = until(&mut rx, |e| {
        matches!(e, ProviderEvent::TurnCompleted { .. })
    })
    .await;
    println!("turn3 (after approval): {post:#?}");
    assert!(
        post.iter().any(|e| matches!(
            e,
            ProviderEvent::CommandCompleted {
                exit_code: Some(0),
                ..
            }
        )),
        "approved command ran"
    );
    assert!(matches!(
        post.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));

    session.shutdown().await.expect("shutdown");
}
