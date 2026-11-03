//! Live test against a real Ollama server and a real model. Opt in with `MAW_LIVE_OLLAMA=1`
//! (and optionally `MAW_LIVE_OLLAMA_MODEL`, `OLLAMA_HOST`); run with `cargo test -- --ignored`.
//! It uses a small context so it fits modest machines.

use maw_core::provider::*;
use maw_core::{PermissionProfile, Role};
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs a running Ollama server with a tool-capable model; set MAW_LIVE_OLLAMA=1"]
async fn live_model_creates_a_file_with_its_tools() {
    if std::env::var("MAW_LIVE_OLLAMA").is_err() {
        return;
    }
    let model = std::env::var("MAW_LIVE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:4b".into());
    let dir = tempfile::tempdir().unwrap();
    let p = OllamaProvider::new(OllamaConfig {
        num_ctx: 4096,
        idle_timeout: Duration::from_secs(600),
        ..Default::default()
    });
    let cfg = SessionConfig {
        agent_id: maw_core::AgentId::new(),
        name: "live".into(),
        role: Role::Worker,
        cwd: dir.path().to_path_buf(),
        model: Some(model),
        effort: None,
        permission: PermissionProfile::Balanced,
        mcp: None,
        instructions: "Be brief.".into(),
        env: Default::default(),
        resume_native_id: None,
    };
    let (s, mut rx) = p.start_session(cfg).await.expect("session");
    s.send_input(AgentInput::text(
        "Create a file named hello.txt containing exactly the word hello. Use the write_file tool.",
    ))
    .await
    .unwrap();
    let (mut thinking, mut tools) = (0usize, vec![]);
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(900), rx.recv())
            .await
            .expect("the model answers in time")
            .expect("events");
        match &ev {
            ProviderEvent::Reasoning { text } => thinking += text.len(),
            ProviderEvent::ToolStarted { name, .. } => tools.push(name.clone()),
            ProviderEvent::Error { message, .. } => eprintln!("provider error: {message}"),
            _ => {}
        }
        if let ProviderEvent::TurnCompleted { outcome, error, .. } = &ev {
            eprintln!("outcome {outcome:?} error {error:?}");
            break;
        }
    }
    eprintln!("thinking chars: {thinking}; tools: {tools:?}");
    let written = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap_or_default();
    assert_eq!(written.trim(), "hello", "tools called: {tools:?}");
    s.shutdown().await.unwrap();
}
