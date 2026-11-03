//! Token-usage derivation. Pure functions of the snapshot and the stored `agent.usage` events.
//!
//! Everything shown comes from numbers the providers themselves report; MAW never estimates
//! tokens. Providers report cumulative figures per session, so the live totals are the latest
//! report per agent, while the rate, sparkline and per-task attribution are built from the
//! increments MAW recorded with each report (`payload.delta`, attributed to the task the agent was
//! working on at that moment).

use chrono::{DateTime, Utc};
use maw_core::{Agent, AgentId, Event, EventKind, ProviderKind, Task, TaskId};
use std::collections::HashMap;

/// Minutes of history in the sparkline.
pub const SPARK_MINUTES: usize = 30;
/// Window for the "tokens per minute" rate.
pub const RATE_WINDOW_SECS: i64 = 300;

#[derive(Clone, Debug)]
pub struct AgentUsageRow {
    pub agent_id: AgentId,
    pub name: String,
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub total: Option<u64>,
    /// Claude Code's own client-side estimate; Codex reports tokens only.
    pub cost_usd: Option<f64>,
    /// The provider has reported usage at least once.
    pub reported: bool,
    /// Share of the project's total tokens, 0..=1.
    pub share: f64,
    /// Tokens per minute over the last [`RATE_WINDOW_SECS`] (None: no reports to compute from).
    pub rate_per_min: Option<f64>,
    /// Tokens added per minute, oldest first, [`SPARK_MINUTES`] entries.
    pub spark: Vec<u64>,
    pub last_report_secs: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct TaskUsageRow {
    pub key: String,
    pub title: String,
    pub tokens: u64,
}

#[derive(Clone, Debug, Default)]
pub struct UsageReport {
    pub agents: Vec<AgentUsageRow>,
    /// Tasks by tokens used, largest first.
    pub tasks: Vec<TaskUsageRow>,
    pub total_tokens: u64,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cost_usd: Option<f64>,
    pub agents_reporting: usize,
    pub agents_total: usize,
    /// Tokens added by reports not tied to any task (planning, idle chatter, Leader overhead).
    pub unattributed_tokens: u64,
}

fn total_of(a: &Agent) -> Option<u64> {
    a.usage
        .total_tokens
        .or(match (a.usage.input_tokens, a.usage.output_tokens) {
            (Some(i), Some(o)) => Some(i + o),
            (Some(i), None) => Some(i),
            (None, Some(o)) => Some(o),
            _ => None,
        })
}

fn delta_total(e: &Event) -> u64 {
    e.payload
        .pointer("/delta/total")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

pub fn derive_usage(
    agents: &[Agent],
    tasks: &[Task],
    events: &[Event],
    now: DateTime<Utc>,
) -> UsageReport {
    let usage_events: Vec<&Event> = events
        .iter()
        .filter(|e| e.event_type == EventKind::AgentUsage)
        .collect();

    let mut rows: Vec<AgentUsageRow> = agents
        .iter()
        .map(|a| {
            let mine: Vec<&&Event> = usage_events
                .iter()
                .filter(|e| e.agent_id.as_ref() == Some(&a.id))
                .collect();
            let mut spark = vec![0u64; SPARK_MINUTES];
            let mut recent = 0u64;
            for e in &mine {
                let age = (now - e.timestamp).num_seconds().max(0);
                let d = delta_total(e);
                let bucket = (age / 60) as usize;
                if bucket < SPARK_MINUTES {
                    spark[SPARK_MINUTES - 1 - bucket] += d;
                }
                if age <= RATE_WINDOW_SECS {
                    recent += d;
                }
            }
            let last = mine
                .iter()
                .map(|e| e.timestamp)
                .max()
                .map(|t| (now - t).num_seconds().max(0));
            let total = total_of(a);
            AgentUsageRow {
                agent_id: a.id.clone(),
                name: a.name.clone(),
                provider: a.provider,
                model: a.model.clone(),
                effort: a.effort.clone(),
                input: a.usage.input_tokens,
                output: a.usage.output_tokens,
                total,
                cost_usd: a.usage.cost_usd,
                reported: total.is_some() || a.usage.cost_usd.is_some(),
                share: 0.0,
                rate_per_min: if mine.is_empty() {
                    None
                } else {
                    Some(recent as f64 / (RATE_WINDOW_SECS as f64 / 60.0))
                },
                spark,
                last_report_secs: last,
            }
        })
        .collect();

    let total_tokens: u64 = rows.iter().filter_map(|r| r.total).sum();
    for r in &mut rows {
        r.share = if total_tokens > 0 {
            r.total.unwrap_or(0) as f64 / total_tokens as f64
        } else {
            0.0
        };
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.total.unwrap_or(0)));

    let mut per_task: HashMap<TaskId, u64> = HashMap::new();
    let mut unattributed = 0u64;
    for e in &usage_events {
        let d = delta_total(e);
        match &e.task_id {
            Some(t) => *per_task.entry(t.clone()).or_default() += d,
            None => unattributed += d,
        }
    }
    let mut task_rows: Vec<TaskUsageRow> = per_task
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(id, tokens)| {
            let t = tasks.iter().find(|t| t.id == id);
            TaskUsageRow {
                key: t
                    .map(|t| t.key.clone())
                    .unwrap_or_else(|| id.0.chars().take(8).collect()),
                title: t
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| "(task no longer listed)".into()),
                tokens,
            }
        })
        .collect();
    task_rows.sort_by_key(|t| std::cmp::Reverse(t.tokens));

    let costs: Vec<f64> = rows.iter().filter_map(|r| r.cost_usd).collect();
    UsageReport {
        total_input: rows.iter().filter_map(|r| r.input).sum(),
        total_output: rows.iter().filter_map(|r| r.output).sum(),
        total_cost_usd: if costs.is_empty() {
            None
        } else {
            Some(costs.iter().sum())
        },
        agents_reporting: rows.iter().filter(|r| r.reported).count(),
        agents_total: rows.len(),
        total_tokens,
        tasks: task_rows,
        unattributed_tokens: unattributed,
        agents: rows,
    }
}

/// 950 -> "950", 12_345 -> "12.3k", 1_234_567 -> "1.23M".
pub fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => {
            let k = n as f64 / 1_000.0;
            if k >= 100.0 {
                format!("{k:.0}k")
            } else {
                format!("{k:.1}k")
            }
        }
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

pub fn fmt_cost(c: f64) -> String {
    if c < 0.1 {
        format!("${c:.4}")
    } else {
        format!("${c:.2}")
    }
}

/// Eight-level sparkline; zero buckets are blank so gaps read as "no traffic".
pub fn sparkline(values: &[u64]) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|&v| {
            if v == 0 || max == 0 {
                ' '
            } else {
                LEVELS[(((v as f64 / max as f64) * 7.0).round() as usize).min(7)]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use chrono::Duration;
    use maw_core::{AgentState, Role, UsageMetrics};
    use serde_json::json;

    fn usage_event(
        agent: &Agent,
        task: Option<&Task>,
        delta: u64,
        age_secs: i64,
        now: DateTime<Utc>,
    ) -> Event {
        let mut e = Event::new(
            "prj_test".into(),
            EventKind::AgentUsage,
            json!({ "delta": { "total": delta, "input": delta, "output": 0, "cost_usd": 0.0 } }),
        );
        e.agent_id = Some(agent.id.clone());
        e.task_id = task.map(|t| t.id.clone());
        e.timestamp = now - Duration::seconds(age_secs);
        e
    }

    fn with_usage(mut a: Agent, input: u64, output: u64, cost: Option<f64>) -> Agent {
        a.usage = UsageMetrics {
            input_tokens: Some(input),
            output_tokens: Some(output),
            total_tokens: Some(input + output),
            cost_usd: cost,
            reported_by_provider: true,
        };
        a
    }

    #[test]
    fn formatting_is_compact_and_stable() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(123_456), "123k");
        assert_eq!(fmt_tokens(1_234_567), "1.23M");
        assert_eq!(fmt_cost(0.00494), "$0.0049");
        assert_eq!(fmt_cost(1.5), "$1.50");
        assert_eq!(sparkline(&[0, 1, 8, 4]), " ▂█▅");
        assert_eq!(sparkline(&[0, 0]), "  ");
    }

    #[test]
    fn totals_shares_and_unreported_agents() {
        let now = Utc::now();
        let a = with_usage(
            agent("claude-lead", Role::Leader, AgentState::Idle),
            700,
            100,
            Some(0.02),
        );
        let b = with_usage(
            agent("codex-1", Role::Worker, AgentState::Working),
            100,
            100,
            None,
        );
        let c = agent("codex-2", Role::Worker, AgentState::Idle); // never reported
        let r = derive_usage(&[a, b, c], &[], &[], now);
        assert_eq!(
            (r.total_tokens, r.total_input, r.total_output),
            (1000, 800, 200)
        );
        assert_eq!((r.agents_reporting, r.agents_total), (2, 3));
        assert_eq!(r.total_cost_usd, Some(0.02));
        assert_eq!(r.agents[0].name, "claude-lead", "largest first");
        assert!((r.agents[0].share - 0.8).abs() < 1e-9 && (r.agents[1].share - 0.2).abs() < 1e-9);
        let unreported = r.agents.iter().find(|x| x.name == "codex-2").unwrap();
        assert!(
            !unreported.reported && unreported.total.is_none() && unreported.rate_per_min.is_none()
        );
    }

    #[test]
    fn rate_sparkline_and_task_attribution_come_from_recorded_increments() {
        let now = Utc::now();
        let a = with_usage(
            agent("codex-1", Role::Worker, AgentState::Working),
            5000,
            0,
            None,
        );
        let snap = sample_snapshot();
        let t1 = &snap.tasks[0];
        let t2 = &snap.tasks[1];
        let events = vec![
            usage_event(&a, Some(t1), 1000, 30, now), // last minute, in rate window
            usage_event(&a, Some(t1), 500, 130, now), // 2 minutes ago, in rate window
            usage_event(&a, Some(t2), 2000, 3000, now), // 50 minutes ago: outside sparkline and rate
            usage_event(&a, None, 700, 40 * 60 + 10, now), // outside the sparkline, unattributed
        ];
        let r = derive_usage(&[a], &snap.tasks, &events, now);
        let row = &r.agents[0];
        assert!(
            (row.rate_per_min.unwrap() - 300.0).abs() < 1e-9,
            "1500 tokens in 5 minutes"
        );
        assert_eq!(row.spark.len(), SPARK_MINUTES);
        assert_eq!(*row.spark.last().unwrap(), 1000);
        assert_eq!(row.spark[SPARK_MINUTES - 3], 500);
        assert_eq!(
            row.spark.iter().sum::<u64>(),
            1500,
            "older increments are not in the 30 minute strip"
        );
        assert_eq!(row.last_report_secs, Some(30));
        // per-task: totals over ALL recorded increments
        assert_eq!(r.tasks[0].tokens, 2000);
        assert_eq!(r.tasks[0].key, t2.key);
        assert_eq!(r.tasks[1].tokens, 1500);
        assert_eq!(r.unattributed_tokens, 700);
    }

    #[test]
    fn empty_inputs_never_panic() {
        let r = derive_usage(&[], &[], &[], Utc::now());
        assert_eq!((r.total_tokens, r.agents_total), (0, 0));
        assert!(r.total_cost_usd.is_none() && r.tasks.is_empty());
    }
}
