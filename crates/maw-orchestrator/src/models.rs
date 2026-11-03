//! Model-id validation shared by the TUI and the CLI, so nobody has to type an id blindly.

use maw_core::provider::ModelChoice;
use maw_core::{CoreError, CoreResult, ProviderKind};

/// Check `id` against a provider's model list.
///
/// * `default` (or empty) is always fine: the provider picks.
/// * Ids in the list are fine.
/// * When the list came live from the provider (`source == "provider"`, e.g. Codex), anything else
///   is rejected: that provider is the authority on what the account can use.
/// * When the list is a static catalog (Claude), ids shaped like `claude-…` are still accepted,
///   because new models appear faster than a catalog is updated; the provider reports a wrong id
///   when the session starts.
pub fn check_model(list: &[ModelChoice], kind: ProviderKind, id: &str) -> CoreResult<()> {
    let id = id.trim();
    if id.is_empty() || id.eq_ignore_ascii_case("default") || list.iter().any(|m| m.id == id) {
        return Ok(());
    }
    let live = list.iter().any(|m| m.source == "provider");
    let plausible = !live && kind == ProviderKind::Claude && id.starts_with("claude-");
    if plausible {
        return Ok(());
    }
    if list.is_empty() {
        // nothing to compare with (provider unavailable): do not block, the session start will tell
        return Ok(());
    }
    Err(CoreError::InvalidInput(format!(
        "'{id}' is not a model {kind} offers. Choose one of: {}",
        list.iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Effort levels to offer for a model: what the provider reports for it, else a common set.
pub fn efforts_for(list: &[ModelChoice], model: Option<&str>) -> Vec<String> {
    let found = list
        .iter()
        .find(|m| Some(m.id.as_str()) == model || (model.is_none() && m.is_default));
    // No model chosen and the provider has no default of its own (Ollama): every entry carries
    // its own levels, so offer a model's levels only when the choice is unambiguous.
    if found.is_none()
        && model.is_none()
        && !list.is_empty()
        && list.iter().all(|m| !m.reasoning_efforts.is_empty())
    {
        return if list.len() == 1 {
            list[0].reasoning_efforts.clone()
        } else {
            vec![]
        };
    }
    match found {
        Some(m) if !m.reasoning_efforts.is_empty() => m.reasoning_efforts.clone(),
        _ => ["low", "medium", "high", "xhigh", "max"]
            .map(String::from)
            .to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, source: &str) -> ModelChoice {
        ModelChoice {
            id: id.into(),
            display_name: id.into(),
            description: String::new(),
            is_default: false,
            source: source.into(),
            reasoning_efforts: vec!["low".into(), "high".into()],
        }
    }

    #[test]
    fn live_catalogs_are_strict_static_ones_are_lenient() {
        let codex = vec![m("gpt-6.1-sol", "provider"), m("gpt-6-luna", "provider")];
        assert!(check_model(&codex, ProviderKind::Codex, "gpt-6-luna").is_ok());
        assert!(check_model(&codex, ProviderKind::Codex, "default").is_ok());
        let e = check_model(&codex, ProviderKind::Codex, "gpt-7")
            .unwrap_err()
            .to_string();
        assert!(e.contains("gpt-6.1-sol") && e.contains("gpt-6-luna"), "{e}");

        let claude = vec![m("sonnet", "catalog"), m("claude-sonnet-5-5", "catalog")];
        assert!(check_model(&claude, ProviderKind::Claude, "sonnet").is_ok());
        assert!(
            check_model(&claude, ProviderKind::Claude, "claude-brand-new-9").is_ok(),
            "newer claude ids pass"
        );
        assert!(
            check_model(&claude, ProviderKind::Claude, "gpt-6").is_err(),
            "a Codex id is not a Claude model"
        );
        assert!(
            check_model(&claude, ProviderKind::Claude, "sonet").is_err(),
            "typos are caught"
        );
        // no list available: do not block
        assert!(check_model(&[], ProviderKind::Codex, "anything").is_ok());
    }

    #[test]
    fn without_a_chosen_model_ollama_style_lists_do_not_get_the_generic_levels() {
        let mk = |id: &str, e: &[&str]| ModelChoice {
            id: id.into(),
            display_name: id.into(),
            description: String::new(),
            is_default: false,
            source: "provider".into(),
            reasoning_efforts: e.iter().map(|s| s.to_string()).collect(),
        };
        let two = [mk("a", &["on", "off"]), mk("b", &["off"])];
        assert!(
            efforts_for(&two, None).is_empty(),
            "ambiguous: only 'default' is offered"
        );
        assert_eq!(efforts_for(&two, Some("b")), ["off"]);
        let one = [mk("a", &["on", "off"])];
        assert_eq!(
            efforts_for(&one, None),
            ["on", "off"],
            "a single installed model is unambiguous"
        );
        // providers without their own levels (Claude catalog) keep the common set
        let claude = [mk("sonnet", &[])];
        assert_eq!(efforts_for(&claude, None).len(), 5);
    }

    #[test]
    fn efforts_follow_the_model() {
        let mut luna = m("luna", "provider");
        luna.reasoning_efforts = vec!["low".into(), "medium".into()];
        let sol = ModelChoice {
            is_default: true,
            ..m("sol", "provider")
        };
        let list = vec![sol, luna];
        assert_eq!(efforts_for(&list, Some("luna")), vec!["low", "medium"]);
        assert_eq!(
            efforts_for(&list, None),
            vec!["low", "high"],
            "default model's levels"
        );
        assert_eq!(efforts_for(&[], None).len(), 5);
    }
}
