//! Single-choice pickers in the style of Claude Code and Codex: a list under a filter line, arrows
//! to move, Enter to choose, Esc to cancel. Used for the model, the effort, the permission profile
//! and downloading a model; each is one decision per screen instead of a multi-field form.

use crate::app::{Action, App, Overlay};
use crate::input::TextInput;
use crossterm::event::{KeyCode, KeyEvent};
use maw_core::provider::ModelChoice;
use maw_core::{Agent, AgentId, PermissionProfile};

#[derive(Clone, Debug, PartialEq)]
pub struct PickItem {
    pub value: String,
    pub label: String,
    pub detail: String,
    /// The value in force right now (marked in the list).
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PickerKind {
    Model {
        agent: AgentId,
    },
    Effort {
        agent: AgentId,
        model: Option<String>,
    },
    Permission {
        agent: AgentId,
    },
    Pull,
}

#[derive(Clone, Debug)]
pub struct PickerState {
    pub title: String,
    pub hint: String,
    pub kind: PickerKind,
    pub items: Vec<PickItem>,
    pub filter: TextInput,
    pub selected: usize,
    /// Enter with a typed filter that matches nothing takes the typed text as the value.
    pub allow_custom: bool,
}

impl PickerState {
    /// Items matching the filter (every whitespace-separated word must appear).
    pub fn visible(&self) -> Vec<&PickItem> {
        let q = self.filter.value().to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.items
            .iter()
            .filter(|i| {
                let hay = format!("{} {} {}", i.value, i.label, i.detail).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    /// The value Enter would choose: the highlighted item, else the typed text when allowed.
    pub fn chosen(&self) -> Option<String> {
        let vis = self.visible();
        if let Some(i) = vis.get(self.selected.min(vis.len().saturating_sub(1))) {
            return Some(i.value.clone());
        }
        let typed = self.filter.value().trim().to_string();
        (self.allow_custom && !typed.is_empty()).then_some(typed)
    }
}

fn item(value: &str, label: &str, detail: String, current: bool) -> PickItem {
    PickItem {
        value: value.into(),
        label: label.into(),
        detail,
        current,
    }
}

fn start_on_current(items: &[PickItem]) -> usize {
    items.iter().position(|i| i.current).unwrap_or(0)
}

pub fn model_detail(m: &ModelChoice) -> String {
    match (m.display_name != m.id, m.description.is_empty()) {
        (true, false) => format!("{} — {}", m.display_name, m.description),
        (true, true) => m.display_name.clone(),
        (false, false) => m.description.clone(),
        (false, true) => String::new(),
    }
}

pub fn model_picker(agent: &Agent, models: &[ModelChoice]) -> PickerState {
    let mut items = vec![item(
        "default",
        "default",
        "the provider's own default".into(),
        agent.model.is_none(),
    )];
    for m in models {
        items.push(item(
            &m.id,
            &m.id,
            model_detail(m),
            agent.model.as_deref() == Some(m.id.as_str()),
        ));
    }
    PickerState {
        title: format!("Model for {} ({})", agent.name, agent.provider),
        hint: if models.is_empty() {
            "no model list available (provider not installed or still loading); type an id".into()
        } else {
            "type to filter, or type any model id".into()
        },
        kind: PickerKind::Model {
            agent: agent.id.clone(),
        },
        selected: start_on_current(&items),
        items,
        filter: TextInput::new(),
        allow_custom: true,
    }
}

pub fn effort_picker(agent: &Agent, model: Option<String>, levels: Vec<String>) -> PickerState {
    let mut items = vec![item(
        "default",
        "default",
        "the model's own default".into(),
        agent.effort.is_none(),
    )];
    for l in levels {
        let cur = agent.effort.as_deref() == Some(l.as_str());
        items.push(item(&l, &l, String::new(), cur));
    }
    PickerState {
        title: format!(
            "Effort for {} · {}",
            agent.name,
            model.as_deref().unwrap_or("default model")
        ),
        hint: "how hard the model should think".into(),
        kind: PickerKind::Effort {
            agent: agent.id.clone(),
            model,
        },
        selected: start_on_current(&items),
        items,
        filter: TextInput::new(),
        allow_custom: false,
    }
}

pub fn permission_picker(agent: &Agent) -> PickerState {
    let cur = agent.permission;
    let items = vec![
        item(
            "safe",
            "safe",
            "read-only; asks before anything else".into(),
            cur == PermissionProfile::Safe,
        ),
        item(
            "balanced",
            "balanced",
            "edits inside the workspace; asks for commands and anything outside it".into(),
            cur == PermissionProfile::Balanced,
        ),
        item(
            "autonomous",
            "autonomous",
            "broad execution; still asks for risky or outside-workspace actions".into(),
            cur == PermissionProfile::Autonomous,
        ),
    ];
    PickerState {
        title: format!("Permissions for {}", agent.name),
        hint: "applies the next time the agent connects".into(),
        kind: PickerKind::Permission {
            agent: agent.id.clone(),
        },
        selected: start_on_current(&items),
        items,
        filter: TextInput::new(),
        allow_custom: false,
    }
}

pub fn pull_picker(suggestions: &[ModelChoice], installed: &[ModelChoice]) -> PickerState {
    let items: Vec<PickItem> = suggestions
        .iter()
        .map(|m| {
            let have = installed.iter().any(|i| i.id == m.id);
            item(
                &m.id,
                &m.id,
                if have {
                    format!("✓ installed · {}", m.description)
                } else {
                    m.description.clone()
                },
                have,
            )
        })
        .collect();
    PickerState {
        title: "Download a model (Ollama)".into(),
        hint: "type to filter, or type any model name (limited to 8B / ~6.5 GB by default)".into(),
        kind: PickerKind::Pull,
        selected: 0,
        items,
        filter: TextInput::new(),
        allow_custom: true,
    }
}

/// Act on the chosen value; may open the next picker (model → effort).
fn finish(app: &mut App, p: &PickerState, value: String) -> Vec<Action> {
    match &p.kind {
        PickerKind::Model { agent } => {
            let model = Some(value).filter(|m| m != "default");
            let Some(a) = app.agents().iter().find(|a| a.id == *agent).cloned() else {
                return vec![];
            };
            let list = app.models.get(&a.provider).cloned().unwrap_or_default();
            let levels =
                maw_orchestrator::models::efforts_for_provider(a.provider, &list, model.as_deref());
            if levels.is_empty() {
                return vec![Action::SetAgentModel {
                    agent: agent.clone(),
                    model,
                    effort: None,
                }];
            }
            app.overlay = Overlay::Picker(effort_picker(&a, model, levels));
            vec![]
        }
        PickerKind::Effort { agent, model } => vec![Action::SetAgentModel {
            agent: agent.clone(),
            model: model.clone(),
            effort: Some(value).filter(|e| e != "default"),
        }],
        PickerKind::Permission { agent } => match value.parse() {
            Ok(permission) => vec![Action::SetAgentPermission {
                agent: agent.clone(),
                permission,
            }],
            Err(_) => vec![],
        },
        PickerKind::Pull => vec![Action::PullModel { name: value }],
    }
}

pub fn picker_key(app: &mut App, mut p: PickerState, key: KeyEvent) -> Vec<Action> {
    let n = p.visible().len();
    let last = n.saturating_sub(1);
    match key.code {
        KeyCode::Esc => return vec![],
        KeyCode::Up => p.selected = p.selected.saturating_sub(1),
        KeyCode::Down => p.selected = (p.selected + 1).min(last),
        KeyCode::PageUp => p.selected = p.selected.saturating_sub(6),
        KeyCode::PageDown => p.selected = (p.selected + 6).min(last),
        KeyCode::Home if p.filter.value().is_empty() => p.selected = 0,
        KeyCode::End if p.filter.value().is_empty() => p.selected = last,
        KeyCode::Enter => {
            return match p.chosen() {
                Some(v) => finish(app, &p, v),
                None => {
                    app.overlay = Overlay::Picker(p);
                    vec![]
                }
            };
        }
        _ => {
            if p.filter.handle_key(key) {
                p.selected = 0;
            }
        }
    }
    app.overlay = Overlay::Picker(p);
    vec![]
}
