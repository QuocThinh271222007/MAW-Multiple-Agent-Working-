use std::time::Duration;

#[derive(Clone, Debug)]
pub struct OllamaConfig {
    /// Server URL. `None` = `$OLLAMA_HOST`, then loopback, then (WSL) the Windows host.
    pub url: Option<String>,
    /// Context window requested for every chat. Bigger needs more memory.
    pub num_ctx: u32,
    /// How long Ollama keeps the model loaded after a request.
    pub keep_alive: String,
    /// Model/tool round trips allowed in one turn.
    pub max_steps: usize,
    /// Give up when the server sends nothing for this long.
    pub idle_timeout: Duration,
    /// Limit for one `run_command`.
    pub command_timeout: Duration,
    /// Start `ollama serve` by itself when an agent needs it and nothing answers.
    pub autostart: bool,
    /// Path to the `ollama` executable; `None` = PATH, then the usual install locations.
    pub binary: Option<String>,
    /// Largest model (billions of parameters) MAW will download.
    pub max_pull_params_b: f64,
    /// Largest download in bytes (checked against what the server reports while pulling).
    pub max_pull_bytes: u64,
    /// Where conversations are kept so an agent can resume after MAW restarts. `None` = not kept.
    pub state_dir: Option<std::path::PathBuf>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        OllamaConfig {
            url: None,
            num_ctx: 8192,
            keep_alive: "10m".into(),
            max_steps: 40,
            idle_timeout: Duration::from_secs(300),
            command_timeout: Duration::from_secs(120),
            autostart: true,
            binary: None,
            max_pull_params_b: 8.0,
            max_pull_bytes: 6_500_000_000,
            state_dir: None,
        }
    }
}
