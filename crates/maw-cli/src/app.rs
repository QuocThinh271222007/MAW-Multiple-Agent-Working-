//! Process bootstrap shared by every command: project resolution, store/engine/supervisor
//! wiring, provider registration and the in-process MCP server.

use crate::config::{Dirs, MawConfig, Registry};
use crate::platform::{self, Os};
use anyhow::{bail, Context, Result};
use maw_core::provider::AgentProvider;
use maw_mcp::McpHttpServer;
use maw_orchestrator::mock::{MockProvider, MockScript};
use maw_orchestrator::{Engine, RecoveryReport, Supervisor};
use maw_storage::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Ctx {
    pub dirs: Dirs,
    pub cfg: MawConfig,
    pub os: Os,
}

impl Ctx {
    pub fn load() -> Result<Ctx> {
        let dirs = Dirs::discover()?;
        let cfg = MawConfig::load(&dirs)?;
        Ok(Ctx {
            dirs,
            cfg,
            os: platform::detect_os(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub name: String,
    pub path: PathBuf,
}

/// Resolve `--project`: a registered name, a path, or (None) the current directory.
/// Unknown paths are registered automatically so `maw` just works inside a repo.
pub fn resolve_project(ctx: &Ctx, arg: Option<&str>, register: bool) -> Result<ProjectRef> {
    let mut reg = Registry::load(&ctx.dirs);
    if let Some(a) = arg {
        if let Some(p) = reg.projects.get(a) {
            return Ok(ProjectRef {
                name: a.to_string(),
                path: PathBuf::from(p),
            });
        }
    }
    let path = match arg {
        Some(a) => platform::normalize_user_path(a, ctx.os),
        None => std::env::current_dir().context("cannot determine the current directory")?,
    };
    let path = std::fs::canonicalize(&path)
        .with_context(|| format!("project path {} does not exist", path.display()))?;
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let name = match reg.find_by_path(&path) {
        Some(n) => n,
        None => {
            let n = reg.add(&path, None);
            if register {
                reg.save(&ctx.dirs)?;
            }
            n
        }
    };
    Ok(ProjectRef { name, path })
}

pub fn open_store(ctx: &Ctx, p: &ProjectRef) -> Result<Arc<Store>> {
    std::fs::create_dir_all(ctx.dirs.project_dir(&p.name))?;
    Ok(Arc::new(
        Store::open(ctx.dirs.db_path(&p.name)).map_err(|e| anyhow::anyhow!("{e}"))?,
    ))
}

/// Engine for short-lived commands (no run recorded).
pub fn open_utility_engine(ctx: &Ctx, p: &ProjectRef) -> Result<Arc<Engine>> {
    let store = open_store(ctx, p)?;
    Engine::open_utility(
        store,
        &p.name,
        &p.path.to_string_lossy(),
        ctx.cfg.orchestrator.clone(),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

pub struct Runtime {
    pub engine: Arc<Engine>,
    pub sup: Arc<Supervisor>,
    pub mcp: McpHttpServer,
    pub recovery: RecoveryReport,
    runner: tokio::task::JoinHandle<()>,
}

pub enum Providers {
    /// Real Claude Code / Codex adapters.
    Real,
    /// Deterministic scripted agents (tests, `maw demo`). Never real evidence.
    Mock(MockScript),
}

/// Real provider adapters from configuration.
pub fn real_providers(
    cfg: &MawConfig,
    ollama_state: Option<std::path::PathBuf>,
) -> Vec<Arc<dyn AgentProvider>> {
    vec![
        crate::providers::codex(cfg.codex.path.clone()),
        crate::providers::claude(cfg.claude.path.clone()),
        crate::providers::ollama(&cfg.ollama, ollama_state),
    ]
}

pub async fn boot(
    ctx: &Ctx,
    p: &ProjectRef,
    providers: Providers,
    connect: bool,
) -> Result<Runtime> {
    let store = open_store(ctx, p)?;
    let engine = Engine::open(
        store,
        &p.name,
        &p.path.to_string_lossy(),
        ctx.cfg.orchestrator.clone(),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let recovery = engine
        .recover()
        .map_err(|e| anyhow::anyhow!("recovery failed: {e}"))?;
    let sup = Supervisor::new(engine.clone());
    match providers {
        Providers::Real => {
            let state = ctx.dirs.project_dir(&p.name).join("ollama-sessions");
            for pr in real_providers(&ctx.cfg, Some(state)) {
                sup.register_provider(pr);
            }
        }
        Providers::Mock(script) => {
            sup.register_provider(MockProvider::new(engine.clone(), script));
            // real providers are consulted for their model lists only (the demo never runs them)
            for pr in real_providers(&ctx.cfg, None) {
                sup.register_catalog_provider(pr);
            }
        }
    }
    // Always allow mock agents alongside real ones only when explicitly requested above.
    let mcp = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse()?).await?;
    sup.set_mcp_url(mcp.url.clone());
    let runner = tokio::spawn(sup.clone().run());
    if connect {
        let s = sup.clone();
        tokio::spawn(async move { s.connect_all().await });
    }
    tracing::info!(project = %p.name, mcp = %mcp.url, "MAW runtime started");
    Ok(Runtime {
        engine,
        sup,
        mcp,
        recovery,
        runner,
    })
}

impl Runtime {
    pub async fn shutdown(self) {
        self.sup.shutdown().await;
        self.runner.abort();
        self.mcp.stop().await;
    }
}

fn runtime_file(ctx: &Ctx, p: &ProjectRef) -> PathBuf {
    ctx.dirs.project_dir(&p.name).join("runtime.json")
}

/// Record that this process is driving the project (pid + MCP URL, never tokens).
pub fn write_runtime_info(ctx: &Ctx, p: &ProjectRef, rt: &Runtime) {
    let info = serde_json::json!({ "pid": std::process::id(), "mcp_url": rt.mcp.url, "started_at": chrono::Utc::now().to_rfc3339() });
    let _ = std::fs::write(runtime_file(ctx, p), info.to_string());
}

pub fn clear_runtime_info(ctx: &Ctx, p: &ProjectRef) {
    let _ = std::fs::remove_file(runtime_file(ctx, p));
}

/// The running process info, if its pid is still alive.
pub fn read_runtime_info(ctx: &Ctx, p: &ProjectRef) -> Option<serde_json::Value> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(runtime_file(ctx, p)).ok()?).ok()?;
    let pid = v["pid"].as_u64()?;
    let alive = if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true
    };
    alive.then_some(v)
}

pub fn is_git_repo(path: &Path) -> bool {
    maw_orchestrator::workspace::is_git_repo(path)
}
