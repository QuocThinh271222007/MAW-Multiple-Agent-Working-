//! Strongly typed, prefix-tagged identifiers. IDs are opaque strings that stay stable
//! across restarts and are safe to expose over MCP.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident, $prefix:expr) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4().simple()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(
    /// A project (one repository).
    ProjectId, "prj"
);
id_type!(
    /// A user goal (objective).
    GoalId, "goal"
);
id_type!(
    /// A persistent agent identity assigned by MAW.
    AgentId, "agt"
);
id_type!(
    /// A task in the task graph.
    TaskId, "tsk"
);
id_type!(
    /// A message in the persistent inbox/outbox.
    MessageId, "msg"
);
id_type!(
    /// One orchestration run (a lifetime of the MAW process driving a project).
    RunId, "run"
);
id_type!(
    /// An isolated workspace (git worktree or the project root).
    WorkspaceId, "wks"
);
id_type!(
    /// An event envelope identifier.
    EventId, "evt"
);
id_type!(
    /// A pending permission/approval request.
    ApprovalId, "apr"
);
id_type!(
    /// A registered artifact.
    ArtifactId, "art"
);
