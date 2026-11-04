//! The model shop: Ollama and Hugging Face in separate tabs, a search box, sorting, and the whole
//! description of the highlighted model.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maw_core::provider::ModelChoice;
use maw_core::ProviderKind;
use maw_tui::app::{handle_key, run_cmd, Action, Cmd, Overlay};
use maw_tui::picker::{shop_meta, SortKey};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

fn choice(id: &str, desc: &str) -> ModelChoice {
    ModelChoice {
        id: id.into(),
        display_name: id.into(),
        description: desc.into(),
        is_default: false,
        source: "catalog".into(),
        reasoning_efforts: vec![],
    }
}

fn shop() -> App {
    let mut a = App::new(Theme::new(false));
    a.apply_snapshot(sample_snapshot());
    a.size = (140, 40);
    a.pull_suggestions = vec![
        choice(
            "qwen3:8b",
            "5.2 GB · 8B · by Alibaba (Qwen) · tools, thinking · the long note that used to be cut off at the end of the row",
        ),
        choice(
            "gemma4:e2b",
            "4.6 GB · 4.6B (2B effective) · by Google · tools, vision",
        ),
        choice("granite4.2:3b", "2.2 GB · 3.7B · by IBM · tools"),
        choice(
            "hf.co/unsloth/Qwen3-1.7B-GGUF:Q4_K_M",
            "1.1 GB · 1.7B · by Alibaba (Qwen), packaged by Unsloth · tiny",
        ),
        choice(
            "hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M",
            "2.0 GB · 3B · by Meta, packaged by bartowski",
        ),
    ];
    a.models.insert(
        ProviderKind::Ollama,
        vec![choice("granite4.2:3b", "installed")],
    );
    run_cmd(&mut a, Cmd::PullModel);
    a
}

fn key(a: &mut App, code: KeyCode) -> Vec<Action> {
    handle_key(a, KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_text(a: &mut App, s: &str) {
    for c in s.chars() {
        key(a, KeyCode::Char(c));
    }
}

fn picker(a: &App) -> &maw_tui::picker::PickerState {
    match &a.overlay {
        Overlay::Picker(p) => p,
        o => panic!("{o:?}"),
    }
}

fn names(a: &App) -> Vec<String> {
    picker(a)
        .visible()
        .iter()
        .map(|i| i.label.clone())
        .collect()
}

#[test]
fn ollama_and_hugging_face_are_separate_sections_with_counts() {
    let mut a = shop();
    let t = render_text(&a, 140, 40);
    assert!(
        t.contains("Ollama (3)") && t.contains("Hugging Face (2)"),
        "{t}"
    );
    assert_eq!(names(&a), ["qwen3:8b", "gemma4:e2b", "granite4.2:3b"]);
    key(&mut a, KeyCode::Tab);
    assert_eq!(
        names(&a),
        [
            "hf.co/unsloth/Qwen3-1.7B-GGUF:Q4_K_M",
            "hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"
        ]
    );
    key(&mut a, KeyCode::BackTab);
    assert_eq!(names(&a).len(), 3);
    // the arrows move between sections while the search is empty
    key(&mut a, KeyCode::Right);
    assert_eq!(picker(&a).current_tab(), Some("Hugging Face"));
}

#[test]
fn the_search_box_filters_within_the_section_by_name_maker_or_size() {
    let mut a = shop();
    let t = render_text(&a, 140, 40);
    assert!(t.contains("search ›"), "{t}");
    type_text(&mut a, "google");
    assert_eq!(names(&a), ["gemma4:e2b"]);
    for _ in 0..6 {
        key(&mut a, KeyCode::Backspace);
    }
    type_text(&mut a, "3b");
    assert_eq!(names(&a), ["granite4.2:3b"]);
    // the other section is searched on its own
    key(&mut a, KeyCode::Tab);
    assert_eq!(
        names(&a),
        ["hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"]
    );
}

#[test]
fn f2_sorts_by_name_size_parameters_maker_and_installed() {
    let mut a = shop();
    key(&mut a, KeyCode::F(2));
    assert_eq!(picker(&a).sort, SortKey::Name);
    assert_eq!(names(&a), ["gemma4:e2b", "granite4.2:3b", "qwen3:8b"]);
    key(&mut a, KeyCode::F(2));
    assert_eq!(
        names(&a),
        ["granite4.2:3b", "gemma4:e2b", "qwen3:8b"],
        "smallest download first"
    );
    assert!(render_text(&a, 140, 40).contains("smallest first"));
    key(&mut a, KeyCode::F(2));
    assert_eq!(
        names(&a),
        ["qwen3:8b", "gemma4:e2b", "granite4.2:3b"],
        "largest first"
    );
    key(&mut a, KeyCode::F(2));
    assert_eq!(
        names(&a),
        ["granite4.2:3b", "gemma4:e2b", "qwen3:8b"],
        "fewest parameters first"
    );
    key(&mut a, KeyCode::F(2));
    assert_eq!(
        names(&a),
        ["qwen3:8b", "gemma4:e2b", "granite4.2:3b"],
        "Alibaba, Google, IBM"
    );
    key(&mut a, KeyCode::F(2));
    assert_eq!(names(&a)[0], "granite4.2:3b", "the installed one first");
    key(&mut a, KeyCode::F(2));
    assert_eq!(picker(&a).sort, SortKey::Default);
    // Ctrl+O does the same where F2 is taken by the terminal
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
    );
    assert_eq!(picker(&a).sort, SortKey::Name);
}

#[test]
fn the_whole_description_of_the_highlighted_model_is_shown() {
    let a = shop();
    let t = render_text(&a, 110, 40);
    assert!(
        t.contains("end of the row") && t.contains("the long note"),
        "the note under the list is complete\n{t}"
    );
}

#[test]
fn a_typed_owner_and_repo_on_the_hugging_face_tab_becomes_an_hf_name() {
    let mut a = shop();
    key(&mut a, KeyCode::Tab);
    type_text(&mut a, "someone/Some-Model-GGUF");
    let acts = key(&mut a, KeyCode::Enter);
    assert!(
        matches!(acts.as_slice(), [Action::PullModel { name }] if name == "hf.co/someone/Some-Model-GGUF"),
        "{acts:?}"
    );
    // on the Ollama tab the typed name is used as it is
    let mut a = shop();
    type_text(&mut a, "phi4-mini");
    let acts = key(&mut a, KeyCode::Enter);
    assert!(matches!(acts.as_slice(), [Action::PullModel { name }] if name == "phi4-mini"));
}

#[test]
fn sizes_parameters_and_makers_are_read_from_the_descriptions() {
    assert_eq!(
        shop_meta("5.2 GB · 8B · by Alibaba (Qwen) · tools"),
        (5.2, 8.0, "Alibaba (Qwen)".into())
    );
    assert_eq!(
        shop_meta("4.6 GB · 4.6B (2B effective) · by Google"),
        (4.6, 4.6, "Google".into())
    );
    assert_eq!(shop_meta("nothing useful here"), (0.0, 0.0, String::new()));
}

#[test]
fn the_shop_draws_at_any_size() {
    let a = shop();
    for (w, h) in [(20, 5), (40, 12), (80, 24), (140, 40), (300, 80)] {
        let _ = render_text(&a, w, h);
    }
}
