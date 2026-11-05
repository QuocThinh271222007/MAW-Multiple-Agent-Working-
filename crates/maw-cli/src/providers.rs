//! The only place that names concrete provider adapters.

use maw_core::provider::AgentProvider;
use std::sync::Arc;

pub fn codex(path: Option<String>) -> Arc<dyn AgentProvider> {
    let mut cfg = maw_provider_codex::CodexConfig::default();
    if let Some(p) = path {
        cfg.binary = p;
    }
    maw_provider_codex::CodexProvider::arc(cfg)
}

/// `state_dir`: where local conversations are kept so agents resume after a restart (`None` for
/// commands that never run an agent session).
pub fn ollama(
    c: &crate::config::OllamaSection,
    state_dir: Option<std::path::PathBuf>,
) -> Arc<dyn AgentProvider> {
    maw_provider_ollama::OllamaProvider::arc(maw_provider_ollama::OllamaConfig {
        state_dir,
        models_dir: c
            .models_dir
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| crate::platform::normalize_user_path(d, crate::platform::detect_os())),
        url: c.url.clone().filter(|u| !u.trim().is_empty()),
        num_ctx: c.num_ctx,
        num_ctx_leader: c.num_ctx_leader,
        keep_alive: c.keep_alive.clone(),
        max_steps: c.max_steps,
        command_timeout: std::time::Duration::from_secs(c.command_timeout_secs.max(5)),
        autostart: c.autostart,
        max_pull_params_b: c.max_pull_params_b,
        max_pull_bytes: (c.max_pull_gb * 1e9) as u64,
        binary: c.path.clone().filter(|p| !p.trim().is_empty()),
        think_budget_leader: c.think_budget_leader,
        think_budget_worker: c.think_budget_worker,
        think_budget_other: c.think_budget_other,
        ..Default::default()
    })
}

pub fn claude(path: Option<String>) -> Arc<dyn AgentProvider> {
    match path {
        Some(p) => Arc::new(maw_provider_claude::ClaudeProvider::with_binary(p)),
        None => Arc::new(maw_provider_claude::ClaudeProvider::new()),
    }
}
