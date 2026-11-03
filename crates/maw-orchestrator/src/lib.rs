//! MAW orchestration: the engine (validated domain operations), the coordination tool surface,
//! the supervisor (provider sessions, scheduling, delivery), recovery and a deterministic mock.

pub mod comms;
pub mod engine;
pub mod mock;
pub mod models;
pub mod perf;
pub mod prompts;
pub mod recovery;
pub mod supervisor;
pub mod tasks;
pub mod tools;
pub mod workspace;

pub use engine::{Actor, Engine, GoalPatch, RemovedAgent, Snapshot};
pub use recovery::RecoveryReport;
pub use supervisor::{LineKind, Supervisor, TranscriptLine};
pub use tasks::{EvidenceInput, NewTask, TaskPatch};
pub use tools::{tool_specs, ToolSpec};
