//! Orchestrator tunables shared between the engine, CLI and TUI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorConfig {
    /// Maximum agents executing a task concurrently.
    pub max_concurrency: usize,
    /// Task lease duration; renewed by progress reports / heartbeats.
    pub lease_secs: i64,
    /// Bounded automatic retries for a task.
    pub max_attempts: u32,
    /// Hard cap on tasks per goal (prevents unbounded recursive task creation).
    pub max_tasks_per_goal: usize,
    /// Maximum task nesting depth.
    pub max_task_depth: usize,
    /// Auto-assign READY tasks that have no owner to idle workers.
    pub auto_assign: bool,
    /// Debounce before waking the Leader with batched notifications.
    pub leader_wake_debounce_ms: u64,
    /// Scheduler tick interval (safety net; the scheduler is primarily event driven).
    pub tick_ms: u64,
    /// Retention: keep at most this many verbose events.
    pub verbose_event_retention: i64,
    /// Where agent worktrees are created: `project` (next to the project, `.maw/worktrees`),
    /// `wsl` (on the Linux filesystem of WSL: fast git) or `windows` (on a Windows drive: visible
    /// in Explorer). Applies to new worktrees; existing ones move when their agent next connects
    /// with no work in flight.
    pub worktree_location: String,
    /// For `windows`: the folder on the Windows side, e.g. `C:\Users\me\MAW` or `/mnt/c/Users/me/MAW`.
    /// Empty = a `MAW` folder in your Windows user profile.
    pub windows_worktree_dir: String,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        OrchestratorConfig {
            max_concurrency: 4,
            lease_secs: 15 * 60,
            max_attempts: 3,
            max_tasks_per_goal: 200,
            max_task_depth: 4,
            auto_assign: true,
            leader_wake_debounce_ms: 1500,
            tick_ms: 1000,
            verbose_event_retention: 20_000,
            worktree_location: "project".into(),
            windows_worktree_dir: String::new(),
        }
    }
}
