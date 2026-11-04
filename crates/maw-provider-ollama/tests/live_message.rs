//! Live: an idle local agent gets a message typed by the user (the same prompt MAW's supervisor
//! builds) and we look at what it does: does it act, does it `maw_message_ack` correctly?
//! Opt in with `MAW_LIVE_OLLAMA=1` (model: `MAW_LIVE_OLLAMA_MODEL`, default `qwen3:4b`).

use maw_core::provider::*;
use maw_core::{MessageType, OrchestratorConfig, PermissionProfile, ProviderKind, Role};
use maw_orchestrator::{prompts, Actor, Engine};
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use maw_storage::Store;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs Ollama with a tool-capable model; set MAW_LIVE_OLLAMA=1"]
async fn live_small_model_gets_a_user_message() {
    if std::env::var("MAW_LIVE_OLLAMA").is_err() {
        return;
    }
    let model = std::env::var("MAW_LIVE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:4b".into());
    let text = std::env::var("MAW_LIVE_MESSAGE")
        .unwrap_or_else(|_| "Create a file named hello.txt containing the word hi.".into());
    let role = if std::env::var("MAW_LIVE_ROLE").as_deref() == Ok("leader") {
        Role::Leader
    } else {
        Role::Worker
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "# demo\n").unwrap();
    let engine = Engine::open(
        Arc::new(Store::open_in_memory().unwrap()),
        "p",
        &dir.path().to_string_lossy(),
        OrchestratorConfig::default(),
    )
    .unwrap();
    let agent = engine
        .add_agent(
            "worker1",
            ProviderKind::Ollama,
            role,
            Some(model.clone()),
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    let sent = engine
        .send_message(
            &Actor::User,
            "worker1",
            MessageType::Instruction,
            "Message from the user",
            &text,
            None,
            None,
        )
        .unwrap();
    let msgs = engine.undelivered_for(&agent.id).unwrap();
    let prompt = prompts::messages_prompt(&msgs, &|_| "user".to_string());
    eprintln!("=== prompt sent to the model:\n{prompt}\n===");
    let token = engine.mint_token(&agent.id);
    let server = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let p = OllamaProvider::new(OllamaConfig {
        idle_timeout: Duration::from_secs(900),
        ..Default::default()
    });
    let cfg = SessionConfig {
        agent_id: agent.id.clone(),
        name: "worker1".into(),
        role,
        cwd: dir.path().to_path_buf(),
        model: Some(model.clone()),
        effort: None,
        permission: PermissionProfile::Balanced,
        mcp: Some(McpEndpoint {
            url: server.url.clone(),
            token,
        }),
        instructions: prompts::role_instructions(role, "worker1"),
        env: Default::default(),
        resume_native_id: None,
        extra_dirs: vec![],
    };
    let (s, mut rx) = p.start_session(cfg).await.expect("session");
    s.send_input(AgentInput::text(prompt)).await.unwrap();
    let mut log = vec![];
    let mut first_in: Option<u64> = None;
    loop {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(300), rx.recv()).await else {
            eprintln!("(gave up after 300 s)");
            break;
        };
        eprintln!(
            "[event] {}",
            format!("{ev:?}").chars().take(160).collect::<String>()
        );
        match &ev {
            ProviderEvent::ToolStarted { name, detail, .. } => log.push(format!(
                "{name} {}",
                detail
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect::<String>()
            )),
            ProviderEvent::CommandStarted { command, .. } => log.push(format!("run {command}")),
            ProviderEvent::ToolCompleted {
                name,
                ok: false,
                detail,
                ..
            } => log.push(format!(
                "  FAILED {name}: {}",
                detail
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(140)
                    .collect::<String>()
            )),
            ProviderEvent::Message { text } => log.push(format!(
                "  says: {}",
                text.chars().take(200).collect::<String>()
            )),
            ProviderEvent::Usage(u) => {
                if first_in.is_none() {
                    first_in = u.input_tokens;
                }
            }
            ProviderEvent::TurnCompleted { .. } => break,
            _ => {}
        }
    }
    let acked = engine
        .inbox(
            &Actor::Agent {
                id: agent.id.clone(),
                role,
            },
            false,
            10,
        )
        .unwrap()
        .iter()
        .any(|m| m.id == sent.id && m.acknowledged_at.is_some());
    eprintln!("=== model {model}");
    for l in &log {
        eprintln!("{l}");
    }
    let wrote: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    eprintln!("files now: {wrote:?}");
    eprintln!("message acknowledged: {acked}");
    eprintln!("role {role:?}: first request used {first_in:?} prompt tokens (instructions + tool schemas + message)");
    s.shutdown().await.unwrap();
    server.stop().await;
}
