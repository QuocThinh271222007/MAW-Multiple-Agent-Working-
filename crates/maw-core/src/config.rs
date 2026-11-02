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
        }
    }
}
