//! Add agent offers the suggested local models even before they are downloaded.

use maw_core::provider::ModelChoice;
use maw_core::ProviderKind;
use maw_tui::app::{Dialog, DialogKind, Overlay};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn choice(id: &str, desc: &str, source: &str) -> ModelChoice {
    ModelChoice {
        id: id.into(),
        display_name: id.into(),
        description: desc.into(),
        is_default: false,
        source: source.into(),
        reasoning_efforts: vec![],
    }
}

fn app() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    // one model installed, three suggested (one of them is the installed one)
    a.models.insert(
        ProviderKind::Ollama,
        vec![choice("qwen3:4b", "2.5 GB installed", "provider")],
    );
    a.pull_suggestions = vec![
        choice("qwen3:4b", "2.5 GB · by Alibaba (Qwen)", "catalog"),
        choice("gemma4:e2b", "4.6 GB · by Google", "catalog"),
        choice("granite4.2:3b", "2.2 GB · by IBM", "catalog"),
    ];
    a
}

#[test]
fn not_downloaded_suggestions_are_listed_after_the_installed_models() {
    let a = app();
    let lists = a.add_agent_models();
    let ids: Vec<&str> = lists[&ProviderKind::Ollama]
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["qwen3:4b", "gemma4:e2b", "granite4.2:3b"],
        "installed first, no duplicates"
    );
    let g = &lists[&ProviderKind::Ollama][1];
    assert!(
        g.description.starts_with('⬇') && g.description.contains("by Google"),
        "{}",
        g.description
    );
    assert_eq!(g.source, "catalog");
    assert!(g.reasoning_efforts.is_empty(), "no invented effort levels");
    // the installed one is left alone
    assert!(!lists[&ProviderKind::Ollama][0].description.contains('⬇'));
    // other providers are untouched
    assert_eq!(lists.len(), a.models.len());
}

#[test]
fn the_add_agent_dialog_offers_them_for_the_ollama_provider() {
    let mut a = app();
    let mut d = Dialog::add_agent_for(&a.add_agent_models(), false);
    // pick the ollama provider (field 1) and read the model list (field 3)
    let providers = d.fields[1].choices.clone();
    d.fields[1].choice = providers
        .iter()
        .position(|p| p == "ollama")
        .expect("ollama is offered");
    a.overlay = Overlay::Dialog(d);
    maw_tui::app::refresh_open_dialog(&mut a);
    let Overlay::Dialog(d) = &a.overlay else {
        panic!()
    };
    assert_eq!(d.kind, DialogKind::AddAgent);
    let models = &d.fields[3].choices;
    for want in ["qwen3:4b", "gemma4:e2b", "granite4.2:3b"] {
        assert!(models.iter().any(|m| m == want), "{want} in {models:?}");
    }
    // and with nothing installed and nothing suggested the list is just empty, not an error
    let mut b = App::new(Theme::new(false));
    b.apply_snapshot(sample_snapshot());
    assert!(!b.add_agent_models().contains_key(&ProviderKind::Ollama));
}
