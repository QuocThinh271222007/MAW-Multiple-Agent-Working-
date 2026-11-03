//! Builders for synthetic snapshots, used by unit/render tests (and handy for demos).

use chrono::Utc;
use maw_core::*;
use maw_orchestrator::Snapshot;
use serde_json::json;

pub fn agent(name: &str, role: Role, state: AgentState) -> Agent {
    Agent {
        id: AgentId::new(),
        project_id: ProjectId::from("prj_test"),
        name: name.to_string(),
        provider: if name.starts_with("claude") {
            ProviderKind::Claude
        } else {
            ProviderKind::Codex
        },
        model: None,
        effort: None,
        role,
        state,
        permission: PermissionProfile::Balanced,
        session_id: None,
        native_session_id: None,
        workspace: None,
        current_task_id: None,
        capabilities: Capabilities::default(),
        last_heartbeat: None,
        last_error: None,
        usage: UsageMetrics::default(),
        created_at: Utc::now(),
    }
}

pub fn task(key: &str, status: TaskStatus) -> Task {
    Task {
        id: TaskId::new(),
        key: key.to_string(),
        project_id: ProjectId::from("prj_test"),
        goal_id: GoalId::from("goal_test"),
        parent_task_id: None,
        title: format!("Task {key}"),
        description: String::new(),
        acceptance_criteria: vec![],
        priority: Priority::Normal,
        status,
        owner_agent_id: None,
        dependencies: vec![],
        estimated_weight: 1.0,
        progress: 0,
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
        created_by: AgentId::from("user"),
        no_file_changes_expected: false,
    }
}

pub fn event(
    seq: i64,
    kind: EventKind,
    agent: Option<&AgentId>,
    payload: serde_json::Value,
) -> Event {
    let mut e = Event::new(ProjectId::from("prj_test"), kind, payload);
    e.sequence = seq;
    e.agent_id = agent.cloned();
    e
}

/// Render `app` into an in-memory terminal of the given size.
pub fn render_buffer(app: &crate::app::App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| crate::ui::render(f, app)).expect("draw");
    term.backend().buffer().clone()
}

pub fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let a = buf.area;
    (0..a.height)
        .map(|y| {
            (0..a.width)
                .map(|x| buf[(a.x + x, a.y + y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_text(app: &crate::app::App, w: u16, h: u16) -> String {
    buffer_text(&render_buffer(app, w, h))
}

/// A mid-flight project: Leader + two Workers + Reviewer, six tasks, goal, plan, events.
pub fn sample_snapshot() -> Snapshot {
    let mut lead = agent("claude-lead", Role::Leader, AgentState::Thinking);
    let mut w1 = agent("codex-backend", Role::Worker, AgentState::Working);
    let mut w2 = agent("codex-tests", Role::Worker, AgentState::WaitingApproval);
    let rev = agent("claude-review", Role::Reviewer, AgentState::Idle);

    let mut t1 = task("T-1", TaskStatus::Done);
    t1.title = "Analyze existing architecture".into();
    t1.progress = 100;
    let mut t2 = task("T-2", TaskStatus::Done);
    t2.title = "Design interfaces".into();
    let mut t3 = task("T-3", TaskStatus::Running);
    t3.title = "Implement backend logic".into();
    t3.owner_agent_id = Some(w1.id.clone());
    t3.dependencies = vec![t2.id.clone()];
    t3.progress = 40;
    t3.estimated_weight = 3.0;
    t3.attempt_count = 1;
    t3.started_at = Some(Utc::now());
    let mut t4 = task("T-4", TaskStatus::Running);
    t4.title = "Write tests".into();
    t4.owner_agent_id = Some(w2.id.clone());
    t4.dependencies = vec![t2.id.clone()];
    t4.estimated_weight = 2.0;
    t4.started_at = Some(Utc::now());
    let mut t5 = task("T-5", TaskStatus::InReview);
    t5.title = "Write documentation".into();
    t5.review_status = ReviewStatus::Pending;
    t5.owner_agent_id = Some(w2.id.clone());
    let mut t6 = task("T-6", TaskStatus::Backlog);
    t6.title = "Integrate and verify".into();
    t6.dependencies = vec![t3.id.clone(), t4.id.clone(), t5.id.clone()];
    let mut t7 = task("T-7", TaskStatus::Blocked);
    t7.title = "Choose hashing scheme".into();
    t7.blocked_reason = Some("needs a decision".into());

    w1.current_task_id = Some(t3.id.clone());
    w2.current_task_id = Some(t4.id.clone());
    lead.current_task_id = None;

    let tasks = vec![t1, t2, t3, t4, t5, t6, t7];
    let progress = Progress::compute(tasks.iter());
    let goal = Goal {
        id: GoalId::from("goal_test"),
        project_id: ProjectId::from("prj_test"),
        text: "Implement a simple authentication module with validation, tests and docs".into(),
        success_criteria: vec!["all tests pass".into(), "docs exist".into()],
        status: GoalStatus::Active,
        revision: 2,
        leader_agent_id: Some(lead.id.clone()),
        current_milestone: Some("Integration".into()),
        current_focus: Some("coordinating T-3/T-4".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let plan = PlanRevision {
        goal_id: goal.id.clone(),
        revision: 1,
        summary: "Analyze, design, then build in parallel".into(),
        milestones: vec![
            Milestone {
                title: "Foundation".into(),
                done: true,
                task_keys: vec!["T-1".into(), "T-2".into()],
            },
            Milestone {
                title: "Implementation".into(),
                done: false,
                task_keys: vec!["T-3".into(), "T-4".into(), "T-5".into()],
            },
            Milestone {
                title: "Integration".into(),
                done: false,
                task_keys: vec!["T-6".into()],
            },
        ],
        next_actions: vec!["verify T-5".into(), "unblock T-7".into()],
        reason: "initial plan".into(),
        author: lead.id.clone(),
        created_at: Utc::now(),
    };
    let (l, a, b, r) = (
        lead.id.0.clone(),
        w1.id.0.clone(),
        w2.id.0.clone(),
        rev.id.0.clone(),
    );
    let events = vec![
        event(
            1,
            EventKind::TaskCreated,
            Some(&lead.id),
            json!({ "by_id": l, "task_key": "T-3", "title": "Implement backend logic" }),
        ),
        event(
            2,
            EventKind::TaskAssigned,
            None,
            json!({ "by_id": "orchestrator", "to_id": a, "task_key": "T-3" }),
        ),
        event(
            3,
            EventKind::TaskAssigned,
            None,
            json!({ "by_id": l, "to_id": b, "task_key": "T-4" }),
        ),
        event(
            4,
            EventKind::TaskProgress,
            Some(&w1.id),
            json!({ "by_id": a, "task_key": "T-3", "percent": 40, "note": "editing auth.rs" }),
        ),
        event(
            5,
            EventKind::TaskSubmitted,
            Some(&w2.id),
            json!({ "by_id": b, "task_key": "T-5" }),
        ),
        event(
            6,
            EventKind::MessageSent,
            Some(&w2.id),
            json!({ "from_id": b, "to_id": l, "message_type": "QUESTION", "subject": "which hash?", "task_key": "T-7" }),
        ),
        event(
            7,
            EventKind::TaskReviewed,
            None,
            json!({ "reviewer_id": r, "verdict": "rejected", "owner": b, "task_key": "T-5" }),
        ),
        event(
            8,
            EventKind::PermissionRequested,
            Some(&w2.id),
            json!({ "summary": "run cargo test", "agent": "codex-tests" }),
        ),
    ];
    let approval = Approval {
        id: ApprovalId::from("apr_1"),
        project_id: ProjectId::from("prj_test"),
        agent_id: w2.id.clone(),
        task_id: None,
        kind: "command".into(),
        summary: "run `cargo test` in the workspace".into(),
        reason: Some("verify the implementation".into()),
        details: json!({ "paths": ["/work/tests"] }),
        request_ref: "req1".into(),
        status: ApprovalStatus::Pending,
        requested_at: Utc::now(),
        resolved_at: None,
    };
    Snapshot {
        project: Project {
            id: ProjectId::from("prj_test"),
            name: "MyApp".into(),
            path: "/work/myapp".into(),
            created_at: Utc::now(),
        },
        goal: Some(goal),
        plan: Some(plan),
        agents: vec![lead, w1, w2, rev],
        tasks,
        progress,
        approvals: vec![approval],
        recent_events: events,
        recent_messages: vec![],
        workspaces: vec![],
        paused: false,
        run_id: RunId::from("run_test"),
    }
}
