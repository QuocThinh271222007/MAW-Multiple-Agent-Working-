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
        let a = engine.add_agent(name, ProviderKind::Mock, role, None, PermissionProfile::Balanced, None).unwrap();
        Actor::Agent { id: a.id, role }
    };
    let leader = mk("lead", Role::Leader);
    let w1 = mk("w1", Role::Worker);
    let w2 = mk("w2", Role::Worker);
    let reviewer = mk("rev", Role::Reviewer);
    engine.create_goal("build auth", vec!["it works".into()]).unwrap();
    Fx { engine, leader, w1, w2, reviewer }
}

fn call(f: &Fx, a: &Actor, tool: &str, args: Value) -> CoreResult<Value> {
    f.engine.call_tool(a, tool, args)
}

fn make_running(f: &Fx, key: &str, worker: &Actor) {
    f.engine.assign_task(&f.leader, key, &worker.id().0).unwrap();
    f.engine.start_task(key).unwrap();
}

#[test]
fn only_leader_creates_tasks_and_identity_is_server_assigned() {
    let f = setup();
    let e = call(&f, &f.w1, "maw_task_create", json!({ "title": "x" })).unwrap_err();
    assert_eq!(e.code(), "forbidden");
    // a worker cannot act as leader by claiming it: the Actor comes from the token, never from args
    let e = call(&f, &f.w1, "maw_goal_update", json!({ "status": "completed" })).unwrap_err();
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
    let a1 = call(&f, &f.leader, "maw_task_create", json!({ "title": "A", "idempotency_key": "k" })).unwrap();
    let a2 = call(&f, &f.leader, "maw_task_create", json!({ "title": "A again", "idempotency_key": "k" })).unwrap();
    assert_eq!(a1["id"], a2["id"]);
    assert_eq!(f.engine.list_tasks().unwrap().len(), 1);
    let b = call(&f, &f.leader, "maw_task_create", json!({ "title": "B", "dependencies": ["T-1"] })).unwrap();
    assert_eq!(b["status"], "BACKLOG");
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Ready);
}

#[test]
fn dependency_cycles_and_bad_refs_are_rejected() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "B", "dependencies": ["T-1"] })).unwrap();
    let e = call(&f, &f.leader, "maw_task_update", json!({ "task": "T-1", "add_dependencies": ["T-2"] })).unwrap_err();
    assert_eq!(e.code(), "dependency_cycle");
    let e = call(&f, &f.leader, "maw_task_update", json!({ "task": "T-1", "add_dependencies": ["T-1"] })).unwrap_err();
    assert_eq!(e.code(), "dependency_cycle");
    let e = call(&f, &f.leader, "maw_task_create", json!({ "title": "C", "dependencies": ["T-99"] })).unwrap_err();
    assert_eq!(e.code(), "not_found");
}

#[test]
fn claim_is_atomic() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    assert!(call(&f, &f.w1, "maw_task_claim", json!({ "task": "T-1" })).is_ok());
    let e = call(&f, &f.w2, "maw_task_claim", json!({ "task": "T-1" })).unwrap_err();
    assert_eq!(e.code(), "conflict");
    assert_eq!(f.engine.get_task("T-1").unwrap().owner_agent_id, Some(f.w1.id()));
    // reviewers cannot claim implementation work
    call(&f, &f.leader, "maw_task_create", json!({ "title": "B" })).unwrap();
    assert_eq!(call(&f, &f.reviewer, "maw_task_claim", json!({})).unwrap_err().code(), "forbidden");
}

#[test]
fn done_requires_evidence_review_and_verification() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "Impl", "requires_review": true, "acceptance_criteria": ["works"] })).unwrap();
    make_running(&f, "T-1", &f.w1);

    // a different worker cannot submit it
    assert_eq!(call(&f, &f.w2, "maw_task_submit_result", json!({ "task": "T-1", "summary": "x" })).unwrap_err().code(), "forbidden");

    // submit with NO evidence: goes to review, but can never reach DONE
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "done!" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::InReview);
    // owner cannot review own work; leader's verify is refused while in review
    assert_eq!(call(&f, &f.w1, "maw_review_submit", json!({ "task": "T-1", "verdict": "approve", "feedback": "ok" })).unwrap_err().code(), "forbidden");
    assert_eq!(call(&f, &f.leader, "maw_task_verify", json!({ "task": "T-1", "pass": true, "notes": "n" })).unwrap_err().code(), "precondition_failed");

    // reject -> back to the owner with feedback
    call(&f, &f.reviewer, "maw_review_submit", json!({ "task": "T-1", "verdict": "reject", "feedback": "needs tests" })).unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!((t.status, t.review_status), (TaskStatus::Assigned, ReviewStatus::Rejected));
    assert!(f.engine.undelivered_for(&f.w1.id()).unwrap().iter().any(|m| m.message_type == MessageType::ReviewFeedback));
    // rejections need feedback
    f.engine.start_task("T-1").unwrap();
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "round 2", "changed_files": ["a.rs"], "evidence": [{ "kind": "test_result", "summary": "5 passed", "passed": true }] })).unwrap();
    assert_eq!(call(&f, &f.reviewer, "maw_review_submit", json!({ "task": "T-1", "verdict": "reject", "feedback": "" })).unwrap_err().code(), "invalid_input");
    call(&f, &f.reviewer, "maw_review_submit", json!({ "task": "T-1", "verdict": "approve", "feedback": "good" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Verifying);

    // only the leader can verify
    assert_eq!(call(&f, &f.w1, "maw_task_verify", json!({ "task": "T-1", "pass": true, "notes": "n" })).unwrap_err().code(), "forbidden");
    call(&f, &f.leader, "maw_task_verify", json!({ "task": "T-1", "pass": true, "notes": "ran the tests" })).unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(t.status, TaskStatus::Done);
    assert!(t.completed_at.is_some());
}

#[test]
fn leader_signoff_alone_cannot_complete_a_task_without_worker_evidence() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "Impl" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "trust me" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Verifying);
    let e = call(&f, &f.leader, "maw_task_verify", json!({ "task": "T-1", "pass": true, "notes": "looks fine" })).unwrap_err();
    assert_eq!(e.code(), "precondition_failed");
    assert_ne!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Done);
}

#[test]
fn no_file_tasks_complete_with_a_verification_note_when_declared() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "Analyze", "no_file_changes_expected": true })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "analysis", "evidence": [{ "kind": "verification", "summary": "findings published", "passed": true }] })).unwrap();
    call(&f, &f.leader, "maw_task_verify", json!({ "task": "T-1", "pass": true, "notes": "read findings" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Done);
}

#[test]
fn failing_latest_test_evidence_blocks_completion() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "Impl" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_submit_result", json!({ "task": "T-1", "summary": "s", "changed_files": ["a"], "evidence": [{ "kind": "test_result", "summary": "2 failed", "passed": false }] })).unwrap();
    let e = call(&f, &f.leader, "maw_task_verify", json!({ "task": "T-1", "pass": true, "notes": "n" })).unwrap_err();
    assert!(e.to_string().contains("failing"), "{e}");
    // failing verification sends it back to the owner
    call(&f, &f.leader, "maw_task_verify", json!({ "task": "T-1", "pass": false, "notes": "fix the failing tests" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Assigned);
}

#[test]
fn reported_progress_never_counts_as_verified() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A", "weight": 3.0 })).unwrap();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "B", "weight": 1.0 })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_progress_report", json!({ "task": "T-1", "percent": 100, "note": "all done" })).unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!(t.progress, 99, "advisory progress is capped below 100");
    let p = f.engine.progress().unwrap();
    assert_eq!(p.verified_pct, 0.0);
    assert!(p.reported_weight > 2.9);
    // scope expansion lowers the denominator share, with a recorded plan revision
    f.engine.propose_plan(&f.leader, "plan", vec![], vec![], "initial").unwrap();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "C", "weight": 4.0 })).unwrap();
    assert_eq!(f.engine.progress().unwrap().total_weight, 8.0);
}

#[test]
fn blocking_notifies_the_leader_and_frees_the_worker() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    call(&f, &f.w1, "maw_task_block", json!({ "task": "T-1", "reason": "need decision", "needs": "hash algo" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().status, TaskStatus::Blocked);
    assert_eq!(f.engine.get_agent(&f.w1.id()).unwrap().current_task_id, None);
    let msgs = f.engine.undelivered_for(&f.leader.id()).unwrap();
    assert!(msgs.iter().any(|m| m.message_type == MessageType::Blocker && m.content.contains("need decision")));
    // creating a message does not mean delivery
    assert!(msgs.iter().all(|m| m.delivered_at.is_none() && m.acknowledged_at.is_none()));
    // a non-owner worker cannot block someone else's task
    assert_eq!(call(&f, &f.w2, "maw_task_block", json!({ "task": "T-1", "reason": "x" })).unwrap_err().code(), "forbidden");
}

#[test]
fn messaging_inbox_ack_and_instruction_rights() {
    let f = setup();
    let r = call(&f, &f.w1, "maw_message_send", json!({ "to": "w2", "message_type": "QUESTION", "subject": "q", "content": "what is the interface?", "idempotency_key": "m1" })).unwrap();
    let r2 = call(&f, &f.w1, "maw_message_send", json!({ "to": "w2", "message_type": "QUESTION", "subject": "q", "content": "again", "idempotency_key": "m1" })).unwrap();
    assert_eq!(r["message_id"], r2["message_id"]);
    assert_eq!(call(&f, &f.w1, "maw_message_send", json!({ "to": "w2", "message_type": "INSTRUCTION", "subject": "s", "content": "c" })).unwrap_err().code(), "forbidden");
    let inbox = call(&f, &f.w2, "maw_message_inbox", json!({})).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    // others cannot ack it
    let id = r["message_id"].as_str().unwrap();
    assert_eq!(call(&f, &f.w1, "maw_message_ack", json!({ "message_id": id })).unwrap_err().code(), "forbidden");
    call(&f, &f.w2, "maw_message_ack", json!({ "message_id": id })).unwrap();
    assert!(call(&f, &f.w2, "maw_message_inbox", json!({})).unwrap()["messages"].as_array().unwrap().is_empty());
}

#[test]
fn handoff_moves_ownership_only_on_accept() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    make_running(&f, "T-1", &f.w1);
    let h = call(&f, &f.w1, "maw_handoff_create", json!({ "task": "T-1", "to": "w2", "summary": "you take it", "context": "see notes" })).unwrap();
    assert_eq!(f.engine.get_task("T-1").unwrap().owner_agent_id, Some(f.w1.id()));
    // only the addressee can accept
    let id = h["handoff_message_id"].as_str().unwrap();
    assert_eq!(call(&f, &f.leader, "maw_handoff_accept", json!({ "message_id": id })).unwrap_err().code(), "forbidden");
    call(&f, &f.w2, "maw_handoff_accept", json!({ "message_id": id })).unwrap();
    let t = f.engine.get_task("T-1").unwrap();
    assert_eq!((t.owner_agent_id, t.status), (Some(f.w2.id()), TaskStatus::Assigned));
}

#[test]
fn goal_cannot_complete_with_open_tasks_and_text_is_user_only() {
    let f = setup();
    call(&f, &f.leader, "maw_task_create", json!({ "title": "A" })).unwrap();
    let e = call(&f, &f.leader, "maw_goal_update", json!({ "status": "completed" })).unwrap_err();
    assert_eq!(e.code(), "precondition_failed");
    let e = call(&f, &f.leader, "maw_goal_update", json!({ "objective": "something else" })).unwrap_err();
    assert_eq!(e.code(), "forbidden");
    // criteria changes are revisions
    call(&f, &f.leader, "maw_goal_update", json!({ "success_criteria": ["a", "b"], "reason": "refined" })).unwrap();
    let hist = call(&f, &f.leader, "maw_goal_get_history", json!({})).unwrap();
    assert_eq!(hist["revisions"].as_array().unwrap().len(), 2);
    // the user may redirect the objective; history keeps both
    f.engine.update_goal(&Actor::User, GoalPatch { text: Some("new objective".into()), reason: Some("pivot".into()), ..Default::default() }).unwrap();
    assert_eq!(f.engine.goal_history().unwrap().len(), 3);
}

#[test]
fn task_creation_is_bounded() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let cfg = OrchestratorConfig { max_tasks_per_goal: 3, ..Default::default() };
    let engine = Engine::open(store, "t", "/tmp/t", cfg).unwrap();
    engine.add_agent("lead", ProviderKind::Mock, Role::Leader, None, PermissionProfile::Safe, None).unwrap();
    engine.create_goal("g", vec![]).unwrap();
    for i in 0..3 {
        engine.create_task(&Actor::User, NewTask { title: format!("t{i}"), ..Default::default() }).unwrap();
    }
    let e = engine.create_task(&Actor::User, NewTask { title: "one too many".into(), ..Default::default() }).unwrap_err();
    assert_eq!(e.code(), "limit_exceeded");
}

#[test]
fn invalid_agent_state_transitions_are_rejected() {
    let f = setup();
    let id = f.w1.id();
    assert!(f.engine.set_agent_state(&id, AgentState::Working, None).is_err(), "DISCONNECTED -> WORKING must fail");
    f.engine.set_agent_state(&id, AgentState::Connecting, None).unwrap();
    f.engine.set_agent_state(&id, AgentState::Idle, None).unwrap();
    f.engine.set_agent_state(&id, AgentState::Working, None).unwrap();
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
    for required in ["maw_goal_get", "maw_task_create", "maw_task_claim", "maw_handoff_accept", "maw_decision_record", "maw_context_search"] {
        assert!(names.contains(required), "missing {required}");
    }
}

#[test]
fn lease_expiry_blocks_instead_of_silently_retrying() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let cfg = OrchestratorConfig { lease_secs: -1, ..Default::default() };
    let engine = Engine::open(store, "t", "/tmp/t", cfg).unwrap();
    engine.add_agent("lead", ProviderKind::Mock, Role::Leader, None, PermissionProfile::Safe, None).unwrap();
    let w = engine.add_agent("w", ProviderKind::Mock, Role::Worker, None, PermissionProfile::Safe, None).unwrap();
    engine.create_goal("g", vec![]).unwrap();
    engine.create_task(&Actor::User, NewTask { title: "A".into(), assignee: Some("w".into()), ..Default::default() }).unwrap();
    engine.start_task("T-1").unwrap();
    assert_eq!(engine.sweep_leases().unwrap().len(), 1);
    let t = engine.get_task("T-1").unwrap();
    assert_eq!(t.status, TaskStatus::Blocked);
    assert!(t.blocked_reason.unwrap().contains("lease expired"));
    assert_eq!(engine.get_agent(&w.id).unwrap().current_task_id, None);
}
