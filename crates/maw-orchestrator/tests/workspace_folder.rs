//! The Leader's tool for choosing the project's working folder: it proposes, the user approves, the
//! agents move. Nothing about it is automatic and nothing about it can be done by a Worker.

use maw_core::provider::ApprovalDecision;
use maw_core::*;
use maw_orchestrator::mock::{MockProvider, MockScript};
use maw_orchestrator::workspace as ws;
use maw_orchestrator::*;
use maw_storage::Store;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

struct Fx {
    engine: Arc<Engine>,
    sup: Arc<Supervisor>,
    store: Arc<Store>,
    leader: Actor,
    worker: Actor,
    /// The folder the project was started in (a git repository with one commit).
    project: tempfile::TempDir,
    /// The one place the Leader may propose folders in.
    roots: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

async fn setup() -> Fx {
    let project = tempfile::tempdir().unwrap();
    git(project.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(project.path().join("README.md"), "x").unwrap();
    git(project.path(), &["add", "-A"]);
    git(
        project.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let roots = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Engine::open(
        store.clone(),
        "p",
        &project.path().to_string_lossy(),
        OrchestratorConfig::default(),
    )
    .unwrap();
    engine.set_workspace_roots(vec![roots.path().to_path_buf()]);
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
    let worker = mk("w1", Role::Worker);
    engine.create_goal("ship", vec![]).unwrap();
    let sup = Supervisor::new(engine.clone());
    sup.register_provider(MockProvider::new(engine.clone(), MockScript::default()));
    sup.connect_all().await;
    Fx {
        engine,
        sup,
        store,
        leader,
        worker,
        project,
        roots,
    }
}

fn propose(f: &Fx, who: &Actor, path: &str) -> CoreResult<Value> {
    f.engine.call_tool(
        who,
        "maw_workspace_propose",
        json!({ "path": path, "reason": "a dedicated folder" }),
    )
}

fn target(f: &Fx, name: &str) -> PathBuf {
    f.roots.path().join(name)
}

fn pending_id(f: &Fx) -> ApprovalId {
    f.engine
        .pending_workspace_proposal()
        .unwrap()
        .expect("a proposal is waiting")
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proposal_changes_nothing_until_the_user_approves_it() {
    let f = setup().await;
    let dest = target(&f, "radix");
    let r = propose(&f, &f.leader, &dest.to_string_lossy()).unwrap();
    assert_eq!(r["status"], "waiting_for_the_user");
    assert_eq!(
        f.engine.workspace_root(),
        f.project.path(),
        "still the project's own folder"
    );
    assert!(!dest.exists(), "not even created yet");
    let ap = f.engine.pending_workspace_proposal().unwrap().unwrap();
    assert_eq!(ap.kind, "workspace");
    assert!(ap.summary.contains("radix") && ap.reason.as_deref() == Some("a dedicated folder"));
    assert_eq!(ap.details["path"], json!(dest));
    // it shows in the Approval Center like any other request
    assert_eq!(f.engine.pending_approvals().unwrap().len(), 1);
    // and survives the agent's session going away (restart, reconnect)
    f.engine.expire_approvals_for(&f.leader.agent_id().unwrap());
    assert_eq!(
        f.engine.pending_approvals().unwrap().len(),
        1,
        "a workspace proposal is not tied to a session"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approving_creates_the_folder_moves_the_agents_and_tells_the_leader() {
    let f = setup().await;
    let dest = target(&f, "radix");
    propose(&f, &f.leader, &dest.to_string_lossy()).unwrap();
    let id = pending_id(&f);
    f.sup
        .resolve_approval(&id, ApprovalDecision::Approve)
        .await
        .unwrap();
    // the folder exists, is a git repo with a commit, and is now the working folder
    assert!(dest.is_dir() && ws::is_git_repo(&dest) && ws::has_commits(&dest));
    assert_eq!(f.engine.workspace_root(), dest);
    assert_eq!(f.engine.work_dir(), Some(dest.clone()));
    let ap = f.engine.read(|db| db.get_approval(&id)).unwrap().unwrap();
    assert_eq!(ap.status, ApprovalStatus::Approved);
    // the connected worker was restarted and now has its worktree in the new folder
    let w = f.engine.get_agent(&f.worker.agent_id().unwrap()).unwrap();
    let wpath = w.workspace.clone().unwrap_or_default();
    assert!(
        wpath.starts_with(&dest.to_string_lossy().to_string()),
        "the worker works under the new folder, not {wpath}"
    );
    assert_eq!(w.state, AgentState::Idle);
    // the Leader is told, in words it can act on
    let mail = f
        .engine
        .undelivered_for(&f.leader.agent_id().unwrap())
        .unwrap();
    assert!(
        mail.iter()
            .any(|m| m.subject == "Working folder changed" && m.content.contains("radix")),
        "{mail:?}"
    );
    // the choice survives a restart of MAW
    let again = Engine::open(
        f.store.clone(),
        "p",
        &f.project.path().to_string_lossy(),
        OrchestratorConfig::default(),
    )
    .unwrap();
    assert_eq!(again.workspace_root(), dest);
    // a folder the agents already work in cannot be proposed again
    assert_eq!(
        propose(&f, &f.leader, &dest.to_string_lossy())
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_changes_nothing_and_says_so() {
    let f = setup().await;
    let dest = target(&f, "radix");
    propose(&f, &f.leader, &dest.to_string_lossy()).unwrap();
    let id = pending_id(&f);
    f.sup
        .resolve_approval(&id, ApprovalDecision::Deny)
        .await
        .unwrap();
    assert_eq!(f.engine.workspace_root(), f.project.path());
    assert!(!dest.exists());
    let ap = f.engine.read(|db| db.get_approval(&id)).unwrap().unwrap();
    assert_eq!(ap.status, ApprovalStatus::Denied);
    let mail = f
        .engine
        .undelivered_for(&f.leader.agent_id().unwrap())
        .unwrap();
    assert!(
        mail.iter()
            .any(|m| m.subject == "Working folder not changed"),
        "{mail:?}"
    );
    // answering twice is refused
    assert!(f
        .sup
        .resolve_approval(&id, ApprovalDecision::Approve)
        .await
        .is_err());
    // after a refusal the Leader may propose something else
    assert!(propose(&f, &f.leader, &target(&f, "other").to_string_lossy()).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_leader_proposes_and_only_inside_the_allowed_places() {
    let f = setup().await;
    let dest = target(&f, "radix").to_string_lossy().to_string();
    assert_eq!(
        propose(&f, &f.worker, &dest).unwrap_err().code(),
        "forbidden"
    );
    assert_eq!(
        propose(&f, &Actor::User, &dest).unwrap_err().code(),
        "forbidden",
        "the user uses `maw workspace folder`"
    );
    // outside every allowed place, relative, or climbing out
    for bad in ["/etc/maw-test", "relative/dir", "../x/y", "/", "~", "C:\\"] {
        let e = propose(&f, &f.leader, bad).unwrap_err();
        assert_eq!(e.code(), "invalid_input", "{bad}: {e}");
    }
    let e = propose(&f, &f.leader, "/etc/maw-test")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains(&f.roots.path().to_string_lossy().to_string()),
        "it names the places that work: {e}"
    );
    // a folder inside the project's own folder is fine
    let sub = f.project.path().join("sub").join("app");
    assert!(propose(&f, &f.leader, &sub.to_string_lossy()).is_ok());
    // a file in the way is refused
    let f2 = setup().await;
    let file = f2.roots.path().join("occupied");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(
        propose(&f2, &f2.leader, &file.to_string_lossy())
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    // one proposal at a time
    assert_eq!(
        propose(&f, &f.leader, &dest).unwrap_err().code(),
        "conflict"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_windows_path_is_understood_from_wsl() {
    if !ws::running_in_wsl() {
        return; // the translation only applies under WSL
    }
    let f = setup().await;
    f.engine
        .set_workspace_roots(vec![PathBuf::from("/mnt/c/Users/someone/projects")]);
    let r = propose(&f, &f.leader, "C:\\Users\\someone\\projects\\radix").unwrap();
    assert_eq!(r["folder"], json!("/mnt/c/Users/someone/projects/radix"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_in_flight_blocks_the_change_when_proposing_and_when_approving() {
    let f = setup().await;
    let dest = target(&f, "radix").to_string_lossy().to_string();
    // a task is assigned: proposing is refused, naming it
    f.engine
        .call_tool(
            &f.leader,
            "maw_task_create",
            json!({ "title": "t", "assignee": "w1" }),
        )
        .unwrap();
    let e = propose(&f, &f.leader, &dest).unwrap_err();
    assert_eq!(e.code(), "precondition_failed");
    assert!(e.to_string().contains("T-1"), "{e}");
    // finish it (cancel), propose, then start more work before the user answers
    f.engine
        .call_tool(
            &f.leader,
            "maw_task_update",
            json!({ "task": "T-1", "cancel": true }),
        )
        .unwrap();
    assert!(f
        .engine
        .list_tasks()
        .unwrap()
        .iter()
        .all(|t| t.status == TaskStatus::Canceled));
    propose(&f, &f.leader, &dest).unwrap();
    f.engine
        .call_tool(
            &f.leader,
            "maw_task_create",
            json!({ "title": "u", "assignee": "w1" }),
        )
        .unwrap();
    let id = pending_id(&f);
    let e = f
        .sup
        .resolve_approval(&id, ApprovalDecision::Approve)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "precondition_failed", "{e}");
    assert!(!Path::new(&dest).exists(), "nothing was created");
    assert!(
        f.engine.pending_workspace_proposal().unwrap().is_some(),
        "still waiting for the user"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_leader_can_look_before_it_proposes_and_the_user_can_go_back() {
    let f = setup().await;
    let info = f
        .engine
        .call_tool(&f.worker, "maw_workspace_get", json!({}))
        .unwrap();
    assert_eq!(info["folder"], json!(f.project.path()));
    assert_eq!(info["can_change_now"], json!(true));
    assert!(info["you_may_propose_inside"].as_array().unwrap().len() >= 2);
    let dest = target(&f, "radix");
    propose(&f, &f.leader, &dest.to_string_lossy()).unwrap();
    let info = f
        .engine
        .call_tool(&f.leader, "maw_workspace_get", json!({}))
        .unwrap();
    assert_eq!(
        info["can_change_now"],
        json!(false),
        "a proposal is already waiting"
    );
    assert!(info["pending_proposal"]["summary"]
        .as_str()
        .unwrap()
        .contains("radix"));
    // the user can set it back directly
    let id = pending_id(&f);
    f.sup
        .resolve_approval(&id, ApprovalDecision::Approve)
        .await
        .unwrap();
    assert_eq!(f.engine.workspace_root(), dest);
    f.engine.set_work_dir(None).unwrap();
    assert_eq!(f.engine.workspace_root(), f.project.path());
    let again = Engine::open(
        f.store.clone(),
        "p",
        &f.project.path().to_string_lossy(),
        OrchestratorConfig::default(),
    )
    .unwrap();
    assert_eq!(
        again.workspace_root(),
        f.project.path(),
        "reset is remembered too"
    );
}
