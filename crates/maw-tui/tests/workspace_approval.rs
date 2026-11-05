//! The Leader's working-folder proposal in the Approval Center: the user has to be able to read the
//! whole path before saying yes.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maw_core::*;
use maw_tui::app::{handle_key, Action, View};
use maw_tui::testutil::*;
use maw_tui::{App, Theme};

const LONG: &str =
    "/mnt/c/Users/Someone/Documents/projects/a-folder-with-a-rather-long-name/radix-sort-demo";

fn app_with_proposal() -> App {
    let mut a = App::new(Theme::new(false));
    let mut snap = sample_snapshot();
    let leader = snap
        .agents
        .iter()
        .find(|x| x.role == Role::Leader)
        .unwrap()
        .id
        .clone();
    snap.approvals.clear();
    snap.approvals.push(Approval {
        id: ApprovalId::new(),
        project_id: snap.project.id.clone(),
        agent_id: leader,
        task_id: None,
        kind: "workspace".into(),
        summary: format!("Leader proposes the project's working folder: {LONG}"),
        reason: Some("the current folder is a home folder with hundreds of unrelated files, so tasks would wander".into()),
        details: serde_json::json!({ "path": LONG, "previous": "/mnt/c/Users/Someone" }),
        request_ref: "maw:workspace".into(),
        status: ApprovalStatus::Pending,
        requested_at: Utc::now(),
        resolved_at: None,
    });
    a.apply_snapshot(snap);
    a.now = Utc::now();
    a.size = (100, 30);
    a.view = View::Approvals;
    a
}

#[test]
fn the_whole_proposed_folder_the_current_one_and_the_reason_are_readable() {
    let a = app_with_proposal();
    let t = render_text(&a, 100, 30);
    assert!(t.contains("[workspace]"), "{t}");
    // the path is wrapped, not cut: every part of it is on screen
    for part in [
        "/mnt/c/Users/Someone/Documents/projects",
        "a-folder-with-a-rather-long-name",
        "radix-sort-demo",
    ] {
        assert!(t.contains(part), "{part} must be readable\n{t}");
    }
    assert!(t.contains("folder:") && t.contains("now:"), "{t}");
    assert!(
        t.contains("hundreds of unrelated files") && t.contains("wander"),
        "the reason is complete\n{t}"
    );
    assert!(
        t.contains("y approves") && t.contains("restarts") && t.contains("declines"),
        "{t}"
    );
}

#[test]
fn y_approves_and_n_declines_it_like_any_request() {
    let mut a = app_with_proposal();
    let acts = handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    assert!(
        matches!(
            acts.as_slice(),
            [Action::ResolveApproval {
                decision: maw_core::provider::ApprovalDecision::Approve,
                ..
            }]
        ),
        "{acts:?}"
    );
    let acts = handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
    );
    assert!(matches!(
        acts.as_slice(),
        [Action::ResolveApproval {
            decision: maw_core::provider::ApprovalDecision::Deny,
            ..
        }]
    ));
}

#[test]
fn it_draws_at_any_size() {
    let a = app_with_proposal();
    for (w, h) in [(20, 5), (40, 12), (60, 14), (100, 30), (200, 60)] {
        let _ = render_text(&a, w, h);
    }
}
