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
    /// Which tab it is listed under (shop: "Ollama" or "Hugging Face"); empty without tabs.
    pub group: String,
    /// Download size in GB, parameters in billions, who made it; 0 / empty when not known.
    pub gb: f64,
    pub params: f64,
    pub maker: String,
}

/// How a long list is ordered (F2 goes to the next).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Default,
    Name,
    SizeAsc,
    SizeDesc,
    ParamsAsc,
    Maker,
    Installed,
}

impl SortKey {
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Default => SortKey::Name,
            SortKey::Name => SortKey::SizeAsc,
            SortKey::SizeAsc => SortKey::SizeDesc,
            SortKey::SizeDesc => SortKey::ParamsAsc,
            SortKey::ParamsAsc => SortKey::Maker,
            SortKey::Maker => SortKey::Installed,
            SortKey::Installed => SortKey::Default,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Default => "recommended order",
            SortKey::Name => "name A-Z",
            SortKey::SizeAsc => "download size, smallest first",
            SortKey::SizeDesc => "download size, largest first",
            SortKey::ParamsAsc => "parameters, fewest first",
            SortKey::Maker => "maker A-Z",
            SortKey::Installed => "installed first",
        }
    }
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
    /// Sections shown as tabs (Tab / Shift+Tab); empty for a single list.
    pub tabs: Vec<String>,
    pub tab: usize,
    pub sort: SortKey,
}

impl PickerState {
    /// The tab being shown, if there are tabs.
    pub fn current_tab(&self) -> Option<&str> {
        self.tabs.get(self.tab).map(|s| s.as_str())
    }

    /// How many items each tab holds (with no search applied).
    pub fn tab_count(&self, tab: &str) -> usize {
        self.items.iter().filter(|i| i.group == tab).count()
    }

    /// Items of the current tab matching the search (every whitespace-separated word must appear
    /// in the name, maker, size or description), in the chosen order.
    pub fn visible(&self) -> Vec<&PickItem> {
        let q = self.filter.value().to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        let tab = self.current_tab();
        let mut v: Vec<&PickItem> = self
            .items
            .iter()
            .filter(|i| tab.is_none_or(|t| i.group == t))
            .filter(|i| {
                let hay = format!("{} {} {}", i.value, i.label, i.detail).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        let known_first = |a: f64, b: f64, asc: bool| {
            // sizes that are not known go last whichever way it is sorted
            match (a > 0.0, b > 0.0) {
                (false, false) => std::cmp::Ordering::Equal,
                (false, true) => std::cmp::Ordering::Greater,
                (true, false) => std::cmp::Ordering::Less,
                (true, true) => {
                    let o = a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
                    if asc {
                        o
                    } else {
                        o.reverse()
                    }
                }
            }
        };
        match self.sort {
            SortKey::Default => {}
            SortKey::Name => v.sort_by_key(|i| i.label.to_lowercase()),
            SortKey::SizeAsc => v.sort_by(|a, b| known_first(a.gb, b.gb, true)),
            SortKey::SizeDesc => v.sort_by(|a, b| known_first(a.gb, b.gb, false)),
            SortKey::ParamsAsc => v.sort_by(|a, b| known_first(a.params, b.params, true)),
            SortKey::Maker => v.sort_by_key(|i| (i.maker.is_empty(), i.maker.to_lowercase())),
            SortKey::Installed => v.sort_by_key(|i| !i.current),
        }
        v
    }

    /// The value Enter would choose: the highlighted item, else the typed text when allowed. On the
    /// Hugging Face tab a typed `owner/repo` becomes `hf.co/owner/repo`.
    pub fn chosen(&self) -> Option<String> {
        let vis = self.visible();
        if let Some(i) = vis.get(self.selected.min(vis.len().saturating_sub(1))) {
            return Some(i.value.clone());
        }
        let typed = self.filter.value().trim().to_string();
        if !self.allow_custom || typed.is_empty() {
            return None;
        }
        if self.current_tab() == Some(HUGGING_FACE_TAB)
            && typed.contains('/')
            && !typed.starts_with("hf.co/")
        {
            return Some(format!("hf.co/{typed}"));
        }
        Some(typed)
    }
}

pub const OLLAMA_TAB: &str = "Ollama";
pub const HUGGING_FACE_TAB: &str = "Hugging Face";

fn item(value: &str, label: &str, detail: String, current: bool) -> PickItem {
    PickItem {
        value: value.into(),
        label: label.into(),
        detail,
        current,
        group: String::new(),
        gb: 0.0,
        params: 0.0,
        maker: String::new(),
    }
}

/// Size, parameters and maker out of a catalogue description
/// (`"2.5 GB · 4B · by Alibaba (Qwen) · tools ..."`); what is not there stays 0 / empty.
pub fn shop_meta(desc: &str) -> (f64, f64, String) {
    let parts: Vec<&str> = desc.split(" · ").map(str::trim).collect();
    let number = |s: &str| -> f64 {
        let digits: String = s
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        digits.parse().unwrap_or(0.0)
    };
    let gb = parts
        .iter()
        .find(|p| p.ends_with(" GB"))
        .map(|p| number(p))
        .unwrap_or(0.0);
    let params = parts
        .iter()
        .find(|p| {
            let t = p.to_ascii_uppercase();
            !p.ends_with(" GB")
                && !p.starts_with("by ")
                && t.chars().any(|c| c == 'B')
                && p.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .map(|p| number(p))
        .unwrap_or(0.0);
    let maker = parts
        .iter()
        .find_map(|p| p.strip_prefix("by "))
        .unwrap_or("")
        .to_string();
    (gb, params, maker)
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
        tabs: vec![],
        tab: 0,
        sort: SortKey::Default,
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
        tabs: vec![],
        tab: 0,
        sort: SortKey::Default,
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
        tabs: vec![],
        tab: 0,
        sort: SortKey::Default,
    }
}

pub fn pull_picker(suggestions: &[ModelChoice], installed: &[ModelChoice]) -> PickerState {
    let items: Vec<PickItem> = suggestions
        .iter()
        .map(|m| {
            let have = installed.iter().any(|i| i.id == m.id);
            let mut it = item(
                &m.id,
                &m.id,
                if have {
                    format!("✓ installed · {}", m.description)
                } else {
                    m.description.clone()
                },
                have,
            );
            it.group = if m.id.starts_with("hf.co/") {
                HUGGING_FACE_TAB
            } else {
                OLLAMA_TAB
            }
            .into();
            (it.gb, it.params, it.maker) = shop_meta(&m.description);
            it
        })
        .collect();
    PickerState {
        title: "Model shop".into(),
        hint: "Enter downloads · type any model name to fetch one that is not listed (limited to 8B / ~6.5 GB by default)".into(),
        kind: PickerKind::Pull,
        selected: 0,
        items,
        filter: TextInput::new(),
        allow_custom: true,
        tabs: vec![OLLAMA_TAB.into(), HUGGING_FACE_TAB.into()],
        tab: 0,
        sort: SortKey::Default,
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
        KeyCode::Tab | KeyCode::BackTab if !p.tabs.is_empty() => {
            let n = p.tabs.len();
            p.tab = if key.code == KeyCode::BackTab {
                (p.tab + n - 1) % n
            } else {
                (p.tab + 1) % n
            };
            p.selected = 0;
        }
        KeyCode::Left | KeyCode::Right if !p.tabs.is_empty() && p.filter.value().is_empty() => {
            let n = p.tabs.len();
            p.tab = if key.code == KeyCode::Left {
                (p.tab + n - 1) % n
            } else {
                (p.tab + 1) % n
            };
            p.selected = 0;
        }
        KeyCode::F(2) => {
            p.sort = p.sort.next();
            p.selected = 0;
        }
        KeyCode::Char('o')
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL) =>
        {
            p.sort = p.sort.next();
            p.selected = 0;
        }
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
