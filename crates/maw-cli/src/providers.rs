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

pub fn claude(path: Option<String>) -> Arc<dyn AgentProvider> {
    match path {
        Some(p) => Arc::new(maw_provider_claude::ClaudeProvider::with_binary(p)),
        None => Arc::new(maw_provider_claude::ClaudeProvider::new()),
    }
}
