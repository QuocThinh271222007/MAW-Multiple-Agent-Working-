use crate::api::{self, ModelInfo};
use crate::catalog;
use crate::config::OllamaConfig;
use crate::mcp::McpConnection;
use crate::server;
use crate::session::{OllamaSession, SessionParams};
use crate::tools::LocalTools;
use async_trait::async_trait;
use futures::StreamExt;
use maw_core::agent::ProviderKind;
use maw_core::provider::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub struct OllamaProvider {
    cfg: OllamaConfig,
    http: reqwest::Client,
    resolved: Mutex<Option<String>>,
    start_lock: tokio::sync::Mutex<()>,
    speed: crate::session::SpeedCell,
}

fn fmt_bytes(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1_073_741_824.0)
}

impl OllamaProvider {
    pub fn new(cfg: OllamaConfig) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        OllamaProvider {
            cfg,
            http,
            resolved: Mutex::new(None),
            start_lock: tokio::sync::Mutex::new(()),
            speed: Default::default(),
        }
    }

    pub fn arc(cfg: OllamaConfig) -> Arc<dyn AgentProvider> {
        Arc::new(Self::new(cfg))
    }

    /// The server URL in use, probing the candidates on first use (and again after a failure).
    pub async fn base_url(&self) -> Option<String> {
        if let Some(u) = self.resolved.lock().ok().and_then(|g| g.clone()) {
            if self.version_at(&u).await.is_some() {
                return Some(u);
            }
        }
        for u in api::candidate_urls(self.cfg.url.as_deref()) {
            if self.version_at(&u).await.is_some() {
                if let Ok(mut g) = self.resolved.lock() {
                    *g = Some(u.clone());
                }
                return Some(u);
            }
        }
        None
    }

    /// The server URL, starting `ollama serve` first when nothing answers and autostart is on.
    /// Only agent sessions call this; listing models or `doctor` never start anything.
    pub async fn ensure_server(&self) -> Result<String, ProviderError> {
        if let Some(u) = self.base_url().await {
            return Ok(u);
        }
        if !self.cfg.autostart {
            return Err(ProviderError::NotInstalled(
                "no Ollama server reachable and autostart is off; start it with `ollama serve`"
                    .into(),
            ));
        }
        let _guard = self.start_lock.lock().await;
        if let Some(u) = self.base_url().await {
            return Ok(u); // another session started it while we waited
        }
        let bin = server::find_binary(self.cfg.binary.as_deref()).ok_or_else(|| {
            ProviderError::NotInstalled(
                "Ollama is not installed (no `ollama` on PATH or in the usual install folders); install it from https://ollama.com".into(),
            )
        })?;
        let host = self.serve_host(&bin);
        tracing::info!(binary = %bin.path.display(), host = ?host, "starting ollama serve");
        server::spawn_serve(&bin, host.as_deref()).map_err(|e| {
            ProviderError::Io(format!("could not start {}: {e}", bin.path.display()))
        })?;
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Some(u) = self.base_url().await {
                return Ok(u);
            }
        }
        Err(ProviderError::Timeout(format!(
            "started {} but no Ollama server answered within 45 s",
            bin.path.display()
        )))
    }

    /// `OLLAMA_HOST` for a server MAW starts: the configured address, or for a Windows Ollama
    /// launched from WSL the Windows side of the WSL network (the only address WSL can reach).
    fn serve_host(&self, bin: &server::OllamaBinary) -> Option<String> {
        if let Some(u) = self.cfg.url.as_deref().and_then(api::normalize_url) {
            return u.split_once("://").map(|(_, h)| h.to_string());
        }
        if bin.windows_from_wsl {
            let t = std::fs::read_to_string("/proc/net/route").ok()?;
            let g = api::gateway_from_route(&t)?;
            return Some(format!("{g}:{}", api::DEFAULT_PORT));
        }
        None
    }

    async fn version_at(&self, base: &str) -> Option<String> {
        let r = tokio::time::timeout(
            Duration::from_millis(2500),
            self.http.get(format!("{base}/api/version")).send(),
        )
        .await
        .ok()?
        .ok()?;
        let v: Value = r.json().await.ok()?;
        v.get("version").and_then(Value::as_str).map(String::from)
    }

    async fn get_json(&self, base: &str, path: &str) -> Result<Value, String> {
        let r = tokio::time::timeout(
            Duration::from_secs(8),
            self.http.get(format!("{base}{path}")).send(),
        )
        .await
        .map_err(|_| format!("{path} timed out"))?
        .map_err(|e| e.to_string())?;
        r.json().await.map_err(|e| e.to_string())
    }

    async fn show(&self, base: &str, model: &str) -> Result<ModelInfo, String> {
        let r = tokio::time::timeout(
            Duration::from_secs(8),
            self.http
                .post(format!("{base}/api/show"))
                .json(&serde_json::json!({ "model": model }))
                .send(),
        )
        .await
        .map_err(|_| "show timed out".to_string())?
        .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("{}", r.status()));
        }
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        Ok(api::parse_show(&v))
    }

    /// Names of the models installed on the server.
    pub async fn installed(&self, base: &str) -> Vec<String> {
        match self.get_json(base, "/api/tags").await {
            Ok(v) => v
                .get("models")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.get("name").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Models currently loaded in memory (`/api/ps`), for the performance view.
    pub async fn loaded_models(&self) -> Vec<api::LoadedModel> {
        let Some(base) = self.base_url().await else {
            return vec![];
        };
        self.get_json(&base, "/api/ps")
            .await
            .map(|v| api::parse_ps(&v))
            .unwrap_or_default()
    }
}

const PREAMBLE: &str = "You are a coding agent running on a local model, working in a git worktree. \
Use the tools to inspect and change files and to run commands; do not paste code for the user to apply. \
Take small steps, read a file before editing it, and run the project's tests when you can. \
Call the maw_* tools exactly as your role instructions say, with the arguments they require; \
when your task is finished, report it with the maw tool for that, then stop. \
If the user only asks a question or greets you, just answer in plain text without any tool.";

#[async_trait]
impl AgentProvider for OllamaProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Ollama
    }

    async fn runtime_status(&self) -> Option<maw_core::perf::RuntimeStatus> {
        // never starts a server: only report on one that is already running
        let base = self.resolved.lock().ok().and_then(|g| g.clone())?;
        let ps = self.get_json(&base, "/api/ps").await.ok()?;
        let loaded = api::parse_ps(&ps)
            .into_iter()
            .map(|m| maw_core::perf::LoadedModel {
                name: m.name,
                size_bytes: m.size_bytes,
                vram_bytes: m.vram_bytes,
            })
            .collect();
        // a speed older than a minute describes nothing that is happening now
        let (decode_tps, prompt_tps) = match self.speed.lock().ok().and_then(|g| *g) {
            Some((at, d, p)) if at.elapsed() < Duration::from_secs(60) => {
                (Some(d), Some(p).filter(|p| *p > 0.0))
            }
            _ => (None, None),
        };
        Some(maw_core::perf::RuntimeStatus {
            loaded,
            decode_tps,
            prompt_tps,
        })
    }

    fn pull_suggestions(&self) -> Vec<ModelChoice> {
        catalog::suggestions()
    }

    async fn pull_model(
        &self,
        name: &str,
        progress: mpsc::Sender<PullProgress>,
    ) -> ProviderResult<String> {
        let name = catalog::check_pull_name(name, self.cfg.max_pull_params_b)
            .map_err(ProviderError::Protocol)?;
        let base = self.ensure_server().await?;
        let resp = self
            .http
            .post(format!("{base}/api/pull"))
            .json(&serde_json::json!({ "model": name, "stream": true }))
            .send()
            .await
            .map_err(|e| ProviderError::Io(format!("cannot reach Ollama at {base}: {e}")))?;
        if !resp.status().is_success() {
            let t = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&t)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(String::from))
                .unwrap_or(t);
            return Err(ProviderError::Protocol(format!(
                "Ollama refused the download: {}",
                msg.trim()
            )));
        }
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut layers: HashMap<String, (u64, u64)> = HashMap::new();
        let mut done = false;
        while !done {
            let next = tokio::time::timeout(Duration::from_secs(180), stream.next())
                .await
                .map_err(|_| ProviderError::Timeout("the download stalled for 3 minutes".into()))?;
            let Some(chunk) = next else { break };
            buf.extend_from_slice(
                &chunk.map_err(|e| ProviderError::Io(format!("download interrupted: {e}")))?,
            );
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let Ok(v) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                if let Some(e) = v.get("error").and_then(Value::as_str) {
                    return Err(ProviderError::Protocol(format!(
                        "Ollama could not download '{name}': {e}"
                    )));
                }
                let status = v
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let (Some(d), Some(t)) = (
                    v.get("digest").and_then(Value::as_str),
                    v.get("total").and_then(Value::as_u64),
                ) {
                    let c = v.get("completed").and_then(Value::as_u64).unwrap_or(0);
                    layers.insert(d.to_string(), (t, c));
                }
                let total: u64 = layers.values().map(|l| l.0).sum();
                let completed: u64 = layers.values().map(|l| l.1.min(l.0)).sum();
                if total > self.cfg.max_pull_bytes {
                    return Err(ProviderError::Protocol(format!(
                        "'{name}' needs a download of {:.1} GB, above the {:.1} GB limit (change `max_pull_gb` under [ollama] to allow more); the download was cancelled",
                        total as f64 / 1e9,
                        self.cfg.max_pull_bytes as f64 / 1e9
                    )));
                }
                let _ = progress.try_send(PullProgress {
                    status: status.clone(),
                    completed,
                    total,
                });
                if status == "success" {
                    done = true;
                }
            }
        }
        if !done {
            return Err(ProviderError::Protocol(
                "the download ended before it finished".into(),
            ));
        }
        let info = self.show(&base, &name).await.unwrap_or_default();
        let size = layers.values().map(|l| l.0).sum::<u64>();
        let mut s = format!("{name} downloaded ({:.1} GB)", size as f64 / 1e9);
        if info.has("tools") {
            s.push_str(" — supports tools");
            if info.has("thinking") {
                s.push_str(" and thinking");
            }
        } else if !info.capabilities.is_empty() {
            s.push_str(" — WARNING: no tool-calling support, so it cannot run as an agent");
        }
        Ok(s)
    }

    async fn list_models(&self) -> ProviderResult<Vec<ModelChoice>> {
        let base = self.base_url().await.ok_or_else(|| {
            ProviderError::NotInstalled(
                "no Ollama server reachable; start it with `ollama serve`".into(),
            )
        })?;
        let tags = self
            .get_json(&base, "/api/tags")
            .await
            .map_err(ProviderError::Io)?;
        let mut out = Vec::new();
        for m in tags
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(name) = m.get("name").and_then(Value::as_str) else {
                continue;
            };
            let size = m.get("size").and_then(Value::as_u64).unwrap_or(0);
            let info = self.show(&base, name).await.unwrap_or_default();
            let mut bits = vec![];
            if let Some(p) = &info.parameter_size {
                bits.push(p.clone());
            }
            if let Some(q) = &info.quantization {
                bits.push(q.clone());
            }
            bits.push(fmt_bytes(size));
            let mut caps = vec![];
            if info.has("tools") {
                caps.push("tools");
            }
            if info.has("thinking") {
                caps.push("thinking");
            }
            let tail = if info.capabilities.is_empty() {
                String::new()
            } else if info.has("tools") {
                format!(" · {}", caps.join(", "))
            } else {
                " · NO tool support: cannot run as an agent".into()
            };
            out.push(ModelChoice {
                id: name.to_string(),
                display_name: name.to_string(),
                description: format!("{}{}", bits.join(" "), tail),
                is_default: false,
                source: "provider".into(),
                reasoning_efforts: api::efforts_for(name, &info),
            });
        }
        Ok(out)
    }

    async fn detect(&self) -> ProviderResult<ProviderInfo> {
        let base = self.base_url().await;
        let version = match &base {
            Some(b) => self.version_at(b).await,
            None => None,
        };
        let mut notes = vec![];
        if base.is_none() {
            notes.push(
                "no Ollama server answered; start it with `ollama serve` (Windows: the Ollama app)"
                    .to_string(),
            );
            if api::is_wsl() {
                notes.push("WSL: the Windows build must listen on an address WSL can reach (OLLAMA_HOST), or enable mirrored networking".to_string());
            }
        }
        Ok(ProviderInfo {
            kind: ProviderKind::Ollama,
            installed: base.is_some(),
            executable: base,
            version,
            execution_modes: vec!["local model, agent loop run by MAW".into()],
            notes,
        })
    }

    async fn auth_status(&self) -> ProviderResult<AuthStatus> {
        Ok(match self.base_url().await {
            Some(b) => AuthStatus::Authenticated {
                method: "local server, no login".into(),
                account: Some(b),
                plan: None,
            },
            // The supervisor stops at "not authenticated" before it ever starts a session, and the
            // session start is what launches the server. With autostart on, a stopped server is
            // therefore "unknown, will be started", not a login problem (e.g. after a reboot).
            None if self.cfg.autostart => AuthStatus::Unknown {
                reason: "Ollama is not running yet; MAW will start it".into(),
            },
            None => AuthStatus::NotAuthenticated {
                hint: "no Ollama server is reachable; start `ollama serve` or turn autostart on"
                    .into(),
            },
        })
    }

    async fn start_session(
        &self,
        cfg: SessionConfig,
    ) -> ProviderResult<(SessionHandle, EventReceiver)> {
        let base = self.ensure_server().await?;
        let installed = self.installed(&base).await;
        let model = match cfg.model.clone().filter(|m| m != "default") {
            Some(m) => m,
            None if installed.len() == 1 => installed[0].clone(),
            None => {
                return Err(ProviderError::Protocol(format!(
                    "choose a model for this agent (`maw agent model {} <model>`); installed: {}",
                    cfg.name,
                    installed.join(", ")
                )))
            }
        };
        let info = self.show(&base, &model).await.map_err(|e| {
            ProviderError::Protocol(format!(
                "model '{model}' is not usable on this server ({e}); installed: {}. Pull it with `ollama pull {model}`",
                installed.join(", ")
            ))
        })?;
        if !info.has("tools") {
            return Err(ProviderError::Unsupported(format!(
                "model '{model}' has no tool-calling support, which MAW needs to run an agent; pick a model whose list entry says 'tools'"
            )));
        }
        let mcp = match &cfg.mcp {
            Some(ep) => Some(
                McpConnection::connect(ep)
                    .await
                    .map_err(ProviderError::Protocol)?,
            ),
            None => None,
        };
        let tools = LocalTools::new(&cfg.cwd, cfg.permission, self.cfg.command_timeout)
            .map_err(|e| ProviderError::Io(format!("workspace {}: {e}", cfg.cwd.display())))?;
        let system = format!(
            "{PREAMBLE}\n\nWorkspace: {}\n\n{}",
            tools.root().display(),
            cfg.instructions
        );
        let params = SessionParams {
            http: self.http.clone(),
            base_url: base,
            think: api::think_value(&model, &info, cfg.effort.as_deref()),
            model,
            num_ctx: self.cfg.num_ctx,
            keep_alive: self.cfg.keep_alive.clone(),
            max_steps: self.cfg.max_steps,
            idle_timeout: self.cfg.idle_timeout,
            system,
            tools,
            mcp,
            speed: self.speed.clone(),
            state_dir: self.cfg.state_dir.clone(),
            resume_id: cfg.resume_native_id.clone(),
        };
        let (handle, rx) = OllamaSession::start(params);
        Ok((handle, rx))
    }
}
