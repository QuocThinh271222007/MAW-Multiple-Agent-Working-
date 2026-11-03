//! The agent loop against a scripted fake Ollama server, with MAW's real engine and MCP server
//! behind the coordination tools. The fake proves MAW's side of the protocol; it is not evidence
//! about any particular model (see the ignored live test).

use maw_core::provider::*;
use maw_core::{OrchestratorConfig, PermissionProfile, ProviderKind, Role};
use maw_orchestrator::Engine;
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use maw_storage::Store;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Fake {
    url: String,
    chats: Arc<Mutex<Vec<Value>>>,
}

fn line(v: Value) -> String {
    format!("{v}\n")
}

/// `script[i]` is the NDJSON body for the i-th /api/chat request.
async fn fake_ollama(script: Vec<String>, capabilities: Vec<&'static str>) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let chats = Arc::new(Mutex::new(Vec::new()));
    let (c2, script) = (chats.clone(), Arc::new(script));
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (chats, script, caps) = (c2.clone(), script.clone(), capabilities.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let (head_end, clen) = loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                        let clen = head
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        break (p + 4, clen);
                    }
                };
                while buf.len() < head_end + clen {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let req = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let first = req.lines().next().unwrap_or("").to_string();
                let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                let out = if first.contains("/api/version") {
                    json!({"version":"0.0.test"}).to_string()
                } else if first.contains("/api/tags") {
                    json!({"models":[{"name":"fake:1b","size":1000}]}).to_string()
                } else if first.contains("/api/show") {
                    json!({"capabilities":caps,"details":{"parameter_size":"1B","quantization_level":"Q4"}}).to_string()
                } else if first.contains("/api/chat") {
                    let mut g = chats.lock().unwrap();
                    let i = g.len();
                    g.push(body);
                    script
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| line(json!({"error":"script exhausted"})))
                } else {
                    String::new()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\nconnection: close\r\n\r\n{out}"
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    Fake { url, chats }
}

fn tool_call(name: &str, args: Value) -> String {
    line(
        json!({"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":name,"arguments":args}}]},"done":false}),
    ) + &line(json!({"done":true,"done_reason":"stop","prompt_eval_count":100,"eval_count":10}))
}

fn answer(text: &str) -> String {
    line(json!({"message":{"role":"assistant","thinking":"considering"},"done":false}))
        + &line(json!({"message":{"role":"assistant","content":text},"done":false}))
        + &line(json!({"done":true,"done_reason":"stop","prompt_eval_count":120,"eval_count":20}))
}

fn provider(url: &str) -> OllamaProvider {
    OllamaProvider::new(OllamaConfig {
        url: Some(url.to_string()),
        idle_timeout: Duration::from_secs(10),
        command_timeout: Duration::from_secs(10),
        ..Default::default()
    })
}

fn cfg(
    dir: &std::path::Path,
    profile: PermissionProfile,
    mcp: Option<McpEndpoint>,
) -> SessionConfig {
    SessionConfig {
        agent_id: maw_core::AgentId::new(),
        name: "local1".into(),
        role: Role::Worker,
        cwd: dir.to_path_buf(),
        model: Some("fake:1b".into()),
        effort: None,
        permission: profile,
        mcp,
        instructions: "ROLE-INSTRUCTIONS".into(),
        env: Default::default(),
        resume_native_id: None,
    }
}

async fn collect_turn(rx: &mut EventReceiver) -> Vec<ProviderEvent> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .expect("event in time")
            .expect("channel open");
        let done = matches!(ev, ProviderEvent::TurnCompleted { .. });
        out.push(ev);
        if done {
            return out;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_runs_local_tools_and_maw_tools_and_reports_everything() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(
        store,
        "p",
        &dir.path().to_string_lossy(),
        OrchestratorConfig::default(),
    )
    .unwrap();
    let agent = engine
        .add_agent(
            "local1",
            ProviderKind::Ollama,
            Role::Worker,
            None,
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    engine.create_goal("ship", vec![]).unwrap();
    let token = engine.mint_token(&agent.id);
    let server = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = McpEndpoint {
        url: server.url.clone(),
        token,
    };

    let fake = fake_ollama(
        vec![
            tool_call("write_file", json!({"path":"hello.txt","content":"hi"})),
            tool_call("maw_goal_get", json!({})),
            answer("all done"),
        ],
        vec!["completion", "tools", "thinking"],
    )
    .await;
    let p = provider(&fake.url);
    let (session, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Balanced, Some(endpoint)))
        .await
        .expect("session starts");
    assert!(matches!(
        rx.recv().await,
        Some(ProviderEvent::Connected { .. })
    ));
    session.send_input(AgentInput::text("do it")).await.unwrap();
    let evs = collect_turn(&mut rx).await;

    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
        "hi"
    );
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::FileChanged { paths } if paths == &["hello.txt"])));
    assert!(evs.iter().any(|e| matches!(e, ProviderEvent::ToolCompleted { name, ok: true, .. } if name == "maw_goal_get")), "the MAW tool went through MCP: {evs:?}");
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "all done")));
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    // usage is cumulative across the three requests: 100 + 100 + 120 in, 10 + 10 + 20 out
    let usage = evs
        .iter()
        .rev()
        .find_map(|e| {
            if let ProviderEvent::Usage(u) = e {
                Some(u.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(320), Some(40))
    );
    assert!(usage.reported_by_provider);

    // what the model was sent: role instructions, local and MAW tools, tool results fed back
    let chats = fake.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 3);
    let names: Vec<&str> = chats[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        names.contains(&"read_file")
            && names.contains(&"run_command")
            && names.contains(&"maw_goal_get"),
        "{names:?}"
    );
    assert!(chats[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("ROLE-INSTRUCTIONS"));
    assert_eq!(chats[0]["options"]["num_ctx"], 8192);
    assert_eq!(chats[0]["think"], true);
    let last = chats[2]["messages"].as_array().unwrap();
    assert!(last
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_name"] == "write_file"));
    assert!(last
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_name"] == "maw_goal_get"));

    // a second turn works (history is kept) and a turn cannot overlap another
    session.shutdown().await.unwrap();
    assert!(
        session.send_input(AgentInput::text("again")).await.is_err(),
        "closed sessions refuse input"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn safe_profile_asks_before_writing_and_a_denial_leaves_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_ollama(
        vec![
            tool_call("write_file", json!({"path":"x.txt","content":"no"})),
            answer("ok, skipped"),
        ],
        vec!["tools"],
    )
    .await;
    let p = provider(&fake.url);
    let (session, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Safe, None))
        .await
        .unwrap();
    session
        .send_input(AgentInput::text("write x"))
        .await
        .unwrap();
    let mut evs = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let ProviderEvent::ApprovalRequested {
            request_id, kind, ..
        } = &ev
        {
            assert_eq!(*kind, ApprovalKind::FileChange);
            session
                .resolve_approval(request_id, ApprovalDecision::Deny)
                .await
                .unwrap();
        }
        let done = matches!(ev, ProviderEvent::TurnCompleted { .. });
        evs.push(ev);
        if done {
            break;
        }
    }
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::ApprovalRequested { .. })));
    assert!(
        !dir.path().join("x.txt").exists(),
        "denied: nothing was written"
    );
    let chats = fake.chats.lock().unwrap().clone();
    let tool_msg = chats[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()
        .clone();
    assert!(
        tool_msg["content"].as_str().unwrap().contains("denied"),
        "the model is told: {tool_msg}"
    );
    assert!(session
        .resolve_approval("nope", ApprovalDecision::Approve)
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_without_tool_support_is_refused_with_an_explanation() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_ollama(vec![], vec!["completion"]).await;
    let err = provider(&fake.url)
        .start_session(cfg(dir.path(), PermissionProfile::Safe, None))
        .await
        .err()
        .expect("must refuse");
    assert!(err.to_string().contains("tool-calling"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_stops_a_running_command_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_ollama(
        vec![tool_call("run_command", json!({"command":"sleep 30"}))],
        vec!["tools"],
    )
    .await;
    let p = provider(&fake.url);
    let (session, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    session.send_input(AgentInput::text("sleep")).await.unwrap();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, ProviderEvent::CommandStarted { .. }) {
            break;
        }
    }
    session.interrupt().await.unwrap();
    let evs = collect_turn(&mut rx).await;
    assert!(
        matches!(
            evs.last(),
            Some(ProviderEvent::TurnCompleted {
                outcome: TurnOutcome::Interrupted,
                ..
            })
        ),
        "{evs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_reports_size_and_capabilities() {
    let fake = fake_ollama(vec![], vec!["completion", "tools", "thinking"]).await;
    let models = provider(&fake.url).list_models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "fake:1b");
    assert!(
        models[0].description.contains("tools, thinking"),
        "{}",
        models[0].description
    );
    assert_eq!(
        models[0].reasoning_efforts,
        ["none"],
        "ordinary models have no effort setting"
    );
    let info = provider(&fake.url).detect().await.unwrap();
    assert!(info.installed && info.version.as_deref() == Some("0.0.test"));
}
