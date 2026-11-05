//! Live: the thinking allowance against a real model. Opt-in (spends a little GPU time):
//! `MAW_LIVE_OLLAMA=1 MAW_OLLAMA_URL=http://host:11434 MAW_OLLAMA_MODEL=lfm3:1.7b cargo test -p
//! maw-provider-ollama --test live_think -- --ignored --nocapture`

use maw_core::provider::*;
use maw_core::{PermissionProfile, Role};
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use std::time::{Duration, Instant};

async fn one_run(url: &str, model: &str, budget: u32) -> (f64, u64, usize, bool, bool) {
    let dir = tempfile::tempdir().unwrap();
    let p = OllamaProvider::new(OllamaConfig {
        url: Some(url.to_string()),
        autostart: false,
        think_budget_worker: budget,
        idle_timeout: Duration::from_secs(120),
        ..Default::default()
    });
    let (s, mut rx) = p
        .start_session(SessionConfig {
            agent_id: maw_core::AgentId::new(),
            name: "worker-1".into(),
            role: Role::Worker,
            cwd: dir.path().to_path_buf(),
            model: Some(model.into()),
            effort: None,
            permission: PermissionProfile::Autonomous,
            mcp: None,
            instructions: "Do the task with the tools (write_file to create a file). When the file is written, answer in one sentence.".into(),
            env: Default::default(),
            resume_native_id: None,
            extra_dirs: vec![],
        })
        .await
        .unwrap();
    let t0 = Instant::now();
    s.send_input(AgentInput::text(
        "Create a radix sort in Python (LSD, base 10, non-negative integers) in radix_sort.py, with a tiny self-test at the bottom.",
    ))
    .await
    .unwrap();
    let (mut tokens, mut thinking_chars, mut cut) = (0u64, 0usize, false);
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(300), rx.recv())
            .await
            .expect("in time")
            .expect("open");
        match ev {
            ProviderEvent::Usage(u) => tokens = u.output_tokens.unwrap_or(0),
            ProviderEvent::Reasoning { text } => {
                if text.contains("thinking stopped") {
                    cut = true;
                } else {
                    thinking_chars += text.len();
                }
            }
            ProviderEvent::TurnCompleted { .. } => break,
            _ => {}
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let ok = std::process::Command::new("python3")
        .arg(dir.path().join("radix_sort.py"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    s.shutdown().await.unwrap();
    (secs, tokens, thinking_chars, cut, ok)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running Ollama and spends GPU time"]
async fn thinking_allowance_against_a_real_model() {
    if std::env::var("MAW_LIVE_OLLAMA").ok().as_deref() != Some("1") {
        eprintln!("set MAW_LIVE_OLLAMA=1 to run");
        return;
    }
    let url = std::env::var("MAW_OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    let model = std::env::var("MAW_OLLAMA_MODEL").unwrap_or_else(|_| "lfm3:1.7b".into());
    // a first run loads the model so the timings below compare like with like
    let _ = one_run(&url, &model, 1500).await;
    for budget in [0u32, 1500, 400, 0, 1500, 400] {
        let (secs, tokens, think, cut, ok) = one_run(&url, &model, budget).await;
        eprintln!(
            "{model} allowance {:>5} tokens: {secs:5.1}s  {tokens:5} output tokens  thinking {think:6} chars  cut={cut:5}  file runs={ok}",
            if budget == 0 { "none".to_string() } else { budget.to_string() }
        );
    }
}
