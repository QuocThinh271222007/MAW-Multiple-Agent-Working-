//! Non-TUI orchestration: `maw run`, `maw resume`, `maw demo --headless`, plus shared
//! formatting for events and task tables.

use crate::app::Runtime;
use anyhow::Result;
use maw_core::*;
use maw_orchestrator::Snapshot;
use std::time::Duration;

pub fn event_line(e: &Event) -> String {
    let p = &e.payload;
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let task = s("task_key");
    let detail = match e.event_type {
        EventKind::MessageSent => format!(
            "{} -> {} [{}] {}",
            s("from"),
            s("to"),
            s("message_type"),
            s("subject")
        ),
        EventKind::MessageDelivered => format!("delivered to {}", s("to")),
        EventKind::TaskAssigned => format!("{task} -> {}", s("to")),
        EventKind::TaskCreated => format!("{task} {} (by {})", s("title"), s("by")),
        EventKind::TaskProgress => format!(
            "{task} {}",
            p.get("percent")
                .map(|v| format!("{v}% "))
                .unwrap_or_default()
                + s("note")
        ),
        EventKind::TaskBlocked => format!("{task}: {}", s("reason")),
        EventKind::TaskSubmitted => format!("{task} submitted by {}", s("by")),
        EventKind::TaskReviewed => format!("{task} {} by {}", s("verdict"), s("reviewer")),
        EventKind::TaskCompleted
        | EventKind::TaskVerified
        | EventKind::TaskStarted
        | EventKind::TaskFailed
        | EventKind::TaskReady => format!("{task} {}", s("title")),
        EventKind::AgentStateChanged => format!("{} {} -> {}", s("name"), s("from"), s("state")),
        EventKind::AgentMessage => format!(
            "{}: {}",
            s("agent"),
            s("text").chars().take(100).collect::<String>()
        ),
        EventKind::McpToolCompleted => format!(
            "{} {}{}",
            s("agent").chars().take(12).collect::<String>(),
            s("tool"),
            if p.get("ok") == Some(&serde_json::Value::Bool(false)) {
                " (error)"
            } else {
                ""
            }
        ),
        EventKind::PermissionRequested => format!("{}: {}", s("agent"), s("summary")),
        _ => p.to_string().chars().take(110).collect(),
    };
    format!(
        "{}  {:<22} {}",
        e.timestamp.format("%H:%M:%S"),
        e.event_type.as_str(),
        detail
    )
}

pub fn print_summary(snap: &Snapshot) {
    println!();
    if let Some(g) = &snap.goal {
        println!("Goal [{}] r{}: {}", g.status.as_str(), g.revision, g.text);
    }
    println!(
        "Verified progress: {:.0}%  (done {}/{}; reported-unverified weight {:.1}; blocked {:.1}; failed {:.1})",
        snap.progress.verified_pct, snap.progress.done_count, snap.progress.task_count, snap.progress.reported_weight, snap.progress.blocked_weight, snap.progress.failed_weight
    );
    print_tasks(&snap.tasks, &snap.agents);
}

pub fn print_tasks(tasks: &[Task], agents: &[Agent]) {
    println!(
        "{:<5} {:<10} {:<14} {:<4} TITLE",
        "TASK", "STATUS", "OWNER", "ATT"
    );
    for t in tasks {
        let owner = t
            .owner_agent_id
            .as_ref()
            .and_then(|o| agents.iter().find(|a| &a.id == o))
            .map(|a| a.name.as_str())
            .unwrap_or("-");
        println!(
            "{:<5} {:<10} {:<14} {:<4} {}",
            t.key,
            t.status.as_str(),
            owner,
            t.attempt_count,
            t.title
        );
    }
}

/// How long a headless run without a terminal waits before denying an approval request.
const HEADLESS_DENY_AFTER: Duration = Duration::from_secs(120);

/// Resolve one approval request in headless mode. With a terminal the *user* decides; without
/// one the request is denied after a timeout. Nothing is ever approved automatically.
async fn resolve_headless_approval(
    sup: std::sync::Arc<maw_orchestrator::Supervisor>,
    id: ApprovalId,
) {
    use maw_core::provider::ApprovalDecision;
    use std::io::IsTerminal;
    let Some(ap) = sup
        .engine
        .pending_approvals()
        .ok()
        .and_then(|v| v.into_iter().find(|a| a.id == id))
    else {
        return;
    };
    let who = sup
        .engine
        .get_agent(&ap.agent_id)
        .map(|a| a.name)
        .unwrap_or_else(|_| ap.agent_id.0.clone());
    eprintln!(
        "\n⏸  APPROVAL NEEDED — {who} wants to: {}{}",
        ap.summary,
        ap.reason
            .as_ref()
            .map(|r| format!("  (reason: {r})"))
            .unwrap_or_default()
    );
    let decision = if std::io::stdin().is_terminal() {
        eprint!("   [y]es once / [s]ession / [n]o / [a]bort turn > ");
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            s
        })
        .await
        .unwrap_or_default();
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => ApprovalDecision::Approve,
            "s" | "session" => ApprovalDecision::ApproveForSession,
            "a" | "abort" => ApprovalDecision::Abort,
            _ => ApprovalDecision::Deny,
        }
    } else {
        eprintln!("   (no terminal: denying in {}s unless you resolve it in the TUI; MAW never auto-approves)", HEADLESS_DENY_AFTER.as_secs());
        tokio::time::sleep(HEADLESS_DENY_AFTER).await;
        ApprovalDecision::Deny
    };
    // it may have been resolved elsewhere in the meantime
    if sup
        .engine
        .pending_approvals()
        .map(|v| v.iter().any(|a| a.id == id))
        .unwrap_or(false)
    {
        if let Err(e) = sup.resolve_approval(&id, decision).await {
            eprintln!("   could not resolve approval: {e}");
        } else {
            eprintln!("   → {decision:?}");
        }
    }
}

pub enum Outcome {
    GoalCompleted,
    Stopped,
    TimedOut,
    Stalled(String),
}

/// Drive the already-booted runtime until the goal completes, the user stops it, or it times out.
pub async fn drive(
    rt: &Runtime,
    goal: Option<(String, Vec<String>)>,
    timeout: Option<Duration>,
    quiet: bool,
) -> Result<Outcome> {
    let engine = &rt.engine;
    if !rt.recovery.needs_attention.is_empty() && !quiet {
        println!("Recovery after previous run:");
        for n in &rt.recovery.needs_attention {
            println!("  ! {n}");
        }
    }
    let printer = if quiet {
        None
    } else {
        let mut rx = engine.subscribe();
        Some(tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(e)
                        if !e.event_type.is_verbose()
                            || matches!(e.event_type, EventKind::AgentMessage) =>
                    {
                        println!("{}", event_line(&e))
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }))
    };

    let approver = {
        let sup = rt.sup.clone();
        let mut rx = engine.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(e) if e.event_type == EventKind::PermissionRequested => {
                        if let Some(id) = e.payload.get("approval_id").and_then(|v| v.as_str()) {
                            tokio::spawn(resolve_headless_approval(
                                sup.clone(),
                                ApprovalId::from(id),
                            ));
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        })
    };

    // wait for agents to come up (connected, or terminally unavailable)
    let up_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let agents = engine.list_agents().map_err(|e| anyhow::anyhow!("{e}"))?;
        if agents.is_empty() {
            anyhow::bail!(
                "no agents configured: add some with `maw agent add` (or use `maw demo`)"
            );
        }
        let settled = agents
            .iter()
            .all(|a| !matches!(a.state, AgentState::Connecting | AgentState::Disconnected));
        if settled || tokio::time::Instant::now() > up_deadline {
            for a in agents.iter().filter(|a| a.state != AgentState::Idle) {
                eprintln!(
                    "warning: agent {} is {}{}",
                    a.name,
                    a.state,
                    a.last_error
                        .as_ref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                );
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !engine
        .list_agents()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .iter()
        .any(|a| a.role == Role::Leader && a.state == AgentState::Idle)
    {
        anyhow::bail!("the Leader is not connected; run `maw doctor` and `maw auth <provider>`");
    }

    match goal {
        Some((text, criteria)) => {
            engine
                .create_goal(&text, criteria)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        None => {
            if engine
                .current_goal()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .is_none()
            {
                anyhow::bail!("no goal yet: `maw run \"<objective>\"`");
            }
        }
    }

    let start = tokio::time::Instant::now();
    let mut last_progress = (tokio::time::Instant::now(), 0i64);
    let outcome = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Outcome::Stopped,
            _ = tokio::time::sleep(Duration::from_millis(400)) => {}
        }
        if let Ok(Some(g)) = engine.current_goal() {
            if g.status == GoalStatus::Completed {
                break Outcome::GoalCompleted;
            }
        }
        // `maw stop` from another terminal sets this flag; consume it so the next run starts clean.
        if engine
            .read(|db| db.kv_get(engine.project_id(), "stop_request"))
            .ok()
            .flatten()
            .is_some_and(|v| !v.is_empty())
        {
            let _ = engine
                .store()
                .write(|db| db.kv_set(engine.project_id(), "stop_request", ""));
            break Outcome::Stopped;
        }
        if timeout.map(|t| start.elapsed() > t).unwrap_or(false) {
            break Outcome::TimedOut;
        }
        // stall detection: no new event for a long time while nothing is active
        let n = engine
            .read(|db| db.count_events(engine.project_id()))
            .unwrap_or(0);
        if n != last_progress.1 {
            last_progress = (tokio::time::Instant::now(), n);
        } else if last_progress.0.elapsed() > Duration::from_secs(15 * 60) {
            break Outcome::Stalled("no activity for 15 minutes".into());
        }
    };
    if let Some(p) = printer {
        p.abort();
    }
    approver.abort();
    Ok(outcome)
}
