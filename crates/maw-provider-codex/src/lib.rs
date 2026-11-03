//! MAW provider adapter for the OpenAI Codex App Server (`codex app-server`, stdio JSON-RPC).
//!
//! One `codex app-server` child process is owned per agent session. See
//! `docs/providers/codex.md` for the tested version, methods used and known limitations.

pub mod protocol;
mod session;

pub use session::CodexSession;

use async_trait::async_trait;
use maw_core::provider::*;
use maw_core::ProviderKind;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use session::Shared;

/// How the MAW MCP server is handed to Codex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpInjection {
    /// `thread/start.config` (per-thread, no argv).
    ThreadConfig,
    /// `codex app-server -c mcp_servers.maw.*=...` (per-process; the token still comes from the environment).
    CommandLine,
}

#[derive(Clone, Debug)]
pub struct CodexConfig {
    /// Path or name of the `codex` executable (default: `codex` on PATH, or `$MAW_CODEX_BIN`).
    pub binary: String,
    /// Append every raw server line (redacted) to this file; for contract fixtures and diagnostics.
    pub capture_path: Option<PathBuf>,
    pub request_timeout: Duration,
    pub mcp_injection: McpInjection,
    /// Let tools of MAW's own `maw` MCP server run without Codex asking per call
    /// (`mcp_servers.maw.default_tools_approval_mode = "approve"`). Default `true`; other MCP
    /// servers are never affected. When `false`, each non-read-only call surfaces as an
    /// `ApprovalRequested` for the user instead.
    pub preapprove_maw_tools: bool,
}

impl Default for CodexConfig {
    fn default() -> Self {
        CodexConfig {
            binary: std::env::var("MAW_CODEX_BIN").unwrap_or_else(|_| "codex".to_string()),
            capture_path: None,
            request_timeout: Duration::from_secs(60),
            mcp_injection: McpInjection::ThreadConfig,
            preapprove_maw_tools: true,
        }
    }
}

pub struct CodexProvider {
    cfg: CodexConfig,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new(CodexConfig::default())
    }
}

impl CodexProvider {
    pub fn new(cfg: CodexConfig) -> Self {
        CodexProvider { cfg }
    }

    pub fn arc(cfg: CodexConfig) -> Arc<CodexProvider> {
        Arc::new(Self::new(cfg))
    }

    async fn run_cli(&self, args: &[&str]) -> ProviderResult<(bool, String)> {
        let out = Command::new(&self.cfg.binary)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    ProviderError::NotInstalled(self.cfg.binary.clone())
                }
                _ => ProviderError::Io(e.to_string()),
            })?;
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.success(), text.trim().to_string()))
    }
}

/// Parse `codex --version` output such as `codex-cli 0.166.0`.
pub fn parse_version(out: &str) -> Option<String> {
    out.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(String::from)
}

/// Interpret `codex login status` output.
pub fn parse_login_status(success: bool, out: &str) -> AuthStatus {
    let lower = out.to_lowercase();
    if success && lower.contains("logged in") {
        let method = if lower.contains("chatgpt") {
            "chatgpt"
        } else if lower.contains("api key") {
            "api_key"
        } else {
            "unknown"
        };
        AuthStatus::Authenticated {
            method: method.into(),
            account: None,
            plan: None,
        }
    } else if lower.contains("not logged in") || !success {
        AuthStatus::NotAuthenticated {
            hint: "run `codex login` (or `maw auth codex`) in a terminal".into(),
        }
    } else {
        AuthStatus::Unknown {
            reason: format!(
                "unrecognised `codex login status` output: {}",
                out.chars().take(120).collect::<String>()
            ),
        }
    }
}

#[async_trait]
impl AgentProvider for CodexProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    /// Live query: Codex reports the models available to the logged-in account via `model/list`.
    async fn list_models(&self) -> ProviderResult<Vec<ModelChoice>> {
        let mut child = Command::new(&self.cfg.binary)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ProviderError::NotInstalled(self.cfg.binary.clone())
                } else {
                    ProviderError::Io(e.to_string())
                }
            })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::Io("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Io("no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        let timeout = self.cfg.request_timeout;

        async fn send(stdin: &mut tokio::process::ChildStdin, v: Value) -> ProviderResult<()> {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(format!("{v}\n").as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        }
        async fn reply(
            lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
            id: i64,
        ) -> ProviderResult<Value> {
            while let Some(line) = lines.next_line().await? {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v.get("id").and_then(Value::as_i64) == Some(id) {
                        if let Some(err) = v.get("error") {
                            return Err(ProviderError::Protocol(err.to_string()));
                        }
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                }
            }
            Err(ProviderError::SessionClosed(
                "codex app-server exited".into(),
            ))
        }

        let result = tokio::time::timeout(timeout, async {
            send(&mut stdin, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"maw","title":"MAW","version":env!("CARGO_PKG_VERSION")}}})).await?;
            reply(&mut lines, 1).await?;
            send(&mut stdin, json!({"method":"initialized"})).await?;
            send(&mut stdin, json!({"id":2,"method":"model/list","params":{}})).await?;
            reply(&mut lines, 2).await
        })
        .await
        .map_err(|_| ProviderError::Timeout("model/list".into()))??;
        let _ = child.kill().await;

        let models = result
            .get("data")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|m| !m.get("hidden").and_then(Value::as_bool).unwrap_or(false))
                    .filter_map(|m| {
                        let id = m
                            .get("model")
                            .or_else(|| m.get("id"))?
                            .as_str()?
                            .to_string();
                        Some(ModelChoice {
                            display_name: m
                                .get("displayName")
                                .and_then(Value::as_str)
                                .unwrap_or(&id)
                                .to_string(),
                            description: m
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            is_default: m
                                .get("isDefault")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            source: "provider".into(),
                            reasoning_efforts: m
                                .get("supportedReasoningEfforts")
                                .and_then(Value::as_array)
                                .map(|e| {
                                    e.iter()
                                        .filter_map(|x| {
                                            x.get("reasoningEffort")
                                                .and_then(Value::as_str)
                                                .map(String::from)
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            id,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(models)
    }

    async fn detect(&self) -> ProviderResult<ProviderInfo> {
        match self.run_cli(&["--version"]).await {
            Ok((true, out)) => Ok(ProviderInfo {
                kind: ProviderKind::Codex,
                installed: true,
                executable: Some(self.cfg.binary.clone()),
                version: parse_version(&out),
                execution_modes: vec!["app-server (stdio JSON-RPC)".into()],
                notes: vec!["App Server is marked experimental by the Codex CLI; the protocol may change between releases".into()],
            }),
            Ok((false, out)) => Ok(ProviderInfo {
                kind: ProviderKind::Codex,
                installed: true,
                executable: Some(self.cfg.binary.clone()),
                version: None,
                execution_modes: vec![],
                notes: vec![format!("`codex --version` failed: {out}")],
            }),
            Err(ProviderError::NotInstalled(b)) => Ok(ProviderInfo {
                kind: ProviderKind::Codex,
                installed: false,
                executable: None,
                version: None,
                execution_modes: vec![],
                notes: vec![format!("`{b}` not found on PATH; install Codex CLI (npm i -g @openai/codex) or set MAW_CODEX_BIN")],
            }),
            Err(e) => Err(e),
        }
    }

    async fn auth_status(&self) -> ProviderResult<AuthStatus> {
        let (ok, out) = self.run_cli(&["login", "status"]).await?;
        Ok(parse_login_status(ok, &out))
    }

    async fn start_session(
        &self,
        cfg: SessionConfig,
    ) -> ProviderResult<(SessionHandle, EventReceiver)> {
        let mut cmd = Command::new(&self.cfg.binary);
        cmd.arg("app-server");
        if self.cfg.mcp_injection == McpInjection::CommandLine {
            if let Some(e) = &cfg.mcp {
                cmd.arg("-c").arg(format!(
                    "mcp_servers.{}.url={}",
                    protocol::MCP_SERVER_NAME,
                    json!(e.url)
                ));
                cmd.arg("-c").arg(format!(
                    "mcp_servers.{}.bearer_token_env_var={}",
                    protocol::MCP_SERVER_NAME,
                    json!(protocol::MCP_TOKEN_ENV)
                ));
                if self.cfg.preapprove_maw_tools {
                    cmd.arg("-c").arg(format!(
                        "mcp_servers.{}.{}={}",
                        protocol::MCP_SERVER_NAME,
                        protocol::PREAPPROVE_KEY,
                        json!(protocol::PREAPPROVE_VALUE)
                    ));
                }
            }
        }
        cmd.current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        if let Some(e) = &cfg.mcp {
            // The bearer token lives only in the child's environment: never in argv or on disk.
            cmd.env(protocol::MCP_TOKEN_ENV, &e.token);
        }
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ProviderError::NotInstalled(self.cfg.binary.clone()),
            _ => ProviderError::Io(format!("failed to start codex app-server: {e}")),
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::Io("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Io("no stdout".into()))?;
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel(PROVIDER_EVENT_BUFFER);
        let shared = Shared::new(
            tx,
            stdin,
            child,
            self.cfg.capture_path.clone(),
            self.cfg.request_timeout,
        );
        shared.set_effort(cfg.effort.clone());
        tokio::spawn(shared.clone().run_reader(stdout));
        if let Some(stderr) = stderr {
            let sh = shared.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    sh.record_stderr(&l);
                }
                sh.mark_stderr_done();
            });
        } else {
            shared.mark_stderr_done();
        }

        match handshake(
            &shared,
            &cfg,
            self.cfg.mcp_injection == McpInjection::ThreadConfig,
            self.cfg.preapprove_maw_tools,
        )
        .await
        {
            Ok(thread_id) => {
                shared
                    .emit(ProviderEvent::Connected {
                        native_session_id: Some(thread_id.clone()),
                    })
                    .await;
                let session = Arc::new(CodexSession {
                    shared,
                    native_id: Some(thread_id),
                });
                Ok((session, rx))
            }
            Err(e) => {
                shared.terminate().await;
                Err(e)
            }
        }
    }
}

/// initialize -> initialized -> thread/resume|start. Returns the thread id.
async fn handshake(
    shared: &Arc<Shared>,
    cfg: &SessionConfig,
    thread_config: bool,
    preapprove: bool,
) -> ProviderResult<String> {
    shared
        .request(
            "initialize",
            json!({
                "clientInfo": { "name": "maw", "title": "MAW - Multiple Agent Working", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true },
            }),
        )
        .await
        .map_err(auth_hint)?;
    shared.notify("initialized", None).await?;

    let params = protocol::thread_params(cfg, thread_config, preapprove);
    if let Some(native) = &cfg.resume_native_id {
        let mut p = params.clone();
        p["threadId"] = json!(native);
        match shared.request("thread/resume", p).await {
            Ok(resp) => {
                if let Some(id) = thread_id_of(&resp) {
                    shared.set_thread(id.clone());
                    return Ok(id);
                }
                shared
                    .emit(ProviderEvent::Error {
                        message: "thread/resume returned no thread id; starting a fresh thread"
                            .into(),
                        fatal: false,
                    })
                    .await;
            }
            Err(e) => {
                shared
                    .emit(ProviderEvent::Error {
                        message: format!(
                            "could not resume Codex thread {native}: {e}; starting a fresh thread"
                        ),
                        fatal: false,
                    })
                    .await;
            }
        }
    }
    let resp = shared
        .request("thread/start", params)
        .await
        .map_err(auth_hint)?;
    let id = thread_id_of(&resp)
        .ok_or_else(|| ProviderError::Protocol("thread/start response has no thread id".into()))?;
    shared.set_thread(id.clone());
    Ok(id)
}

fn thread_id_of(resp: &Value) -> Option<String> {
    resp.pointer("/thread/id")
        .and_then(Value::as_str)
        .map(String::from)
}

/// Surface authentication failures as `AuthRequired` so MAW shows the right state.
fn auth_hint(e: ProviderError) -> ProviderError {
    let t = e.to_string().to_lowercase();
    if t.contains("auth")
        && (t.contains("required")
            || t.contains("log in")
            || t.contains("login")
            || t.contains("unauthorized"))
    {
        ProviderError::AuthRequired(e.to_string())
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_and_login_output() {
        assert_eq!(parse_version("codex-cli 0.166.0"), Some("0.166.0".into()));
        assert_eq!(parse_version("garbage"), None);
        assert!(
            matches!(parse_login_status(true, "Logged in using ChatGPT"), AuthStatus::Authenticated { ref method, .. } if method == "chatgpt")
        );
        assert!(
            matches!(parse_login_status(true, "Logged in using an API key - sk-***"), AuthStatus::Authenticated { ref method, .. } if method == "api_key")
        );
        assert!(matches!(
            parse_login_status(false, "Not logged in"),
            AuthStatus::NotAuthenticated { .. }
        ));
        assert!(matches!(
            parse_login_status(true, "???"),
            AuthStatus::Unknown { .. }
        ));
    }
}
