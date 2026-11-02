//! Claude Code provider adapter.
//!
//! Transport: the officially supported CLI structured-streaming interface
//! (`claude -p --input-format stream-json --output-format stream-json`), one long-lived process
//! per agent. Authentication is whatever the installed, unmodified Claude Code is logged in with
//! (`claude auth login`); MAW never reads, stores or proxies Claude credentials.
//!
//! See `docs/providers/claude.md` for the verified protocol details and the permission model.

pub mod args;
pub mod mapper;
mod session;

pub use args::{permission_flags, PermissionFlags, SessionArg, MCP_SERVER_NAME, MCP_TOKEN_ENV};
pub use session::ClaudeSession;

use async_trait::async_trait;
use maw_core::agent::ProviderKind;
use maw_core::provider::*;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;

/// Version this adapter was verified against. Other versions are allowed but reported.
pub const TESTED_VERSION: &str = "2.1.309";
/// Environment variable overriding the executable (used by tests and non-PATH installs).
pub const BIN_ENV: &str = "MAW_CLAUDE_BIN";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct ClaudeProvider {
    bin: PathBuf,
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeProvider {
    /// Uses `$MAW_CLAUDE_BIN` if set, otherwise `claude` from `PATH`.
    pub fn new() -> Self {
        let bin = std::env::var_os(BIN_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("claude"));
        ClaudeProvider { bin }
    }

    pub fn with_binary(bin: impl Into<PathBuf>) -> Self {
        ClaudeProvider { bin: bin.into() }
    }

    pub fn binary(&self) -> &Path {
        &self.bin
    }

    async fn run_probe(&self, args: &[&str]) -> ProviderResult<std::process::Output> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());
        let fut = cmd.output();
        match tokio::time::timeout(PROBE_TIMEOUT, fut).await {
            Err(_) => Err(ProviderError::Timeout(format!(
                "`{} {}` did not finish in {:?}",
                self.bin.display(),
                args.join(" "),
                PROBE_TIMEOUT
            ))),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(ProviderError::NotInstalled(format!(
                    "{} not found; install Claude Code and make sure it is on PATH",
                    self.bin.display()
                )))
            }
            Ok(Err(e)) => Err(ProviderError::Io(e.to_string())),
            Ok(Ok(o)) => Ok(o),
        }
    }
}

/// First dotted-number token of `claude --version` output ("2.1.309 (Claude Code)").
pub fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.'))
        .map(String::from)
}

/// Interpret `claude auth status` JSON.
pub fn parse_auth_status(stdout: &str, exit_ok: bool) -> AuthStatus {
    match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(v) => {
            if v.get("loggedIn").and_then(Value::as_bool) == Some(true) {
                AuthStatus::Authenticated {
                    method: v
                        .get("authMethod")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    account: v.get("email").and_then(Value::as_str).map(String::from),
                    plan: v
                        .get("subscriptionType")
                        .and_then(Value::as_str)
                        .map(String::from),
                }
            } else {
                AuthStatus::NotAuthenticated { hint: "run `maw auth claude` (or `claude auth login`) to sign in with the official Claude Code flow".into() }
            }
        }
        Err(e) => AuthStatus::Unknown {
            reason: format!(
                "could not parse `claude auth status` output (exit ok: {exit_ok}): {e}"
            ),
        },
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    async fn detect(&self) -> ProviderResult<ProviderInfo> {
        let mut info = ProviderInfo {
            kind: ProviderKind::Claude,
            installed: false,
            executable: None,
            version: None,
            execution_modes: vec!["cli-stream-json".into()],
            notes: vec![format!(
                "adapter verified against Claude Code {TESTED_VERSION}"
            )],
        };
        match self.run_probe(&["--version"]).await {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                info.installed = true;
                info.executable = Some(self.bin.to_string_lossy().to_string());
                info.version = parse_version(&text);
                match info.version.as_deref() {
                    Some(v) if v != TESTED_VERSION => info.notes.push(format!("installed version {v} differs from the tested {TESTED_VERSION}; stream-json details may differ")),
                    None => info.notes.push("could not parse the version output".into()),
                    _ => {}
                }
            }
            Ok(o) => info.notes.push(format!(
                "`--version` failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(ProviderError::NotInstalled(m)) => info.notes.push(m),
            Err(e) => return Err(e),
        }
        Ok(info)
    }

    async fn auth_status(&self) -> ProviderResult<AuthStatus> {
        // The CLI reports its own login state; MAW never touches credential files.
        let o = self.run_probe(&["auth", "status", "--json"]).await?;
        Ok(parse_auth_status(
            &String::from_utf8_lossy(&o.stdout),
            o.status.success(),
        ))
    }

    async fn start_session(
        &self,
        cfg: SessionConfig,
    ) -> ProviderResult<(SessionHandle, EventReceiver)> {
        if !cfg.cwd.is_dir() {
            return Err(ProviderError::Other(format!(
                "working directory {} does not exist",
                cfg.cwd.display()
            )));
        }
        let arg = match cfg.resume_native_id.as_deref() {
            Some(id) if args::looks_like_uuid(id) => SessionArg::Resume(id.to_string()),
            Some(id) => {
                tracing::warn!("ignoring non-UUID resume id '{id}'");
                SessionArg::New(uuid::Uuid::new_v4().to_string())
            }
            None => SessionArg::New(uuid::Uuid::new_v4().to_string()),
        };
        // Spawn here so a missing executable surfaces as a start error rather than a later disconnect.
        let running = session::spawn_process(&self.bin, &cfg, &arg)?;
        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let native = arg.id().to_string();
        let session = ClaudeSession::start(self.bin.clone(), cfg, arg, running, tx.clone());
        if tx
            .send(ProviderEvent::Connected {
                native_session_id: Some(native),
            })
            .await
            .is_err()
        {
            return Err(ProviderError::SessionClosed(
                "event receiver dropped".into(),
            ));
        }
        Ok((session, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_version("2.1.309 (Claude Code)\n").as_deref(),
            Some("2.1.309")
        );
        assert_eq!(parse_version("claude 3.0.1"), Some("3.0.1".into()));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn auth_status_parsing() {
        let ok = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"a@b.c","subscriptionType":"pro"}"#;
        assert_eq!(
            parse_auth_status(ok, true),
            AuthStatus::Authenticated {
                method: "claude.ai".into(),
                account: Some("a@b.c".into()),
                plan: Some("pro".into())
            }
        );
        assert!(matches!(
            parse_auth_status(r#"{"loggedIn":false}"#, false),
            AuthStatus::NotAuthenticated { .. }
        ));
        assert!(matches!(
            parse_auth_status("not json", false),
            AuthStatus::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn missing_binary_is_reported_not_panicked() {
        let p = ClaudeProvider::with_binary("/definitely/not/claude");
        let info = p.detect().await.unwrap();
        assert!(!info.installed && !info.notes.is_empty());
        assert!(matches!(
            p.auth_status().await,
            Err(ProviderError::NotInstalled(_))
        ));
    }
}
