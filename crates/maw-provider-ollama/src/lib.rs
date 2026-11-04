//! Local-model provider: coding agents backed by an Ollama server.
//!
//! Unlike the Codex and Claude adapters this one wraps no agent CLI. Ollama serves models over
//! HTTP, so MAW runs the agent loop itself (see `session`). No account, API key or internet is
//! needed. Limits are listed in `docs/providers/ollama.md`.

pub mod api;
pub mod catalog;
mod config;
pub mod history;
pub mod mcp;
pub mod persist;
mod provider;
pub mod server;
pub mod session;
pub mod tools;

pub use config::OllamaConfig;
pub use provider::OllamaProvider;
