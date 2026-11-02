//! MAW core: provider-neutral domain model with no I/O.

pub mod agent;
pub mod config;
pub mod error;
pub mod event;
pub mod goal;
pub mod ids;
pub mod provider;
pub mod redact;
pub mod task;
pub mod workspace;

pub use agent::*;
pub use config::OrchestratorConfig;
pub use error::{CoreError, CoreResult};
pub use event::{Event, EventKind};
pub use goal::*;
pub use ids::*;
pub use task::*;
pub use workspace::*;
