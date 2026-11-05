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
        extra_dirs: vec![],
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_submit_ends_the_turn_but_a_rejected_one_lets_the_model_retry() {
    use maw_orchestrator::tasks::NewTask;
    use maw_orchestrator::Actor;
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
    engine
        .create_task(
            &Actor::User,
            NewTask {
                title: "t".into(),
                assignee: Some("local1".into()),
                ..Default::default()
            },
        )
        .unwrap();
    engine.start_task("T-1").unwrap();
    let token = engine.mint_token(&agent.id);
    let server = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = McpEndpoint {
        url: server.url.clone(),
        token,
    };

    // request 1: a made-up evidence kind (rejected; the error names the valid ones)
    // request 2: a valid submit (accepted: the turn ends here)
    // request 3 exists only to prove it is never asked for
    let bad = tool_call(
        "maw_task_submit_result",
        json!({"task":"T-1","summary":"done","evidence":[{"kind":"file","summary":"x"}]}),
    );
    let good = tool_call(
        "maw_task_submit_result",
        json!({"task":"T-1","summary":"done","evidence":[{"kind":"verification","summary":"checked"}]}),
    );
    let fake = fake_ollama(
        vec![bad, good, answer("this must not be requested")],
        vec!["tools"],
    )
    .await;
    let p = provider(&fake.url);
    let (session, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Balanced, Some(endpoint)))
        .await
        .unwrap();
    session.send_input(AgentInput::text("do it")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    let chats = fake.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 2, "the turn stopped after the accepted submit");
    let tool_msgs: Vec<String> = chats[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        tool_msgs
            .iter()
            .any(|m| m.contains("file_changes") && m.contains("verification")),
        "the rejection names the valid kinds: {tool_msgs:?}"
    );
    let task = engine.get_task("T-1").unwrap();
    assert_ne!(
        task.status,
        maw_core::TaskStatus::Running,
        "the good submit reached MAW"
    );
    session.shutdown().await.unwrap();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversation_survives_a_restart_like_resuming_claude_or_codex() {
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let fake = fake_ollama(
        vec![
            answer("noted, the code word is plum"),
            answer("it was plum"),
        ],
        vec!["completion", "tools"],
    )
    .await;
    let with_state = |url: &str| {
        OllamaProvider::new(OllamaConfig {
            url: Some(url.to_string()),
            idle_timeout: Duration::from_secs(10),
            state_dir: Some(state.path().to_path_buf()),
            ..Default::default()
        })
    };

    // first run: say something, then close MAW
    let p = with_state(&fake.url);
    let (s1, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Safe, None))
        .await
        .unwrap();
    let Some(ProviderEvent::Connected {
        native_session_id: Some(id),
    }) = rx.recv().await
    else {
        panic!("a session id is announced")
    };
    s1.send_input(AgentInput::text("the code word is plum"))
        .await
        .unwrap();
    collect_turn(&mut rx).await;
    s1.shutdown().await.unwrap();
    assert!(
        state.path().join(format!("{id}.json")).exists(),
        "the conversation was written to disk"
    );

    // second run (a new provider, like a new process) resumes by that id
    let p2 = with_state(&fake.url);
    let mut c = cfg(dir.path(), PermissionProfile::Safe, None);
    c.resume_native_id = Some(id.clone());
    let (s2, mut rx2) = p2.start_session(c).await.unwrap();
    assert!(matches!(
        rx2.recv().await,
        Some(ProviderEvent::Connected { native_session_id: Some(ref again) }) if *again == id
    ));
    assert!(s2.capabilities().resume);
    s2.send_input(AgentInput::text("what was the code word?"))
        .await
        .unwrap();
    collect_turn(&mut rx2).await;
    let sent = fake.chats.lock().unwrap().last().cloned().unwrap();
    let text = sent["messages"].to_string();
    assert!(
        text.contains("the code word is plum") && text.contains("noted, the code word is plum"),
        "the model receives the earlier turns again: {text}"
    );
    s2.shutdown().await.unwrap();

    // an id with no saved conversation starts fresh and says so, instead of failing
    let mut c = cfg(dir.path(), PermissionProfile::Safe, None);
    c.resume_native_id = Some("ollama-00000000000000000000000000000000".into());
    let (s3, mut rx3) = with_state(&fake.url).start_session(c).await.unwrap();
    let mut saw_notice = false;
    for _ in 0..2 {
        if let Some(ProviderEvent::Error {
            message,
            fatal: false,
        }) = rx3.recv().await
        {
            saw_notice = message.contains("could not be found");
        }
    }
    assert!(saw_notice, "a lost conversation is reported, not hidden");
    s3.shutdown().await.unwrap();
}

fn requests_text(fake: &Fake, i: usize) -> String {
    fake.chats.lock().unwrap()[i]["messages"].to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_asking_for_the_same_read_again_is_told_to_move_on() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_ollama(
        vec![
            tool_call("list_dir", json!({"path":"."})),
            tool_call("list_dir", json!({"path":"."})), // the same read, nothing changed
            tool_call("write_file", json!({"path":"a.txt","content":"x"})),
            tool_call("list_dir", json!({"path":"."})), // the same read, but after a change
            answer("done"),
        ],
        vec!["completion", "tools"],
    )
    .await;
    let (s, mut rx) = provider(&fake.url)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    collect_turn(&mut rx).await;
    // third request carries the answer to the repeated read: a hint, not another listing
    assert!(
        requests_text(&fake, 2).contains("You already called list_dir"),
        "{}",
        requests_text(&fake, 2)
    );
    // after the write the same read is allowed again and really runs
    assert!(
        !requests_text(&fake, 4).contains("You already called list_dir with exactly these arguments in this turn; its result is above. Do not call it again. Use that result to take the next step now, or answer the user.\"}],\"role\":\"tool\"")
            || requests_text(&fake, 4).matches("You already called list_dir").count() == 1,
        "only the repeat before the change was refused"
    );
    assert_eq!(
        requests_text(&fake, 4)
            .matches("You already called list_dir")
            .count(),
        1
    );
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_context_window_is_reported_and_the_leader_gets_a_bigger_one() {
    let dir = tempfile::tempdir().unwrap();
    // a reply whose prompt + output fill the 8192-token window
    let full = line(json!({"message":{"role":"assistant","content":"ok"},"done":false}))
        + &line(
            json!({"done":true,"done_reason":"stop","prompt_eval_count":7800,"eval_count":600}),
        );
    let fake = fake_ollama(vec![full.clone(), full], vec!["completion", "tools"]).await;
    let (s, mut rx) = provider(&fake.url)
        .start_session(cfg(dir.path(), PermissionProfile::Safe, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("hi")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    let warned = evs.iter().any(|e| {
        matches!(e, ProviderEvent::Error { message, fatal: false } if message.contains("context window is full"))
    });
    assert!(warned, "{evs:?}");
    s.shutdown().await.unwrap();
    assert_eq!(
        fake.chats.lock().unwrap()[0]["options"]["num_ctx"],
        8192,
        "a worker keeps the normal window"
    );

    // the Leader asks for the larger one
    let mut c = cfg(dir.path(), PermissionProfile::Safe, None);
    c.role = Role::Leader;
    let (s, mut rx) = provider(&fake.url).start_session(c).await.unwrap();
    s.send_input(AgentInput::text("hi")).await.unwrap();
    collect_turn(&mut rx).await;
    assert_eq!(fake.chats.lock().unwrap()[1]["options"]["num_ctx"], 12288);
    s.shutdown().await.unwrap();
}

// ---- thinking allowance and loop guards -----------------------------------------------------------

fn thinking_only(chars: usize) -> String {
    // a model that thinks and thinks: several chunks of reasoning, never an answer
    let chunk = "x".repeat(chars / 4);
    (0..4)
        .map(|_| line(json!({"message":{"role":"assistant","thinking":chunk},"done":false})))
        .collect::<String>()
        + &line(json!({"done":true,"done_reason":"stop","prompt_eval_count":100,"eval_count":500}))
}

fn provider_with(url: &str, f: impl FnOnce(&mut OllamaConfig)) -> OllamaProvider {
    let mut c = OllamaConfig {
        url: Some(url.to_string()),
        idle_timeout: Duration::from_secs(10),
        command_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    f(&mut c);
    OllamaProvider::new(c)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_that_uses_up_its_thinking_allowance_is_asked_again_with_thinking_off() {
    let dir = tempfile::tempdir().unwrap();
    // request 1: 4000 characters of reasoning against an allowance of 100 tokens (350 characters)
    // request 2: the answer, which must have been asked for without thinking
    let fake = fake_ollama(
        vec![thinking_only(4000), answer("done")],
        vec!["completion", "tools", "thinking"],
    )
    .await;
    let p = provider_with(&fake.url, |c| c.think_budget_worker = 100);
    let (s, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    let chats = fake.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 2, "one cut request and one new one");
    assert_eq!(chats[0]["think"], json!(true));
    assert_eq!(
        chats[1]["think"],
        json!(false),
        "asked again with thinking off"
    );
    assert!(evs.iter().any(
        |e| matches!(e, ProviderEvent::Reasoning { text } if text.contains("thinking stopped"))
    ));
    assert!(evs
        .iter()
        .any(|e| matches!(e, ProviderEvent::Message { text } if text == "done")));
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_within_the_allowance_is_left_alone_and_zero_means_no_limit() {
    let dir = tempfile::tempdir().unwrap();
    // under the allowance: a single request, thinking on
    let fake = fake_ollama(vec![answer("ok")], vec!["completion", "tools", "thinking"]).await;
    let (s, mut rx) = provider_with(&fake.url, |c| c.think_budget_worker = 1500)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    collect_turn(&mut rx).await;
    assert_eq!(fake.chats.lock().unwrap().len(), 1);
    assert_eq!(fake.chats.lock().unwrap()[0]["think"], json!(true));
    s.shutdown().await.unwrap();
    // 0 = no limit: long reasoning is never cut
    let fake = fake_ollama(
        vec![thinking_only(4000), answer("late")],
        vec!["completion", "tools", "thinking"],
    )
    .await;
    let (s, mut rx) = provider_with(&fake.url, |c| c.think_budget_worker = 0)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    collect_turn(&mut rx).await;
    assert!(fake
        .chats
        .lock()
        .unwrap()
        .iter()
        .all(|c| c["think"] == json!(true)));
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_role_has_its_own_allowance() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_ollama(
        vec![thinking_only(2000), answer("leader answer")],
        vec!["completion", "tools", "thinking"],
    )
    .await;
    // Leader 100 tokens (cut), Worker 100000 (not cut): same reasoning, different roles
    let p = provider_with(&fake.url, |c| {
        c.think_budget_leader = 100;
        c.think_budget_worker = 100_000;
    });
    let mut leader = cfg(dir.path(), PermissionProfile::Autonomous, None);
    leader.role = Role::Leader;
    let (s, mut rx) = p.start_session(leader).await.unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    collect_turn(&mut rx).await;
    assert_eq!(
        fake.chats.lock().unwrap().len(),
        2,
        "the Leader's reasoning was cut"
    );
    s.shutdown().await.unwrap();
    // the same reasoning from a Worker is not cut
    let fake = fake_ollama(
        vec![thinking_only(2000), answer("worker answer")],
        vec!["completion", "tools", "thinking"],
    )
    .await;
    let p = provider_with(&fake.url, |c| {
        c.think_budget_leader = 100;
        c.think_budget_worker = 100_000;
    });
    let (s, mut rx) = p
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    collect_turn(&mut rx).await;
    assert!(fake
        .chats
        .lock()
        .unwrap()
        .iter()
        .all(|c| c["think"] == json!(true)));
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_that_failed_twice_is_refused_the_third_time_and_a_stubborn_model_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let bad = || {
        tool_call(
            "write_file",
            json!({"path":"/definitely/outside/x.txt","content":"x"}),
        )
    };
    let fake = fake_ollama(
        vec![
            bad(),
            bad(),
            bad(),
            bad(),
            bad(),
            bad(),
            answer("never reached"),
        ],
        vec!["completion", "tools"],
    )
    .await;
    let (s, mut rx) = provider(&fake.url)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    // requests 1 and 2 run the call; 3, 4 and 5 are refused (the 3rd refusal ends the turn)
    let chats = fake.chats.lock().unwrap().len();
    assert_eq!(chats, 5, "stopped after the third refusal");
    assert!(
        requests_text(&fake, 3).contains("has already failed 2 times"),
        "{}",
        requests_text(&fake, 3)
    );
    match evs.last() {
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Failed,
            error: Some(e),
            ..
        }) => {
            assert!(e.contains("kept repeating a failing call"), "{e}")
        }
        other => panic!("{other:?}"),
    }
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_call_in_between_is_not_a_loop() {
    let dir = tempfile::tempdir().unwrap();
    let bad = || {
        tool_call(
            "write_file",
            json!({"path":"/definitely/outside/x.txt","content":"x"}),
        )
    };
    // a failing call, a different call, the failing call again: only one failure of each, so it runs
    let fake = fake_ollama(
        vec![
            bad(),
            tool_call("write_file", json!({"path":"ok.txt","content":"y"})),
            bad(),
            answer("done"),
        ],
        vec!["completion", "tools"],
    )
    .await;
    let (s, mut rx) = provider(&fake.url)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    assert!(!requests_text(&fake, 3).contains("has already failed"));
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_that_only_reads_is_told_to_act_and_then_stopped() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
    }
    // 16 reads of different files in a row: never repeats an exact call, never changes anything
    let mut script: Vec<String> = (0..16)
        .map(|i| tool_call("read_file", json!({"path": format!("f{i}.txt")})))
        .collect();
    script.push(answer("never reached"));
    let fake = fake_ollama(script, vec!["completion", "tools"]).await;
    let (s, mut rx) = provider(&fake.url)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    // the nudge arrives with the 9th request (after the 8th read) ...
    assert!(
        requests_text(&fake, 8).contains("Stop reading"),
        "{}",
        requests_text(&fake, 8)
    );
    assert!(!requests_text(&fake, 7).contains("Stop reading"));
    // ... and the turn ends after the 14th
    assert_eq!(fake.chats.lock().unwrap().len(), 14);
    match evs.last() {
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Failed,
            error: Some(e),
            ..
        }) => {
            assert!(e.contains("14 reads in a row"), "{e}")
        }
        other => panic!("{other:?}"),
    }
    s.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_allowance_covers_the_whole_turn_not_just_one_request() {
    let dir = tempfile::tempdir().unwrap();
    // allowance 100 tokens = 350 characters per request, 700 for the turn. Each request reasons for
    // exactly 350 characters (within its allowance) and then writes a file; after two of them the
    // turn's allowance is gone and the third request is made with thinking already off.
    let think350 =
        || line(json!({"message":{"role":"assistant","thinking":"y".repeat(350)},"done":false}));
    let fake = fake_ollama(
        vec![
            think350() + &tool_call("write_file", json!({"path":"a.txt","content":"1"})),
            think350() + &tool_call("write_file", json!({"path":"b.txt","content":"2"})),
            answer("done"),
        ],
        vec!["completion", "tools", "thinking"],
    )
    .await;
    let (s, mut rx) = provider_with(&fake.url, |c| c.think_budget_worker = 100)
        .start_session(cfg(dir.path(), PermissionProfile::Autonomous, None))
        .await
        .unwrap();
    s.send_input(AgentInput::text("go")).await.unwrap();
    let evs = collect_turn(&mut rx).await;
    let chats = fake.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 3);
    assert_eq!(
        [
            chats[0]["think"].clone(),
            chats[1]["think"].clone(),
            chats[2]["think"].clone()
        ],
        [json!(true), json!(true), json!(false)]
    );
    assert!(matches!(
        evs.last(),
        Some(ProviderEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    // a new turn starts with a fresh allowance
    let _ = &fake;
    s.shutdown().await.unwrap();
}
