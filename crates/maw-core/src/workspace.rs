//! Workspaces, approvals, execution attempts and runs.

use crate::ids::{AgentId, ApprovalId, ProjectId, RunId, TaskId, WorkspaceId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub agent_id: Option<AgentId>,
    pub task_id: Option<TaskId>,
    pub path: String,
    pub branch: Option<String>,
    pub base_revision: Option<String>,
    /// `root` (the project checkout), `worktree`, or `directory` (non-git fallback).
    pub kind: String,
    /// `active`, `integrated`, `abandoned`.
    pub status: String,
    pub changed_files: Vec<String>,
    /// `none`, `pending`, `integrated`, `conflict`.
    pub integration_status: String,
    pub conflict: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    ApprovedForSession,
    Denied,
    /// The owning session disappeared before a decision was made.
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Approved => "approved",
            ApprovalStatus::ApprovedForSession => "approved_for_session",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Expired => "expired",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "approved" => Self::Approved,
            "approved_for_session" => Self::ApprovedForSession,
            "denied" => Self::Denied,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: ApprovalId,
    pub project_id: ProjectId,
    pub agent_id: AgentId,
    pub task_id: Option<TaskId>,
    /// `command`, `file_change`, `permissions`, `other`.
    pub kind: String,
    pub summary: String,
    pub reason: Option<String>,
    pub details: serde_json::Value,
    /// Provider-side request identifier needed to answer the request.
    pub request_ref: String,
    pub status: ApprovalStatus,
    pub requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: i64,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub attempt_no: u32,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub outcome: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub project_id: ProjectId,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub clean_shutdown: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: String,
    pub created_at: DateTime<Utc>,
}
