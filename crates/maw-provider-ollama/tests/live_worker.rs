//! Live: a small local model plays MAW's Worker. It gets the real role instructions, the real task
//! prompt and the real `maw_*` tools over MCP, and we look at what it actually does. Opt in with
//! `MAW_LIVE_OLLAMA=1` (model: `MAW_LIVE_OLLAMA_MODEL`, default `qwen3:4b`; context:
//! `MAW_LIVE_OLLAMA_CTX`, default 8192). It reports; the only hard assertion is that the session ran.

use maw_core::provider::*;
use maw_core::{OrchestratorConfig, PermissionProfile, ProviderKind, Role, TaskStatus};
use maw_orchestrator::tasks::NewTask;
use maw_orchestrator::{prompts, Actor, Engine};
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use maw_storage::Store;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs Ollama with a tool-capable model; set MAW_LIVE_OLLAMA=1"]
async fn live_small_model_as_a_maw_worker() {
    if std::env::var("MAW_LIVE_OLLAMA").is_err() {
        return;
    }
    let model = std::env::var("MAW_LIVE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:4b".into());
    let ctx: u32 = std::env::var("MAW_LIVE_OLLAMA_CTX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8192);
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
            Role::Worker,
            Some(model.clone()),
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    engine.create_goal("demo goal", vec![]).unwrap();
    engine
        .create_task(
            &Actor::User,
            NewTask {
                title: "Create notes.txt".into(),
                description: "Create a file named notes.txt in the workspace containing exactly the word done.".into(),
                acceptance_criteria: vec!["notes.txt exists and contains the word done".into()],
                assignee: Some("worker1".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let started = engine.start_task("T-1").unwrap();
    let token = engine.mint_token(&agent.id);
    let server = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let prompt = prompts::task_prompt(&started, &[], &dir.path().to_string_lossy(), None, &[]);

    let p = OllamaProvider::new(OllamaConfig {
        num_ctx: ctx,
        idle_timeout: Duration::from_secs(900),
        ..Default::default()
    });
    let cfg = SessionConfig {
        agent_id: agent.id.clone(),
        name: "worker1".into(),
        role: Role::Worker,
        cwd: dir.path().to_path_buf(),
        model: Some(model.clone()),
        effort: None,
        permission: PermissionProfile::Balanced,
        mcp: Some(McpEndpoint {
            url: server.url.clone(),
            token,
        }),
        instructions: prompts::role_instructions(Role::Worker, "worker1"),
        env: Default::default(),
        resume_native_id: None,
        extra_dirs: vec![],
    };
    let (s, mut rx) = p.start_session(cfg).await.expect("session");
    s.send_input(AgentInput::text(prompt)).await.unwrap();

    let (mut calls, mut first_prompt_tokens, mut last_usage) = (vec![], None, None);
    let outcome;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(1200), rx.recv())
            .await
            .expect("in time")
            .expect("events");
        match &ev {
            ProviderEvent::ToolStarted { name, detail, .. } => calls.push(format!(
                "{name} {}",
                detail
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(110)
                    .collect::<String>()
            )),
            ProviderEvent::CommandStarted { command, .. } => {
                calls.push(format!("run_command {command}"))
            }
            ProviderEvent::ToolCompleted {
                name,
                ok: false,
                detail,
                ..
            } => calls.push(format!(
                "  ✗ {name} failed: {}",
                detail
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(140)
                    .collect::<String>()
            )),
            ProviderEvent::Usage(u) => {
                if first_prompt_tokens.is_none() {
                    first_prompt_tokens = u.input_tokens;
                }
                last_usage = Some(u.clone());
            }
            ProviderEvent::Message { text } => calls.push(format!(
                "  says: {}",
                text.chars().take(160).collect::<String>()
            )),
            ProviderEvent::Error { message, .. } => calls.push(format!("  error: {message}")),
            ProviderEvent::TurnCompleted {
                outcome: o, error, ..
            } => {
                outcome = Some((*o, error.clone()));
                break;
            }
            _ => {}
        }
    }
    let task = engine.get_task("T-1").unwrap();
    let wrote = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap_or_default();
    eprintln!("=== model {model}, context {ctx}");
    for c in &calls {
        eprintln!("{c}");
    }
    eprintln!("turn outcome: {outcome:?}");
    eprintln!(
        "first request prompt tokens (instructions + tool schemas + task): {first_prompt_tokens:?}"
    );
    eprintln!("total usage: {last_usage:?}");
    eprintln!("notes.txt: {:?}", wrote.trim());
    eprintln!(
        "task status in MAW: {} (submitted through maw_task_submit_result: {})",
        task.status,
        task.status != TaskStatus::Running
    );
    s.shutdown().await.unwrap();
    server.stop().await;
    assert!(outcome.is_some());
}
