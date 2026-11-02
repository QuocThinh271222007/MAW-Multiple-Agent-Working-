//! Interactive demo: a Leader, two Workers and a Reviewer driven by the deterministic mock
//! provider (NOT a real Claude/Codex run) against a throw-away git repository.
//!
//!     cargo run -p maw-tui --example tui_demo
//!
//! Press G to set the objective (e.g. "Implement auth"), then watch the team work. The demo
//! asks for one approval (press o, then y) and blocks once, so every view has something to show.

use maw_core::*;
use maw_orchestrator::mock::{MockProvider, MockScript};
use maw_orchestrator::{Engine, Supervisor};
use maw_storage::Store;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn git(dir: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let st = Command::new("git").arg("-C").arg(dir).args(args).status()?;
    anyhow::ensure!(st.success(), "git {args:?} failed");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("maw-tui-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    git(&dir, &["init", "-q", "-b", "main"])?;
    std::fs::write(dir.join("README.md"), "# demo project\n")?;
    git(&dir, &["add", "-A"])?;
    git(
        &dir,
        &[
            "-c",
            "user.name=demo",
            "-c",
            "user.email=demo@localhost",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    )?;

    let cfg = OrchestratorConfig {
        tick_ms: 250,
        leader_wake_debounce_ms: 300,
        ..Default::default()
    };
    let engine = Engine::open(
        Arc::new(Store::open_in_memory()?),
        "demo",
        &dir.to_string_lossy(),
        cfg,
    )?;
    for (name, role) in [
        ("claude-lead", Role::Leader),
        ("codex-backend", Role::Worker),
        ("codex-tests", Role::Worker),
        ("claude-review", Role::Reviewer),
    ] {
        engine.add_agent(
            name,
            ProviderKind::Mock,
            role,
            None,
            PermissionProfile::Balanced,
            None,
        )?;
    }
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(
        engine.clone(),
        MockScript {
            block_first_backend: true,
            reject_first_review: true,
            approval_demo: true,
            step_delay: Duration::from_millis(900),
        },
    ));
    sup.connect_all().await;
    tokio::spawn(sup.clone().run());

    let res = maw_tui::run(engine, sup, maw_tui::TuiOptions::default()).await;
    let _ = std::fs::remove_dir_all(&dir);
    res
}
