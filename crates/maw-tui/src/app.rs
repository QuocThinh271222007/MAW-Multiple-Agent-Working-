//! Pure UI state and the key reducer. Nothing in here performs I/O: `handle_key` mutates
//! `App` and returns [`Action`]s that the runtime executes against the engine/supervisor.

use crate::input::TextInput;
use crate::theme::Theme;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use maw_core::provider::{ApprovalDecision, ModelChoice};
use maw_core::*;
use maw_orchestrator::{LineKind, Snapshot, TranscriptLine};
use std::cell::Cell;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    Dashboard,
    Agents,
    AgentDetail,
    Tasks,
    Goal,
    Flow,
    Approvals,
    Logs,
    Config,
    Tokens,
    Perf,
}

impl View {
    pub const TABS: [View; 10] = [
        View::Dashboard,
        View::Agents,
        View::Tasks,
        View::Goal,
        View::Flow,
        View::Approvals,
        View::Logs,
        View::Tokens,
        View::Perf,
        View::Config,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            View::Dashboard => "Dashboard",
            View::Agents => "Agents",
            View::AgentDetail => "Agent",
            View::Tasks => "Tasks",
            View::Goal => "Goal",
            View::Flow => "Flow",
            View::Approvals => "Approvals",
            View::Logs => "Logs",
            View::Config => "Config",
            View::Tokens => "Tokens",
            View::Perf => "Perf",
        }
    }
    pub fn key(&self) -> char {
        match self {
            View::Dashboard => 'd',
            View::Agents | View::AgentDetail => 's',
            View::Tasks => 't',
            View::Goal => 'g',
            View::Flow => 'f',
            View::Approvals => 'o',
            View::Logs => 'l',
            View::Config => 'c',
            View::Tokens => '$',
            View::Perf => '%',
        }
    }
}

/// Focusable panels on the dashboard (Tab cycles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Goal,
    Agents,
    Tasks,
    Events,
}

impl Panel {
    fn next(self) -> Panel {
        match self {
            Panel::Goal => Panel::Agents,
            Panel::Agents => Panel::Tasks,
            Panel::Tasks => Panel::Events,
            Panel::Events => Panel::Goal,
        }
    }
    fn prev(self) -> Panel {
        match self {
            Panel::Goal => Panel::Events,
            Panel::Agents => Panel::Goal,
            Panel::Tasks => Panel::Agents,
            Panel::Events => Panel::Tasks,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub level: Level,
    pub at: DateTime<Utc>,
}

/// Commands the runtime executes. Produced by the reducer, never by rendering.
#[derive(Clone, Debug)]
pub enum Action {
    Quit,
    CreateGoal {
        text: String,
        criteria: Vec<String>,
    },
    RedirectGoal {
        text: String,
        reason: String,
        interrupt: bool,
    },
    AddAgent {
        name: String,
        provider: ProviderKind,
        role: Role,
        model: Option<String>,
        effort: Option<String>,
        permission: PermissionProfile,
    },
    ConnectAgent(AgentId),
    /// Download a model for local agents (Ollama).
    PullModel {
        name: String,
    },
    /// Run the provider's official login for this agent's provider, then connect it.
    LoginAgent(AgentId),
    /// Remove an agent from the project (stops its session). `force` releases unfinished tasks.
    RemoveAgent {
        agent: AgentId,
        force: bool,
    },
    ConnectAll,
    /// Reload model lists and reconnect agents that are down.
    Refresh,
    DisconnectAgent(AgentId),
    InterruptAgent(AgentId),
    MessageAgent {
        agent: String,
        text: String,
        interrupt: bool,
    },
    /// Change the model and reasoning effort used by the agent's next session
    /// (`None` = provider default for each).
    SetAgentModel {
        agent: AgentId,
        model: Option<String>,
        effort: Option<String>,
    },
    /// Change the permission profile used by the agent's next session.
    SetAgentPermission {
        agent: AgentId,
        permission: PermissionProfile,
    },
    /// Run a shell command in the project directory (user initiated, `!`).
    RunShell(String),
    Pause,
    Resume,
    RequeueTask(String),
    ReleaseTask {
        key: String,
        reason: String,
    },
    CancelTask {
        key: String,
        reason: String,
    },
    VerifyTask {
        key: String,
        pass: bool,
        notes: String,
    },
    ResolveApproval {
        id: ApprovalId,
        decision: ApprovalDecision,
    },
    Integrate,
    SetConfig(OrchestratorConfig),
    /// Place text on the terminal clipboard (OSC 52); the app also keeps an internal buffer.
    Copy(String),
}

/// A small window shown beside the conversation in the agent view (`/performance`, `/tokens`,
/// `/status`), so the message box keeps working. Esc closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidePanel {
    Perf,
    Tokens,
    Status,
}

/// One agent's turn clock: filled in while it works, summarised in `last` when it stops.
#[derive(Clone, Debug, Default)]
pub struct TurnMark {
    /// When the running turn began (`None` while the agent is not busy).
    pub started: Option<DateTime<Utc>>,
    /// The agent's total token count when this screen first saw the turn.
    pub base_tokens: u64,
    /// Tokens reported during the running turn.
    pub tokens: u64,
    /// The turn that just ended: (seconds, tokens).
    pub last: Option<(i64, u64)>,
}

// ---------------------------------------------------------------- commands (palette)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    ViewDashboard,
    ViewAgents,
    ViewGoal,
    ViewTasks,
    ViewFlow,
    ViewApprovals,
    ViewLogs,
    ViewConfig,
    ViewTokens,
    ViewPerf,
    PanelPerf,
    PanelTokens,
    PanelStatus,
    Workspace,
    ToggleThinking,
    ToggleLogs,
    ToggleProtocol,
    ConversationOnly,
    ShowAllLines,
    GoalSet,
    GoalRedirect,
    AddAgent,
    ConnectAgent,
    AgentLogin,
    AgentRemove,
    ConnectAll,
    Refresh,
    DisconnectAgent,
    InterruptAgent,
    MessageAgent,
    AgentModel,
    PullModel,
    AgentPermission,
    Shell,
    Pause,
    Resume,
    TaskRequeue,
    TaskRelease,
    TaskCancel,
    TaskVerify,
    Integrate,
    ConcurrencyUp,
    ConcurrencyDown,
    ToggleMono,
    CopyTranscript,
    Help,
    Quit,
}

pub struct CommandSpec {
    pub cmd: Cmd,
    pub name: &'static str,
    pub desc: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        cmd: Cmd::PanelPerf,
        name: "performance",
        desc: "CPU / RAM / GPU / VRAM / speed charts in a window beside the conversation; add 1m, 5m or 30m (%)",
    },
    CommandSpec {
        cmd: Cmd::Workspace,
        name: "workspace",
        desc: "where agent worktrees are created: /workspace project | wsl | windows (without an argument: show)",
    },
    CommandSpec {
        cmd: Cmd::PanelStatus,
        name: "status",
        desc: "this agent: state, model, effort, permission, session, workspace, tokens",
    },
    CommandSpec {
        cmd: Cmd::PanelTokens,
        name: "tokens",
        desc: "token usage per agent and task in a window beside the conversation ($)",
    },
    CommandSpec {
        cmd: Cmd::ViewDashboard,
        name: "view dashboard",
        desc: "overview of goal, agents, tasks and events (d)",
    },
    CommandSpec {
        cmd: Cmd::ViewAgents,
        name: "view agents",
        desc: "agent grid with live output (s)",
    },
    CommandSpec {
        cmd: Cmd::ViewTasks,
        name: "view tasks",
        desc: "task board (t)",
    },
    CommandSpec {
        cmd: Cmd::ViewGoal,
        name: "view goal",
        desc: "objective, plan and progress breakdown (g)",
    },
    CommandSpec {
        cmd: Cmd::ViewFlow,
        name: "view flow",
        desc: "data-flow graph derived from events (f)",
    },
    CommandSpec {
        cmd: Cmd::ViewApprovals,
        name: "view approvals",
        desc: "approval center (o)",
    },
    CommandSpec {
        cmd: Cmd::ViewLogs,
        name: "view logs",
        desc: "logs and diagnostics (l)",
    },
    CommandSpec {
        cmd: Cmd::ViewConfig,
        name: "view config",
        desc: "configuration (c)",
    },
    CommandSpec {
        cmd: Cmd::ViewPerf,
        name: "view performance",
        desc: "CPU, RAM, GPU, VRAM and generation speed charts while local models run (%)",
    },
    CommandSpec {
        cmd: Cmd::ViewTokens,
        name: "view tokens",
        desc: "token usage per agent and task, as reported by the providers ($)",
    },
    CommandSpec {
        cmd: Cmd::GoalSet,
        name: "goal set",
        desc: "set the project objective (G)",
    },
    CommandSpec {
        cmd: Cmd::GoalRedirect,
        name: "goal redirect",
        desc: "change the objective; completed work is kept",
    },
    CommandSpec {
        cmd: Cmd::AddAgent,
        name: "agent add",
        desc: "register and connect a new agent (a)",
    },
    CommandSpec {
        cmd: Cmd::ConnectAgent,
        name: "agent connect",
        desc: "connect the selected agent (C)",
    },
    CommandSpec {
        cmd: Cmd::AgentRemove,
        name: "agent remove",
        desc: "remove the selected agent (X); unfinished work blocks it unless you release it",
    },
    CommandSpec {
        cmd: Cmd::AgentLogin,
        name: "agent login",
        desc: "log the selected agent's provider in (official login, then connect)",
    },
    CommandSpec {
        cmd: Cmd::Refresh,
        name: "refresh",
        desc: "reload the model lists and reconnect agents that are down (same as `maw refresh`)",
    },
    CommandSpec {
        cmd: Cmd::ConnectAll,
        name: "agent connect all",
        desc: "connect every disconnected agent",
    },
    CommandSpec {
        cmd: Cmd::DisconnectAgent,
        name: "agent disconnect",
        desc: "stop the selected agent session (D)",
    },
    CommandSpec {
        cmd: Cmd::InterruptAgent,
        name: "agent interrupt",
        desc: "interrupt the selected agent's current turn (i)",
    },
    CommandSpec {
        cmd: Cmd::MessageAgent,
        name: "agent message",
        desc: "send a message to the selected agent (m)",
    },
    CommandSpec {
        cmd: Cmd::ToggleThinking,
        name: "transcript thinking",
        desc: "show/hide the agent's reasoning: type 'thinking on' or 'thinking off' (T)",
    },
    CommandSpec {
        cmd: Cmd::ToggleLogs,
        name: "transcript tool logs",
        desc: "show/hide tool calls, commands, output, system lines: 'tool logs on|off' (L)",
    },
    CommandSpec {
        cmd: Cmd::ToggleProtocol,
        name: "transcript protocol",
        desc: "show/hide MAW coordination traffic (ack, inbox, progress): 'protocol on|off' (P)",
    },
    CommandSpec {
        cmd: Cmd::ConversationOnly,
        name: "transcript conversation only",
        desc: "hide tool logs and protocol; keep messages, errors and thinking",
    },
    CommandSpec {
        cmd: Cmd::ShowAllLines,
        name: "transcript show all",
        desc: "show every kind of line again",
    },
    CommandSpec {
        cmd: Cmd::AgentModel,
        name: "model",
        desc: "choose the selected agent's model; idle agents reconnect now (M)",
    },
    CommandSpec {
        cmd: Cmd::PullModel,
        name: "download model",
        desc: "download a local model for Ollama agents (suggestions up to 8B)",
    },
    CommandSpec {
        cmd: Cmd::AgentPermission,
        name: "permissions",
        desc: "set safe / balanced / autonomous for the selected agent",
    },
    CommandSpec {
        cmd: Cmd::Shell,
        name: "shell",
        desc: "run a shell command in the project directory (!)",
    },
    CommandSpec {
        cmd: Cmd::Pause,
        name: "scheduling pause",
        desc: "stop dispatching new work (p)",
    },
    CommandSpec {
        cmd: Cmd::Resume,
        name: "scheduling resume",
        desc: "resume dispatching work (r)",
    },
    CommandSpec {
        cmd: Cmd::TaskRequeue,
        name: "task requeue",
        desc: "re-queue the selected FAILED/BLOCKED task (R)",
    },
    CommandSpec {
        cmd: Cmd::TaskRelease,
        name: "task release",
        desc: "release the selected task back to the pool (e)",
    },
    CommandSpec {
        cmd: Cmd::TaskCancel,
        name: "task cancel",
        desc: "cancel the selected task (x)",
    },
    CommandSpec {
        cmd: Cmd::TaskVerify,
        name: "task verify",
        desc: "accept/reject the selected VERIFYING task (v)",
    },
    CommandSpec {
        cmd: Cmd::Integrate,
        name: "integrate",
        desc: "merge finished worker branches into maw/integration",
    },
    CommandSpec {
        cmd: Cmd::ConcurrencyUp,
        name: "concurrency up",
        desc: "allow one more concurrently active agent (+)",
    },
    CommandSpec {
        cmd: Cmd::ConcurrencyDown,
        name: "concurrency down",
        desc: "allow one fewer concurrently active agent (-)",
    },
    CommandSpec {
        cmd: Cmd::ToggleMono,
        name: "theme toggle monochrome",
        desc: "switch between color and monochrome",
    },
    CommandSpec {
        cmd: Cmd::CopyTranscript,
        name: "copy transcript",
        desc: "copy the selected agent's transcript (Y)",
    },
    CommandSpec {
        cmd: Cmd::Help,
        name: "help",
        desc: "show key bindings (?)",
    },
    CommandSpec {
        cmd: Cmd::Quit,
        name: "quit",
        desc: "leave MAW (q / Ctrl+C)",
    },
];

/// Subsequence fuzzy score: `None` if `query` isn't a subsequence of `text`. Higher is better
/// (prefix/contiguous/word-start matches score more).
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut qi, mut score, mut last) = (0usize, 0i32, None::<usize>);
    for (i, &c) in t.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            score += 1;
            if last == Some(i.wrapping_sub(1)) {
                score += 3;
            }
            if i == 0 || t[i - 1] == ' ' {
                score += 4;
            }
            last = Some(i);
            qi += 1;
        }
    }
    if qi == q.len() {
        Some(score - (t.len() as i32 / 8))
    } else {
        None
    }
}

pub fn palette_matches(query: &str) -> Vec<&'static CommandSpec> {
    let mut v: Vec<(i32, &CommandSpec)> = COMMANDS
        .iter()
        .filter_map(|c| {
            let name = fuzzy_score(query, c.name).map(|s| s + 10);
            let desc = fuzzy_score(query, c.desc);
            name.or(desc).map(|s| (s, c))
        })
        .collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.0));
    v.into_iter().map(|(_, c)| c).collect()
}

// ---------------------------------------------------------------- overlays & dialogs

#[derive(Clone, Debug, Default)]
pub enum Overlay {
    #[default]
    None,
    Help,
    Palette(PaletteState),
    Dialog(Dialog),
    /// One-decision list (model, effort, permissions, download).
    Picker(crate::picker::PickerState),
    /// Scrollable text result (e.g. of a `!` shell command).
    Output(OutputState),
}

#[derive(Clone, Debug, Default)]
pub struct OutputState {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PaletteState {
    pub input: TextInput,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub label: String,
    pub input: TextInput,
    /// When non-empty the field is a selector (Left/Right/Space cycles).
    pub choices: Vec<String>,
    pub choice: usize,
    pub required: bool,
    pub hint: String,
    /// Optional description per choice (same order), shown beside it in a long list.
    pub details: Vec<String>,
}

impl Field {
    pub fn text(label: &str, required: bool, hint: &str) -> Field {
        Field {
            label: label.into(),
            input: TextInput::new(),
            choices: vec![],
            choice: 0,
            required,
            hint: hint.into(),
            details: vec![],
        }
    }
    pub fn text_with(label: &str, value: &str, required: bool, hint: &str) -> Field {
        let mut f = Field::text(label, required, hint);
        f.input.set(value);
        f
    }
    pub fn select(label: &str, choices: &[&str], default: usize) -> Field {
        Field {
            label: label.into(),
            input: TextInput::new(),
            choices: choices.iter().map(|s| s.to_string()).collect(),
            choice: default,
            required: true,
            hint: "←/→ to change".into(),
            details: vec![],
        }
    }
    pub fn is_select(&self) -> bool {
        !self.choices.is_empty()
    }
    /// A selector with many options is shown as a scrollable vertical list (↑↓ to choose).
    pub fn is_list(&self) -> bool {
        self.choices.len() >= 4
    }
    pub fn value(&self) -> String {
        if self.is_select() {
            self.choices.get(self.choice).cloned().unwrap_or_default()
        } else {
            self.input.value().trim().to_string()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DialogKind {
    SetGoal,
    RedirectGoal,
    AddAgent,
    MessageAgent { agent: String },
    RemoveAgent { agent: AgentId },
    Shell,
    VerifyTask { key: String },
    ReleaseTask { key: String },
    CancelTask { key: String },
}

#[derive(Clone, Debug)]
pub struct Dialog {
    pub title: String,
    pub kind: DialogKind,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
}

impl Dialog {
    pub fn set_goal() -> Dialog {
        Dialog {
            title: "Set objective".into(),
            kind: DialogKind::SetGoal,
            fields: vec![
                Field::text("Objective", true, "what should the team build?"),
                Field::text("Success criteria", false, "separate with ';'"),
            ],
            focus: 0,
            error: None,
        }
    }
    pub fn redirect_goal(current: &str) -> Dialog {
        Dialog {
            title: "Redirect objective (completed tasks are kept)".into(),
            kind: DialogKind::RedirectGoal,
            fields: vec![
                Field::text_with("New objective", current, true, ""),
                Field::text("Reason", true, "recorded in the goal history"),
                Field::select("Interrupt Leader now", &["no", "yes"], 0),
            ],
            focus: 0,
            error: None,
        }
    }
    pub fn add_agent() -> Dialog {
        Self::add_agent_with(&HashMap::new())
    }
    /// Add-agent dialog whose Model and Effort are pick-lists drawn from the providers' catalogs
    /// (nothing to type, so nothing to mistype). They follow the chosen provider and model.
    pub fn add_agent_with(models: &HashMap<ProviderKind, Vec<ModelChoice>>) -> Dialog {
        Self::add_agent_for(models, false)
    }
    /// `include_mock` offers the scripted mock provider; only `maw demo` projects want that.
    pub fn add_agent_for(
        models: &HashMap<ProviderKind, Vec<ModelChoice>>,
        include_mock: bool,
    ) -> Dialog {
        let providers: &[&str] = if include_mock {
            &["claude", "codex", "ollama", "mock"]
        } else {
            &["claude", "codex", "ollama"]
        };
        let mut d = Dialog {
            title: "Add agent".into(),
            kind: DialogKind::AddAgent,
            fields: vec![
                Field::text("Name", true, "e.g. codex-backend"),
                Field::select("Provider", providers, 1),
                Field::select("Role", &["worker", "leader", "reviewer", "specialist"], 0),
                Field::select("Model", &["default"], 0),
                Field::select("Effort", &["default"], 0),
                Field::select("Permissions", &["safe", "balanced", "autonomous"], 1),
            ],
            focus: 0,
            error: None,
        };
        refresh_add_agent(&mut d, models, true);
        d
    }
    pub fn remove_agent(agent: &Agent) -> Dialog {
        Dialog {
            title: format!(
                "Remove {} ({} / {})",
                agent.name, agent.provider, agent.role
            ),
            kind: DialogKind::RemoveAgent {
                agent: agent.id.clone(),
            },
            fields: vec![
                Field::select("Really remove it?", &["no, keep it", "yes, remove it"], 0),
                Field::select(
                    "If it has unfinished work",
                    &["refuse to remove", "release its tasks and remove"],
                    0,
                ),
            ],
            focus: 0,
            error: None,
        }
    }
    pub fn shell() -> Dialog {
        Dialog {
            title: "Run in the project folder (output shown after it finishes, 60 s limit)".into(),
            kind: DialogKind::Shell,
            fields: vec![Field::text("$", true, "e.g. git status")],
            focus: 0,
            error: None,
        }
    }
    pub fn message(agent: &str) -> Dialog {
        Dialog {
            title: format!("Message {agent}"),
            kind: DialogKind::MessageAgent {
                agent: agent.to_string(),
            },
            fields: vec![
                Field::text("Message", true, ""),
                Field::select("Interrupt current turn", &["no", "yes"], 0),
            ],
            focus: 0,
            error: None,
        }
    }
    pub fn verify(key: &str) -> Dialog {
        Dialog {
            title: format!("Verify {key}"),
            kind: DialogKind::VerifyTask {
                key: key.to_string(),
            },
            fields: vec![
                Field::select("Result", &["pass", "fail (send back)"], 0),
                Field::text("Notes", true, "what did you check?"),
            ],
            focus: 1,
            error: None,
        }
    }
    pub fn release(key: &str) -> Dialog {
        Dialog {
            title: format!("Release {key}"),
            kind: DialogKind::ReleaseTask {
                key: key.to_string(),
            },
            fields: vec![Field::text("Reason", true, "")],
            focus: 0,
            error: None,
        }
    }
    pub fn cancel(key: &str) -> Dialog {
        Dialog {
            title: format!("Cancel {key}"),
            kind: DialogKind::CancelTask {
                key: key.to_string(),
            },
            fields: vec![Field::text("Reason", true, "")],
            focus: 0,
            error: None,
        }
    }

    /// Validate and convert to an [`Action`].
    pub fn submit(&self) -> Result<Action, String> {
        for f in &self.fields {
            if f.required && f.value().is_empty() {
                return Err(format!("{} is required", f.label));
            }
        }
        let v = |i: usize| self.fields.get(i).map(|f| f.value()).unwrap_or_default();
        Ok(match &self.kind {
            DialogKind::SetGoal => Action::CreateGoal {
                text: v(0),
                criteria: v(1)
                    .split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            },
            DialogKind::RedirectGoal => Action::RedirectGoal {
                text: v(0),
                reason: v(1),
                interrupt: v(2) == "yes",
            },
            DialogKind::AddAgent => {
                let name = v(0);
                if name.len() > 64 || name.chars().any(|c| c.is_control()) {
                    return Err("name must be 1..64 printable characters".into());
                }
                Action::AddAgent {
                    name,
                    provider: v(1).parse().map_err(|e: String| e)?,
                    role: v(2).parse().map_err(|e: String| e)?,
                    model: Some(v(3)).filter(|m| !m.is_empty() && m != "default"),
                    effort: Some(v(4)).filter(|e| !e.is_empty() && e != "default"),
                    permission: v(5).parse().map_err(|e: String| e)?,
                }
            }
            DialogKind::RemoveAgent { agent } => {
                if !v(0).starts_with("yes") {
                    return Err("choose 'yes, remove it' to confirm (Esc cancels)".into());
                }
                Action::RemoveAgent {
                    agent: agent.clone(),
                    force: v(1).starts_with("release"),
                }
            }
            DialogKind::Shell => Action::RunShell(v(0)),
            DialogKind::MessageAgent { agent } => Action::MessageAgent {
                agent: agent.clone(),
                text: v(0),
                interrupt: v(1) == "yes",
            },
            DialogKind::VerifyTask { key } => Action::VerifyTask {
                key: key.clone(),
                pass: v(0) == "pass",
                notes: v(1),
            },
            DialogKind::ReleaseTask { key } => Action::ReleaseTask {
                key: key.clone(),
                reason: v(0),
            },
            DialogKind::CancelTask { key } => Action::CancelTask {
                key: key.clone(),
                reason: v(0),
            },
        })
    }
}

// ---------------------------------------------------------------- app state

#[derive(Clone, Debug, Default)]
pub struct TaskDetail {
    pub key: String,
    pub attempts: Vec<Attempt>,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchState {
    pub input: TextInput,
    pub editing: bool,
    /// Index into the list of matches.
    pub current: usize,
}

pub struct App {
    pub theme: Theme,
    pub snap: Option<Snapshot>,
    pub view: View,
    pub prev_view: View,
    pub panel: Panel,
    pub selected_agent: usize,
    pub task_sel: usize,
    pub task_detail_focus: bool,
    pub task_detail_scroll: usize,
    pub detail_scroll: usize,
    pub search: Option<SearchState>,
    pub approval_sel: usize,
    pub logs_scroll: usize,
    pub goal_scroll: usize,
    pub flow_scroll: usize,
    pub flow_list_mode: bool,
    pub events_scroll: usize,
    pub overlay: Overlay,
    pub toast: Option<Toast>,
    pub transcripts: HashMap<AgentId, Vec<TranscriptLine>>,
    pub task_detail: Option<TaskDetail>,
    pub extra_events: Vec<Event>,
    /// Stored `agent.usage` events (loaded while the Tokens view is open).
    pub usage_events: Vec<Event>,
    /// Conversation filters (agent views): the provider's streamed reasoning, tool/command/output/
    /// system lines, and MAW's own coordination traffic (maw_* tool calls, dispatch notices).
    pub show_thinking: bool,
    pub show_logs: bool,
    pub show_protocol: bool,
    pub goal_history: Vec<GoalRevision>,
    pub plan_history: Vec<PlanRevision>,
    pub config: OrchestratorConfig,
    pub clipboard: String,
    /// Models each provider offers (filled in the background at startup).
    pub models: HashMap<ProviderKind, Vec<ModelChoice>>,
    /// Models the local provider suggests downloading.
    pub pull_suggestions: Vec<ModelChoice>,
    /// Machine performance samples (filled while the Perf view or a local agent needs them).
    pub perf: maw_core::perf::PerfHistory,
    /// Index into `ui::perf_view::WINDOWS`.
    pub perf_window: usize,
    /// The message box at the bottom of the agent view (always ready for typing, like Claude Code).
    pub composer: TextInput,
    /// `true`: keys go to the message box; `false` (Tab): browse keys (copy, search, filters).
    pub composer_focus: bool,
    /// Highlighted entry of the `/` command menu above the message box.
    pub composer_sel: usize,
    /// Horizontal scroll offsets of the title bar and the hint bar (mouse wheel over the bar).
    pub header_scroll: usize,
    pub footer_scroll: usize,
    /// The window open beside the conversation, if any.
    pub side: Option<SidePanel>,
    /// Per agent: the running turn's clock and token count (see `track_turns`).
    pub turn_marks: HashMap<AgentId, TurnMark>,
    /// Mouse events received so far. Zero after using the wheel means the terminal does not pass
    /// the mouse on (it may turn the wheel into arrow keys), which `?` explains.
    pub mouse_seen: u64,
    /// Largest useful offsets, written by the renderer so the reducer can clamp.
    pub header_max: Cell<usize>,
    pub footer_max: Cell<usize>,
    pub now: DateTime<Utc>,
    pub size: (u16, u16),
    pub quit: bool,
    /// Runtime should reload per-selection detail data (task events/attempts).
    pub detail_dirty: bool,
}

impl App {
    pub fn new(theme: Theme) -> App {
        App {
            theme,
            snap: None,
            view: View::Dashboard,
            prev_view: View::Dashboard,
            panel: Panel::Agents,
            selected_agent: 0,
            task_sel: 0,
            task_detail_focus: false,
            task_detail_scroll: 0,
            detail_scroll: 0,
            search: None,
            approval_sel: 0,
            logs_scroll: 0,
            goal_scroll: 0,
            flow_scroll: 0,
            flow_list_mode: false,
            events_scroll: 0,
            overlay: Overlay::None,
            toast: None,
            transcripts: HashMap::new(),
            task_detail: None,
            extra_events: vec![],
            usage_events: vec![],
            show_thinking: true,
            show_logs: true,
            show_protocol: true,
            goal_history: vec![],
            plan_history: vec![],
            config: OrchestratorConfig::default(),
            clipboard: String::new(),
            models: HashMap::new(),
            pull_suggestions: Vec::new(),
            perf: maw_core::perf::PerfHistory::new(1),
            perf_window: 1,
            composer: TextInput::new(),
            composer_focus: true,
            composer_sel: 0,
            header_scroll: 0,
            footer_scroll: 0,
            mouse_seen: 0,
            turn_marks: HashMap::new(),
            side: None,
            header_max: Cell::new(0),
            footer_max: Cell::new(0),
            now: Utc::now(),
            size: (120, 36),
            quit: false,
            detail_dirty: true,
        }
    }

    /// Follow each agent's turns from the snapshots: when a turn began and how many tokens the
    /// provider has reported since. Shown on the message box like Claude Code's "working" line.
    fn track_turns(&mut self) {
        let Some(snap) = self.snap.as_ref() else {
            return;
        };
        let now = self.now;
        for a in &snap.agents {
            let total = a
                .usage
                .total_tokens
                .unwrap_or(a.usage.input_tokens.unwrap_or(0) + a.usage.output_tokens.unwrap_or(0));
            let mark = self.turn_marks.entry(a.id.clone()).or_default();
            if a.state.is_busy() {
                if mark.started.is_none() {
                    // the event log knows when the turn really began (this screen may be newer)
                    let began = snap
                        .recent_events
                        .iter()
                        .rev()
                        .find(|e| {
                            e.event_type == EventKind::AgentTurnStarted
                                && e.agent_id.as_ref() == Some(&a.id)
                        })
                        .map(|e| e.timestamp)
                        .filter(|t| now - *t < chrono::Duration::hours(6));
                    mark.started = Some(began.unwrap_or(now));
                    mark.base_tokens = total;
                }
                mark.tokens = total.saturating_sub(mark.base_tokens);
            } else if let Some(s) = mark.started.take() {
                mark.last = Some((
                    (now - s).num_seconds().max(0),
                    total.saturating_sub(mark.base_tokens),
                ));
                mark.tokens = 0;
            }
        }
    }

    pub fn apply_snapshot(&mut self, snap: Snapshot) {
        self.snap = Some(snap);
        self.track_turns();
        let na = self.agents().len();
        self.selected_agent = self.selected_agent.min(na.saturating_sub(1));
        let nt = self.snap.as_ref().map(|s| s.tasks.len()).unwrap_or(0);
        self.task_sel = self.task_sel.min(nt.saturating_sub(1));
        let np = self.pending_approvals().len();
        self.approval_sel = self.approval_sel.min(np.saturating_sub(1));
        self.detail_dirty = true;
    }

    pub fn agents(&self) -> &[Agent] {
        self.snap
            .as_ref()
            .map(|s| s.agents.as_slice())
            .unwrap_or(&[])
    }
    pub fn tasks(&self) -> &[Task] {
        self.snap
            .as_ref()
            .map(|s| s.tasks.as_slice())
            .unwrap_or(&[])
    }
    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agents().get(self.selected_agent)
    }
    pub fn agent_name(&self, id: &str) -> String {
        match id {
            "user" => "user".into(),
            "orchestrator" => "MCP".into(),
            _ => self
                .agents()
                .iter()
                .find(|a| a.id.0 == id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| id.to_string()),
        }
    }
    pub fn pending_approvals(&self) -> Vec<&Approval> {
        self.snap
            .as_ref()
            .map(|s| {
                s.approvals
                    .iter()
                    .filter(|a| a.status == ApprovalStatus::Pending)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Task indices in board order (canonical status order, then key number).
    pub fn board_order(&self) -> Vec<usize> {
        let tasks = self.tasks();
        let rank = |s: TaskStatus| TaskStatus::ALL.iter().position(|x| *x == s).unwrap_or(99);
        let num = |t: &Task| {
            t.key
                .trim_start_matches("T-")
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        };
        let mut idx: Vec<usize> = (0..tasks.len()).collect();
        idx.sort_by_key(|&i| (rank(tasks[i].status), num(&tasks[i])));
        idx
    }

    pub fn selected_task(&self) -> Option<&Task> {
        let order = self.board_order();
        order.get(self.task_sel).and_then(|&i| self.tasks().get(i))
    }

    pub fn set_toast(&mut self, text: impl Into<String>, level: Level) {
        self.toast = Some(Toast {
            text: text.into(),
            level,
            at: self.now,
        });
    }

    pub fn goto(&mut self, v: View) {
        if self.view != v {
            if self.view != View::AgentDetail || v != View::AgentDetail {
                self.prev_view = self.view;
            }
            self.view = v;
            if v == View::AgentDetail {
                self.composer_focus = true;
            }
        }
        self.detail_scroll = 0;
        self.detail_dirty = true;
    }

    pub fn is_text_mode(&self) -> bool {
        !matches!(self.overlay, Overlay::None)
            || self.search.as_ref().map(|s| s.editing).unwrap_or(false)
    }

    /// Whether a transcript line of this kind is shown under the current filters. The agent's
    /// messages, the user's messages and errors are always shown.
    pub fn line_visible(&self, kind: LineKind) -> bool {
        match kind {
            LineKind::Thinking => self.show_thinking,
            LineKind::Protocol => self.show_protocol,
            LineKind::Tool | LineKind::Command | LineKind::Output | LineKind::System => {
                self.show_logs
            }
            LineKind::Assistant | LineKind::User | LineKind::Error => true,
        }
    }

    /// Display lines of the selected agent's transcript (shared by rendering and search).
    pub fn transcript_lines(&self) -> Vec<crate::ui::agents::DisplayLine> {
        let width = (self.size.0 as usize).saturating_sub(4).max(10);
        match self
            .selected_agent()
            .and_then(|a| self.transcripts.get(&a.id))
        {
            Some(t) => {
                let visible: Vec<TranscriptLine> = t
                    .iter()
                    .filter(|l| self.line_visible(l.kind))
                    .cloned()
                    .collect();
                crate::ui::agents::display_lines(&visible, width)
            }
            None => vec![],
        }
    }

    /// Indices (into `transcript_lines`) matching the current search.
    pub fn search_matches(&self) -> Vec<usize> {
        let Some(s) = &self.search else { return vec![] };
        let q = s.input.value().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        self.transcript_lines()
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    fn scroll_to_line(&mut self, line: usize) {
        let total = self.transcript_lines().len();
        let page = (self.size.1 as usize).saturating_sub(10).max(3);
        // offset from bottom such that `line` sits mid-page
        let bottom_of_view = (line + page / 2).min(total.saturating_sub(1));
        self.detail_scroll = total.saturating_sub(1).saturating_sub(bottom_of_view);
    }

    pub fn last_assistant_block(&self) -> Option<String> {
        let a = self.selected_agent()?;
        self.transcripts
            .get(&a.id)?
            .iter()
            .rev()
            .find(|l| l.kind == maw_orchestrator::LineKind::Assistant)
            .map(|l| l.text.clone())
    }

    pub fn full_transcript_text(&self) -> String {
        let Some(a) = self.selected_agent() else {
            return String::new();
        };
        let mut out = String::new();
        for l in self
            .transcripts
            .get(&a.id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|l| self.line_visible(l.kind))
        {
            out.push_str(&l.text);
            out.push('\n');
            if out.len() > 100_000 {
                break;
            }
        }
        out
    }
}

// ---------------------------------------------------------------- reducer

fn copy_action(app: &mut App, text: String, what: &str) -> Vec<Action> {
    if text.is_empty() {
        app.set_toast(format!("nothing to copy ({what})"), Level::Info);
        return vec![];
    }
    app.clipboard = text.clone();
    app.set_toast(
        format!("copied {what} ({} chars)", text.chars().count()),
        Level::Ok,
    );
    vec![Action::Copy(text)]
}

fn need_agent(app: &mut App) -> Option<Agent> {
    match app.selected_agent().cloned() {
        Some(a) => Some(a),
        None => {
            app.set_toast("no agent selected", Level::Error);
            None
        }
    }
}

fn need_task(app: &mut App) -> Option<Task> {
    match app.selected_task().cloned() {
        Some(t) => Some(t),
        None => {
            app.set_toast("no task selected", Level::Error);
            None
        }
    }
}

/// Execute a named command (palette entries and many hotkeys route through here).
pub fn run_cmd(app: &mut App, cmd: Cmd) -> Vec<Action> {
    match cmd {
        Cmd::ViewDashboard => app.goto(View::Dashboard),
        Cmd::ViewAgents => app.goto(View::Agents),
        Cmd::ViewGoal => app.goto(View::Goal),
        Cmd::ViewTasks => app.goto(View::Tasks),
        Cmd::ViewFlow => app.goto(View::Flow),
        Cmd::ViewApprovals => app.goto(View::Approvals),
        Cmd::ViewLogs => app.goto(View::Logs),
        Cmd::ViewConfig => app.goto(View::Config),
        Cmd::ViewTokens => app.goto(View::Tokens),
        Cmd::ViewPerf => app.goto(View::Perf),
        Cmd::Workspace => app.set_toast(
            "worktrees: type /workspace project | wsl | windows in the message box (without an argument it shows the current one)",
            Level::Info,
        ),
        Cmd::PanelPerf => open_panel(app, SidePanel::Perf, true),
        Cmd::PanelTokens => open_panel(app, SidePanel::Tokens, true),
        Cmd::PanelStatus => open_panel(app, SidePanel::Status, true),
        Cmd::ToggleThinking => {
            app.show_thinking = !app.show_thinking;
            let msg = format!(
                "thinking {}",
                if app.show_thinking { "shown" } else { "hidden" }
            );
            app.set_toast(msg, Level::Info);
        }
        Cmd::ToggleLogs => {
            app.show_logs = !app.show_logs;
            let msg = format!(
                "tool logs {}",
                if app.show_logs { "shown" } else { "hidden" }
            );
            app.set_toast(msg, Level::Info);
        }
        Cmd::ToggleProtocol => {
            app.show_protocol = !app.show_protocol;
            let msg = format!(
                "MAW protocol lines {}",
                if app.show_protocol { "shown" } else { "hidden" }
            );
            app.set_toast(msg, Level::Info);
        }
        Cmd::ConversationOnly => {
            app.show_logs = false;
            app.show_protocol = false;
            app.set_toast(
                "conversation only: tool logs and protocol hidden (thinking unchanged)",
                Level::Info,
            );
        }
        Cmd::ShowAllLines => {
            app.show_thinking = true;
            app.show_logs = true;
            app.show_protocol = true;
            app.set_toast("all lines shown", Level::Info);
        }
        Cmd::GoalSet => app.overlay = Overlay::Dialog(Dialog::set_goal()),
        Cmd::GoalRedirect => match app
            .snap
            .as_ref()
            .and_then(|s| s.goal.as_ref())
            .map(|g| g.text.clone())
        {
            Some(t) => app.overlay = Overlay::Dialog(Dialog::redirect_goal(&t)),
            None => app.overlay = Overlay::Dialog(Dialog::set_goal()),
        },
        Cmd::AddAgent => {
            let demo = app
                .agents()
                .iter()
                .any(|a| a.provider == ProviderKind::Mock);
            app.overlay = Overlay::Dialog(Dialog::add_agent_for(&app.models, demo));
        }
        Cmd::ConnectAgent => {
            if let Some(a) = need_agent(app) {
                return vec![Action::ConnectAgent(a.id)];
            }
        }
        Cmd::AgentRemove => {
            if let Some(a) = need_agent(app) {
                app.overlay = Overlay::Dialog(Dialog::remove_agent(&a));
            }
        }
        Cmd::AgentLogin => {
            if let Some(a) = need_agent(app) {
                return vec![Action::LoginAgent(a.id)];
            }
        }
        Cmd::ConnectAll => return vec![Action::ConnectAll],
        Cmd::Refresh => return vec![Action::Refresh],
        Cmd::DisconnectAgent => {
            if let Some(a) = need_agent(app) {
                return vec![Action::DisconnectAgent(a.id)];
            }
        }
        Cmd::InterruptAgent => {
            if let Some(a) = need_agent(app) {
                return vec![Action::InterruptAgent(a.id)];
            }
        }
        Cmd::MessageAgent => {
            if let Some(a) = need_agent(app) {
                app.overlay = Overlay::Dialog(Dialog::message(&a.name));
            }
        }
        Cmd::AgentModel => {
            if let Some(a) = need_agent(app) {
                let models = app.models.get(&a.provider).cloned().unwrap_or_default();
                app.overlay = Overlay::Picker(crate::picker::model_picker(&a, &models));
            }
        }
        Cmd::PullModel => {
            let installed = app
                .models
                .get(&ProviderKind::Ollama)
                .cloned()
                .unwrap_or_default();
            if app.pull_suggestions.is_empty() {
                app.set_toast(
                    "no downloadable models: the Ollama provider is not available",
                    Level::Error,
                );
            } else {
                app.overlay = Overlay::Picker(crate::picker::pull_picker(
                    &app.pull_suggestions,
                    &installed,
                ));
            }
        }
        Cmd::AgentPermission => {
            if let Some(a) = need_agent(app) {
                app.overlay = Overlay::Picker(crate::picker::permission_picker(&a));
            }
        }
        Cmd::Shell => app.overlay = Overlay::Dialog(Dialog::shell()),
        Cmd::Pause => return vec![Action::Pause],
        Cmd::Resume => return vec![Action::Resume],
        Cmd::TaskRequeue => {
            if let Some(t) = need_task(app) {
                if matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked) {
                    return vec![Action::RequeueTask(t.key)];
                }
                app.set_toast(
                    format!(
                        "{} is {}; only FAILED or BLOCKED tasks can be requeued",
                        t.key, t.status
                    ),
                    Level::Error,
                );
            }
        }
        Cmd::TaskRelease => {
            if let Some(t) = need_task(app) {
                app.overlay = Overlay::Dialog(Dialog::release(&t.key));
            }
        }
        Cmd::TaskCancel => {
            if let Some(t) = need_task(app) {
                app.overlay = Overlay::Dialog(Dialog::cancel(&t.key));
            }
        }
        Cmd::TaskVerify => {
            if let Some(t) = need_task(app) {
                if t.status == TaskStatus::Verifying {
                    app.overlay = Overlay::Dialog(Dialog::verify(&t.key));
                } else {
                    app.set_toast(
                        format!(
                            "{} is {}; only VERIFYING tasks can be verified",
                            t.key, t.status
                        ),
                        Level::Error,
                    );
                }
            }
        }
        Cmd::Integrate => return vec![Action::Integrate],
        Cmd::ConcurrencyUp | Cmd::ConcurrencyDown => {
            let mut c = app.config.clone();
            c.max_concurrency = if cmd == Cmd::ConcurrencyUp {
                (c.max_concurrency + 1).min(32)
            } else {
                c.max_concurrency.saturating_sub(1).max(1)
            };
            app.config = c.clone();
            return vec![Action::SetConfig(c)];
        }
        Cmd::ToggleMono => {
            app.theme.mono = !app.theme.mono;
        }
        Cmd::CopyTranscript => {
            let t = app.full_transcript_text();
            return copy_action(app, t, "transcript");
        }
        Cmd::Help => app.overlay = Overlay::Help,
        Cmd::Quit => {
            app.quit = true;
            return vec![Action::Quit];
        }
    }
    vec![]
}

pub fn handle_mouse(app: &mut App, m: MouseEvent) -> Vec<Action> {
    // Wheel over the title bar or the hint bar scrolls that bar sideways (Shift+wheel and
    // horizontal wheels work anywhere on it), so long key lists can always be read.
    app.mouse_seen += 1;
    let on_header = m.row == 0;
    let on_footer = app.size.1 > 0 && m.row == app.size.1 - 1;
    let dir: Option<isize> = match m.kind {
        MouseEventKind::ScrollLeft => Some(-1),
        MouseEventKind::ScrollRight => Some(1),
        MouseEventKind::ScrollUp
            if on_header || on_footer || m.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            Some(-1)
        }
        MouseEventKind::ScrollDown
            if on_header || on_footer || m.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            Some(1)
        }
        _ => None,
    };
    if let Some(d) = dir {
        const STEP: usize = 8;
        let (off, max) = if on_footer && !on_header {
            (&mut app.footer_scroll, app.footer_max.get())
        } else {
            (&mut app.header_scroll, app.header_max.get())
        };
        *off = if d < 0 {
            off.saturating_sub(STEP)
        } else {
            (*off + STEP).min(max)
        };
        return vec![];
    }
    let code = match m.kind {
        MouseEventKind::ScrollUp => KeyCode::Up,
        MouseEventKind::ScrollDown => KeyCode::Down,
        _ => return vec![],
    };
    let mut out = vec![];
    for _ in 0..3 {
        out.extend(handle_key(app, KeyEvent::new(code, KeyModifiers::NONE)));
    }
    out
}

fn output_key(app: &mut App, mut o: OutputState, key: KeyEvent) -> Vec<Action> {
    let page = (app.size.1 as usize).saturating_sub(8).max(3);
    let max = o.lines.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return vec![],
        KeyCode::Up | KeyCode::Char('k') => o.scroll = o.scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => o.scroll = (o.scroll + 1).min(max),
        KeyCode::PageUp => o.scroll = o.scroll.saturating_sub(page),
        KeyCode::PageDown => o.scroll = (o.scroll + page).min(max),
        KeyCode::Home => o.scroll = 0,
        KeyCode::End => o.scroll = max,
        KeyCode::Char('y') => {
            let text = o.lines.join(
                "
",
            );
            app.clipboard = text.clone();
            app.overlay = Overlay::Output(o);
            return vec![Action::Copy(text)];
        }
        _ => {}
    }
    app.overlay = Overlay::Output(o);
    vec![]
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Vec<Action> {
    if key.kind == KeyEventKind::Release {
        return vec![];
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        app.quit = true;
        return vec![Action::Quit];
    }
    match std::mem::take(&mut app.overlay) {
        Overlay::Dialog(d) => return dialog_key(app, d, key),
        Overlay::Palette(p) => return palette_key(app, p, key),
        Overlay::Picker(p) => return crate::picker::picker_key(app, p, key),
        Overlay::Output(o) => return output_key(app, o, key),
        Overlay::Help => {
            if !matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
            ) {
                app.overlay = Overlay::Help;
            }
            return vec![];
        }
        Overlay::None => {}
    }
    if app.search.as_ref().map(|s| s.editing).unwrap_or(false) {
        return search_key(app, key);
    }
    normal_key(app, key)
}

fn dialog_key(app: &mut App, mut d: Dialog, key: KeyEvent) -> Vec<Action> {
    let n = d.fields.len();
    let in_list = d.fields.get(d.focus).map(|f| f.is_list()).unwrap_or(false);
    match key.code {
        KeyCode::Esc => return vec![],
        // inside a long pick-list the arrows move through the options (and on to the next or
        // previous field past either end); Tab / Shift+Tab / Enter always move between fields
        KeyCode::Down
        | KeyCode::Up
        | KeyCode::PageDown
        | KeyCode::PageUp
        | KeyCode::Home
        | KeyCode::End
            if in_list =>
        {
            let (at_top, at_bottom) = d
                .fields
                .get(d.focus)
                .map(|f| (f.choice == 0, f.choice + 1 >= f.choices.len()))
                .unwrap_or((false, false));
            // past either end of the list the arrows carry on to the neighbouring field
            if key.code == KeyCode::Up && at_top && d.focus > 0 {
                d.focus -= 1;
            } else if key.code == KeyCode::Down && at_bottom && d.focus + 1 < n {
                d.focus += 1;
            } else if let Some(f) = d.fields.get_mut(d.focus) {
                let last = f.choices.len() - 1;
                f.choice = match key.code {
                    KeyCode::Down => (f.choice + 1).min(last),
                    KeyCode::Up => f.choice.saturating_sub(1),
                    KeyCode::PageDown => (f.choice + 5).min(last),
                    KeyCode::PageUp => f.choice.saturating_sub(5),
                    KeyCode::Home => 0,
                    _ => last,
                };
            }
            d.error = None;
            after_dialog_edit(app, &mut d);
        }
        KeyCode::Tab | KeyCode::Down => d.focus = (d.focus + 1) % n.max(1),
        KeyCode::BackTab | KeyCode::Up => d.focus = (d.focus + n.max(1) - 1) % n.max(1),
        KeyCode::Enter => {
            if d.focus + 1 < n {
                d.focus += 1;
            } else {
                match d.submit() {
                    Ok(a) => return vec![a],
                    Err(e) => d.error = Some(e),
                }
            }
        }
        _ => {
            if let Some(f) = d.fields.get_mut(d.focus) {
                if f.is_select() {
                    let len = f.choices.len();
                    match key.code {
                        KeyCode::Left => f.choice = (f.choice + len - 1) % len,
                        KeyCode::Right | KeyCode::Char(' ') => f.choice = (f.choice + 1) % len,
                        _ => {}
                    }
                } else {
                    f.input.handle_key(key);
                }
                d.error = None;
            }
            after_dialog_edit(app, &mut d);
        }
    }
    app.overlay = Overlay::Dialog(d);
    vec![]
}

/// Keep the Model / Effort pick-lists consistent with what was just chosen.
fn after_dialog_edit(app: &App, d: &mut Dialog) {
    match d.kind.clone() {
        DialogKind::AddAgent if d.focus == 1 => refresh_add_agent(d, &app.models, true),
        DialogKind::AddAgent if d.focus == 3 => refresh_add_agent(d, &app.models, false),
        _ => {}
    }
}

/// Re-run the add-agent list refresh when the model catalog arrives while the dialog is open.
pub fn refresh_open_dialog(app: &mut App) {
    if let Overlay::Dialog(d) = &mut app.overlay {
        if d.kind == DialogKind::AddAgent
            && d.fields
                .get(3)
                .map(|f| f.choices.len() <= 1)
                .unwrap_or(false)
        {
            refresh_add_agent(d, &app.models, true);
        }
    }
}

/// Rebuild the Model list (when the provider changed) and the Effort list of an add-agent dialog.
fn refresh_add_agent(
    d: &mut Dialog,
    models: &HashMap<ProviderKind, Vec<ModelChoice>>,
    provider_changed: bool,
) {
    let provider: Option<ProviderKind> = d.fields.get(1).and_then(|f| f.value().parse().ok());
    let list: Vec<ModelChoice> = provider
        .and_then(|p| models.get(&p).cloned())
        .unwrap_or_default();
    if provider_changed {
        let mut ids: Vec<String> = vec!["default".into()];
        ids.extend(list.iter().map(|m| m.id.clone()));
        if let Some(f) = d.fields.get_mut(3) {
            f.choices = ids;
            f.choice = 0;
            f.details = std::iter::once("the provider's own default".to_string())
                .chain(list.iter().map(crate::picker::model_detail))
                .collect();
            f.hint = if list.is_empty() {
                "no model list for this provider (not installed / still loading): the default is used"
                    .into()
            } else {
                "←/→ to change".into()
            };
        }
    }
    let model = d
        .fields
        .get(3)
        .map(|f| f.value())
        .filter(|m| m != "default");
    let mut efforts: Vec<String> = vec!["default".into()];
    efforts.extend(maw_orchestrator::models::efforts_for_provider(
        provider.unwrap_or(ProviderKind::Claude),
        &list,
        model.as_deref(),
    ));
    if let Some(f) = d.fields.get_mut(4) {
        let keep = f.choices.get(f.choice).cloned();
        f.choice = keep
            .and_then(|k| efforts.iter().position(|e| *e == k))
            .unwrap_or(0);
        f.hint = if efforts.len() == 1 {
            "this model has no effort setting".into()
        } else {
            String::new()
        };
        f.choices = efforts;
    }
}

fn palette_key(app: &mut App, mut p: PaletteState, key: KeyEvent) -> Vec<Action> {
    let matches = palette_matches(p.input.value());
    match key.code {
        KeyCode::Esc => return vec![],
        KeyCode::Up => p.selected = p.selected.saturating_sub(1),
        KeyCode::Down => p.selected = (p.selected + 1).min(matches.len().saturating_sub(1)),
        KeyCode::Enter => {
            return match matches.get(p.selected) {
                Some(c) => run_cmd(app, c.cmd),
                None => vec![],
            };
        }
        _ => {
            if p.input.handle_key(key) {
                p.selected = 0;
            }
        }
    }
    app.overlay = Overlay::Palette(p);
    vec![]
}

/// Entries of the `/` menu for the text in the message box (empty unless it starts with `/`).
pub fn composer_menu(app: &App) -> Vec<&'static CommandSpec> {
    let v = app.composer.value();
    if !v.starts_with('/') {
        return vec![];
    }
    palette_matches(slash_parts(v).0)
}

/// Split `/thinking off` into the command query and an explicit on/off. The last word counts as
/// an argument as soon as it starts like one (`o`, `of`, `off`, `on`, `show`, `hide`), so the
/// menu keeps listing the command while the argument is being typed.
fn slash_parts(text: &str) -> (&str, &str) {
    let body = text.strip_prefix('/').unwrap_or(text).trim();
    if let Some((q, last)) = body.rsplit_once(char::is_whitespace) {
        let known = SLASH_ARGS.iter().any(|w| {
            // a complete word, or at least two letters of one (one letter is more likely a search)
            *w == last || (last.chars().count() >= 2 && w.starts_with(last))
        });
        if known {
            return (q.trim(), last);
        }
    }
    (body, "")
}

/// Words that count as an argument at the end of a slash command.
const SLASH_ARGS: &[&str] = &[
    "on", "off", "show", "hide", "1m", "5m", "30m", "project", "wsl", "windows",
];

/// The chart windows of `/performance 1m|5m|30m`, in the order of `perf_view::WINDOWS`.
const WINDOW_ARGS: [&str; 3] = ["1m", "5m", "30m"];

/// Open (or with `toggle`, close when already open) a side panel in the agent view. Outside the
/// agent view the matching full view opens instead.
fn open_panel(app: &mut App, panel: SidePanel, toggle: bool) {
    if app.view != View::AgentDetail {
        match panel {
            SidePanel::Perf => app.goto(View::Perf),
            SidePanel::Tokens => app.goto(View::Tokens),
            SidePanel::Status => {
                if app.selected_agent().is_some() {
                    app.goto(View::AgentDetail);
                    app.side = Some(panel);
                }
            }
        }
        return;
    }
    app.side = if toggle && app.side == Some(panel) {
        None
    } else {
        Some(panel)
    };
}

/// `/thinking on|off`, `/tools on|off`, `/protocol on|off`: set a transcript filter explicitly.
fn set_filter(app: &mut App, cmd: Cmd, on: bool) -> bool {
    let (flag, label) = match cmd {
        Cmd::ToggleThinking => (&mut app.show_thinking, "thinking"),
        Cmd::ToggleLogs => (&mut app.show_logs, "tool logs"),
        Cmd::ToggleProtocol => (&mut app.show_protocol, "MAW protocol lines"),
        _ => return false,
    };
    *flag = on;
    let msg = format!("{label} {}", if on { "shown" } else { "hidden" });
    app.set_toast(msg, Level::Info);
    true
}

/// Keys in the message box. `None` = not handled here (scrolling, Ctrl shortcuts, Esc to leave).
fn composer_key(app: &mut App, key: &KeyEvent) -> Option<Vec<Action>> {
    let menu = composer_menu(app);
    match key.code {
        KeyCode::Tab => {
            app.composer_focus = false;
            Some(vec![])
        }
        KeyCode::Esc if !app.composer.value().is_empty() => {
            app.composer.set("");
            app.composer_sel = 0;
            Some(vec![])
        }
        KeyCode::Esc if app.side.is_some() => {
            app.side = None;
            Some(vec![])
        }
        KeyCode::Up if !menu.is_empty() => {
            app.composer_sel = app.composer_sel.saturating_sub(1);
            Some(vec![])
        }
        KeyCode::Down if !menu.is_empty() => {
            app.composer_sel = (app.composer_sel + 1).min(menu.len() - 1);
            Some(vec![])
        }
        KeyCode::Enter => Some(composer_submit(app)),
        // with nothing typed, Home / End jump through the transcript instead of moving a cursor
        KeyCode::Home | KeyCode::End if app.composer.value().is_empty() => None,
        _ => {
            if app.composer.handle_key(*key) {
                app.composer_sel = 0;
                Some(vec![])
            } else {
                None
            }
        }
    }
}

fn composer_submit(app: &mut App) -> Vec<Action> {
    let text = app.composer.value().trim().to_string();
    if text.is_empty() {
        return vec![];
    }
    if text.starts_with('/') {
        let menu = composer_menu(app);
        let Some(cmd) = menu.get(app.composer_sel.min(menu.len().saturating_sub(1))) else {
            app.set_toast(format!("no command matches '{text}'"), Level::Error);
            return vec![];
        };
        let cmd = cmd.cmd;
        let arg = slash_parts(&text).1.to_string();
        app.composer.set("");
        app.composer_sel = 0;
        let flag = match arg.as_str() {
            "on" | "show" => Some(true),
            "off" | "hide" => Some(false),
            _ => None,
        };
        if let Some(on) = flag {
            if set_filter(app, cmd, on) {
                return vec![];
            }
        }
        if cmd == Cmd::Workspace {
            let mut c = app.config.clone();
            if ["project", "wsl", "windows"].contains(&arg.as_str()) {
                c.worktree_location = arg.clone();
                app.config = c.clone();
                app.set_toast(
                    format!(
                        "worktrees now go to: {arg} (new worktrees at once; existing ones move when their agent next connects with no work in flight; saved for this session only: `maw workspace {arg}` makes it permanent)"
                    ),
                    Level::Ok,
                );
                return vec![Action::SetConfig(c)];
            }
            let now = if c.worktree_location.is_empty() {
                "project"
            } else {
                c.worktree_location.as_str()
            };
            app.set_toast(
                format!("worktrees go to: {now}. Change with /workspace project | wsl | windows"),
                Level::Info,
            );
            return vec![];
        }
        if cmd == Cmd::PanelPerf {
            if let Some(i) = WINDOW_ARGS.iter().position(|w| *w == arg) {
                app.perf_window = i;
                open_panel(app, SidePanel::Perf, false);
                return vec![];
            }
        }
        return run_cmd(app, cmd);
    }
    if let Some(shell) = text.strip_prefix('!') {
        app.composer.set("");
        let shell = shell.trim();
        return if shell.is_empty() {
            run_cmd(app, Cmd::Shell)
        } else {
            vec![Action::RunShell(shell.to_string())]
        };
    }
    let Some(name) = app.selected_agent().map(|a| a.name.clone()) else {
        return vec![];
    };
    app.composer.set("");
    app.set_toast(format!("message sent to {name}"), Level::Ok);
    vec![Action::MessageAgent {
        agent: name,
        text,
        interrupt: false,
    }]
}

fn search_key(app: &mut App, key: KeyEvent) -> Vec<Action> {
    let Some(mut s) = app.search.take() else {
        return vec![];
    };
    match key.code {
        KeyCode::Esc => return vec![],
        KeyCode::Enter => {
            s.editing = false;
            app.search = Some(s);
            jump_match(app, 0);
            return vec![];
        }
        _ => {
            if s.input.handle_key(key) {
                s.current = 0;
            }
        }
    }
    app.search = Some(s);
    jump_match(app, 0);
    vec![]
}

fn jump_match(app: &mut App, delta: isize) {
    let matches = app.search_matches();
    if matches.is_empty() {
        return;
    }
    let cur = app.search.as_ref().map(|s| s.current).unwrap_or(0) as isize;
    let next = (cur + delta).rem_euclid(matches.len() as isize) as usize;
    if let Some(s) = app.search.as_mut() {
        s.current = next;
    }
    app.scroll_to_line(matches[next]);
}

/// Move a selection index by `delta`, clamped to `len`.
fn step(sel: &mut usize, delta: isize, len: usize) {
    if len == 0 {
        *sel = 0;
        return;
    }
    *sel = (*sel as isize + delta).clamp(0, len as isize - 1) as usize;
}

fn normal_key(app: &mut App, key: KeyEvent) -> Vec<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let page = (app.size.1 as isize - 8).max(3);
    let in_agent_view = matches!(app.view, View::Dashboard | View::Agents | View::AgentDetail);

    // Alt+Left / Alt+Right scroll the title bar. Some terminals (cmd, for one) turn the wheel into
    // arrow keys and never report where the pointer is, so the bar needs a keyboard way too.
    if key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Left | KeyCode::Right)
    {
        const STEP: usize = 8;
        app.header_scroll = if key.code == KeyCode::Left {
            app.header_scroll.saturating_sub(STEP)
        } else {
            (app.header_scroll + STEP).min(app.header_max.get())
        };
        return vec![];
    }

    // the agent view has a message box that takes the keys until Tab moves to browse mode
    if app.view == View::AgentDetail && app.search.is_none() {
        if app.composer_focus {
            if let Some(actions) = composer_key(app, &key) {
                return actions;
            }
        } else if matches!(key.code, KeyCode::Tab | KeyCode::Enter) {
            app.composer_focus = true;
            return vec![];
        }
    }

    // keys that always work
    match key.code {
        KeyCode::Char('p') if ctrl => {
            app.overlay = Overlay::Palette(PaletteState::default());
            return vec![];
        }
        KeyCode::Char(':') => {
            app.overlay = Overlay::Palette(PaletteState::default());
            return vec![];
        }
        KeyCode::Char('/') if app.view != View::AgentDetail => {
            app.overlay = Overlay::Palette(PaletteState::default());
            return vec![];
        }
        KeyCode::Char('/') => {
            app.search = Some(SearchState {
                editing: true,
                ..Default::default()
            });
            return vec![];
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Help;
            return vec![];
        }
        KeyCode::Char('!') => return run_cmd(app, Cmd::Shell),
        KeyCode::Char('M') if in_agent_view => return run_cmd(app, Cmd::AgentModel),
        KeyCode::Char('X') if in_agent_view => return run_cmd(app, Cmd::AgentRemove),
        KeyCode::Char('T') if in_agent_view => return run_cmd(app, Cmd::ToggleThinking),
        KeyCode::Char('L') if in_agent_view => return run_cmd(app, Cmd::ToggleLogs),
        KeyCode::Char('P') if in_agent_view => return run_cmd(app, Cmd::ToggleProtocol),
        KeyCode::Char('q') => return run_cmd(app, Cmd::Quit),
        KeyCode::Esc => {
            if app.side.is_some() {
                app.side = None;
            } else if app.search.is_some() {
                app.search = None;
            } else if app.view == View::AgentDetail {
                let back = if app.prev_view == View::AgentDetail {
                    View::Agents
                } else {
                    app.prev_view
                };
                app.view = back;
            } else if app.task_detail_focus {
                app.task_detail_focus = false;
            }
            return vec![];
        }
        _ => {}
    }

    // digits select agents
    if let KeyCode::Char(c @ '1'..='9') = key.code {
        let idx = c as usize - '1' as usize;
        if idx < app.agents().len() {
            app.selected_agent = idx;
            app.detail_scroll = 0;
            app.search = None;
            app.detail_dirty = true;
        } else {
            app.set_toast(format!("no agent #{c}"), Level::Info);
        }
        return vec![];
    }

    // view-specific keys first
    let handled = match app.view {
        View::Approvals => approvals_key(app, &key),
        View::Tasks => tasks_key(app, &key, page),
        View::Config => config_key(app, &key),
        View::Flow => flow_key(app, &key, page),
        _ => None,
    };
    if let Some(a) = handled {
        return a;
    }

    match key.code {
        KeyCode::Char('d') => return run_cmd(app, Cmd::ViewDashboard),
        KeyCode::Char('s') => return run_cmd(app, Cmd::ViewAgents),
        KeyCode::Char('g') => return run_cmd(app, Cmd::ViewGoal),
        KeyCode::Char('t') => return run_cmd(app, Cmd::ViewTasks),
        KeyCode::Char('f') => return run_cmd(app, Cmd::ViewFlow),
        KeyCode::Char('l') => return run_cmd(app, Cmd::ViewLogs),
        KeyCode::Char('c') => return run_cmd(app, Cmd::ViewConfig),
        KeyCode::Char('$') => return run_cmd(app, Cmd::ViewTokens),
        KeyCode::Char('%') => return run_cmd(app, Cmd::ViewPerf),
        KeyCode::Char('w') if app.view == View::Perf => {
            app.perf_window = (app.perf_window + 1) % crate::ui::perf_view::WINDOWS.len();
        }
        KeyCode::Char('o') => return run_cmd(app, Cmd::ViewApprovals),
        KeyCode::Char('a') => return run_cmd(app, Cmd::AddAgent),
        KeyCode::Char('p') => return run_cmd(app, Cmd::Pause),
        KeyCode::Char('r') => return run_cmd(app, Cmd::Resume),
        KeyCode::Char('G') => {
            let has_active = app
                .snap
                .as_ref()
                .and_then(|s| s.goal.as_ref())
                .map(|g| matches!(g.status, GoalStatus::Active | GoalStatus::Verifying))
                .unwrap_or(false);
            return run_cmd(
                app,
                if has_active {
                    Cmd::GoalRedirect
                } else {
                    Cmd::GoalSet
                },
            );
        }
        KeyCode::Char('C') if in_agent_view => return run_cmd(app, Cmd::ConnectAgent),
        KeyCode::Char('D') if in_agent_view => return run_cmd(app, Cmd::DisconnectAgent),
        KeyCode::Char('i') if in_agent_view => return run_cmd(app, Cmd::InterruptAgent),
        KeyCode::Char('m') if in_agent_view => return run_cmd(app, Cmd::MessageAgent),
        _ => {}
    }

    match (app.view, key.code) {
        (View::AgentDetail, KeyCode::Char('y')) => {
            let t = app.last_assistant_block().unwrap_or_default();
            return copy_action(app, t, "last message");
        }
        (View::AgentDetail, KeyCode::Char('Y')) => return run_cmd(app, Cmd::CopyTranscript),
        (View::AgentDetail, KeyCode::Char('n')) => {
            jump_match(app, 1);
            return vec![];
        }
        (View::AgentDetail, KeyCode::Char('N')) => {
            jump_match(app, -1);
            return vec![];
        }
        (View::AgentDetail, KeyCode::Up | KeyCode::Char('k')) => app.detail_scroll += 1,
        (View::AgentDetail, KeyCode::Down | KeyCode::Char('j')) => {
            app.detail_scroll = app.detail_scroll.saturating_sub(1)
        }
        (View::AgentDetail, KeyCode::PageUp) => app.detail_scroll += page as usize,
        (View::AgentDetail, KeyCode::PageDown) => {
            app.detail_scroll = app.detail_scroll.saturating_sub(page as usize)
        }
        (View::AgentDetail, KeyCode::Home) => app.detail_scroll = usize::MAX / 2,
        (View::AgentDetail, KeyCode::End) => app.detail_scroll = 0,

        (View::Dashboard, KeyCode::Tab) => app.panel = app.panel.next(),
        (View::Dashboard, KeyCode::BackTab) => app.panel = app.panel.prev(),
        (View::Dashboard, KeyCode::Enter) => match app.panel {
            Panel::Agents if !app.agents().is_empty() => app.goto(View::AgentDetail),
            Panel::Tasks => app.goto(View::Tasks),
            Panel::Goal => app.goto(View::Goal),
            _ => app.goto(View::Logs),
        },
        (
            View::Dashboard,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('j')
            | KeyCode::Char('k'),
        ) => {
            let d: isize = if matches!(key.code, KeyCode::Up | KeyCode::Left | KeyCode::Char('k')) {
                -1
            } else {
                1
            };
            match app.panel {
                Panel::Agents => {
                    let n = app.agents().len();
                    step(&mut app.selected_agent, d, n);
                    app.detail_dirty = true;
                }
                Panel::Tasks => {
                    let n = app.tasks().len();
                    step(&mut app.task_sel, d, n);
                    app.detail_dirty = true;
                }
                Panel::Events => {
                    app.events_scroll = (app.events_scroll as isize - d).max(0) as usize
                }
                Panel::Goal => {}
            }
        }

        (View::Agents, KeyCode::Tab) => step_agent(app, 1),
        (View::Agents, KeyCode::BackTab) => step_agent(app, -1),
        (View::Agents, KeyCode::Right | KeyCode::Down | KeyCode::Char('j')) => step_agent(app, 1),
        (View::Agents, KeyCode::Left | KeyCode::Up | KeyCode::Char('k')) => step_agent(app, -1),
        (View::Agents, KeyCode::Enter) => {
            if !app.agents().is_empty() {
                app.goto(View::AgentDetail)
            }
        }

        (View::Goal, KeyCode::Up | KeyCode::Char('k')) => {
            app.goal_scroll = app.goal_scroll.saturating_sub(1)
        }
        (View::Goal, KeyCode::Down | KeyCode::Char('j')) => app.goal_scroll += 1,
        (View::Goal, KeyCode::PageUp) => {
            app.goal_scroll = app.goal_scroll.saturating_sub(page as usize)
        }
        (View::Goal, KeyCode::PageDown) => app.goal_scroll += page as usize,
        (View::Goal, KeyCode::Enter) => return run_cmd(app, Cmd::GoalRedirect),

        (View::Logs, KeyCode::Up | KeyCode::Char('k')) => app.logs_scroll += 1,
        (View::Logs, KeyCode::Down | KeyCode::Char('j')) => {
            app.logs_scroll = app.logs_scroll.saturating_sub(1)
        }
        (View::Logs, KeyCode::PageUp) => app.logs_scroll += page as usize,
        (View::Logs, KeyCode::PageDown) => {
            app.logs_scroll = app.logs_scroll.saturating_sub(page as usize)
        }
        (View::Logs, KeyCode::End) => app.logs_scroll = 0,
        _ => {
            let _ = shift;
        }
    }
    vec![]
}

fn step_agent(app: &mut App, d: isize) {
    let n = app.agents().len();
    if n > 0 {
        app.selected_agent = (app.selected_agent as isize + d).rem_euclid(n as isize) as usize;
        app.detail_scroll = 0;
        app.search = None;
        app.detail_dirty = true;
    }
}

fn approvals_key(app: &mut App, key: &KeyEvent) -> Option<Vec<Action>> {
    let pending: Vec<ApprovalId> = app
        .pending_approvals()
        .iter()
        .map(|a| a.id.clone())
        .collect();
    let decide = |app: &mut App, d: ApprovalDecision| -> Vec<Action> {
        match pending.get(app.approval_sel) {
            Some(id) => vec![Action::ResolveApproval {
                id: id.clone(),
                decision: d,
            }],
            None => {
                app.set_toast("no pending approval selected", Level::Info);
                vec![]
            }
        }
    };
    Some(match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.approval_sel = app.approval_sel.saturating_sub(1);
            vec![]
        }
        KeyCode::Down | KeyCode::Char('j') => {
            step(&mut app.approval_sel, 1, pending.len());
            vec![]
        }
        KeyCode::Char('y') => decide(app, ApprovalDecision::Approve),
        KeyCode::Char('Y') => decide(app, ApprovalDecision::ApproveForSession),
        KeyCode::Char('n') => decide(app, ApprovalDecision::Deny),
        KeyCode::Char('x') => decide(app, ApprovalDecision::Abort),
        _ => return None,
    })
}

fn tasks_key(app: &mut App, key: &KeyEvent, page: isize) -> Option<Vec<Action>> {
    let n = app.tasks().len();
    let detail = app.task_detail_focus;
    match key.code {
        KeyCode::Up | KeyCode::Char('k') if !detail => step(&mut app.task_sel, -1, n),
        KeyCode::Down | KeyCode::Char('j') if !detail => step(&mut app.task_sel, 1, n),
        KeyCode::PageUp if !detail => step(&mut app.task_sel, -page, n),
        KeyCode::PageDown if !detail => step(&mut app.task_sel, page, n),
        KeyCode::Home if !detail => app.task_sel = 0,
        KeyCode::End if !detail => app.task_sel = n.saturating_sub(1),
        KeyCode::Left | KeyCode::Right if !detail => {
            jump_status_group(app, matches!(key.code, KeyCode::Right))
        }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => {
            app.task_detail_scroll =
                app.task_detail_scroll
                    .saturating_sub(if key.code == KeyCode::PageUp {
                        page as usize
                    } else {
                        1
                    })
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
            app.task_detail_scroll += if key.code == KeyCode::PageDown {
                page as usize
            } else {
                1
            }
        }
        KeyCode::Enter | KeyCode::Tab => {
            app.task_detail_focus = !app.task_detail_focus;
            app.task_detail_scroll = 0;
        }
        KeyCode::Char('R') => return Some(run_cmd(app, Cmd::TaskRequeue)),
        KeyCode::Char('e') => return Some(run_cmd(app, Cmd::TaskRelease)),
        KeyCode::Char('x') => return Some(run_cmd(app, Cmd::TaskCancel)),
        KeyCode::Char('v') => return Some(run_cmd(app, Cmd::TaskVerify)),
        _ => return None,
    }
    app.detail_dirty = true;
    if !matches!(key.code, KeyCode::Enter | KeyCode::Tab) {
        // changing selection resets the detail scroll
        if !detail {
            app.task_detail_scroll = 0;
        }
    }
    Some(vec![])
}

/// Left/Right on the board: jump to the first task of the previous/next non-empty status.
fn jump_status_group(app: &mut App, forward: bool) {
    let order = app.board_order();
    let tasks = app.tasks();
    let Some(&cur_idx) = order.get(app.task_sel) else {
        return;
    };
    let cur = tasks[cur_idx].status;
    let target = if forward {
        order.iter().position(|&i| {
            tasks[i].status != cur
                && TaskStatus::ALL.iter().position(|s| *s == tasks[i].status)
                    > TaskStatus::ALL.iter().position(|s| *s == cur)
        })
    } else {
        // first task of the previous non-empty group
        let prev_status = order.iter().rev().map(|&i| tasks[i].status).find(|s| {
            *s != cur
                && TaskStatus::ALL.iter().position(|x| x == s)
                    < TaskStatus::ALL.iter().position(|x| *x == cur)
        });
        prev_status.and_then(|ps| order.iter().position(|&i| tasks[i].status == ps))
    };
    if let Some(t) = target {
        app.task_sel = t;
    }
}

fn config_key(app: &mut App, key: &KeyEvent) -> Option<Vec<Action>> {
    match key.code {
        KeyCode::Char('+') | KeyCode::Char('=') => Some(run_cmd(app, Cmd::ConcurrencyUp)),
        KeyCode::Char('-') => Some(run_cmd(app, Cmd::ConcurrencyDown)),
        KeyCode::Char('[') | KeyCode::Char(']') => {
            let mut c = app.config.clone();
            let d: i64 = if key.code == KeyCode::Char(']') {
                60
            } else {
                -60
            };
            c.lease_secs = (c.lease_secs + d).clamp(60, 24 * 3600);
            app.config = c.clone();
            Some(vec![Action::SetConfig(c)])
        }
        KeyCode::Char('u') => {
            let mut c = app.config.clone();
            c.auto_assign = !c.auto_assign;
            app.config = c.clone();
            Some(vec![Action::SetConfig(c)])
        }
        _ => None,
    }
}

fn flow_key(app: &mut App, key: &KeyEvent, page: isize) -> Option<Vec<Action>> {
    match key.code {
        KeyCode::Tab => app.flow_list_mode = !app.flow_list_mode,
        KeyCode::Up | KeyCode::Char('k') => app.flow_scroll = app.flow_scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.flow_scroll += 1,
        KeyCode::PageUp => app.flow_scroll = app.flow_scroll.saturating_sub(page as usize),
        KeyCode::PageDown => app.flow_scroll += page as usize,
        _ => return None,
    }
    Some(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn press(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }
    fn app() -> App {
        let mut a = App::new(Theme::new(false));
        a.apply_snapshot(sample_snapshot());
        a
    }
    fn type_str(a: &mut App, s: &str) {
        for c in s.chars() {
            handle_key(a, ch(c));
        }
    }

    #[test]
    fn view_hotkeys_switch_views() {
        let mut a = app();
        for (k, v) in [
            ('g', View::Goal),
            ('t', View::Tasks),
            ('f', View::Flow),
            ('l', View::Logs),
            ('o', View::Approvals),
            ('c', View::Config),
            ('s', View::Agents),
            ('d', View::Dashboard),
        ] {
            handle_key(&mut a, ch(k));
            assert_eq!(a.view, v, "key {k}");
        }
    }

    #[test]
    fn ctrl_c_and_q_quit() {
        let mut a = app();
        let acts = handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(a.quit && matches!(acts[0], Action::Quit));
        let mut b = app();
        handle_key(&mut b, ch('q'));
        assert!(b.quit);
    }

    #[test]
    fn digits_select_agents_and_enter_opens_detail() {
        let mut a = app();
        handle_key(&mut a, ch('3'));
        assert_eq!(a.selected_agent, 2);
        handle_key(&mut a, ch('9'));
        assert_eq!(a.selected_agent, 2, "out of range ignored");
        assert!(a.toast.is_some());
        handle_key(&mut a, press(KeyCode::Enter));
        assert_eq!(a.view, View::AgentDetail);
        // the message box has the keys; digits select agents in browse mode (Tab)
        handle_key(&mut a, ch('1'));
        assert_eq!(
            (a.view, a.selected_agent, a.composer.value()),
            (View::AgentDetail, 2, "1")
        );
        handle_key(&mut a, press(KeyCode::Esc)); // clears the text first
        handle_key(&mut a, press(KeyCode::Tab));
        handle_key(&mut a, ch('1'));
        assert_eq!((a.view, a.selected_agent), (View::AgentDetail, 0));
        handle_key(&mut a, press(KeyCode::Esc));
        assert_eq!(a.view, View::Dashboard, "Esc returns to the previous view");
    }

    #[test]
    fn tab_cycles_dashboard_panels() {
        let mut a = app();
        let start = a.panel;
        for _ in 0..4 {
            handle_key(&mut a, press(KeyCode::Tab));
        }
        assert_eq!(a.panel, start);
        handle_key(&mut a, press(KeyCode::BackTab));
        assert_eq!(a.panel, Panel::Goal);
    }

    #[test]
    fn pause_and_resume_emit_actions() {
        let mut a = app();
        assert!(matches!(handle_key(&mut a, ch('p'))[..], [Action::Pause]));
        assert!(matches!(handle_key(&mut a, ch('r'))[..], [Action::Resume]));
    }

    #[test]
    fn help_overlay_opens_and_esc_closes() {
        let mut a = app();
        handle_key(&mut a, ch('?'));
        assert!(matches!(a.overlay, Overlay::Help));
        // other keys are swallowed while help is open
        handle_key(&mut a, ch('g'));
        assert!(matches!(a.overlay, Overlay::Help) && a.view == View::Dashboard);
        handle_key(&mut a, press(KeyCode::Esc));
        assert!(matches!(a.overlay, Overlay::None));
    }

    #[test]
    fn command_palette_filters_and_runs() {
        let mut a = app();
        handle_key(&mut a, ch('/'));
        assert!(matches!(a.overlay, Overlay::Palette(_)));
        type_str(&mut a, "sch pau");
        if let Overlay::Palette(p) = &a.overlay {
            assert_eq!(palette_matches(p.input.value())[0].cmd, Cmd::Pause);
        }
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        assert!(matches!(acts[..], [Action::Pause]));
        assert!(matches!(a.overlay, Overlay::None));
    }

    #[test]
    fn palette_can_switch_views_and_esc_cancels() {
        let mut a = app();
        handle_key(&mut a, ch(':'));
        type_str(&mut a, "flow");
        handle_key(&mut a, press(KeyCode::Enter));
        assert_eq!(a.view, View::Flow);
        handle_key(&mut a, ch('/'));
        handle_key(&mut a, press(KeyCode::Esc));
        assert!(matches!(a.overlay, Overlay::None));
    }

    #[test]
    fn fuzzy_ranks_prefix_and_rejects_non_subsequences() {
        assert!(fuzzy_score("zzz", "scheduling pause").is_none());
        let a = fuzzy_score("pause", "scheduling pause").unwrap();
        let b = fuzzy_score("pause", "agent interrupt the pause button").unwrap();
        assert!(a > 0 && b > 0);
        assert_eq!(fuzzy_score("", "x"), Some(0));
        assert!(palette_matches("quit").first().map(|c| c.cmd) == Some(Cmd::Quit));
        assert_eq!(palette_matches("").len(), COMMANDS.len());
    }

    #[test]
    fn set_goal_dialog_validates_then_creates() {
        let mut a = App::new(Theme::new(false));
        handle_key(&mut a, ch('G'));
        assert!(matches!(&a.overlay, Overlay::Dialog(d) if d.kind == DialogKind::SetGoal));
        // Enter on the first field advances; submitting an empty objective is refused
        handle_key(&mut a, press(KeyCode::Enter));
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        assert!(acts.is_empty());
        match &a.overlay {
            Overlay::Dialog(d) => assert!(d.error.as_deref().unwrap_or("").contains("Objective")),
            _ => panic!("dialog should stay open"),
        }
        // go back and fill in
        handle_key(&mut a, press(KeyCode::BackTab));
        type_str(&mut a, "Build auth");
        handle_key(&mut a, press(KeyCode::Tab));
        type_str(&mut a, "tests pass; docs exist");
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        match &acts[..] {
            [Action::CreateGoal { text, criteria }] => {
                assert_eq!(text, "Build auth");
                assert_eq!(
                    criteria,
                    &vec!["tests pass".to_string(), "docs exist".to_string()]
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(a.overlay, Overlay::None));
    }

    #[test]
    fn g_with_active_goal_opens_redirect_prefilled() {
        let mut a = app();
        handle_key(&mut a, ch('G'));
        match &a.overlay {
            Overlay::Dialog(d) => {
                assert_eq!(d.kind, DialogKind::RedirectGoal);
                assert!(d.fields[0].value().contains("authentication"));
            }
            _ => panic!("expected dialog"),
        }
    }

    #[test]
    fn add_agent_dialog_builds_a_typed_action() {
        let mut a = app();
        handle_key(&mut a, ch('a'));
        type_str(&mut a, "codex-9");
        handle_key(&mut a, press(KeyCode::Tab)); // provider (default codex)
        handle_key(&mut a, press(KeyCode::Tab)); // role
        handle_key(&mut a, press(KeyCode::Right)); // worker -> leader
        handle_key(&mut a, press(KeyCode::Tab)); // model
        handle_key(&mut a, press(KeyCode::Tab)); // effort
        handle_key(&mut a, press(KeyCode::Tab)); // permissions (last)
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        match &acts[..] {
            [Action::AddAgent {
                name,
                provider,
                role,
                model,
                effort,
                permission,
            }] => {
                assert!(effort.is_none());
                assert_eq!(name, "codex-9");
                assert_eq!(*provider, ProviderKind::Codex);
                assert_eq!(*role, Role::Leader);
                assert!(model.is_none());
                assert_eq!(*permission, PermissionProfile::Balanced);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn dialog_esc_cancels_without_action() {
        let mut a = app();
        handle_key(&mut a, ch('a'));
        let acts = handle_key(&mut a, press(KeyCode::Esc));
        assert!(acts.is_empty() && matches!(a.overlay, Overlay::None));
    }

    #[test]
    fn message_dialog_targets_selected_agent() {
        let mut a = app();
        handle_key(&mut a, ch('2'));
        handle_key(&mut a, ch('m'));
        type_str(&mut a, "please add tests");
        let acts = handle_key(&mut a, press(KeyCode::Tab));
        assert!(acts.is_empty());
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        match &acts[..] {
            [Action::MessageAgent {
                agent,
                text,
                interrupt,
            }] => {
                assert_eq!(agent, "codex-backend");
                assert_eq!(text, "please add tests");
                assert!(!interrupt);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn task_board_selection_and_status_group_jumps() {
        let mut a = app();
        handle_key(&mut a, ch('t'));
        assert_eq!(a.view, View::Tasks);
        let order = a.board_order();
        // sorted by status (BACKLOG first) then key
        assert_eq!(a.tasks()[order[0]].key, "T-6");
        handle_key(&mut a, press(KeyCode::Down));
        assert_eq!(a.task_sel, 1);
        let status = |a: &App| a.tasks()[a.board_order()[a.task_sel]].status;
        assert_eq!(status(&a), TaskStatus::Running);
        handle_key(&mut a, press(KeyCode::Right));
        assert_eq!(
            status(&a),
            TaskStatus::Blocked,
            "Right jumps to the next non-empty status group"
        );
        handle_key(&mut a, press(KeyCode::Left));
        assert_eq!(status(&a), TaskStatus::Running);
        handle_key(&mut a, press(KeyCode::Left));
        assert_eq!(status(&a), TaskStatus::Backlog);
        handle_key(&mut a, press(KeyCode::Left));
        assert_eq!(
            status(&a),
            TaskStatus::Backlog,
            "no group before the first one"
        );
    }

    #[test]
    fn task_actions_respect_state() {
        let mut a = app();
        handle_key(&mut a, ch('t'));
        // select the BLOCKED task T-7 and requeue it
        let order = a.board_order();
        a.task_sel = order
            .iter()
            .position(|&i| a.tasks()[i].key == "T-7")
            .unwrap();
        assert!(
            matches!(handle_key(&mut a, ch('R'))[..], [Action::RequeueTask(ref k)] if k == "T-7")
        );
        // a RUNNING task can't be requeued or verified
        a.task_sel = order
            .iter()
            .position(|&i| a.tasks()[i].key == "T-3")
            .unwrap();
        assert!(handle_key(&mut a, ch('R')).is_empty());
        assert_eq!(a.toast.as_ref().unwrap().level, Level::Error);
        handle_key(&mut a, ch('v'));
        assert!(matches!(a.overlay, Overlay::None));
        // cancel opens a dialog that yields CancelTask
        handle_key(&mut a, ch('x'));
        type_str(&mut a, "obsolete");
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        assert!(matches!(&acts[..], [Action::CancelTask { key, .. }] if key == "T-3"));
    }

    #[test]
    fn verify_dialog_for_verifying_task() {
        let mut a = app();
        if let Some(s) = a.snap.as_mut() {
            s.tasks[3].status = TaskStatus::Verifying;
        }
        handle_key(&mut a, ch('t'));
        a.task_sel = a
            .board_order()
            .iter()
            .position(|&i| a.tasks()[i].key == "T-4")
            .unwrap();
        handle_key(&mut a, ch('v'));
        type_str(&mut a, "ran the suite");
        let acts = handle_key(&mut a, press(KeyCode::Enter));
        assert!(
            matches!(&acts[..], [Action::VerifyTask { key, pass: true, notes }] if key == "T-4" && notes == "ran the suite")
        );
    }

    #[test]
    fn approval_center_keys_resolve_the_selected_request() {
        let mut a = app();
        handle_key(&mut a, ch('o'));
        assert!(matches!(
            &handle_key(&mut a, ch('y'))[..],
            [Action::ResolveApproval {
                decision: ApprovalDecision::Approve,
                ..
            }]
        ));
        assert!(matches!(
            &handle_key(&mut a, ch('Y'))[..],
            [Action::ResolveApproval {
                decision: ApprovalDecision::ApproveForSession,
                ..
            }]
        ));
        assert!(matches!(
            &handle_key(&mut a, ch('n'))[..],
            [Action::ResolveApproval {
                decision: ApprovalDecision::Deny,
                ..
            }]
        ));
        assert!(matches!(
            &handle_key(&mut a, ch('x'))[..],
            [Action::ResolveApproval {
                decision: ApprovalDecision::Abort,
                ..
            }]
        ));
        if let Some(s) = a.snap.as_mut() {
            s.approvals.clear();
        }
        assert!(handle_key(&mut a, ch('y')).is_empty());
    }

    #[test]
    fn config_keys_adjust_concurrency_with_bounds() {
        let mut a = app();
        handle_key(&mut a, ch('c'));
        a.config.max_concurrency = 1;
        let acts = handle_key(&mut a, ch('-'));
        assert!(matches!(&acts[..], [Action::SetConfig(c)] if c.max_concurrency == 1));
        let acts = handle_key(&mut a, ch('+'));
        assert!(matches!(&acts[..], [Action::SetConfig(c)] if c.max_concurrency == 2));
        let acts = handle_key(&mut a, ch('u'));
        assert!(matches!(&acts[..], [Action::SetConfig(c)] if !c.auto_assign));
    }

    #[test]
    fn agent_detail_scroll_search_and_copy() {
        use maw_orchestrator::{LineKind, TranscriptLine};
        let mut a = app();
        let id = a.agents()[0].id.clone();
        let lines: Vec<TranscriptLine> = (0..100)
            .map(|i| TranscriptLine {
                at: Utc::now(),
                kind: if i % 10 == 0 {
                    LineKind::Assistant
                } else {
                    LineKind::Output
                },
                text: if i == 42 {
                    "needle here".into()
                } else {
                    format!("line {i}")
                },
                open: false,
            })
            .collect();
        a.transcripts.insert(id, lines);
        a.size = (100, 30);
        handle_key(&mut a, ch('1'));
        handle_key(&mut a, press(KeyCode::Enter));
        assert_eq!(a.view, View::AgentDetail);
        handle_key(&mut a, press(KeyCode::Tab)); // browse mode: search and copy keys
        handle_key(&mut a, press(KeyCode::PageUp));
        assert!(a.detail_scroll > 0);
        handle_key(&mut a, press(KeyCode::End));
        assert_eq!(a.detail_scroll, 0);
        // search
        handle_key(&mut a, ch('/'));
        assert!(a.search.as_ref().unwrap().editing);
        type_str(&mut a, "needle");
        assert_eq!(a.search_matches().len(), 1);
        handle_key(&mut a, press(KeyCode::Enter));
        assert!(!a.search.as_ref().unwrap().editing);
        assert!(a.detail_scroll > 0, "scrolled so the match is visible");
        // copy last assistant block (internal buffer + OSC52 action)
        let acts = handle_key(&mut a, ch('y'));
        assert!(matches!(&acts[..], [Action::Copy(t)] if t == "line 90"));
        assert_eq!(a.clipboard, "line 90");
        let acts = handle_key(&mut a, ch('Y'));
        assert!(matches!(&acts[..], [Action::Copy(t)] if t.contains("needle here")));
        // Esc clears the search first, then leaves the view
        handle_key(&mut a, press(KeyCode::Esc));
        assert!(a.search.is_none() && a.view == View::AgentDetail);
        handle_key(&mut a, press(KeyCode::Esc));
        assert_ne!(a.view, View::AgentDetail);
    }

    #[test]
    fn agent_hotkeys_produce_agent_actions() {
        let mut a = app();
        handle_key(&mut a, ch('2'));
        let id = a.agents()[1].id.clone();
        assert!(
            matches!(&handle_key(&mut a, ch('i'))[..], [Action::InterruptAgent(x)] if *x == id)
        );
        assert!(matches!(&handle_key(&mut a, ch('C'))[..], [Action::ConnectAgent(x)] if *x == id));
        assert!(
            matches!(&handle_key(&mut a, ch('D'))[..], [Action::DisconnectAgent(x)] if *x == id)
        );
    }

    #[test]
    fn empty_app_never_panics_on_any_key() {
        let mut a = App::new(Theme::new(true));
        let codes = [
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Esc,
            KeyCode::Backspace,
        ];
        for v in View::TABS {
            a.view = v;
            for c in codes.iter() {
                handle_key(&mut a, press(*c));
                a.overlay = Overlay::None;
            }
            for c in "abcdefghijklmnopqrstuvwxyzGCDRY123456789+-[]=:?/".chars() {
                handle_key(&mut a, ch(c));
                a.overlay = Overlay::None;
                a.search = None;
            }
        }
    }

    #[test]
    fn mouse_wheel_scrolls_like_arrow_keys() {
        let mut a = app();
        a.view = View::Logs;
        handle_mouse(
            &mut a,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 10, // inside the body (row 0 is the title bar, which scrolls sideways)
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(a.logs_scroll, 3);
    }

    #[test]
    fn agent_login_command_targets_the_selected_agent() {
        let mut a = app();
        let id = a.selected_agent().unwrap().id.clone();
        let acts = run_cmd(&mut a, Cmd::AgentLogin);
        assert!(
            matches!(acts.as_slice(), [Action::LoginAgent(x)] if *x == id),
            "{acts:?}"
        );
        assert!(
            COMMANDS.iter().any(|c| c.name == "agent login"),
            "discoverable in the palette"
        );
    }

    #[test]
    fn release_events_are_ignored() {
        let mut a = app();
        let mut k = ch('g');
        k.kind = KeyEventKind::Release;
        handle_key(&mut a, k);
        assert_eq!(a.view, View::Dashboard);
    }
}
