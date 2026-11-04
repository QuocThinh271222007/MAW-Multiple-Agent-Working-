//! User configuration, data directories and the project registry.

use anyhow::{Context, Result};
use maw_core::OrchestratorConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub monochrome: bool,
    /// Capture the mouse (clicks, drags, wheel). On unless turned off: the board and the side
    /// windows are built for it.
    pub mouse: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            monochrome: false,
            mouse: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProviderConfig {
    /// Override the executable (otherwise found on PATH).
    pub path: Option<String>,
    pub model: Option<String>,
}

/// Local models served by Ollama (`[ollama]` in config.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaSection {
    /// Server URL; empty = $OLLAMA_HOST, then loopback, then (WSL) the Windows host.
    pub url: Option<String>,
    /// Context window per chat (more memory for bigger values).
    pub num_ctx: u32,
    /// Context window for the Leader (its tools and instructions are large). More memory.
    pub num_ctx_leader: u32,
    pub keep_alive: String,
    /// Model/tool round trips allowed in one turn.
    pub max_steps: usize,
    /// Seconds a single `run_command` may take.
    pub command_timeout_secs: u64,
    /// Start `ollama serve` automatically when an Ollama agent needs it (default true).
    pub autostart: bool,
    /// Path to the `ollama` executable (default: PATH, then the usual install folders).
    pub path: Option<String>,
    /// Largest model (billions of parameters) `maw models pull` will download.
    pub max_pull_params_b: f64,
    /// Largest download in GB.
    pub max_pull_gb: f64,
    /// Folder for downloaded models (empty = Ollama's standard folder, e.g. C:/Users/<you>/.ollama/models
    /// for the Windows app). Applies when MAW starts Ollama for you.
    pub models_dir: Option<String>,
}

impl Default for OllamaSection {
    fn default() -> Self {
        let d = maw_provider_ollama::OllamaConfig::default();
        OllamaSection {
            url: None,
            num_ctx: d.num_ctx,
            num_ctx_leader: d.num_ctx_leader,
            keep_alive: d.keep_alive,
            max_steps: d.max_steps,
            command_timeout_secs: d.command_timeout.as_secs(),
            autostart: d.autostart,
            path: None,
            max_pull_params_b: d.max_pull_params_b,
            max_pull_gb: d.max_pull_bytes as f64 / 1e9,
            models_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MawConfig {
    pub ui: UiConfig,
    pub orchestrator: OrchestratorConfig,
    pub codex: ProviderConfig,
    pub claude: ProviderConfig,
    pub ollama: OllamaSection,
}

#[derive(Debug, Clone)]
pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Dirs {
    /// `MAW_HOME` overrides both directories (used by tests and portable installs).
    pub fn discover() -> Result<Dirs> {
        if let Some(h) = std::env::var_os("MAW_HOME") {
            let h = PathBuf::from(h);
            return Ok(Dirs {
                config: h.join("config"),
                data: h.join("data"),
            });
        }
        let pd = directories::ProjectDirs::from("dev", "maw", "maw")
            .context("cannot determine the user's home directory")?;
        Ok(Dirs {
            config: pd.config_dir().to_path_buf(),
            data: pd.data_dir().to_path_buf(),
        })
    }
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
    pub fn registry_file(&self) -> PathBuf {
        self.config.join("projects.json")
    }
    pub fn project_dir(&self, name: &str) -> PathBuf {
        self.data.join("projects").join(sanitize(name))
    }
    pub fn db_path(&self, name: &str) -> PathBuf {
        self.project_dir(name).join("maw.db")
    }
    pub fn log_file(&self) -> PathBuf {
        self.data.join("logs").join("maw.log")
    }
}

pub fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() {
        "project".into()
    } else {
        s
    }
}

impl MawConfig {
    pub fn load(dirs: &Dirs) -> Result<MawConfig> {
        let p = dirs.config_file();
        if !p.exists() {
            return Ok(MawConfig::default());
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("{} is not valid; fix it or delete it to reset", p.display()))
    }
    pub fn save(&self, dirs: &Dirs) -> Result<()> {
        std::fs::create_dir_all(&dirs.config)?;
        std::fs::write(dirs.config_file(), toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    /// project name -> absolute path
    pub projects: BTreeMap<String, String>,
}

impl Registry {
    pub fn load(dirs: &Dirs) -> Registry {
        std::fs::read_to_string(dirs.registry_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, dirs: &Dirs) -> Result<()> {
        std::fs::create_dir_all(&dirs.config)?;
        std::fs::write(dirs.registry_file(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
    pub fn find_by_path(&self, path: &Path) -> Option<String> {
        self.projects
            .iter()
            .find(|(_, p)| Path::new(p) == path)
            .map(|(n, _)| n.clone())
    }
    /// Register a path, choosing a unique name derived from the directory name.
    pub fn add(&mut self, path: &Path, name: Option<&str>) -> String {
        if let Some(existing) = self.find_by_path(path) {
            return existing;
        }
        let base = name
            .map(String::from)
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "project".into());
        let mut candidate = base.clone();
        let mut i = 2;
        while self.projects.contains_key(&candidate) {
            candidate = format!("{base}-{i}");
            i += 1;
        }
        self.projects
            .insert(candidate.clone(), path.to_string_lossy().to_string());
        candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_picks_unique_names_and_is_idempotent() {
        let mut r = Registry::default();
        let a = r.add(Path::new("/x/app"), None);
        let b = r.add(Path::new("/y/app"), None);
        assert_eq!((a.as_str(), b.as_str()), ("app", "app-2"));
        assert_eq!(r.add(Path::new("/x/app"), None), "app");
    }

    #[test]
    fn config_roundtrips_and_defaults_missing_fields() {
        let c: MawConfig = toml::from_str("[orchestrator]\nmax_concurrency = 2\n").unwrap();
        assert_eq!(c.orchestrator.max_concurrency, 2);
        assert_eq!(
            c.orchestrator.max_attempts,
            OrchestratorConfig::default().max_attempts
        );
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(toml::from_str::<MawConfig>(&s).is_ok());
    }

    /// A derived `Default` once made this `false`, so a normal `maw` never captured the mouse (only
    /// the demo did) and every click was lost.
    #[test]
    fn the_mouse_is_captured_unless_the_user_turns_it_off() {
        assert!(MawConfig::default().ui.mouse, "no config file");
        let c: MawConfig = toml::from_str("").unwrap();
        assert!(c.ui.mouse, "empty config file");
        let c: MawConfig = toml::from_str("[ui]\nmonochrome = true\n").unwrap();
        assert!(c.ui.mouse && c.ui.monochrome, "[ui] without a mouse key");
        let c: MawConfig = toml::from_str("[ui]\nmouse = false\n").unwrap();
        assert!(!c.ui.mouse, "an explicit false still turns it off");
    }

    #[test]
    fn sanitize_names() {
        assert_eq!(sanitize("My App/1"), "My-App-1");
        assert_eq!(sanitize(""), "project");
    }
}
