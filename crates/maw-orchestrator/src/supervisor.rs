//! Runtime that owns provider sessions and drives the engine: connects agents, pumps
//! provider events into engine state, dispatches tasks, delivers messages, handles approvals
//! and recovers from disconnects. Rendering never touches this; the TUI only reads snapshots.

use crate::engine::{Actor, Engine};
use crate::prompts;
use crate::workspace as ws;
use chrono::{DateTime, Utc};
use maw_core::provider::*;
use maw_core::*;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{watch, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TRANSCRIPT_CAP: usize = 2000;
const MAX_RECONNECT_ATTEMPTS: u32 = 3;
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Assistant,
    Tool,
    Command,
    Output,
    System,
    Error,
    User,
    /// The provider's own streamed reasoning (summaries / thinking blocks).
    Thinking,
    /// MAW coordination traffic: calls to the `maw_*` tools (message ack, inbox, progress...) and
    /// MAW's own dispatch notices.
    Protocol,
}

/// True for calls to MAW's own coordination tools (`mcp__maw__maw_*` in Claude Code, `maw/maw_*`
/// in Codex).
pub fn is_maw_tool(name: &str) -> bool {
    name.starts_with("mcp__maw__") || name.starts_with("maw/") || name.starts_with("maw_")
}

#[derive(Clone, Debug)]
pub struct TranscriptLine {
    pub at: DateTime<Utc>,
    pub kind: LineKind,
    pub text: String,
    /// Streaming assistant text that may still be extended.
    pub open: bool,
}

impl LineKind {
    fn as_str(self) -> &'static str {
        match self {
            LineKind::Assistant => "assistant",
            LineKind::Tool => "tool",
            LineKind::Command => "command",
            LineKind::Output => "output",
            LineKind::System => "system",
            LineKind::Error => "error",
            LineKind::User => "user",
            LineKind::Thinking => "thinking",
            LineKind::Protocol => "protocol",
        }
    }
    fn parse(s: &str) -> Option<LineKind> {
        Some(match s {
            "assistant" => LineKind::Assistant,
            "tool" => LineKind::Tool,
            "command" => LineKind::Command,
            "output" => LineKind::Output,
            "system" => LineKind::System,
            "error" => LineKind::Error,
            "user" => LineKind::User,
            "thinking" => LineKind::Thinking,
            "protocol" => LineKind::Protocol,
            _ => return None,
        })
    }
}

/// The conversation of every agent: kept in memory for the TUI and written to the database as
/// lines are completed, so it survives closing MAW (the way a Claude Code or Codex session can be
/// resumed). A line still being streamed is written when it ends, when the turn ends, when the
/// agent disconnects and at shutdown. Text was already redacted when it was pushed.
#[derive(Default)]
struct Transcripts {
    lines: Mutex<HashMap<AgentId, VecDeque<TranscriptLine>>>,
    store: Option<Arc<maw_storage::Store>>,
    /// Finished lines waiting for the next batch write.
    pending: Mutex<Vec<(AgentId, TranscriptLine)>>,
}

/// Lines that trigger a write by themselves.
const COMMIT_BATCH: usize = 64;

impl Transcripts {
    fn with_store(store: Arc<maw_storage::Store>) -> Self {
        Transcripts {
            lines: Mutex::default(),
            store: Some(store),
            pending: Mutex::default(),
        }
    }
    /// The agent's queue, loading what was saved by earlier runs the first time it is needed.
    fn queue<'a>(
        &self,
        m: &'a mut HashMap<AgentId, VecDeque<TranscriptLine>>,
        agent: &AgentId,
    ) -> &'a mut VecDeque<TranscriptLine> {
        if !m.contains_key(agent) {
            let mut q = VecDeque::new();
            if let Some(s) = &self.store {
                if let Ok(rows) = s.read(|db| db.recent_transcript(agent, TRANSCRIPT_CAP)) {
                    for (at, kind, text) in rows {
                        if let Some(kind) = LineKind::parse(&kind) {
                            q.push_back(TranscriptLine {
                                at,
                                kind,
                                text,
                                open: false,
                            });
                        }
                    }
                }
            }
            m.insert(agent.clone(), q);
        }
        m.get_mut(agent).expect("inserted above")
    }
    /// Queue a finished line for the database. Lines are written in batches (one transaction,
    /// one disk sync) by `commit`: a command can print thousands of lines.
    fn save(&self, agent: &AgentId, line: &TranscriptLine) {
        if self.store.is_none() {
            return;
        }
        let len = match self.pending.lock() {
            Ok(mut p) => {
                p.push((agent.clone(), line.clone()));
                p.len()
            }
            Err(_) => return,
        };
        if len >= COMMIT_BATCH {
            self.commit();
        }
    }
    /// Write everything queued. Called when a batch fills up, on the supervisor's tick, when a
    /// turn ends, when an agent disconnects and at shutdown.
    fn commit(&self) {
        let Some(s) = &self.store else { return };
        let batch: Vec<(AgentId, TranscriptLine)> = match self.pending.lock() {
            Ok(mut p) => std::mem::take(&mut *p),
            Err(_) => return,
        };
        if batch.is_empty() {
            return;
        }
        let res = s.write(|db| {
            for (agent, l) in &batch {
                db.append_transcript_line(agent, l.at, l.kind.as_str(), &l.text, TRANSCRIPT_CAP)?;
            }
            Ok(())
        });
        if let Err(e) = res {
            tracing::warn!("could not save {} transcript lines: {e}", batch.len());
        }
    }
    /// Write out the lines still being streamed (they become finished lines).
    fn flush(&self, agent: &AgentId) {
        if let Ok(mut m) = self.lines.lock() {
            if let Some(q) = m.get_mut(agent) {
                for l in q.iter_mut().filter(|l| l.open) {
                    l.open = false;
                    self.save(agent, l);
                }
            }
        }
        self.commit();
    }
    fn flush_all(&self) {
        let ids: Vec<AgentId> = self
            .lines
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        for id in ids {
            self.flush(&id);
        }
        self.commit();
    }
    fn forget(&self, agent: &AgentId) {
        if let Ok(mut m) = self.lines.lock() {
            m.remove(agent);
        }
        if let Ok(mut p) = self.pending.lock() {
            p.retain(|(a, _)| a != agent);
        }
        if let Some(s) = &self.store {
            let _ = s.write(|db| db.delete_transcript(agent));
        }
    }
    fn push(&self, agent: &AgentId, kind: LineKind, text: impl Into<String>) {
        let text = maw_core::redact::redact(&text.into());
        if let Ok(mut m) = self.lines.lock() {
            let q = self.queue(&mut m, agent);
            // a non-assistant line closes any streaming assistant line
            if let Some(last) = q.back_mut() {
                if last.open {
                    last.open = false;
                    self.save(agent, last);
                }
            }
            let line = TranscriptLine {
                at: Utc::now(),
                kind,
                text,
                open: false,
            };
            self.save(agent, &line);
            q.push_back(line);
            while q.len() > TRANSCRIPT_CAP {
                q.pop_front();
            }
        }
    }
    fn delta(&self, agent: &AgentId, text: &str) {
        self.delta_kind(agent, LineKind::Assistant, text);
    }
    /// Append streamed text to the open line of `kind`, or start a new one (closing any other
    /// open streaming line first).
    fn delta_kind(&self, agent: &AgentId, kind: LineKind, text: &str) {
        let text = maw_core::redact::redact(text);
        if let Ok(mut m) = self.lines.lock() {
            let q = self.queue(&mut m, agent);
            if let Some(l) = q.back_mut() {
                if l.open && l.kind != kind {
                    l.open = false;
                    self.save(agent, l);
                }
            }
            match q.back_mut() {
                Some(l) if l.kind == kind && l.open => l.text.push_str(&text),
                _ => {
                    q.push_back(TranscriptLine {
                        at: Utc::now(),
                        kind,
                        text,
                        open: true,
                    });
                    while q.len() > TRANSCRIPT_CAP {
                        q.pop_front();
                    }
                }
            }
        }
    }
    fn finish_message(&self, agent: &AgentId, text: &str) {
        let text = maw_core::redact::redact(text);
        if let Ok(mut m) = self.lines.lock() {
            let q = self.queue(&mut m, agent);
            match q.back_mut() {
                Some(l) if l.kind == LineKind::Assistant && l.open => {
                    l.text = text;
                    l.open = false;
                    self.save(agent, l);
                }
                _ => {
                    let line = TranscriptLine {
                        at: Utc::now(),
                        kind: LineKind::Assistant,
                        text,
                        open: false,
                    };
                    self.save(agent, &line);
                    q.push_back(line);
                }
            }
        }
    }
    fn get(&self, agent: &AgentId) -> Vec<TranscriptLine> {
        match self.lines.lock() {
            Ok(mut m) => self.queue(&mut m, agent).iter().cloned().collect(),
            Err(_) => vec![],
        }
    }
}

struct Live {
    handle: SessionHandle,
    turn_active: AtomicBool,
    pump: Mutex<Option<JoinHandle<()>>>,
}

pub struct Supervisor {
    pub engine: Arc<Engine>,
    providers: RwLock<HashMap<ProviderKind, Arc<dyn AgentProvider>>>,
    /// Providers used ONLY to list models (never to start sessions), e.g. in `maw demo`.
    catalog_providers: RwLock<HashMap<ProviderKind, Arc<dyn AgentProvider>>>,
    sessions: AsyncMutex<HashMap<AgentId, Arc<Live>>>,
    mcp_url: RwLock<Option<String>>,
    transcripts: Transcripts,
    ui_tick: watch::Sender<u64>,
    cancel: CancellationToken,
    reminded: Mutex<HashSet<TaskId>>,
    review_dispatched: Mutex<HashMap<TaskId, u32>>,
    briefed: Mutex<HashSet<AgentId>>,
    /// Messages already written to the conversation (typed by the user), so delivery does not repeat them.
    shown: Mutex<HashSet<MessageId>>,
    user_stopped: Mutex<HashSet<AgentId>>,
    reconnecting: Mutex<HashSet<AgentId>>,
    /// goal -> (nudges sent, when the last one was sent)
    nudges: Mutex<HashMap<GoalId, (u32, std::time::Instant)>>,
    perf: Arc<crate::perf::PerfMonitor>,
}

impl Supervisor {
    pub fn new(engine: Arc<Engine>) -> Arc<Supervisor> {
        let (tx, _) = watch::channel(0);
        let store = engine.store().clone();
        Arc::new(Supervisor {
            engine,
            providers: RwLock::new(HashMap::new()),
            catalog_providers: RwLock::new(HashMap::new()),
            sessions: AsyncMutex::new(HashMap::new()),
            mcp_url: RwLock::new(None),
            transcripts: Transcripts::with_store(store),
            ui_tick: tx,
            cancel: CancellationToken::new(),
            reminded: Mutex::new(HashSet::new()),
            review_dispatched: Mutex::new(HashMap::new()),
            briefed: Mutex::new(HashSet::new()),
            shown: Mutex::new(HashSet::new()),
            user_stopped: Mutex::new(HashSet::new()),
            reconnecting: Mutex::new(HashSet::new()),
            nudges: Mutex::new(HashMap::new()),
            perf: crate::perf::PerfMonitor::new(),
        })
    }

    /// Start sampling machine performance (CPU, RAM, GPU, loaded models, generation speed).
    /// Idempotent; call it when something wants to show the figures.
    pub fn start_perf(self: &Arc<Self>) {
        self.perf.start(Arc::downgrade(self));
    }

    /// The sampled history so far (empty until `start_perf`).
    pub fn perf(&self) -> maw_core::perf::PerfHistory {
        self.perf.snapshot()
    }

    /// What the local-model providers are doing right now, combined.
    pub async fn runtime_status(&self) -> maw_core::perf::RuntimeStatus {
        let ps: Vec<Arc<dyn AgentProvider>> = self
            .providers
            .read()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        let mut out = maw_core::perf::RuntimeStatus::default();
        for p in ps {
            if let Ok(Some(s)) =
                tokio::time::timeout(Duration::from_secs(3), p.runtime_status()).await
            {
                out.loaded.extend(s.loaded);
                out.decode_tps = out.decode_tps.or(s.decode_tps);
                out.prompt_tps = out.prompt_tps.or(s.prompt_tps);
            }
        }
        out
    }

    pub fn register_provider(&self, p: Arc<dyn AgentProvider>) {
        if let Ok(mut m) = self.providers.write() {
            m.insert(p.kind(), p);
        }
    }
    /// Register a provider for model listing only; it can never run an agent.
    pub fn register_catalog_provider(&self, p: Arc<dyn AgentProvider>) {
        if let Ok(mut m) = self.catalog_providers.write() {
            m.insert(p.kind(), p);
        }
    }
    pub fn provider(&self, kind: ProviderKind) -> Option<Arc<dyn AgentProvider>> {
        self.providers.read().ok()?.get(&kind).cloned()
    }
    pub fn set_mcp_url(&self, url: String) {
        if let Ok(mut u) = self.mcp_url.write() {
            *u = Some(url);
        }
    }
    pub fn transcript(&self, agent: &AgentId) -> Vec<TranscriptLine> {
        self.transcripts.get(agent)
    }
    /// Increments whenever live output changes (for the TUI to redraw without polling).
    pub fn ui_updates(&self) -> watch::Receiver<u64> {
        self.ui_tick.subscribe()
    }
    fn bump(&self) {
        self.ui_tick.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    fn project_root(&self) -> PathBuf {
        PathBuf::from(&self.engine.project().path)
    }

    fn set_state(&self, id: &AgentId, st: AgentState, err: Option<String>) {
        if let Err(e) = self.engine.set_agent_state(id, st, err) {
            tracing::debug!(agent = %id, ?st, "state change rejected: {e}");
        }
        self.bump();
    }

    // ------------------------------------------------------------------ connect / disconnect

    pub async fn live_agents(&self) -> Vec<AgentId> {
        self.sessions.lock().await.keys().cloned().collect()
    }

    pub async fn is_live(&self, id: &AgentId) -> bool {
        self.sessions.lock().await.contains_key(id)
    }

    pub async fn connect_all(self: &Arc<Self>) {
        let agents = self.engine.list_agents().unwrap_or_default();
        let mut set = tokio::task::JoinSet::new();
        for a in agents {
            if self
                .user_stopped
                .lock()
                .map(|s| s.contains(&a.id))
                .unwrap_or(false)
            {
                continue;
            }
            let me = self.clone();
            set.spawn(async move {
                if let Err(e) = me.connect_agent(&a.id).await {
                    tracing::warn!(agent = %a.name, "connect failed: {e}");
                }
            });
        }
        while set.join_next().await.is_some() {}
    }

    /// Boxed to break the async recursion cycle (connect -> pump -> reconnect -> connect).
    pub fn connect_agent<'a>(
        self: &'a Arc<Self>,
        id: &'a AgentId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CoreResult<()>> + Send + 'a>> {
        Box::pin(self.connect_agent_inner(id))
    }

    async fn connect_agent_inner(self: &Arc<Self>, id: &AgentId) -> CoreResult<()> {
        if self.is_live(id).await {
            return Ok(());
        }
        if let Ok(mut s) = self.user_stopped.lock() {
            s.remove(id);
        }
        let agent = self.engine.get_agent(id)?;
        let provider = self.provider(agent.provider).ok_or_else(|| {
            CoreError::Unsupported(format!("no provider registered for {}", agent.provider))
        })?;
        self.set_state(id, AgentState::Connecting, None);
        self.transcripts.push(
            id,
            LineKind::System,
            format!("connecting via {}...", agent.provider),
        );

        match provider.auth_status().await {
            Ok(a) if a.is_authenticated() => {}
            Ok(AuthStatus::NotAuthenticated { hint }) => {
                self.set_state(id, AgentState::AuthRequired, Some(hint.clone()));
                self.transcripts.push(
                    id,
                    LineKind::Error,
                    format!("authentication required: {hint}"),
                );
                return Err(CoreError::Precondition(format!(
                    "{} requires authentication: {hint}",
                    agent.provider
                )));
            }
            Ok(AuthStatus::Unknown { reason }) => {
                // Unknown is not a failure: the session start will surface a real error if auth is missing.
                self.transcripts.push(
                    id,
                    LineKind::System,
                    format!("auth status unknown ({reason}); attempting to start"),
                );
            }
            Ok(_) => {}
            Err(e) => {
                self.set_state(id, AgentState::Failed, Some(e.to_string()));
                return Err(CoreError::Precondition(e.to_string()));
            }
        }

        let (cwd, workspace_note) = self.prepare_cwd(&agent).await;
        if let Some(n) = workspace_note {
            self.transcripts.push(id, LineKind::System, n);
        }
        let mcp = self
            .mcp_url
            .read()
            .ok()
            .and_then(|u| u.clone())
            .map(|url| McpEndpoint {
                url,
                token: self.engine.mint_token(id),
            });
        let cfg = SessionConfig {
            agent_id: id.clone(),
            name: agent.name.clone(),
            role: agent.role,
            cwd: cwd.clone(),
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            permission: agent.permission,
            mcp,
            instructions: prompts::role_instructions(agent.role, &agent.name),
            env: HashMap::new(),
            resume_native_id: agent.native_session_id.clone(),
        };
        let (handle, rx) = match provider.start_session(cfg).await {
            Ok(x) => x,
            Err(e) => {
                let msg = e.to_string();
                let state = if matches!(e, ProviderError::AuthRequired(_)) {
                    AgentState::AuthRequired
                } else {
                    AgentState::Failed
                };
                self.set_state(id, state, Some(msg.clone()));
                self.transcripts.push(id, LineKind::Error, msg.clone());
                self.engine.emit_custom(
                    EventKind::ProviderError,
                    Some(id),
                    None,
                    json!({ "agent": agent.name, "error": maw_core::redact::redact(&msg) }),
                );
                return Err(CoreError::Precondition(msg));
            }
        };
        let live = Arc::new(Live {
            handle: handle.clone(),
            turn_active: AtomicBool::new(false),
            pump: Mutex::new(None),
        });
        self.sessions.lock().await.insert(id.clone(), live.clone());
        let me = self.clone();
        let aid = id.clone();
        let pump = tokio::spawn(async move { me.pump(aid, rx).await });
        if let Ok(mut p) = live.pump.lock() {
            *p = Some(pump);
        }
        let _ = self.engine.update_agent_session(
            id,
            handle.native_session_id(),
            handle.capabilities(),
            Some(cwd.to_string_lossy().to_string()),
        );
        self.set_state(id, AgentState::Idle, None);
        self.engine.emit_custom(
            EventKind::AgentConnected,
            Some(id),
            None,
            json!({ "agent": agent.name, "provider": agent.provider.as_str(), "native_session_id": handle.native_session_id(), "cwd": cwd }),
        );
        self.transcripts.push(
            id,
            LineKind::System,
            format!(
                "connected (session {})",
                handle.native_session_id().unwrap_or_else(|| "n/a".into())
            ),
        );
        self.engine.wake_handle().notify_one();
        Ok(())
    }

    /// Choose the working directory for an agent: a private worktree for implementers when the
    /// project is a git repository; the project root otherwise.
    async fn prepare_cwd(&self, agent: &Agent) -> (PathBuf, Option<String>) {
        let root = self.project_root();
        if !agent.role.is_executor() {
            return (root, None);
        }
        let name = agent.name.clone();
        let r = root.clone();
        let res = tokio::task::spawn_blocking(move || ws::ensure_worktree(&r, &name)).await;
        match res {
            Ok(Ok(info)) => {
                let w = Workspace {
                    id: WorkspaceId::new(),
                    project_id: self.engine.project_id().clone(),
                    agent_id: Some(agent.id.clone()),
                    task_id: None,
                    path: info.path.to_string_lossy().to_string(),
                    branch: Some(info.branch.clone()),
                    base_revision: Some(info.base_revision.clone()),
                    kind: "worktree".into(),
                    status: "active".into(),
                    changed_files: vec![],
                    integration_status: "none".into(),
                    conflict: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                };
                // reuse the existing workspace row for this agent if present
                let existing = self.engine.snapshot().ok().and_then(|s| s.workspaces.into_iter().find(|x| x.agent_id.as_ref() == Some(&agent.id) && x.kind == "worktree"));
                let w = existing.map(|mut e| {
                    e.path = w.path.clone();
                    e.branch = w.branch.clone();
                    e
                }).unwrap_or(w);
                let _ = self.engine.store().write(|db| db.upsert_workspace(&w));
                self.engine.emit_custom(EventKind::WorkspaceUpdated, Some(&agent.id), None, json!({ "path": w.path, "branch": w.branch, "kind": "worktree", "created": info.created }));
                (info.path.clone(), Some(format!("workspace {} on branch {}", info.path.display(), info.branch)))
            }
            Ok(Err(e)) => (root, Some(format!("no isolated worktree ({e}); working in the project directory, so avoid overlapping edits"))),
            Err(e) => (root, Some(format!("workspace setup failed: {e}"))),
        }
    }

    /// Apply a changed model/permission now if that is safe. A busy agent is never torn down
    /// mid-turn: the change then takes effect the next time it connects. Returns what happened.
    pub async fn reconfigure_agent(self: &Arc<Self>, id: &AgentId) -> CoreResult<&'static str> {
        if !self.is_live(id).await {
            return Ok("saved; applies when the agent connects");
        }
        let agent = self.engine.get_agent(id)?;
        if !agent.state.is_available() {
            return Ok("saved; the agent is busy, so it applies on its next connect (use disconnect + connect to force)");
        }
        self.disconnect_agent(id, false).await;
        self.connect_agent(id).await?;
        Ok("applied: the agent reconnected with the new settings")
    }

    /// Models the agent's provider offers (live from the provider when it supports that).
    pub async fn list_models(
        &self,
        kind: ProviderKind,
    ) -> CoreResult<Vec<maw_core::provider::ModelChoice>> {
        let p = self
            .provider(kind)
            .or_else(|| {
                self.catalog_providers
                    .read()
                    .ok()
                    .and_then(|m| m.get(&kind).cloned())
            })
            .ok_or_else(|| CoreError::Unsupported(format!("no provider registered for {kind}")))?;
        p.list_models()
            .await
            .map_err(|e| CoreError::Precondition(e.to_string()))
    }

    /// Models a local-model provider suggests downloading (empty for hosted providers).
    pub fn pull_suggestions(&self, kind: ProviderKind) -> Vec<maw_core::provider::ModelChoice> {
        self.provider(kind)
            .map(|p| p.pull_suggestions())
            .unwrap_or_default()
    }

    /// Download a model through the provider. Dropping the returned future cancels the download.
    pub async fn pull_model(
        &self,
        kind: ProviderKind,
        name: &str,
        progress: tokio::sync::mpsc::Sender<maw_core::provider::PullProgress>,
    ) -> CoreResult<String> {
        let p = self
            .provider(kind)
            .ok_or_else(|| CoreError::Unsupported(format!("no provider registered for {kind}")))?;
        p.pull_model(name, progress)
            .await
            .map_err(|e| CoreError::Precondition(e.to_string()))
    }

    /// Authentication state of a provider that can run agents (catalog-only providers excluded).
    pub async fn auth_status(&self, kind: ProviderKind) -> CoreResult<AuthStatus> {
        let p = self
            .provider(kind)
            .ok_or_else(|| CoreError::Unsupported(format!("no provider registered for {kind}")))?;
        p.auth_status()
            .await
            .map_err(|e| CoreError::Precondition(e.to_string()))
    }

    /// The provider's interactive login command, for providers that can run agents.
    pub fn login_command(&self, kind: ProviderKind) -> Option<Vec<String>> {
        self.provider(kind).and_then(|p| p.login_command())
    }

    /// Remove an agent: refuse while it has unfinished work (unless `force`), stop its session,
    /// then delete it from the project. Its git worktree is left on disk.
    pub async fn remove_agent(
        self: &Arc<Self>,
        id: &AgentId,
        force: bool,
    ) -> CoreResult<crate::engine::RemovedAgent> {
        let agent = self.engine.get_agent(id)?;
        let blockers = self.engine.removal_blockers(id)?;
        if !force && !blockers.is_empty() {
            return Err(CoreError::Conflict(format!(
                "{} has unfinished work: {}. Remove it with force to release its tasks",
                agent.name,
                blockers.join("; ")
            )));
        }
        // stop the session first so nothing is running on behalf of a deleted agent
        self.disconnect_agent(id, true).await;
        let removed = self.engine.remove_agent(id, force)?;
        if let Ok(mut s) = self.user_stopped.lock() {
            s.remove(id);
        }
        self.transcripts.forget(id);
        self.bump();
        Ok(removed)
    }

    /// Reject model ids the provider does not offer (see `models::check_model`).
    pub async fn validate_model(&self, kind: ProviderKind, id: &str) -> CoreResult<()> {
        let list = self.list_models(kind).await.unwrap_or_default();
        crate::models::check_model(&list, kind, id)
    }

    pub async fn disconnect_agent(self: &Arc<Self>, id: &AgentId, by_user: bool) {
        if by_user {
            if let Ok(mut s) = self.user_stopped.lock() {
                s.insert(id.clone());
            }
        }
        let live = self.sessions.lock().await.remove(id);
        self.transcripts.flush(id);
        if let Some(l) = live {
            let _ = tokio::time::timeout(Duration::from_secs(10), l.handle.shutdown()).await;
            if let Ok(mut p) = l.pump.lock() {
                if let Some(h) = p.take() {
                    h.abort();
                }
            }
        }
        self.after_session_gone(
            id,
            if by_user {
                "stopped by user"
            } else {
                "disconnected"
            },
            by_user,
        )
        .await;
    }

    async fn after_session_gone(self: &Arc<Self>, id: &AgentId, reason: &str, stopped: bool) {
        self.engine.revoke_tokens(id);
        self.engine.expire_approvals_for(id);
        if let Ok(agent) = self.engine.get_agent(id) {
            // The task it was running may have uncertain side effects: block it for a decision.
            if let Some(tid) = &agent.current_task_id {
                if let Ok(t) = self.engine.get_task(&tid.0) {
                    if t.status == TaskStatus::Running {
                        let _ = self.engine.block_task(
                            &Actor::System,
                            &tid.0,
                            &format!(
                                "owner '{}' {reason}; side effects of the attempt are unknown",
                                agent.name
                            ),
                            Some("Leader decision: release/reassign or cancel"),
                        );
                    }
                }
            }
            if let Ok(mut r) = self.review_dispatched.lock() {
                r.retain(|t, _| Some(t) != agent.current_task_id.as_ref());
            }
        }
        self.set_state(
            id,
            if stopped {
                AgentState::Stopped
            } else {
                AgentState::Disconnected
            },
            Some(reason.to_string()),
        );
        self.engine.emit_custom(
            EventKind::AgentDisconnected,
            Some(id),
            None,
            json!({ "reason": reason }),
        );
        self.transcripts
            .push(id, LineKind::System, format!("session ended: {reason}"));
    }

    async fn handle_session_end(self: &Arc<Self>, id: &AgentId, reason: &str) {
        let removed = self.sessions.lock().await.remove(id);
        if removed.is_none() {
            return; // already handled by disconnect_agent
        }
        self.after_session_gone(id, reason, false).await;
        let stopped = self
            .user_stopped
            .lock()
            .map(|s| s.contains(id))
            .unwrap_or(false);
        if !stopped && !self.cancel.is_cancelled() {
            let me = self.clone();
            let aid = id.clone();
            tokio::spawn(async move { me.reconnect(aid).await });
        }
    }

    /// Bounded exponential backoff; never reconnects agents the user stopped.
    async fn reconnect(self: Arc<Self>, id: AgentId) {
        if let Ok(mut r) = self.reconnecting.lock() {
            if !r.insert(id.clone()) {
                return;
            }
        }
        for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
            let delay = Duration::from_secs(2u64.pow(attempt));
            self.engine.emit_custom(
                EventKind::ProviderReconnecting,
                Some(&id),
                None,
                json!({ "attempt": attempt, "delay_secs": delay.as_secs() }),
            );
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.cancel.cancelled() => break,
            }
            if self
                .user_stopped
                .lock()
                .map(|s| s.contains(&id))
                .unwrap_or(false)
            {
                break;
            }
            if self.connect_agent(&id).await.is_ok() {
                break;
            }
        }
        if let Ok(mut r) = self.reconnecting.lock() {
            r.remove(&id);
        }
    }

    // ------------------------------------------------------------------ provider event pump

    async fn pump(self: Arc<Self>, id: AgentId, mut rx: EventReceiver) {
        while let Some(ev) = rx.recv().await {
            self.handle_event(&id, ev).await;
            self.bump();
        }
        self.handle_session_end(&id, "provider event stream closed")
            .await;
    }

    async fn handle_event(self: &Arc<Self>, id: &AgentId, ev: ProviderEvent) {
        let name = self
            .engine
            .get_agent(id)
            .map(|a| a.name)
            .unwrap_or_default();
        match ev {
            ProviderEvent::Connected { native_session_id } => {
                if native_session_id.is_some() {
                    let caps = self
                        .engine
                        .get_agent(id)
                        .map(|a| a.capabilities)
                        .unwrap_or_default();
                    let _ = self
                        .engine
                        .update_agent_session(id, native_session_id, caps, None);
                }
            }
            ProviderEvent::TurnStarted { turn_id } => {
                self.set_state(id, AgentState::Thinking, None);
                self.engine.emit_custom(
                    EventKind::AgentTurnStarted,
                    Some(id),
                    None,
                    json!({ "agent": name, "turn_id": turn_id }),
                );
            }
            ProviderEvent::MessageDelta { text } => self.transcripts.delta(id, &text),
            ProviderEvent::Reasoning { text } => {
                self.transcripts.delta_kind(id, LineKind::Thinking, &text)
            }
            ProviderEvent::Message { text } => {
                self.transcripts.finish_message(id, &text);
                let snippet: String = text.chars().take(400).collect();
                self.engine.emit_custom(
                    EventKind::AgentMessage,
                    Some(id),
                    None,
                    json!({ "agent": name, "text": maw_core::redact::redact(&snippet) }),
                );
            }
            ProviderEvent::ToolStarted {
                call_id,
                name: tool,
                detail,
            } => {
                self.set_state(id, AgentState::Working, None);
                self.transcripts.push(
                    id,
                    if is_maw_tool(&tool) {
                        LineKind::Protocol
                    } else {
                        LineKind::Tool
                    },
                    format!("▶ {tool} {}", detail.clone().unwrap_or_default()),
                );
                self.engine.emit_custom(EventKind::AgentToolStarted, Some(id), None, json!({ "agent": name, "call_id": call_id, "tool": tool, "detail": detail.map(|d| maw_core::redact::redact(&d)) }));
            }
            ProviderEvent::ToolCompleted {
                call_id,
                name: tool,
                ok,
                detail,
            } => {
                self.transcripts.push(
                    id,
                    if !ok {
                        LineKind::Error
                    } else if is_maw_tool(&tool) {
                        LineKind::Protocol
                    } else {
                        LineKind::Tool
                    },
                    format!(
                        "{} {tool} {}",
                        if ok { "✓" } else { "✗" },
                        detail.clone().unwrap_or_default()
                    ),
                );
                self.engine.emit_custom(
                    EventKind::AgentToolCompleted,
                    Some(id),
                    None,
                    json!({ "agent": name, "call_id": call_id, "tool": tool, "ok": ok }),
                );
            }
            ProviderEvent::CommandStarted {
                call_id,
                command,
                cwd,
            } => {
                self.set_state(id, AgentState::Working, None);
                self.transcripts
                    .push(id, LineKind::Command, format!("$ {command}"));
                self.engine.emit_custom(EventKind::AgentCommandStarted, Some(id), None, json!({ "agent": name, "call_id": call_id, "command": maw_core::redact::redact(&command), "cwd": cwd }));
            }
            ProviderEvent::CommandOutput { chunk, .. } => {
                for line in chunk.lines().take(40) {
                    self.transcripts
                        .push(id, LineKind::Output, line.to_string());
                }
            }
            ProviderEvent::CommandCompleted { call_id, exit_code } => {
                self.engine.emit_custom(
                    EventKind::AgentCommandCompleted,
                    Some(id),
                    None,
                    json!({ "agent": name, "call_id": call_id, "exit_code": exit_code }),
                );
            }
            ProviderEvent::FileChanged { paths } => {
                self.transcripts
                    .push(id, LineKind::Tool, format!("✎ {}", paths.join(", ")));
                self.engine.emit_custom(
                    EventKind::AgentFileChanged,
                    Some(id),
                    None,
                    json!({ "agent": name, "paths": paths }),
                );
            }
            ProviderEvent::ApprovalRequested {
                request_id,
                kind,
                summary,
                reason,
                paths,
                details,
            } => {
                self.set_state(id, AgentState::WaitingApproval, None);
                let k = match kind {
                    ApprovalKind::Command => "command",
                    ApprovalKind::FileChange => "file_change",
                    ApprovalKind::Permissions => "permissions",
                    ApprovalKind::Other => "other",
                };
                let mut d = details;
                if let serde_json::Value::Object(m) = &mut d {
                    m.insert("paths".into(), json!(paths));
                }
                match self
                    .engine
                    .record_approval(id, k, &summary, reason, d, &request_id)
                {
                    Ok(_) => self.transcripts.push(
                        id,
                        LineKind::System,
                        format!("⏸ approval needed: {summary}"),
                    ),
                    Err(e) => tracing::error!("recording approval failed: {e}"),
                }
            }
            ProviderEvent::TurnCompleted {
                turn_id,
                outcome,
                error,
            } => {
                self.transcripts.flush(id); // a finished turn leaves no half-written line behind
                self.engine.emit_custom(EventKind::AgentTurnCompleted, Some(id), None, json!({ "agent": name, "turn_id": turn_id, "outcome": format!("{outcome:?}"), "error": error }));
                if let Some(e) = &error {
                    self.transcripts
                        .push(id, LineKind::Error, format!("turn {outcome:?}: {e}"));
                }
                self.after_turn(id, outcome, error).await;
            }
            ProviderEvent::Usage(u) => {
                let _ = self.engine.record_usage(id, u);
            }
            ProviderEvent::RateLimited {
                retry_after_ms,
                detail,
            } => {
                self.set_state(id, AgentState::RateLimited, Some(detail.clone()));
                self.engine.emit_custom(
                    EventKind::ProviderRateLimited,
                    Some(id),
                    None,
                    json!({ "agent": name, "retry_after_ms": retry_after_ms, "detail": detail }),
                );
                self.transcripts
                    .push(id, LineKind::Error, format!("rate limited: {detail}"));
                let me = self.clone();
                let aid = id.clone();
                let wait = Duration::from_millis(retry_after_ms.unwrap_or(60_000).min(15 * 60_000));
                tokio::spawn(async move {
                    tokio::select! { _ = tokio::time::sleep(wait) => {}, _ = me.cancel.cancelled() => return }
                    if me
                        .engine
                        .get_agent(&aid)
                        .map(|a| a.state == AgentState::RateLimited)
                        .unwrap_or(false)
                    {
                        me.set_state(&aid, AgentState::Idle, None);
                        me.engine.wake_handle().notify_one();
                    }
                });
            }
            ProviderEvent::Error { message, fatal } => {
                self.transcripts.push(id, LineKind::Error, message.clone());
                self.engine.emit_custom(EventKind::ProviderError, Some(id), None, json!({ "agent": name, "error": maw_core::redact::redact(&message), "fatal": fatal }));
                if fatal {
                    self.set_state(id, AgentState::Failed, Some(message));
                }
            }
            ProviderEvent::Disconnected { reason } => {
                self.handle_session_end(id, &reason).await;
            }
        }
    }

    async fn after_turn(
        self: &Arc<Self>,
        id: &AgentId,
        outcome: TurnOutcome,
        error: Option<String>,
    ) {
        if let Some(l) = self.sessions.lock().await.get(id) {
            l.turn_active.store(false, Ordering::SeqCst);
        }
        let st = if outcome == TurnOutcome::Failed {
            error.clone()
        } else {
            None
        };
        self.set_state(id, AgentState::Idle, st);
        // Never infer completion from the end of a turn: if the task is still RUNNING the agent
        // neither submitted nor blocked. Remind once, then block for the Leader to decide.
        if let Ok(agent) = self.engine.get_agent(id) {
            // Look at tasks the agent *owns* (not only its current slot) so none can be orphaned.
            let mut candidates: Vec<TaskId> = self
                .engine
                .list_tasks()
                .unwrap_or_default()
                .into_iter()
                .filter(|t| {
                    t.owner_agent_id.as_ref() == Some(id) && t.status == TaskStatus::Running
                })
                .map(|t| t.id)
                .collect();
            if let Some(cur) = agent.current_task_id.clone() {
                if !candidates.contains(&cur) {
                    candidates.push(cur);
                }
            }
            for tid in candidates {
                if let Ok(task) = self.engine.get_task(&tid.0) {
                    match task.status {
                        TaskStatus::Running if outcome == TurnOutcome::Interrupted => {
                            // user interruption: leave the task running; the user will redirect.
                        }
                        TaskStatus::Running => {
                            let first = self
                                .reminded
                                .lock()
                                .map(|mut r| r.insert(tid.clone()))
                                .unwrap_or(true);
                            if first {
                                let _ = self.engine.send_message(
                                    &Actor::System,
                                    &agent.name,
                                    MessageType::Instruction,
                                    &format!("{} is still RUNNING", task.key),
                                    &format!("Your turn ended but {} has no result or blocker. Call maw_task_submit_result with evidence, or maw_task_block with the reason.", task.key),
                                    Some(&task.key),
                                    Some(format!("remind-{}-{}", task.id, task.attempt_count)),
                                );
                            } else {
                                let _ = self.engine.block_task(&Actor::System, &task.key, "agent ended its turn twice without submitting a result or reporting a blocker", Some("Leader decision"));
                            }
                        }
                        TaskStatus::InReview => {
                            // reviewer finished without a verdict
                            let n = self
                                .review_dispatched
                                .lock()
                                .map(|r| r.get(&task.id).copied().unwrap_or(0))
                                .unwrap_or(0);
                            if n >= 2 {
                                let _ = self.engine.send_message(&Actor::System, "leader", MessageType::Blocker, &format!("{} review stalled", task.key), &format!("Reviewer {} ended twice without a verdict on {}. Review it yourself with maw_review_submit.", agent.name, task.key), Some(&task.key), Some(format!("review-stalled-{}", task.id)));
                            } else if let Ok(mut r) = self.review_dispatched.lock() {
                                // allow one more dispatch attempt
                                r.insert(task.id.clone(), n);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        self.engine.wake_handle().notify_one();
    }

    // ------------------------------------------------------------------ user-facing control

    pub async fn interrupt_agent(&self, id: &AgentId) -> CoreResult<()> {
        let live = self
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::Precondition("agent is not connected".into()))?;
        if !live.handle.capabilities().interrupt {
            return Err(CoreError::Unsupported(
                "this provider session cannot be interrupted".into(),
            ));
        }
        live.handle
            .interrupt()
            .await
            .map_err(|e| CoreError::Precondition(e.to_string()))?;
        self.transcripts
            .push(id, LineKind::System, "interrupt requested");
        Ok(())
    }

    pub async fn resolve_approval(
        &self,
        approval_id: &ApprovalId,
        decision: ApprovalDecision,
    ) -> CoreResult<()> {
        let ap = self
            .engine
            .read(|db| db.get_approval(approval_id))?
            .ok_or_else(|| CoreError::not_found("approval", approval_id))?;
        let live = self.sessions.lock().await.get(&ap.agent_id).cloned();
        let status = match decision {
            ApprovalDecision::Approve => ApprovalStatus::Approved,
            ApprovalDecision::ApproveForSession => ApprovalStatus::ApprovedForSession,
            ApprovalDecision::Deny | ApprovalDecision::Abort => ApprovalStatus::Denied,
        };
        match live {
            Some(l) => {
                l.handle
                    .resolve_approval(&ap.request_ref, decision)
                    .await
                    .map_err(|e| CoreError::Precondition(e.to_string()))?;
                self.engine.resolve_approval(approval_id, status)?;
                if matches!(
                    decision,
                    ApprovalDecision::Approve | ApprovalDecision::ApproveForSession
                ) {
                    self.set_state(&ap.agent_id, AgentState::Working, None);
                }
            }
            None => {
                self.engine
                    .resolve_approval(approval_id, ApprovalStatus::Expired)?;
                return Err(CoreError::Precondition(
                    "agent session is gone; approval expired".into(),
                ));
            }
        }
        self.bump();
        Ok(())
    }

    /// Send the user's message to an agent (delivered by the scheduler when the agent is idle,
    /// or immediately interrupting if `interrupt_now`).
    pub async fn message_agent(
        &self,
        agent: &str,
        text: &str,
        interrupt_now: bool,
    ) -> CoreResult<()> {
        let a = self.engine.resolve_agent(agent)?;
        let m = self.engine.send_message(
            &Actor::User,
            &a.id.0,
            MessageType::Instruction,
            "Message from the user",
            text,
            None,
            None,
        )?;
        // Show it at once: it may wait a while for the agent to finish its current turn.
        if let Ok(mut s) = self.shown.lock() {
            s.insert(m.id.clone());
        }
        self.transcripts
            .push(&a.id, LineKind::User, text.to_string());
        self.bump();
        if interrupt_now && a.state.is_busy() {
            let _ = self.interrupt_agent(&a.id).await;
        }
        Ok(())
    }

    /// Change the objective without discarding completed work; the Leader is told to re-plan.
    pub async fn redirect_goal(
        &self,
        new_text: &str,
        reason: &str,
        interrupt_leader: bool,
    ) -> CoreResult<Goal> {
        let g = self.engine.update_goal(
            &Actor::User,
            crate::engine::GoalPatch {
                text: Some(new_text.to_string()),
                reason: Some(reason.to_string()),
                ..Default::default()
            },
        )?;
        if let Some(l) = self.engine.leader()? {
            self.engine.send_message(
                &Actor::User,
                &l.id.0,
                MessageType::Instruction,
                &format!("Objective revised (revision {})", g.revision),
                &format!("The user changed the objective. Completed tasks are preserved.\nNew objective: {}\nReason: {reason}\nRe-plan with maw_plan_propose_revision, cancel or edit tasks that no longer apply, and add new ones.", g.text),
                None,
                None,
            )?;
            if interrupt_leader && l.state.is_busy() {
                let _ = self.interrupt_agent(&l.id).await;
            }
        }
        Ok(g)
    }

    // ------------------------------------------------------------------ scheduler

    /// Main loop: event driven with a periodic safety tick.
    pub async fn run(self: Arc<Self>) {
        let wake = self.engine.wake_handle();
        let react = {
            let me = self.clone();
            tokio::spawn(async move { me.react_to_events().await })
        };
        let tick_ms = self.engine.config().tick_ms.max(200);
        let mut tick = tokio::time::interval(Duration::from_millis(tick_ms));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = wake.notified() => {}
                // finished transcript lines reach the database at most once per tick
                _ = tick.tick() => self.transcripts.commit(),
            }
            if let Err(e) = self.scheduler_pass().await {
                tracing::warn!("scheduler pass failed: {e}");
            }
            // coalesce bursts of wakeups
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        react.abort();
    }

    async fn react_to_events(self: Arc<Self>) {
        let mut rx = self.engine.subscribe();
        loop {
            let ev = tokio::select! { _ = self.cancel.cancelled() => return, r = rx.recv() => r };
            let ev = match ev {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event consumer lagged by {n}");
                    continue;
                }
                Err(_) => return,
            };
            self.bump();
            match ev.event_type {
                EventKind::TaskCanceled | EventKind::TaskReleased => {
                    // stop a worker that is still executing a task that was canceled/re-assigned
                    if let (Some(agent), Some(tid)) = (ev.agent_id.clone(), ev.task_id.clone()) {
                        let busy = self
                            .engine
                            .get_agent(&agent)
                            .map(|a| a.state.is_busy())
                            .unwrap_or(false);
                        let was_running =
                            ev.payload.get("from").and_then(|f| f.as_str()) == Some("RUNNING");
                        if busy && was_running {
                            tracing::info!(task = %tid, agent = %agent, "interrupting agent after task stop");
                            let _ = self.interrupt_agent(&agent).await;
                        }
                    }
                }
                EventKind::TaskSubmitted => {
                    if let Some(tid) = ev.task_id.clone() {
                        self.capture_submission(&tid).await;
                    }
                }
                EventKind::TaskCompleted => {
                    // continuous integration: merge finished workers' branches as soon as they are quiet
                    if let Err(e) = self.integrate_done_work().await {
                        tracing::warn!("integration after task completion failed: {e}");
                    }
                }
                _ => {}
            }
        }
    }

    /// When a task is submitted, commit any leftover changes in the worker's worktree and record
    /// what changed so reviewers and the integration step see an accurate diff summary.
    async fn capture_submission(&self, tid: &TaskId) {
        let Ok(task) = self.engine.get_task(&tid.0) else {
            return;
        };
        let Some(owner) = task.owner_agent_id.clone() else {
            return;
        };
        let snapshot = self.engine.snapshot().ok();
        let Some(w) = snapshot.and_then(|s| {
            s.workspaces
                .into_iter()
                .find(|w| w.agent_id.as_ref() == Some(&owner) && w.kind == "worktree")
        }) else {
            return;
        };
        let path = PathBuf::from(&w.path);
        let base = w.base_revision.clone().unwrap_or_default();
        let msg = format!("[{}] {} (committed by MAW)", task.key, task.title);
        let res = tokio::task::spawn_blocking(move || {
            let committed = ws::commit_all(&path, &msg).unwrap_or(false);
            (committed, ws::changed_files(&path, &base))
        })
        .await;
        if let Ok((committed, files)) = res {
            let mut w2 = w.clone();
            w2.changed_files = files.clone();
            w2.integration_status = "pending".into();
            w2.task_id = Some(task.id.clone());
            w2.updated_at = Utc::now();
            let _ = self.engine.attach_workspace(&task.key, &w2);
            if committed {
                self.engine.emit_custom(
                    EventKind::WorkspaceUpdated,
                    Some(&owner),
                    Some(&task.id),
                    json!({ "note": "uncommitted changes committed by MAW", "files": files }),
                );
            }
        }
    }

    /// One scheduling pass. Idempotent: safe to run at any time.
    pub async fn scheduler_pass(self: &Arc<Self>) -> CoreResult<()> {
        let cfg = self.engine.config();
        let _ = self.engine.sweep_leases()?;
        self.engine
            .tx(|db, emit| self.engine.refresh_ready(db, emit))?;
        let live = self.live_agents().await.into_iter().collect::<HashSet<_>>();
        let paused = self.engine.is_paused();

        self.kickoff_leader(&live).await?;
        self.nudge_completion(&live).await?;
        if !paused {
            self.auto_assign(&live, &cfg).await?;
            self.dispatch_tasks(&live, &cfg).await?;
            self.dispatch_reviews(&live, &cfg).await?;
        }
        self.deliver_messages(&live, &cfg).await?;
        Ok(())
    }

    /// When every task is verified but the Leader is idle and the goal is still open, integrate the
    /// finished work and tell the Leader so it can verify the criteria and complete the goal.
    /// Bounded (2 nudges per goal, spaced) so a stuck Leader cannot cause a message loop.
    async fn nudge_completion(self: &Arc<Self>, live: &HashSet<AgentId>) -> CoreResult<()> {
        let Some(leader) = self.engine.leader()? else {
            return Ok(());
        };
        if !live.contains(&leader.id) || !leader.state.is_available() {
            return Ok(());
        }
        let Some(goal) = self.engine.current_goal()? else {
            return Ok(());
        };
        if !matches!(goal.status, GoalStatus::Active | GoalStatus::Verifying) {
            return Ok(());
        }
        let tasks = self.engine.list_tasks()?;
        let mine: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.goal_id == goal.id && t.status != TaskStatus::Canceled)
            .collect();
        if mine.is_empty() || mine.iter().any(|t| t.status != TaskStatus::Done) {
            return Ok(());
        }
        if !self.engine.undelivered_for(&leader.id)?.is_empty() {
            return Ok(()); // it has mail to process first
        }
        let sent = {
            let g = self
                .nudges
                .lock()
                .map_err(|_| CoreError::Internal("nudge lock".into()))?;
            match g.get(&goal.id) {
                Some((n, at)) if *n >= 2 || at.elapsed() < Duration::from_secs(45) => return Ok(()),
                Some((n, _)) => *n,
                None => 0,
            }
        };
        let results = self.integrate_done_work().await.unwrap_or_else(|e| {
            tracing::warn!("integration failed: {e}");
            vec![]
        });
        if results
            .iter()
            .any(|(_, o)| matches!(o, ws::IntegrationOutcome::Conflict { .. }))
        {
            return Ok(()); // integrate_done_work already told the Leader about the conflict
        }
        let integ = if results.is_empty() {
            "Worker branches are already integrated into `maw/integration`.".to_string()
        } else {
            format!(
                "MAW integrated {} into `maw/integration` without conflicts.",
                results
                    .iter()
                    .map(|(b, _)| format!("`{b}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        self.engine.send_message(
            &Actor::System,
            &leader.id.0,
            MessageType::Instruction,
            "All tasks verified — complete the goal",
            &format!(
                "Every task of the goal is DONE and verified. {integ} The merged result is at {} (read-only inspection).\nCheck the goal's success criteria against it. If they are met, call maw_goal_update(status=\"completed\"). If something is missing, create corrective tasks with maw_task_create.",
                self.integration_path().display()
            ),
            None,
            Some(format!("complete-nudge-{}-{sent}", goal.id)),
        )?;
        if let Ok(mut g) = self.nudges.lock() {
            g.insert(goal.id.clone(), (sent + 1, std::time::Instant::now()));
        }
        Ok(())
    }

    async fn kickoff_leader(self: &Arc<Self>, live: &HashSet<AgentId>) -> CoreResult<()> {
        let Some(leader) = self.engine.leader()? else {
            return Ok(());
        };
        if !live.contains(&leader.id) || !leader.state.is_available() {
            return Ok(());
        }
        let Some(goal) = self.engine.current_goal()? else {
            return Ok(());
        };
        if goal.status != GoalStatus::Active {
            return Ok(());
        }
        let key = format!("kickoff:{}", goal.id);
        let done = self
            .engine
            .read(|db| db.kv_get(self.engine.project_id(), &key))?;
        let briefed = self
            .briefed
            .lock()
            .map(|b| b.contains(&leader.id))
            .unwrap_or(true);
        let prompt = if done.is_none() {
            self.engine
                .store()
                .write(|db| db.kv_set(self.engine.project_id(), &key, "1"))?;
            prompts::goal_prompt(&goal)
        } else if !briefed {
            prompts::resume_briefing(Role::Leader)
        } else {
            return Ok(());
        };
        if let Ok(mut b) = self.briefed.lock() {
            b.insert(leader.id.clone());
        }
        self.send_turn(&leader.id, prompt).await;
        Ok(())
    }

    async fn auto_assign(
        &self,
        live: &HashSet<AgentId>,
        cfg: &OrchestratorConfig,
    ) -> CoreResult<()> {
        if !cfg.auto_assign {
            return Ok(());
        }
        let tasks = self.engine.list_tasks()?;
        let agents = self.engine.list_agents()?;
        let mut free: Vec<&Agent> = agents
            .iter()
            .filter(|a| {
                a.role.is_executor()
                    && live.contains(&a.id)
                    && a.state.is_available()
                    && a.current_task_id.is_none()
            })
            .filter(|a| {
                !tasks.iter().any(|t| {
                    t.owner_agent_id.as_ref() == Some(&a.id)
                        && matches!(t.status, TaskStatus::Assigned | TaskStatus::Running)
                })
            })
            .collect();
        let mut ready: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Ready && t.owner_agent_id.is_none())
            .collect();
        ready.sort_by(|a, b| {
            b.priority
                .rank()
                .cmp(&a.priority.rank())
                .then(a.created_at.cmp(&b.created_at))
        });
        for t in ready {
            let Some(a) = free.pop() else { break };
            if let Err(e) = self.engine.assign_task(&Actor::System, &t.key, &a.id.0) {
                tracing::debug!("auto-assign {} -> {} failed: {e}", t.key, a.name);
            }
        }
        Ok(())
    }

    async fn dispatch_tasks(
        self: &Arc<Self>,
        live: &HashSet<AgentId>,
        cfg: &OrchestratorConfig,
    ) -> CoreResult<()> {
        let tasks = self.engine.list_tasks()?;
        let agents = self.engine.list_agents()?;
        let running = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .count()
            + self.review_dispatched.lock().map(|r| r.len()).unwrap_or(0);
        let mut slots = cfg.max_concurrency.saturating_sub(running);
        let mut assigned: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Assigned)
            .collect();
        assigned.sort_by(|a, b| {
            b.priority
                .rank()
                .cmp(&a.priority.rank())
                .then(a.created_at.cmp(&b.created_at))
        });
        // An agent runs one task at a time; `agents` is a snapshot, so track who was used this pass.
        let mut used: HashSet<AgentId> = HashSet::new();
        for t in assigned {
            if slots == 0 {
                break;
            }
            let Some(owner) = t
                .owner_agent_id
                .as_ref()
                .and_then(|o| agents.iter().find(|a| &a.id == o))
            else {
                continue;
            };
            if used.contains(&owner.id) {
                continue;
            }
            // re-read: the snapshot may be stale by now
            let Ok(owner) = self.engine.get_agent(&owner.id) else {
                continue;
            };
            if !live.contains(&owner.id)
                || !owner.state.is_available()
                || owner.current_task_id.is_some()
            {
                continue;
            }
            let owner = &owner;
            // dependencies must be done (defense in depth; the engine already guarantees it)
            if t.dependencies.iter().any(|d| {
                tasks
                    .iter()
                    .find(|x| &x.id == d)
                    .map(|x| x.status != TaskStatus::Done)
                    .unwrap_or(true)
            }) {
                continue;
            }
            used.insert(owner.id.clone());
            if self.dispatch_task(t, owner, &tasks).await {
                slots -= 1;
            }
        }
        Ok(())
    }

    async fn dispatch_task(self: &Arc<Self>, t: &Task, owner: &Agent, all: &[Task]) -> bool {
        let started = match self.engine.start_task(&t.key) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("cannot start {}: {e}", t.key);
                return false;
            }
        };
        if started.status != TaskStatus::Running {
            return false; // exceeded attempts -> FAILED
        }
        if let Ok(mut r) = self.reminded.lock() {
            r.remove(&started.id);
        }
        // Task-related pending messages (review feedback, answers) travel with the assignment.
        let pending: Vec<Message> = self.engine.undelivered_for(&owner.id).unwrap_or_default();
        let (related, _other): (Vec<Message>, Vec<Message>) = pending
            .into_iter()
            .partition(|m| m.task_id.as_ref() == Some(&started.id) || m.task_id.is_none());
        let deps: Vec<Task> = started
            .dependencies
            .iter()
            .filter_map(|d| all.iter().find(|x| &x.id == d).cloned())
            .collect();
        let workspace = self
            .engine
            .get_agent(&owner.id)
            .ok()
            .and_then(|a| a.workspace)
            .unwrap_or_else(|| self.engine.project().path.clone());
        let branch = self
            .engine
            .snapshot()
            .ok()
            .and_then(|s| {
                s.workspaces
                    .into_iter()
                    .find(|w| w.agent_id.as_ref() == Some(&owner.id) && w.kind == "worktree")
            })
            .and_then(|w| w.branch);
        let prompt = prompts::task_prompt(&started, &deps, &workspace, branch.as_deref(), &related);
        for m in &related {
            let _ = self.engine.mark_delivered(m);
        }
        self.transcripts.push(
            &owner.id,
            LineKind::Protocol,
            format!("[MAW] assigned {} — {}", started.key, started.title),
        );
        if !self.send_turn(&owner.id, prompt).await {
            // nothing was executed: put the task back so another dispatch can retry (attempt count bounds this)
            let _ = self.engine.release_task(
                &Actor::System,
                &started.key,
                "dispatch failed before the agent started",
            );
            let _ = self
                .engine
                .assign_task(&Actor::System, &started.key, &owner.id.0);
            return false;
        }
        true
    }

    async fn dispatch_reviews(
        self: &Arc<Self>,
        live: &HashSet<AgentId>,
        cfg: &OrchestratorConfig,
    ) -> CoreResult<()> {
        let tasks = self.engine.list_tasks()?;
        let agents = self.engine.list_agents()?;
        let running = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .count()
            + self.review_dispatched.lock().map(|r| r.len()).unwrap_or(0);
        let mut slots = cfg.max_concurrency.saturating_sub(running);
        for t in tasks.iter().filter(|t| t.status == TaskStatus::InReview) {
            if slots == 0 {
                break;
            }
            let attempts = self
                .review_dispatched
                .lock()
                .map(|r| r.get(&t.id).copied())
                .unwrap_or(None);
            if attempts.map(|n| n >= 2).unwrap_or(false) {
                continue;
            }
            // an in-flight review is tracked via the reviewer's current task
            if agents.iter().any(|a| {
                a.current_task_id.as_ref() == Some(&t.id)
                    && a.role == Role::Reviewer
                    && a.state.is_busy()
            }) {
                continue;
            }
            let candidates = agents.iter().filter(|a| {
                a.role == Role::Reviewer
                    && live.contains(&a.id)
                    && a.state.is_available()
                    && a.current_task_id.is_none()
                    && Some(&a.id) != t.owner_agent_id.as_ref()
            });
            let reviewer = match &t.reviewer_agent_id {
                Some(r) => candidates.clone().find(|a| &a.id == r),
                None => candidates.clone().next(),
            };
            let Some(reviewer) = reviewer else { continue };
            if self.engine.assign_reviewer(&t.key, &reviewer.id).is_err() {
                continue;
            }
            if let Ok(mut r) = self.review_dispatched.lock() {
                *r.entry(t.id.clone()).or_insert(0) += 1;
            }
            let ws_path = t
                .owner_agent_id
                .as_ref()
                .and_then(|o| agents.iter().find(|a| &a.id == o))
                .and_then(|a| a.workspace.clone());
            let prompt = prompts::review_prompt(t, ws_path.as_deref());
            self.transcripts.push(
                &reviewer.id,
                LineKind::Protocol,
                format!("[MAW] review requested for {}", t.key),
            );
            if self.send_turn(&reviewer.id, prompt).await {
                slots -= 1;
            }
        }
        // drop bookkeeping for tasks that left review
        if let Ok(mut r) = self.review_dispatched.lock() {
            r.retain(|tid, _| {
                tasks
                    .iter()
                    .any(|t| &t.id == tid && t.status == TaskStatus::InReview)
            });
        }
        Ok(())
    }

    async fn deliver_messages(
        self: &Arc<Self>,
        live: &HashSet<AgentId>,
        cfg: &OrchestratorConfig,
    ) -> CoreResult<()> {
        let agents = self.engine.list_agents()?;
        for aid in self.engine.agents_with_pending_messages()? {
            let Some(agent) = agents.iter().find(|a| a.id == aid) else {
                continue;
            };
            if !live.contains(&aid) || !agent.state.is_available() {
                continue; // do not interrupt an agent that is mid-turn
            }
            // an agent with an ASSIGNED task gets its messages with the task prompt instead
            let msgs = self.engine.undelivered_for(&aid)?;
            if msgs.is_empty() {
                continue;
            }
            if agent.role == Role::Leader {
                let oldest = msgs
                    .iter()
                    .map(|m| m.created_at)
                    .min()
                    .unwrap_or_else(Utc::now);
                let age = (Utc::now() - oldest).num_milliseconds().max(0) as u64;
                let from_user = msgs.iter().any(|m| m.sender_agent_id.as_str() == "user");
                if age < cfg.leader_wake_debounce_ms && !from_user {
                    let wake = self.engine.wake_handle();
                    let wait = cfg.leader_wake_debounce_ms - age;
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(wait + 20)).await;
                        wake.notify_one();
                    });
                    continue;
                }
            } else if self.engine.list_tasks()?.iter().any(|t| {
                t.owner_agent_id.as_ref() == Some(&aid) && t.status == TaskStatus::Assigned
            }) && !self.engine.is_paused()
            {
                continue; // will travel with the dispatch
            }
            let names: HashMap<AgentId, String> = agents
                .iter()
                .map(|a| (a.id.clone(), a.name.clone()))
                .collect();
            let name_of = |id: &AgentId| names.get(id).cloned().unwrap_or_else(|| id.0.clone());
            let prompt = prompts::messages_prompt(&msgs, &name_of);
            for m in &msgs {
                let _ = self.engine.mark_delivered(m);
            }
            // The conversation shows the messages themselves, not MAW's wrapper around them.
            // The user's own messages were shown when they were typed.
            for m in &msgs {
                let already = self
                    .shown
                    .lock()
                    .map(|mut s| s.remove(&m.id))
                    .unwrap_or(false);
                if !already {
                    let from = name_of(&m.sender_agent_id);
                    let body = if m.subject.trim().is_empty() {
                        m.content.clone()
                    } else {
                        format!("{}: {}", m.subject, m.content)
                    };
                    self.transcripts.push(
                        &aid,
                        LineKind::User,
                        format!("[{from}] {}", body.chars().take(1500).collect::<String>()),
                    );
                }
            }
            self.send_turn_with(&aid, prompt, None).await;
        }
        Ok(())
    }

    /// Start a turn on an agent session. Returns false if nothing was started.
    async fn send_turn(&self, id: &AgentId, text: String) -> bool {
        let headline = text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        self.send_turn_with(id, text, Some(headline)).await
    }

    /// `shown`: the line to put in the conversation for this prompt (`None`: the caller already did).
    async fn send_turn_with(&self, id: &AgentId, text: String, shown: Option<String>) -> bool {
        let live = match self.sessions.lock().await.get(id).cloned() {
            Some(l) => l,
            None => return false,
        };
        if live.turn_active.swap(true, Ordering::SeqCst) {
            return false; // a turn is already active; never stack turns
        }
        if let Some(line) = shown {
            self.transcripts.push(id, LineKind::User, line);
        }
        self.set_state(id, AgentState::Thinking, None);
        let res =
            tokio::time::timeout(SEND_TIMEOUT, live.handle.send_input(AgentInput::text(text)))
                .await;
        match res {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                live.turn_active.store(false, Ordering::SeqCst);
                self.set_state(id, AgentState::Idle, Some(e.to_string()));
                self.transcripts
                    .push(id, LineKind::Error, format!("send failed: {e}"));
                false
            }
            Err(_) => {
                live.turn_active.store(false, Ordering::SeqCst);
                self.set_state(
                    id,
                    AgentState::Failed,
                    Some("timed out starting a turn".into()),
                );
                false
            }
        }
    }

    /// Integrate completed (DONE) tasks' worker branches into the integration branch.
    pub async fn integrate_done_work(&self) -> CoreResult<Vec<(String, ws::IntegrationOutcome)>> {
        let snapshot = self.engine.snapshot()?;
        let root = self.project_root();
        let mut results = vec![];
        for w in snapshot
            .workspaces
            .iter()
            .filter(|w| w.kind == "worktree" && w.integration_status == "pending")
        {
            let Some(branch) = w.branch.clone() else {
                continue;
            };
            // only integrate when every task owned by that agent which has submitted work is DONE
            let blocked = snapshot.tasks.iter().any(|t| {
                t.owner_agent_id == w.agent_id
                    && matches!(
                        t.status,
                        TaskStatus::InReview | TaskStatus::Verifying | TaskStatus::Running
                    )
            });
            if blocked {
                continue;
            }
            let r = root.clone();
            let b = branch.clone();
            let outcome = tokio::task::spawn_blocking(move || ws::integrate(&r, &b))
                .await
                .map_err(|e| CoreError::Internal(e.to_string()))?;
            let outcome = outcome.map_err(CoreError::Precondition)?;
            let mut w2 = w.clone();
            w2.updated_at = Utc::now();
            match &outcome {
                ws::IntegrationOutcome::Merged { .. } | ws::IntegrationOutcome::NothingToMerge => {
                    w2.integration_status = "integrated".into();
                    w2.conflict = None;
                }
                ws::IntegrationOutcome::Conflict { files } => {
                    w2.integration_status = "conflict".into();
                    w2.conflict = Some(files.join(", "));
                }
            }
            self.engine.store().write(|db| db.upsert_workspace(&w2))?;
            self.engine.emit_custom(EventKind::WorkspaceUpdated, w2.agent_id.as_ref(), None, json!({ "branch": branch, "integration": w2.integration_status, "conflict": w2.conflict }));
            if let ws::IntegrationOutcome::Conflict { files } = &outcome {
                let _ = self.engine.send_message(&Actor::System, "leader", MessageType::Blocker, &format!("Integration conflict on {branch}"), &format!("Merging {branch} into maw/integration conflicts in: {}. Create a corrective task.", files.join(", ")), None, Some(format!("conflict-{branch}-{}", files.join(","))));
            }
            results.push((branch, outcome));
        }
        Ok(results)
    }

    pub fn integration_path(&self) -> PathBuf {
        self.project_root()
            .join(".maw")
            .join("worktrees")
            .join("_integration")
    }

    // ------------------------------------------------------------------ shutdown

    pub async fn shutdown(self: &Arc<Self>) {
        self.cancel.cancel();
        let sessions: Vec<(AgentId, Arc<Live>)> = self.sessions.lock().await.drain().collect();
        self.transcripts.flush_all();
        self.transcripts.commit();
        for (id, l) in sessions {
            let _ = tokio::time::timeout(Duration::from_secs(10), l.handle.shutdown()).await;
            if let Ok(mut p) = l.pump.lock() {
                if let Some(h) = p.take() {
                    h.abort();
                }
            }
            self.engine.revoke_tokens(&id);
            let _ = self
                .engine
                .set_agent_state(&id, AgentState::Disconnected, None);
        }
        let _ = self.engine.shutdown();
    }
}

#[allow(dead_code)]
fn _assert_path(_: &Path) {}

#[cfg(test)]
mod transcript_tests {
    use super::*;

    fn kinds(v: &[TranscriptLine]) -> Vec<(LineKind, String)> {
        v.iter().map(|l| (l.kind, l.text.clone())).collect()
    }

    #[test]
    fn the_conversation_is_saved_and_comes_back_after_a_restart() {
        let store = Arc::new(maw_storage::Store::open_in_memory().unwrap());
        let a = AgentId::from("agent_1");
        let first = Transcripts::with_store(store.clone());
        first.push(&a, LineKind::User, "say hello");
        first.delta(&a, "Hel");
        first.delta(&a, "lo!"); // one streamed reply, still open
        first.push(&a, LineKind::Tool, "read README.md"); // closes it
        first.push(
            &a,
            LineKind::Error,
            "key sk-ant-api03-AAAAAAAAAAAAAAAAAAAA leaked",
        );
        first.delta_kind(&a, LineKind::Thinking, "hmm"); // open when "MAW closes"
        first.flush_all();

        // a new process: nothing in memory, everything from the database, in order
        let second = Transcripts::with_store(store.clone());
        let got = second.get(&a);
        assert_eq!(
            kinds(&got)[..3],
            [
                (LineKind::User, "say hello".to_string()),
                (LineKind::Assistant, "Hello!".to_string()),
                (LineKind::Tool, "read README.md".to_string()),
            ]
        );
        assert!(got.iter().all(|l| !l.open), "restored lines are finished");
        assert!(got[3].text.contains("[REDACTED]") && !got[3].text.contains("AAAAAAAA"));
        assert_eq!(
            got[4].kind,
            LineKind::Thinking,
            "the line open at shutdown was flushed"
        );
        // new lines continue after the restored ones, and other agents are separate
        second.push(&a, LineKind::User, "again");
        assert_eq!(second.get(&a).len(), got.len() + 1);
        assert!(second.get(&AgentId::from("agent_2")).is_empty());
        // removing an agent forgets its conversation for good
        second.forget(&a);
        assert!(Transcripts::with_store(store).get(&a).is_empty());
    }

    #[test]
    fn nothing_open_is_written_twice_and_the_history_is_bounded() {
        let store = Arc::new(maw_storage::Store::open_in_memory().unwrap());
        let a = AgentId::from("agent_1");
        let t = Transcripts::with_store(store.clone());
        t.delta(&a, "streaming");
        t.flush(&a);
        t.flush(&a); // second flush finds nothing open
        assert_eq!(Transcripts::with_store(store.clone()).get(&a).len(), 1);
        for i in 0..(TRANSCRIPT_CAP + 200) {
            t.push(&a, LineKind::System, format!("line {i}"));
        }
        t.commit();
        let rows = store
            .read(|db| db.recent_transcript(&a, usize::MAX >> 1))
            .unwrap();
        assert!(
            rows.len() <= TRANSCRIPT_CAP + 64,
            "stored lines stay bounded: {}",
            rows.len()
        );
        assert_eq!(
            rows.last().unwrap().2,
            format!("line {}", TRANSCRIPT_CAP + 199)
        );
    }
}
