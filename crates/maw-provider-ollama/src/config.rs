use std::time::Duration;

#[derive(Clone, Debug)]
pub struct OllamaConfig {
    /// Server URL. `None` = `$OLLAMA_HOST`, then loopback, then (WSL) the Windows host.
    pub url: Option<String>,
    /// Context window requested for every chat. Bigger needs more memory.
    pub num_ctx: u32,
    /// Context window for the Leader. Its instructions and the many MAW tools take about 4,200 tokens
    /// before any conversation, and a thinking model spends thousands more per step; when the window
    /// is full Ollama silently drops the START of the prompt (the instructions), and the model forgets
    /// what it is doing. Used when larger than `num_ctx`.
    pub num_ctx_leader: u32,
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
    /// Where downloaded models are stored, for a server MAW starts (`OLLAMA_MODELS`). `None` = the
    /// server's own standard folder. A server that was already running keeps its own folder.
    pub models_dir: Option<std::path::PathBuf>,
    /// Where conversations are kept so an agent can resume after MAW restarts. `None` = not kept.
    pub state_dir: Option<std::path::PathBuf>,
    /// Most tokens a thinking model may spend reasoning before each answer or tool call, per role
    /// (0 = no limit). When it runs out MAW stops that request and asks again with thinking off, so
    /// the model acts on what it has. The Leader mostly makes simple coordination calls and gets the
    /// smallest allowance. Not applied to `gpt-oss` (it has real effort levels) or to models whose
    /// thinking cannot be switched off cleanly.
    pub think_budget_leader: u32,
    pub think_budget_worker: u32,
    /// Reviewers and specialists.
    pub think_budget_other: u32,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        OllamaConfig {
            url: None,
            num_ctx: 8192,
            num_ctx_leader: 12288,
            keep_alive: "10m".into(),
            max_steps: 40,
            idle_timeout: Duration::from_secs(300),
            command_timeout: Duration::from_secs(120),
            autostart: true,
            binary: None,
            max_pull_params_b: 8.0,
            max_pull_bytes: 6_500_000_000,
            models_dir: None,
            state_dir: None,
            think_budget_leader: 400,
            think_budget_worker: 1500,
            think_budget_other: 1500,
        }
    }
}
