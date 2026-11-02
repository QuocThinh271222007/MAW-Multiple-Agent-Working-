//! Goals, plan revisions and messages.

use crate::ids::{AgentId, GoalId, MessageId, ProjectId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    /// Leader believes the goal is ready; final verification pending.
    Verifying,
    Completed,
    Canceled,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Verifying => "verifying",
            GoalStatus::Completed => "completed",
            GoalStatus::Canceled => "canceled",
        }
    }
}

impl FromStr for GoalStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "verifying" => Ok(Self::Verifying),
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            o => Err(format!("unknown goal status '{o}'")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub project_id: ProjectId,
    pub text: String,
    pub success_criteria: Vec<String>,
    pub status: GoalStatus,
    pub revision: u32,
    pub leader_agent_id: Option<AgentId>,
    pub current_milestone: Option<String>,
    pub current_focus: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalRevision {
    pub goal_id: GoalId,
    pub revision: u32,
    pub text: String,
    pub success_criteria: Vec<String>,
    pub reason: String,
    pub author: AgentId,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Milestone {
    pub title: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub task_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanRevision {
    pub goal_id: GoalId,
    pub revision: u32,
    pub summary: String,
    pub milestones: Vec<Milestone>,
    pub next_actions: Vec<String>,
    pub reason: String,
    pub author: AgentId,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageType {
    Instruction,
    Question,
    Answer,
    Status,
    Result,
    Blocker,
    ReviewRequest,
    ReviewFeedback,
    Handoff,
}

impl MessageType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageType::Instruction => "INSTRUCTION",
            MessageType::Question => "QUESTION",
            MessageType::Answer => "ANSWER",
            MessageType::Status => "STATUS",
            MessageType::Result => "RESULT",
            MessageType::Blocker => "BLOCKER",
            MessageType::ReviewRequest => "REVIEW_REQUEST",
            MessageType::ReviewFeedback => "REVIEW_FEEDBACK",
            MessageType::Handoff => "HANDOFF",
        }
    }
}

impl fmt::Display for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MessageType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "INSTRUCTION" => Ok(Self::Instruction),
            "QUESTION" => Ok(Self::Question),
            "ANSWER" => Ok(Self::Answer),
            "STATUS" => Ok(Self::Status),
            "RESULT" => Ok(Self::Result),
            "BLOCKER" => Ok(Self::Blocker),
            "REVIEW_REQUEST" => Ok(Self::ReviewRequest),
            "REVIEW_FEEDBACK" => Ok(Self::ReviewFeedback),
            "HANDOFF" => Ok(Self::Handoff),
            o => Err(format!("unknown message type '{o}'")),
        }
    }
}

/// Persistent inbox message. `delivered_at` is set by the Orchestrator when the message
/// was actually handed to the recipient's session; `acknowledged_at` when the recipient
/// confirmed it via `maw_message_ack`. Creating a message implies neither.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub project_id: ProjectId,
    pub sender_agent_id: AgentId,
    pub recipient_agent_id: AgentId,
    pub task_id: Option<TaskId>,
    pub message_type: MessageType,
    pub subject: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub correlation_id: Option<String>,
}

/// A published shared-context entry or recorded decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextEntry {
    pub id: String,
    pub project_id: ProjectId,
    /// `context` or `decision`.
    pub kind: String,
    pub key: String,
    pub content: String,
    pub tags: Vec<String>,
    pub task_id: Option<TaskId>,
    pub author: AgentId,
    pub created_at: DateTime<Utc>,
}
