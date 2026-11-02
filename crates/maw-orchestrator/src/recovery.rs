//! Startup reconciliation. Provider sessions never survive a MAW restart, so anything that was
//! mid-flight is reconciled conservatively: tasks that were RUNNING have unknown side effects and
//! are parked as BLOCKED for an explicit decision rather than silently re-executed.

use crate::engine::{Actor, Engine};
use maw_core::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub unclean_previous_runs: usize,
    pub tasks_parked: Vec<String>,
    pub approvals_expired: usize,
    pub agents_reset: usize,
    /// Human-readable actions the operator still needs to take.
    pub needs_attention: Vec<String>,
}

impl Engine {
    /// Reconcile persisted state after (re)start. Call once, before the supervisor starts.
    pub fn recover(&self) -> CoreResult<RecoveryReport> {
        let mut report = RecoveryReport::default();
        let project = self.project.id.clone();
        let this_run = self.run_id.clone();
        self.tx(|db, emit| {
            let unclean: Vec<Run> = db.unclean_runs(&project)?.into_iter().filter(|r| r.id != this_run).collect();
            report.unclean_previous_runs = unclean.len();

            // Agents: no provider session survives a restart.
            for mut a in db.list_agents(&project)? {
                if a.state != AgentState::Disconnected {
                    a.state = AgentState::Disconnected;
                    a.session_id = None;
                    a.current_task_id = None;
                    db.update_agent(&a)?;
                    report.agents_reset += 1;
                }
                report.approvals_expired += db.expire_agent_approvals(&a.id)?;
            }

            // Tasks that were executing: park, never auto-retry (side effects unknown).
            for mut t in db.list_tasks(&project)? {
                match t.status {
                    TaskStatus::Running => {
                        let owner = t.owner_agent_id.clone();
                        t.blocked_reason = Some("MAW restarted while this task was running; the previous attempt's side effects are unknown".into());
                        self.transition(emit, db, &mut t, TaskStatus::Blocked, EventKind::RecoveryRequired, json!({ "owner": owner }))?;
                        db.end_attempts(&t.id, "interrupted_by_restart", None)?;
                        report.tasks_parked.push(t.key.clone());
                        report.needs_attention.push(format!("{} \"{}\" was running: inspect the workspace, then release/reassign or cancel it", t.key, t.title));
                    }
                    TaskStatus::InReview => {
                        report.needs_attention.push(format!("{} awaits review; it will be re-dispatched to a reviewer", t.key));
                    }
                    _ => {}
                }
            }
            if report.unclean_previous_runs > 0 {
                report.needs_attention.push(format!("{} previous run(s) did not shut down cleanly", report.unclean_previous_runs));
            }
            db.close_unclean_runs(&project)?;
            // close_unclean_runs also closes the current run's row marker; restore it.
            db.kv_set(&project, "last_recovery", &chrono::Utc::now().to_rfc3339())?;
            emit.push(EventKind::SessionRecovered, None, None, json!({ "report": report }));
            Ok(())
        })?;
        // the current run must stay open until a clean shutdown
        self.store.write(|db| db.reopen_run(&this_run))?;
        if !report.tasks_parked.is_empty() {
            let leader = self.leader()?;
            if leader.is_some() {
                let _ = self.send_message(
                    &Actor::System,
                    "leader",
                    MessageType::Blocker,
                    "Recovery: tasks need a decision",
                    &format!("MAW restarted. These tasks were running and are now BLOCKED: {}. Decide for each: maw_task_release (to retry) or maw_task_update cancel.", report.tasks_parked.join(", ")),
                    None,
                    Some(format!("recovery-{}", self.run_id)),
                );
            }
        }
        Ok(report)
    }
}
