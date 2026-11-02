//! Task graph model: statuses, evidence, dependency validation and verified-progress math.

use crate::error::{CoreError, CoreResult};
use crate::ids::{AgentId, ArtifactId, GoalId, ProjectId, TaskId, WorkspaceId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    Backlog,
    Ready,
    Assigned,
    Running,
    Blocked,
    InReview,
    Verifying,
    Done,
    Failed,
    Canceled,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 10] = [
        TaskStatus::Backlog,
        TaskStatus::Ready,
        TaskStatus::Assigned,
        TaskStatus::Running,
        TaskStatus::Blocked,
        TaskStatus::InReview,
        TaskStatus::Verifying,
        TaskStatus::Done,
        TaskStatus::Failed,
        TaskStatus::Canceled,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "BACKLOG",
            TaskStatus::Ready => "READY",
            TaskStatus::Assigned => "ASSIGNED",
            TaskStatus::Running => "RUNNING",
            TaskStatus::Blocked => "BLOCKED",
            TaskStatus::InReview => "IN_REVIEW",
            TaskStatus::Verifying => "VERIFYING",
            TaskStatus::Done => "DONE",
            TaskStatus::Failed => "FAILED",
            TaskStatus::Canceled => "CANCELED",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled)
    }

    /// Whether the task currently occupies an agent.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TaskStatus::Assigned | TaskStatus::Running | TaskStatus::InReview | TaskStatus::Verifying
        )
    }

    /// Validated task state machine. `Done` is only reachable from `Verifying`.
    pub fn can_transition_to(&self, to: TaskStatus) -> bool {
        use TaskStatus::*;
        if *self == to {
            return true;
        }
        if self.is_terminal() {
            // A failed task may be explicitly re-queued; Done / Canceled are final.
            return matches!((self, to), (Failed, Ready) | (Failed, Backlog));
        }
        if to == Canceled || to == Failed {
            return true;
        }
        match self {
            Backlog => matches!(to, Ready),
            Ready => matches!(to, Assigned | Backlog | Blocked),
            Assigned => matches!(to, Running | Ready | Blocked),
            Running => matches!(to, Blocked | InReview | Verifying | Ready),
            Blocked => matches!(to, Ready | Assigned | Running),
            // Rejected reviews / failed verification route back into the correction workflow.
            InReview => matches!(to, Verifying | Running | Assigned | Ready | Blocked),
            Verifying => matches!(to, Done | Running | Assigned | Ready | InReview),
            Done | Failed | Canceled => false,
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TaskStatus::ALL
            .iter()
            .copied()
            .find(|t| t.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown task status '{s}'"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    #[default]
    NotRequired,
    Pending,
    Approved,
    Rejected,
}

impl ReviewStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewStatus::NotRequired => "not_required",
            ReviewStatus::Pending => "pending",
            ReviewStatus::Approved => "approved",
            ReviewStatus::Rejected => "rejected",
        }
    }
}

impl FromStr for ReviewStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "not_required" => Ok(Self::NotRequired),
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            o => Err(format!("unknown review status '{o}'")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

impl Priority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }
    pub fn rank(&self) -> i64 {
        match self {
            Priority::Low => 0,
            Priority::Normal => 1,
            Priority::High => 2,
            Priority::Critical => 3,
        }
    }
}

impl FromStr for Priority {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "normal" | "medium" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            "critical" | "urgent" => Ok(Self::Critical),
            o => Err(format!("unknown priority '{o}'")),
        }
    }
}

/// Evidence supporting a completion claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// File changes (diff summary / changed paths).
    FileChanges,
    TestResult,
    BuildResult,
    /// A recorded review decision.
    Review,
    /// A written verification of acceptance criteria (valid for tasks that change no files).
    Verification,
    Note,
}

impl EvidenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceKind::FileChanges => "file_changes",
            EvidenceKind::TestResult => "test_result",
            EvidenceKind::BuildResult => "build_result",
            EvidenceKind::Review => "review",
            EvidenceKind::Verification => "verification",
            EvidenceKind::Note => "note",
        }
    }
}

impl FromStr for EvidenceKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file_changes" => Ok(Self::FileChanges),
            "test_result" => Ok(Self::TestResult),
            "build_result" => Ok(Self::BuildResult),
            "review" => Ok(Self::Review),
            "verification" => Ok(Self::Verification),
            "note" => Ok(Self::Note),
            o => Err(format!("unknown evidence kind '{o}'")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub summary: String,
    /// Whether the evidence indicates success (e.g. tests passed).
    pub passed: bool,
    pub detail: serde_json::Value,
    pub recorded_by: AgentId,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub task_id: Option<TaskId>,
    pub kind: String,
    pub path: Option<String>,
    pub description: String,
    pub registered_by: AgentId,
    pub registered_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// Human-friendly stable key, e.g. `T-3`.
    pub key: String,
    pub project_id: ProjectId,
    pub goal_id: GoalId,
    pub parent_task_id: Option<TaskId>,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: Priority,
    pub status: TaskStatus,
    pub owner_agent_id: Option<AgentId>,
    pub dependencies: Vec<TaskId>,
    pub estimated_weight: f64,
    /// Agent-reported progress, 0..=100. Never counted as verified completion.
    pub progress: u8,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub attempt_count: u32,
    pub workspace_id: Option<WorkspaceId>,
    pub artifacts: Vec<Artifact>,
    pub evidence: Vec<Evidence>,
    pub review_status: ReviewStatus,
    pub reviewer_agent_id: Option<AgentId>,
    pub blocked_reason: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    /// Plan revision that introduced the task.
    pub plan_revision: u32,
    pub created_by: AgentId,
    /// Tasks that legitimately change no files can declare this to skip the
    /// `file_changes` expectation (evidence is still required).
    pub no_file_changes_expected: bool,
}

impl Task {
    /// Check the evidence rules required before a task may be marked DONE.
    pub fn completion_blockers(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.evidence.iter().all(|e| !e.passed) {
            out.push("no passing evidence recorded (file changes, test/build result, or verification)".into());
        }
        if self.evidence.iter().any(|e| {
            matches!(e.kind, EvidenceKind::TestResult | EvidenceKind::BuildResult) && !e.passed
        }) && !self
            .evidence
            .iter()
            .rev()
            .find(|e| matches!(e.kind, EvidenceKind::TestResult | EvidenceKind::BuildResult))
            .map(|e| e.passed)
            .unwrap_or(false)
        {
            out.push("most recent test/build evidence is failing".into());
        }
        if !self.no_file_changes_expected
            && !self
                .evidence
                .iter()
                .any(|e| e.kind == EvidenceKind::FileChanges && e.passed)
            && !self
                .evidence
                .iter()
                .any(|e| e.kind == EvidenceKind::Verification && e.passed)
        {
            out.push("task is expected to change files: attach file_changes evidence or a verification explaining why none were needed".into());
        }
        match self.review_status {
            ReviewStatus::Pending => out.push("review is still pending".into()),
            ReviewStatus::Rejected => out.push("review was rejected".into()),
            _ => {}
        }
        out
    }
}

/// Detect whether adding `new_deps` to `task` would create a cycle in `edges`
/// (map task -> its dependencies). Returns the offending path on failure.
pub fn would_create_cycle(
    edges: &HashMap<TaskId, Vec<TaskId>>,
    task: &TaskId,
    new_deps: &[TaskId],
) -> Option<Vec<TaskId>> {
    for dep in new_deps {
        if dep == task {
            return Some(vec![task.clone(), task.clone()]);
        }
        // Is `task` reachable from `dep` via dependency edges?
        let mut stack = vec![(dep.clone(), vec![task.clone(), dep.clone()])];
        let mut seen: HashSet<TaskId> = HashSet::new();
        while let Some((cur, path)) = stack.pop() {
            if &cur == task {
                return Some(path);
            }
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(next) = edges.get(&cur) {
                for n in next {
                    let mut p = path.clone();
                    p.push(n.clone());
                    stack.push((n.clone(), p));
                }
            }
        }
    }
    None
}

pub fn validate_acyclic(edges: &HashMap<TaskId, Vec<TaskId>>) -> CoreResult<()> {
    for (task, deps) in edges {
        if let Some(path) = would_create_cycle(edges, task, deps) {
            let p: Vec<String> = path.iter().map(|t| t.to_string()).collect();
            return Err(CoreError::DependencyCycle(p.join(" -> ")));
        }
    }
    Ok(())
}

/// Progress breakdown. Verified completion is kept strictly separate from agent-reported
/// progress; unknown work is reported as such rather than guessed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub total_weight: f64,
    pub verified_weight: f64,
    /// Weighted agent-reported progress on non-terminal tasks (not verified).
    pub reported_weight: f64,
    pub blocked_weight: f64,
    pub failed_weight: f64,
    /// Weight of tasks nobody has started and nobody reported progress on.
    pub unknown_weight: f64,
    pub verified_pct: f64,
    pub task_count: usize,
    pub done_count: usize,
}

impl Progress {
    /// `Progress = completed verified weight / total planned weight` (canceled tasks excluded).
    pub fn compute<'a>(tasks: impl IntoIterator<Item = &'a Task>) -> Progress {
        let mut p = Progress::default();
        for t in tasks {
            if t.status == TaskStatus::Canceled {
                continue;
            }
            let w = t.estimated_weight.max(0.0);
            p.task_count += 1;
            p.total_weight += w;
            match t.status {
                TaskStatus::Done => {
                    p.verified_weight += w;
                    p.done_count += 1;
                }
                TaskStatus::Failed => p.failed_weight += w,
                TaskStatus::Blocked => p.blocked_weight += w,
                _ => {
                    if t.progress > 0 {
                        p.reported_weight += w * (t.progress as f64 / 100.0);
                    } else if matches!(t.status, TaskStatus::Backlog | TaskStatus::Ready) {
                        p.unknown_weight += w;
                    }
                }
            }
        }
        p.verified_pct = if p.total_weight > 0.0 {
            (p.verified_weight / p.total_weight * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: &str, status: TaskStatus, w: f64, progress: u8) -> Task {
        Task {
            id: id.into(),
            key: id.into(),
            project_id: "p".into(),
            goal_id: "g".into(),
            parent_task_id: None,
            title: id.into(),
            description: String::new(),
            acceptance_criteria: vec![],
            priority: Priority::Normal,
            status,
            owner_agent_id: None,
            dependencies: vec![],
            estimated_weight: w,
            progress,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            started_at: None,
            completed_at: None,
            attempt_count: 0,
            workspace_id: None,
            artifacts: vec![],
            evidence: vec![],
            review_status: ReviewStatus::NotRequired,
            reviewer_agent_id: None,
            blocked_reason: None,
            lease_expires_at: None,
            last_heartbeat_at: None,
            plan_revision: 1,
            created_by: "a".into(),
            no_file_changes_expected: false,
        }
    }

    #[test]
    fn done_only_from_verifying() {
        for s in TaskStatus::ALL {
            let ok = s.can_transition_to(TaskStatus::Done);
            assert_eq!(ok, matches!(s, TaskStatus::Verifying | TaskStatus::Done), "{s}");
        }
    }

    #[test]
    fn rejected_work_routes_back_to_correction() {
        assert!(TaskStatus::InReview.can_transition_to(TaskStatus::Assigned));
        assert!(TaskStatus::Verifying.can_transition_to(TaskStatus::Assigned));
        // but work can't skip review/verification on the way forward
        assert!(!TaskStatus::Running.can_transition_to(TaskStatus::Done));
        assert!(!TaskStatus::InReview.can_transition_to(TaskStatus::Done));
    }

    #[test]
    fn terminal_states_are_sticky() {
        assert!(!TaskStatus::Done.can_transition_to(TaskStatus::Running));
        assert!(!TaskStatus::Canceled.can_transition_to(TaskStatus::Ready));
        assert!(TaskStatus::Failed.can_transition_to(TaskStatus::Ready));
    }

    #[test]
    fn cycle_detection() {
        let a: TaskId = "a".into();
        let b: TaskId = "b".into();
        let c: TaskId = "c".into();
        let mut e = HashMap::new();
        e.insert(b.clone(), vec![a.clone()]);
        e.insert(c.clone(), vec![b.clone()]);
        // a -> c would close the loop a -> c -> b -> a
        assert!(would_create_cycle(&e, &a, &[c.clone()]).is_some());
        // new independent dep is fine
        assert!(would_create_cycle(&e, &c, &[a.clone()]).is_none());
        // self dependency
        assert!(would_create_cycle(&e, &a, &[a.clone()]).is_some());
        assert!(validate_acyclic(&e).is_ok());
        e.insert(a.clone(), vec![c.clone()]);
        assert!(validate_acyclic(&e).is_err());
    }

    #[test]
    fn progress_counts_only_verified() {
        let tasks = vec![
            mk("a", TaskStatus::Done, 2.0, 100),
            mk("b", TaskStatus::Running, 2.0, 90),
            mk("c", TaskStatus::Backlog, 1.0, 0),
            mk("d", TaskStatus::Blocked, 1.0, 10),
            mk("e", TaskStatus::Canceled, 50.0, 0),
        ];
        let p = Progress::compute(&tasks);
        assert_eq!(p.total_weight, 6.0);
        assert_eq!(p.verified_weight, 2.0);
        assert!((p.verified_pct - 33.333).abs() < 0.01);
        assert!((p.reported_weight - 1.8).abs() < 1e-9);
        assert_eq!(p.unknown_weight, 1.0);
        assert_eq!(p.blocked_weight, 1.0);
    }

    #[test]
    fn scope_expansion_lowers_progress() {
        let mut tasks = vec![mk("a", TaskStatus::Done, 1.0, 100), mk("b", TaskStatus::Done, 1.0, 100)];
        assert_eq!(Progress::compute(&tasks).verified_pct, 100.0);
        tasks.push(mk("c", TaskStatus::Backlog, 2.0, 0));
        assert_eq!(Progress::compute(&tasks).verified_pct, 50.0);
    }

    #[test]
    fn completion_requires_evidence() {
        let mut t = mk("a", TaskStatus::Verifying, 1.0, 100);
        assert!(!t.completion_blockers().is_empty());
        t.evidence.push(Evidence {
            kind: EvidenceKind::FileChanges,
            summary: "x".into(),
            passed: true,
            detail: serde_json::Value::Null,
            recorded_by: "a".into(),
            recorded_at: Utc::now(),
        });
        assert!(t.completion_blockers().is_empty());
        t.review_status = ReviewStatus::Pending;
        assert!(!t.completion_blockers().is_empty());
    }

    #[test]
    fn no_file_task_needs_verification_only_when_flagged() {
        let mut t = mk("a", TaskStatus::Verifying, 1.0, 100);
        t.evidence.push(Evidence {
            kind: EvidenceKind::Note,
            summary: "analysis done".into(),
            passed: true,
            detail: serde_json::Value::Null,
            recorded_by: "a".into(),
            recorded_at: Utc::now(),
        });
        // Not flagged: a note alone isn't enough for a file-changing task.
        assert!(!t.completion_blockers().is_empty());
        t.no_file_changes_expected = true;
        assert!(t.completion_blockers().is_empty());
    }
}
