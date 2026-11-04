//! Live: two local agents through MAW's real supervisor. The user asks the Leader (a small local
//! model) to hand a task to the worker (another small local model), and we watch what happens:
//! does the Leader find the worker, create the goal and the task, does the worker take it?
//! Opt in with `MAW_LIVE_OLLAMA=1`. Models: `MAW_LIVE_LEADER_MODEL` (default qwen3:4b),
//! `MAW_LIVE_WORKER_MODEL` (default qwen3:1.7b). Time limit: `MAW_LIVE_SECS` (default 600).
//! It reports; the only hard assertion is that the run did not crash.

use maw_core::{OrchestratorConfig, PermissionProfile, ProviderKind, Role};
use maw_orchestrator::{Engine, LineKind, Supervisor};
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use maw_storage::Store;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn git(dir: &std::path::Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: needs Ollama with two tool-capable models; set MAW_LIVE_OLLAMA=1"]
async fn live_leader_hands_a_task_to_a_local_worker() {
    if std::env::var("MAW_LIVE_OLLAMA").is_err() {
        return;
    }
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let leader_model = env("MAW_LIVE_LEADER_MODEL", "qwen3:4b");
    let worker_model = env("MAW_LIVE_WORKER_MODEL", "qwen3:1.7b");
    let secs: u64 = env("MAW_LIVE_SECS", "600").parse().unwrap_or(600);
    let ask = env(
        "MAW_LIVE_MESSAGE",
        "Give the worker named tester a simple task: create a file hello.txt that contains the word hi.",
    );

    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "# demo\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(
        repo.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ],
    );
    let engine = Engine::open(
        Arc::new(Store::open_in_memory().unwrap()),
        "live",
        &repo.path().to_string_lossy(),
        OrchestratorConfig::default(),
    )
    .unwrap();
    for (name, role, model) in [
        ("leader", Role::Leader, &leader_model),
        ("tester", Role::Worker, &worker_model),
    ] {
        engine
            .add_agent(
                name,
                ProviderKind::Ollama,
                role,
                Some(model.clone()),
                PermissionProfile::Balanced,
                None,
            )
            .unwrap();
    }
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(OllamaProvider::arc(OllamaConfig {
        idle_timeout: Duration::from_secs(900),
        ..Default::default()
    }));
    let mcp = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    sup.set_mcp_url(mcp.url.clone());
    let runner = tokio::spawn(sup.clone().run());
    sup.connect_all().await;

    sup.message_agent("leader", &ask, false).await.unwrap();
    let started = Instant::now();
    let mut last_report = String::new();
    while started.elapsed() < Duration::from_secs(secs) {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let tasks = engine.list_tasks().unwrap();
        let report = format!(
            "{:>4}s goal={} tasks=[{}]",
            started.elapsed().as_secs(),
            engine
                .current_goal()
                .unwrap()
                .map(|g| g.status.as_str().to_string())
                .unwrap_or_else(|| "none".into()),
            tasks
                .iter()
                .map(|t| format!("{} {} {}", t.key, t.status, t.title))
                .collect::<Vec<_>>()
                .join("; ")
        );
        if report != last_report {
            eprintln!("{report}");
            last_report = report;
        }
        let done = tasks.iter().any(|t| {
            matches!(
                t.status.as_str(),
                "VERIFYING" | "DONE" | "BLOCKED" | "FAILED"
            )
        });
        let busy = engine
            .list_agents()
            .unwrap()
            .iter()
            .any(|a| a.state.is_busy());
        if done && !busy {
            break;
        }
    }
    for a in engine.list_agents().unwrap() {
        eprintln!("--- {} ({}) {}", a.name, a.role, a.state);
        let lines = sup.transcript(&a.id);
        for l in lines.iter().rev().take(14).rev() {
            let k = format!("{:?}", l.kind);
            let text: String = l.text.replace('\n', " ").chars().take(170).collect();
            if l.kind != LineKind::Thinking {
                eprintln!("  {k:<9} {text}");
            }
        }
    }
    eprintln!(
        "files in the project: {:?}",
        std::fs::read_dir(repo.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect::<Vec<_>>()
    );
    sup.shutdown().await;
    runner.abort();
    mcp.stop().await;
}
