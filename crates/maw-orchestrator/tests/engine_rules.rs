use maw_core::*;
use maw_orchestrator::*;
use maw_storage::Store;
use serde_json::{json, Value};
use std::sync::Arc;

struct Fx {
    engine: Arc<Engine>,
    leader: Actor,
    w1: Actor,
    w2: Actor,
    reviewer: Actor,
}

fn setup() -> Fx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Engine::open(store, "t", "/tmp/t", OrchestratorConfig::default()).unwrap();
    let mk = |name: &str, role: Role| {
        let a = engine
            .add_agent(
                name,
                ProviderKind::Mock,
                role,
                None,
                PermissionProfile::Balanced,
                None,
            )
            .unwrap();
        Actor::Agent { id: a.id, role }
    };
    let leader = mk("lead", Role::Leader);
    let w1 = mk("w1", Role::Worker);
    let w2 = mk("w2", Role::Worker);
    let reviewer = mk("rev", Role::Reviewer);
    engine
        .create_goal("build auth", vec!["it works".into()])
        .unwrap();
    Fx {
        engine,
        leader,
        w1,
        w2,
        reviewer,
    }
}

fn call(f: &Fx, a: &Actor, tool: &str, args: Value) -> CoreResult<Value> {
    f.engine.call_tool(a, tool, args)
}

fn make_running(f: &Fx, key: &str, worker: &Actor) {
    f.engine
        .assign_task(&f.leader, key, &worker.id().0)
        .unwrap();
    f.engine.start_task(key).unwrap();
}

#[test]
fn only_leader_creates_tasks_and_identity_is_server_assigned() {
    let f = setup();
    let e = call(&f, &f.w1, "maw_task_create", json!({ "title": "x" })).unwrap_err();
    assert_eq!(e.code(), "forbidden");
    // a worker cannot act as leader by claiming it: the Actor comes from the token, never from args
    let e = call(
        &f,
        &f.w1,
        "maw_goal_update",
        json!({ "status": "completed" }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "forbidden");
    assert!(call(&f, &f.leader, "maw_task_create", json!({ "title": "x" })).is_ok());
}

#[test]
fn tokens_resolve_to_the_minted_identity_only() {
    let f = setup();
    let t = f.engine.mint_token(&f.w1.id());
    assert_eq!(f.engine.authenticate(&t), Some(f.w1.clone()));
    assert_eq!(f.engine.authenticate("maw_tok_forged"), None);
    f.engine.revoke_tokens(&f.w1.id());
    assert_eq!(f.engine.authenticate(&t), None);
}

#[test]
fn creation_is_idempotent_and_deps_gate_readiness() {
    let f = setup();
    let a1 = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "A", "idempotency_key": "k" }),
    )
    .unwrap();
    let a2 = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "A again", "idempotency_key": "k" }),
    )
    .unwrap();
    assert_eq!(a1["id"], a2["id"]);
    assert_eq!(f.engine.list_tasks().unwrap().len(), 1);
    let b = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "B", "dependencies": ["T-1"] }),
    )
    .unwrap();
    assert_eq!(b["status"], "BACKLOG");
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Ready);
}

#[test]
fn dependency_cycles_and_bad_refs_are_rejected() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "B", "dependencies": ["T-1"] }),
    )
    .unwrap();
    let e = call(
        &f,
        &f.leader,
        "maw_task_update",
        json!({ "task": "T-1", "add_dependencies": ["T-2"] }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "dependency_cycle");
    let e = call(
        &f,
        &f.leader,
        "maw_task_update",
        json!({ "task": "T-1", "add_dependencies": ["T-1"] }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "dependency_cycle");
    let e = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "C", "dependencies": ["T-99"] }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "not_found");
}

#[test]
fn claim_is_atomic() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    assert!(call(&f, &f.w1, "maw_task_claim", json!({ "task": "T-1" })).is_ok());
    let e = call(&f, &f.w2, "maw_task_claim", json!({ "task": "T-1" })).unwrap_err();
    assert_eq!(e.code(), "conflict");
    assert_eq!(
        f.engine.get_task("T-1").unwrap().owner_agent_id,
        Some(f.w1.id())
    );
    // reviewers cannot claim implementation work
    call(&f, &f.leader, "maw_task_create", json!({ "title": "B" })).unwrap();
    assert_eq!(
        call(&f, &f.reviewer, "maw_task_claim", json!({}))
            .unwrap_err()
            .code(),
        "forbidden"
    );
}

#[test]
fn done_requires_evidence_review_and_verification() {
    let f = setup();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Impl", "requires_review": true, "acceptance_criteria": ["works"] }),
    )
    .unwrap();
    make_running(&f, "T-1", &f.w1);

    // a different worker cannot submit it
    assert_eq!(
        call(
            &f,
            &f.w2,
            "maw_task_submit_result",
            json!({ "task": "T-1", "summary": "x" })
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );

    // submit with NO evidence: goes to review, but can never reach DONE
    call(
        &f,
        &f.w1,
        "maw_task_submit_result",
        json!({ "task": "T-1", "summary": "done!" }),
    )
    .unwrap();
    assert_eq!(
        f.engine.get_task("T-1").unwrap().status,
        TaskStatus::InReview
    );
    // owner cannot review own work; leader's verify is refused while in review
    assert_eq!(
        call(
            &f,
            &f.w1,
            "maw_review_submit",
            json!({ "task": "T-1", "verdict": "approve", "feedback": "ok" })
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );
    assert_eq!(
        call(
            &f,
            &f.leader,
            "maw_task_verify",
            json!({ "task": "T-1", "pass": true, "notes": "n" })
        )
        .unwrap_err()
        .code(),
        "precondition_failed"
    );

    // reject -> back to the owner with feedback
    call(
        &f,
        &f.reviewer,
        "maw_review_submit",
        json!({ "task": "T-1", "verdict": "reject", "feedback": "needs tests" }),
    )
    .unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(
        (t.status, t.review_status),
        (TaskStatus::Assigned, ReviewStatus::Rejected)
    );
    assert!(f
        .engine
        .undelivered_for(&f.w1.id())
        .unwrap()
        .iter()
        .any(|m| m.message_type == MessageType::ReviewFeedback));
    // rejections need feedback
    f.engine.start_task("T-1").unwrap();
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "round 2", "changed_files": ["a.rs"], "evidence": [{ "kind": "test_result", "summary": "5 passed", "passed": true }] })).unwrap();
    assert_eq!(
        call(
            &f,
            &f.reviewer,
            "maw_review_submit",
            json!({ "task": "T-1", "verdict": "reject", "feedback": "" })
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    call(
        &f,
        &f.reviewer,
        "maw_review_submit",
        json!({ "task": "T-1", "verdict": "approve", "feedback": "good" }),
    )
    .unwrap();
    assert_eq!(
        f.engine.get_task("T-1").unwrap().status,
        TaskStatus::Verifying
    );

    // only the leader can verify
    assert_eq!(
        call(
            &f,
            &f.w1,
            "maw_task_verify",
            json!({ "task": "T-1", "pass": true, "notes": "n" })
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );
    call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "ran the tests" }),
    )
    .unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(t.status, TaskStatus::Done);
    assert!(t.completed_at.is_some());
}

#[test]
fn leader_signoff_alone_cannot_complete_a_task_without_worker_evidence() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "Impl" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(
        &f,
        &f.w1,
        "maw_task_submit_result",
        json!({ "task": "T-1", "summary": "trust me" }),
    )
    .unwrap();
    assert_eq!(
        f.engine.get_task("T-1").unwrap().status,
        TaskStatus::Verifying
    );
    let e = call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "looks fine" }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "precondition_failed");
    assert_ne!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Done);
}

#[test]
fn no_file_tasks_complete_with_a_verification_note_when_declared() {
    let f = setup();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Analyze", "no_file_changes_expected": true }),
    )
    .unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "analysis", "evidence": [{ "kind": "verification", "summary": "findings published", "passed": true }] })).unwrap();
    call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "read findings" }),
    )
    .unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Done);
}

#[test]
fn failing_latest_test_evidence_blocks_completion() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "Impl" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "s", "changed_files": ["a"], "evidence": [{ "kind": "test_result", "summary": "2 failed", "passed": false }] })).unwrap();
    let e = call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "n" }),
    )
    .unwrap_err();
    assert!(e.to_string().contains("failing"), "{e}");
    // failing verification sends it back to the owner
    call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": false, "notes": "fix the failing tests" }),
    )
    .unwrap();
    assert_eq!(
        f.engine.get_task("T-1").unwrap().status,
        TaskStatus::Assigned
    );
}

#[test]
fn reported_progress_never_counts_as_verified() {
    let f = setup();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "A", "weight": 3.0 }),
    )
    .unwrap();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "B", "weight": 1.0 }),
    )
    .unwrap();
    make_running(&f, "T-1", &f.w1);
    call(
        &f,
        &f.w1,
        "maw_progress_report",
        json!({ "task": "T-1", "percent": 100, "note": "all done" }),
    )
    .unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(t.progress, 99, "advisory progress is capped below 100");
    let p = f.engine.progress().unwrap();
    assert_eq!(p.verified_pct, 0.0);
    assert!(p.reported_weight > 2.9);
    // scope expansion lowers the denominator share, with a recorded plan revision
    f.engine
        .propose_plan(&f.leader, "plan", vec![], vec![], "initial")
        .unwrap();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "C", "weight": 4.0 }),
    )
    .unwrap();
    assert_eq!(f.engine.progress().unwrap().total_weight, 8.0);
}

#[test]
fn blocking_notifies_the_leader_and_frees_the_worker() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(
        &f,
        &f.w1,
        "maw_task_block",
        json!({ "task": "T-1", "reason": "need decision", "needs": "hash algo" }),
    )
    .unwrap();
    assert_eq!(
        f.engine.get_task("T-1").unwrap().status,
        TaskStatus::Blocked
    );
    assert_eq!(
        f.engine.get_agent(&f.w1.id()).unwrap().current_task_id,
        None
    );
    let msgs = f.engine.undelivered_for(&f.leader.id()).unwrap();
    assert!(msgs
        .iter()
        .any(|m| m.message_type == MessageType::Blocker && m.content.contains("need decision")));
    // creating a message does not mean delivery
    assert!(msgs
        .iter()
        .all(|m| m.delivered_at.is_none() && m.acknowledged_at.is_none()));
    // a non-owner worker cannot block someone else's task
    assert_eq!(
        call(
            &f,
            &f.w2,
            "maw_task_block",
            json!({ "task": "T-1", "reason": "x" })
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );
}

#[test]
fn messaging_inbox_ack_and_instruction_rights() {
    let f = setup();
    let r = call(&f, &f.w1, "maw_message_send", json!({ "to": "w2", "message_type": "QUESTION", "subject": "q", "content": "what is the interface?", "idempotency_key": "m1" })).unwrap();
    let r2 = call(&f, &f.w1, "maw_message_send", json!({ "to": "w2", "message_type": "QUESTION", "subject": "q", "content": "again", "idempotency_key": "m1" })).unwrap();
    assert_eq!(r["message_id"], r2["message_id"]);
    assert_eq!(
        call(
            &f,
            &f.w1,
            "maw_message_send",
            json!({ "to": "w2", "message_type": "INSTRUCTION", "subject": "s", "content": "c" })
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );
    let inbox = call(&f, &f.w2, "maw_message_inbox", json!({})).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    // others cannot ack it
    let id = r["message_id"].as_str().unwrap();
    assert_eq!(
        call(&f, &f.w1, "maw_message_ack", json!({ "message_id": id }))
            .unwrap_err()
            .code(),
        "forbidden"
    );
    call(&f, &f.w2, "maw_message_ack", json!({ "message_id": id })).unwrap();
    assert!(
        call(&f, &f.w2, "maw_message_inbox", json!({})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn handoff_moves_ownership_only_on_accept() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    let h = call(
        &f,
        &f.w1,
        "maw_handoff_create",
        json!({ "task": "T-1", "to": "w2", "summary": "you take it", "context": "see notes" }),
    )
    .unwrap();
    assert_eq!(
        f.engine.get_task("T-1").unwrap().owner_agent_id,
        Some(f.w1.id())
    );
    // only the addressee can accept
    let id = h["handoff_message_id"].as_str().unwrap();
    assert_eq!(
        call(
            &f,
            &f.leader,
            "maw_handoff_accept",
            json!({ "message_id": id })
        )
        .unwrap_err()
        .code(),
        "forbidden"
    );
    call(&f, &f.w2, "maw_handoff_accept", json!({ "message_id": id })).unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(
        (t.owner_agent_id, t.status),
        (Some(f.w2.id()), TaskStatus::Assigned)
    );
}

#[test]
fn an_old_failure_is_not_shown_on_an_agent_that_works_again() {
    let f = setup();
    let Actor::Agent { id, .. } = &f.w1 else {
        panic!("agent actor")
    };
    let get = || f.engine.get_agent(id).unwrap();
    f.engine
        .set_agent_state(id, AgentState::Connecting, None)
        .unwrap();
    f.engine
        .set_agent_state(
            id,
            AgentState::Failed,
            Some("no Ollama server is reachable".into()),
        )
        .unwrap();
    assert!(
        get().last_error.is_some(),
        "a failure is shown while it lasts"
    );
    // reconnecting starts a new attempt: the old message goes
    f.engine
        .set_agent_state(id, AgentState::Connecting, None)
        .unwrap();
    assert_eq!(get().last_error, None);
    f.engine
        .set_agent_state(id, AgentState::Idle, None)
        .unwrap();
    assert_eq!(get().last_error, None, "a healthy agent shows no error");
    // a new failure sets a new message, and going down keeps the cause visible
    f.engine
        .set_agent_state(id, AgentState::Failed, Some("boom".into()))
        .unwrap();
    f.engine
        .set_agent_state(id, AgentState::Disconnected, None)
        .unwrap();
    assert_eq!(get().last_error.as_deref(), Some("boom"));
}

#[test]
fn the_leader_can_create_a_goal_only_when_none_is_active() {
    let f = setup(); // the user's goal "build auth" is active
                     // a Leader can never replace an objective the user set
    let e = call(
        &f,
        &f.leader,
        "maw_goal_create",
        json!({ "objective": "something else" }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "conflict");
    assert_eq!(f.engine.current_goal().unwrap().unwrap().text, "build auth");
    // workers and reviewers cannot create goals at all
    for who in [&f.w1, &f.reviewer] {
        let e = call(&f, who, "maw_goal_create", json!({ "objective": "x" })).unwrap_err();
        assert_eq!(e.code(), "forbidden");
    }
    // once the user has cancelled it, the Leader may set the next one
    f.engine
        .update_goal(
            &Actor::User,
            GoalPatch {
                status: Some(GoalStatus::Canceled),
                ..Default::default()
            },
        )
        .unwrap();
    let r = call(
        &f,
        &f.leader,
        "maw_goal_create",
        json!({ "objective": "ship the login page", "success_criteria": ["login works"] }),
    )
    .unwrap();
    assert_eq!(r["goal"]["text"], "ship the login page");
    let hist = f.engine.goal_history().unwrap();
    assert_eq!(hist.len(), 1);
    assert!(hist[0].reason.contains("Leader") && hist[0].author != Actor::User.id());
    // and the Leader still cannot rewrite the objective it created
    let e = call(
        &f,
        &f.leader,
        "maw_goal_update",
        json!({ "objective": "other" }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "forbidden");
}

#[test]
fn goal_cannot_complete_with_open_tasks_and_text_is_user_only() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    let e = call(
        &f,
        &f.leader,
        "maw_goal_update",
        json!({ "status": "completed" }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "precondition_failed");
    let e = call(
        &f,
        &f.leader,
        "maw_goal_update",
        json!({ "objective": "something else" }),
    )
    .unwrap_err();
    assert_eq!(e.code(), "forbidden");
    // criteria changes are revisions
    call(
        &f,
        &f.leader,
        "maw_goal_update",
        json!({ "success_criteria": ["a", "b"], "reason": "refined" }),
    )
    .unwrap();
    let hist = call(&f, &f.leader, "maw_goal_get_history", json!({})).unwrap();
    assert_eq!(hist["revisions"].as_array().unwrap().len(), 2);
    // the user may redirect the objective; history keeps both
    f.engine
        .update_goal(
            &Actor::User,
            GoalPatch {
                text: Some("new objective".into()),
                reason: Some("pivot".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(f.engine.goal_history().unwrap().len(), 3);
}

#[test]
fn task_creation_is_bounded() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let cfg = OrchestratorConfig {
        max_tasks_per_goal: 3,
        ..Default::default()
    };
    let engine = Engine::open(store, "t", "/tmp/t", cfg).unwrap();
    engine
        .add_agent(
            "lead",
            ProviderKind::Mock,
            Role::Leader,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
    engine.create_goal("g", vec![]).unwrap();
    for i in 0..3 {
        engine
            .create_task(
                &Actor::User,
                NewTask {
                    title: format!("t{i}"),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let e = engine
        .create_task(
            &Actor::User,
            NewTask {
                title: "one too many".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code(), "limit_exceeded");
}

#[test]
fn invalid_agent_state_transitions_are_rejected() {
    let f = setup();
    let id = f.w1.id();
    assert!(
        f.engine
            .set_agent_state(&id, AgentState::Working, None)
            .is_err(),
        "DISCONNECTED -> WORKING must fail"
    );
    f.engine
        .set_agent_state(&id, AgentState::Connecting, None)
        .unwrap();
    f.engine
        .set_agent_state(&id, AgentState::Idle, None)
        .unwrap();
    f.engine
        .set_agent_state(&id, AgentState::Working, None)
        .unwrap();
}

#[test]
fn every_tool_has_a_schema_and_a_unique_name() {
    let specs = tool_specs();
    let mut names = std::collections::HashSet::new();
    for s in &specs {
        assert!(names.insert(s.name), "duplicate {}", s.name);
        let schema = (s.schema)();
        assert_eq!(schema["type"], "object", "{}", s.name);
        assert!(!s.description.is_empty());
    }
    assert!(specs.len() >= 35);
    for required in [
        "maw_goal_get",
        "maw_task_create",
        "maw_task_claim",
        "maw_handoff_accept",
        "maw_decision_record",
        "maw_context_search",
    ] {
        assert!(names.contains(required), "missing {required}");
    }
}

#[test]
fn lease_expiry_blocks_instead_of_silently_retrying() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let cfg = OrchestratorConfig {
        lease_secs: -1,
        ..Default::default()
    };
    let engine = Engine::open(store, "t", "/tmp/t", cfg).unwrap();
    engine
        .add_agent(
            "lead",
            ProviderKind::Mock,
            Role::Leader,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
    let w = engine
        .add_agent(
            "w",
            ProviderKind::Mock,
            Role::Worker,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
    engine.create_goal("g", vec![]).unwrap();
    engine
        .create_task(
            &Actor::User,
            NewTask {
                title: "A".into(),
                assignee: Some("w".into()),
                ..Default::default()
            },
        )
        .unwrap();
    engine.start_task("T-1").unwrap();
    assert_eq!(engine.sweep_leases().unwrap().len(), 1);
    let t = engine.get_task("T-1").unwrap();
    assert_eq!(t.status, TaskStatus::Blocked);
    assert!(t.blocked_reason.unwrap().contains("lease expired"));
    assert_eq!(engine.get_agent(&w.id).unwrap().current_task_id, None);
}

#[test]
fn orchestrator_initiated_blocks_still_notify_the_leader() {
    // Regression: a real run parked a task after the worker ended its turn twice without a result,
    // but the Leader was never told, so nothing progressed.
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    f.engine
        .block_task(
            &Actor::System,
            "T-1",
            "agent ended its turn twice without a result",
            Some("Leader decision"),
        )
        .unwrap();
    let msgs = f.engine.undelivered_for(&f.leader.id()).unwrap();
    assert!(
        msgs.iter().any(|m| m.message_type == MessageType::Blocker
            && m.content.contains("ended its turn twice")),
        "{msgs:?}"
    );
    // the Leader blocking a task itself does not message itself
    call(&f, &f.leader, "maw_task_create", json!({ "title": "B" })).unwrap();
    f.engine
        .assign_task(&f.leader, "T-2", &f.w2.id().0)
        .unwrap();
    f.engine
        .block_task(&f.leader, "T-2", "parked by leader", None)
        .unwrap();
    assert!(f
        .engine
        .undelivered_for(&f.leader.id())
        .unwrap()
        .iter()
        .all(|m| !m.content.contains("parked by leader")));
}

#[test]
fn model_effort_and_permission_are_validated_and_persisted() {
    let f = setup();
    let id = f.w1.id();
    // model: any provider-accepted id, `default` clears it, garbage is rejected
    assert_eq!(
        f.engine
            .set_agent_model(&id, Some("gpt-6.1-sol".into()))
            .unwrap()
            .model
            .as_deref(),
        Some("gpt-6.1-sol")
    );
    assert_eq!(
        f.engine
            .set_agent_model(&id, Some("claude-sonnet-5-5".into()))
            .unwrap()
            .model
            .as_deref(),
        Some("claude-sonnet-5-5")
    );
    assert!(f
        .engine
        .set_agent_model(&id, Some("x; rm -rf /".into()))
        .is_err());
    assert_eq!(
        f.engine
            .set_agent_model(&id, Some("default".into()))
            .unwrap()
            .model,
        None
    );
    // effort
    assert_eq!(
        f.engine
            .set_agent_effort(&id, " HIGH ")
            .unwrap()
            .effort
            .as_deref(),
        Some("high")
    );
    assert!(f.engine.set_agent_effort(&id, "high; ls").is_err());
    assert_eq!(
        f.engine.get_agent(&id).unwrap().effort.as_deref(),
        Some("high"),
        "a rejected value changes nothing"
    );
    assert_eq!(
        f.engine.set_agent_effort(&id, "default").unwrap().effort,
        None
    );
    // permission
    assert_eq!(
        f.engine
            .set_agent_permission(&id, PermissionProfile::Autonomous)
            .unwrap()
            .permission,
        PermissionProfile::Autonomous
    );
    // every change is an event
    let ev = f
        .engine
        .read(|db| db.events_since(f.engine.project_id(), 0, 10_000))
        .unwrap();
    assert!(ev
        .iter()
        .any(|e| e.payload["change"] == "effort" && e.payload["effort"] == "high"));
    assert!(ev.iter().any(|e| e.payload["change"] == "model"));
    assert!(ev.iter().any(|e| e.payload["change"] == "permission"));
}

#[test]
fn effort_survives_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maw.db");
    let id;
    {
        let store = Arc::new(Store::open(&path).unwrap());
        let engine =
            Engine::open_utility(store, "t", "/tmp/t", OrchestratorConfig::default()).unwrap();
        let a = engine
            .add_agent(
                "w",
                ProviderKind::Codex,
                Role::Worker,
                Some("gpt-6-luna".into()),
                PermissionProfile::Safe,
                None,
            )
            .unwrap();
        engine.set_agent_effort(&a.id, "xhigh").unwrap();
        id = a.id;
    }
    let store = Arc::new(Store::open(&path).unwrap());
    let engine = Engine::open_utility(store, "t", "/tmp/t", OrchestratorConfig::default()).unwrap();
    let a = engine.get_agent(&id).unwrap();
    assert_eq!(
        (a.model.as_deref(), a.effort.as_deref()),
        (Some("gpt-6-luna"), Some("xhigh"))
    );
}

// ---------------------------------------------------------------- removing agents

#[test]
fn removing_an_agent_releases_idle_work_but_protects_work_in_flight() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "B" })).unwrap();
    f.engine
        .assign_task(&f.leader, "T-2", &f.w1.id().0)
        .unwrap(); // ASSIGNED, not started
    make_running(&f, "T-1", &f.w1); // RUNNING

    // unfinished work blocks a plain removal and changes nothing
    let e = f.engine.remove_agent(&f.w1.id(), false).unwrap_err();
    assert_eq!(e.code(), "conflict");
    assert!(e.to_string().contains("T-1"), "{e}");
    assert!(f.engine.get_agent(&f.w1.id()).is_ok());
    assert_eq!(
        f.engine.get_task("T-1").unwrap().status,
        TaskStatus::Running
    );

    // with force both tasks go back to the pool
    let r = f.engine.remove_agent(&f.w1.id(), true).unwrap();
    assert_eq!(r.name, "w1");
    let mut released = r.released_tasks.clone();
    released.sort();
    assert_eq!(released, vec!["T-1", "T-2"]);
    for k in ["T-1", "T-2"] {
        let t = f.engine.get_task(k).unwrap();
        assert_eq!(
            (t.status, t.owner_agent_id),
            (TaskStatus::Ready, None),
            "{k}"
        );
    }
    assert!(f.engine.get_agent(&f.w1.id()).is_err(), "the agent is gone");
    // the name can be reused
    assert!(f.engine.resolve_agent("w1").is_err());
    f.engine
        .add_agent(
            "w1",
            ProviderKind::Mock,
            Role::Worker,
            None,
            PermissionProfile::Balanced,
            None,
        )
        .unwrap();
    let ev = f
        .engine
        .read(|db| db.events_since(f.engine.project_id(), 0, 10_000))
        .unwrap();
    let removed = ev
        .iter()
        .find(|e| e.event_type == EventKind::AgentRemoved)
        .expect("agent.removed event");
    assert_eq!(removed.payload["name"], "w1");
    assert_eq!(removed.payload["forced"], true);
}

#[test]
fn removal_keeps_history_and_closes_undelivered_mail() {
    let f = setup();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Done thing", "no_file_changes_expected": true }),
    )
    .unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "s", "evidence": [{ "kind": "verification", "summary": "ok", "passed": true }] })).unwrap();
    call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "checked" }),
    )
    .unwrap();
    // mail the agent never read
    call(
        &f,
        &f.leader,
        "maw_message_send",
        json!({ "to": "w1", "message_type": "STATUS", "subject": "fyi", "content": "hello" }),
    )
    .unwrap();
    assert_eq!(f.engine.undelivered_for(&f.w1.id()).unwrap().len(), 1);

    // a finished task is not "unfinished work": plain removal works
    f.engine.remove_agent(&f.w1.id(), false).unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(t.status, TaskStatus::Done);
    assert_eq!(
        t.owner_agent_id,
        Some(f.w1.id()),
        "history keeps who did it"
    );
    assert!(
        f.engine.undelivered_for(&f.w1.id()).unwrap().is_empty(),
        "no mail stays pending for a deleted agent"
    );
    assert!(
        !f.engine
            .agents_with_pending_messages()
            .unwrap()
            .contains(&f.w1.id()),
        "the scheduler never looks for a deleted agent's mail"
    );
}

#[test]
fn removing_the_leader_of_an_active_goal_needs_force() {
    let f = setup();
    let e = f.engine.remove_agent(&f.leader.id(), false).unwrap_err();
    assert_eq!(e.code(), "conflict");
    assert!(e.to_string().contains("Leader"), "{e}");
    f.engine.remove_agent(&f.leader.id(), true).unwrap();
    assert!(f.engine.leader().unwrap().is_none());
    assert_eq!(
        f.engine.current_goal().unwrap().unwrap().leader_agent_id,
        None
    );
    assert_eq!(
        f.engine
            .remove_agent(&f.leader.id(), true)
            .unwrap_err()
            .code(),
        "not_found"
    );
}

// ---------------------------------------------------------------- token accounting

fn usage(i: u64, o: u64, cost: Option<f64>) -> UsageMetrics {
    UsageMetrics {
        input_tokens: Some(i),
        output_tokens: Some(o),
        total_tokens: Some(i + o),
        cost_usd: cost,
        reported_by_provider: true,
    }
}

#[test]
fn usage_reports_become_increments_attributed_to_the_current_task() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    let w = f.w1.id();
    f.engine.record_usage(&w, usage(100, 20, None)).unwrap(); // before any task
    make_running(&f, "T-1", &f.w1);
    f.engine
        .record_usage(&w, usage(300, 60, Some(0.01)))
        .unwrap(); // cumulative: +200 in, +40 out
    f.engine
        .record_usage(&w, usage(350, 70, Some(0.015)))
        .unwrap(); // +50 in, +10 out
    f.engine.record_usage(&w, usage(10, 5, None)).unwrap(); // lower than before: the session restarted, count in full

    let ev = f
        .engine
        .read(|db| db.events_of_kind(f.engine.project_id(), EventKind::AgentUsage, 100))
        .unwrap();
    let deltas: Vec<u64> = ev
        .iter()
        .map(|e| e.payload["delta"]["total"].as_u64().unwrap())
        .collect();
    assert_eq!(deltas, vec![120, 240, 60, 15]);
    let t1 = f.engine.get_task("T-1").unwrap();
    assert!(
        ev[0].task_id.is_none(),
        "reports before any task are unattributed"
    );
    assert_eq!(ev[1].task_id.as_ref(), Some(&t1.id));
    assert!((ev[2].payload["delta"]["cost_usd"].as_f64().unwrap() - 0.005).abs() < 1e-9);
    // the stored agent usage is the latest cumulative report, flagged as provider-reported
    let a = f.engine.get_agent(&w).unwrap();
    assert_eq!(
        (a.usage.total_tokens, a.usage.reported_by_provider),
        (Some(15), true)
    );
}

#[test]
fn asking_again_for_an_open_task_returns_it_instead_of_a_twin() {
    let f = setup();
    let first = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Create hello.txt", "assignee": "w1" }),
    )
    .unwrap();
    let key = first["task"].as_str().unwrap().to_string();
    assert!(first.get("already_existed").is_none());

    // the same request again (a small model repeating itself), also with other letter case
    let again = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "  create HELLO.txt ", "assignee": "w1" }),
    )
    .unwrap();
    assert_eq!(again["task"], key.as_str());
    assert_eq!(again["already_existed"], true);
    assert_eq!(
        f.engine.list_tasks().unwrap().len(),
        1,
        "no twin was created"
    );

    // another worker, or a task that is finished or canceled, is a different matter
    let other = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Create hello.txt", "assignee": "w2" }),
    )
    .unwrap();
    assert_ne!(other["task"], key.as_str());
    call(
        &f,
        &f.leader,
        "maw_task_update",
        json!({ "task": key, "cancel": true }),
    )
    .unwrap();
    let after_cancel = call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Create hello.txt", "assignee": "w1" }),
    )
    .unwrap();
    assert!(after_cancel.get("already_existed").is_none());
    assert_eq!(f.engine.list_tasks().unwrap().len(), 3);
}

#[test]
fn a_worker_who_names_its_old_task_is_told_which_one_it_is_running() {
    let f = setup();
    for t in ["First", "Second"] {
        call(
            &f,
            &f.leader,
            "maw_task_create",
            json!({ "title": t, "assignee": "w1" }),
        )
        .unwrap();
    }
    f.engine
        .assign_task(&f.leader, "T-1", &f.w1.id().0)
        .unwrap();
    f.engine.start_task("T-1").unwrap();
    // T-2 is w1's but not running: submitting it names T-1 as the running one
    let e = call(
        &f,
        &f.w1,
        "maw_task_submit_result",
        json!({ "task": "T-2", "summary": "s" }),
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("Your RUNNING task is T-1") && e.contains("task=\"T-1\""),
        "{e}"
    );
}

#[test]
fn files_maw_saw_change_become_evidence_when_the_worker_forgot_to_list_them() {
    let f = setup();
    call(
        &f,
        &f.leader,
        "maw_task_create",
        json!({ "title": "Make a.txt", "assignee": "w1" }),
    )
    .unwrap();
    make_running(&f, "T-1", &f.w1);
    // a summary only: no changed_files, no evidence
    call(
        &f,
        &f.w1,
        "maw_task_submit_result",
        json!({ "task": "T-1", "summary": "made a.txt" }),
    )
    .unwrap();
    // the worker's word alone is not evidence of a file change
    let e = call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "ok" }),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("file_changes"), "{e}");
    // nothing observed, nothing recorded
    assert!(!f.engine.record_observed_changes("T-1", &[]).unwrap());
    // what MAW saw in the worktree is recorded once, as the orchestrator's observation
    assert!(f
        .engine
        .record_observed_changes("T-1", &["a.txt".to_string()])
        .unwrap());
    assert!(
        !f.engine
            .record_observed_changes("T-1", &["a.txt".to_string()])
            .unwrap(),
        "only once"
    );
    let t = f.engine.get_task("T-1").unwrap();
    let ev = t
        .evidence
        .iter()
        .find(|e| e.kind == EvidenceKind::FileChanges)
        .unwrap();
    assert_eq!(ev.recorded_by.as_str(), "orchestrator");
    assert!(ev.summary.contains("a.txt"));
    call(
        &f,
        &f.leader,
        "maw_task_verify",
        json!({ "task": "T-1", "pass": true, "notes": "ok" }),
    )
    .unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Done);
}
