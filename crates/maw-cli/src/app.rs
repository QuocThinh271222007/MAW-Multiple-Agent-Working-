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

/// The folders where agent worktrees can live besides the project itself: on the Linux filesystem
/// of WSL, or on a Windows drive. Each is the folder or the reason it cannot be used.
pub fn worktree_bases(
    ctx: &Ctx,
    p: &ProjectRef,
) -> (Result<PathBuf, String>, Result<PathBuf, String>) {
    let wsl = match ctx.os {
        Os::Wsl | Os::Linux => Ok(ctx.dirs.project_dir(&p.name).join("worktrees")),
        _ => Err("MAW is not running on Linux or in WSL".to_string()),
    };
    let windows = if ctx.os != Os::Wsl {
        Err("MAW is not running in WSL, so there is no Windows drive to reach".to_string())
    } else {
        let configured = ctx.cfg.orchestrator.windows_worktree_dir.trim().to_string();
        let dir = if configured.is_empty() {
            platform::windows_home()
                .map(|h| h.join("MAW"))
                .ok_or_else(|| {
                    "could not find your Windows profile folder; set orchestrator.windows_worktree_dir".to_string()
                })
        } else {
            Ok(platform::normalize_user_path(&configured, ctx.os))
        };
        dir.and_then(|d| {
            if d.starts_with("/mnt/") {
                Ok(d.join(crate::config::sanitize(&p.name)).join("worktrees"))
            } else {
                Err(format!(
                    "{} is not on a Windows drive (expected /mnt/<drive>/... or C:\\...)",
                    d.display()
                ))
            }
        })
    };
    (wsl, windows)
}

/// Whether `path` may be offered to agents as a place they can write besides their worktree. The
/// check is on the real path (symlinks resolved). Disks, system folders, the home folder itself and
/// credential folders are refused: the list is meant for a download or output folder, not for
/// handing over the machine.
pub fn check_extra_dir(path: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    let real = std::fs::canonicalize(path)
        .map_err(|e| format!("{} cannot be used: {e}", path.display()))?;
    if !real.is_dir() {
        return Err(format!("{} is not a folder", real.display()));
    }
    let comps: Vec<String> = real
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(x) => Some(x.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    let first = comps.first().map(String::as_str).unwrap_or("");
    let refuse = |why: &str| Err(format!("{} is refused: {why}", real.display()));
    if comps.is_empty() {
        return refuse("that is the whole disk");
    }
    if first == "mnt" && comps.len() <= 2 {
        return refuse("that is a whole drive or the drives folder; pick a folder inside it");
    }
    if let Some(h) = home.and_then(|h| std::fs::canonicalize(h).ok()) {
        if real == h || h.starts_with(&real) {
            return refuse(
                "that is your home folder (or a folder that contains it); pick a subfolder",
            );
        }
    }
    const SYSTEM: &[&str] = &[
        "etc", "usr", "bin", "sbin", "lib", "lib32", "lib64", "boot", "dev", "proc", "sys", "root",
        "var", "run", "home",
    ];
    if SYSTEM.contains(&first) && !(first == "home" && comps.len() >= 3) {
        return refuse("that is a system folder");
    }
    if first == "mnt" && comps.len() >= 3 {
        let top = comps[2].to_lowercase();
        if [
            "windows",
            "program files",
            "program files (x86)",
            "programdata",
            "$recycle.bin",
        ]
        .contains(&top.as_str())
        {
            return refuse("that is a Windows system folder");
        }
    }
    if comps
        .iter()
        .any(|c| matches!(c.as_str(), ".ssh" | ".gnupg" | ".aws" | ".kube"))
    {
        return refuse("that holds credentials");
    }
    Ok(real)
}

/// The folders from `orchestrator.extra_dirs` that pass `check_extra_dir`, and why the others do not.
pub fn allowed_extra_dirs(ctx: &Ctx) -> (Vec<PathBuf>, Vec<String>) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (mut ok, mut bad) = (vec![], vec![]);
    for raw in &ctx.cfg.orchestrator.extra_dirs {
        match check_extra_dir(&platform::normalize_user_path(raw, ctx.os), home.as_deref()) {
            Ok(p) if !ok.contains(&p) => ok.push(p),
            Ok(_) => {}
            Err(e) => bad.push(e),
        }
    }
    (ok, bad)
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
    let (wsl_base, windows_base) = worktree_bases(ctx, p);
    sup.set_worktree_bases(wsl_base, windows_base);
    let (dirs, problems) = allowed_extra_dirs(ctx);
    for why in problems {
        tracing::warn!("ignoring an entry of orchestrator.extra_dirs: {why}");
    }
    sup.set_extra_dirs(dirs);
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

#[cfg(test)]
mod extra_dir_tests {
    use super::*;

    #[test]
    fn only_a_real_output_folder_can_be_offered_to_agents() {
        let home = tempfile::tempdir().unwrap();
        let ok = home.path().join("download");
        std::fs::create_dir(&ok).unwrap();
        assert!(check_extra_dir(&ok, Some(home.path())).is_ok());
        // refused: the disk, system folders, the home folder, credentials, plain files, missing paths
        for bad in ["/", "/etc", "/usr/bin", "/proc", "/root"] {
            let e = check_extra_dir(Path::new(bad), Some(home.path()));
            assert!(e.is_err(), "{bad} should be refused");
        }
        assert!(
            check_extra_dir(home.path(), Some(home.path())).is_err(),
            "the home folder itself"
        );
        let ssh = home.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        assert!(check_extra_dir(&ssh, Some(home.path()))
            .unwrap_err()
            .contains("credentials"));
        let file = home.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(check_extra_dir(&file, Some(home.path())).is_err());
        assert!(check_extra_dir(&home.path().join("missing"), Some(home.path())).is_err());
        // a folder that contains the home folder is as bad as the home folder
        assert!(check_extra_dir(home.path().parent().unwrap(), Some(home.path())).is_err());
    }
}
