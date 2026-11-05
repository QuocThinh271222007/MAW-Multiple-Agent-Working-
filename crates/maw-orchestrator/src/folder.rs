//! The folder the project's agents work in, and the Leader's way to ask for a different one.
//!
//! By default it is the folder `maw` was started in. The Leader may PROPOSE another one
//! (`maw_workspace_propose`), for instance a dedicated folder for the project instead of a home
//! folder. Nothing changes until the user approves it in the Approval Center: an agent never widens
//! where agents may write on its own. Folders on Windows drives and inside WSL are both accepted
//! (see [`crate::workspace::normalize_folder`]); the proposal is limited to the roots the user
//! allowed (`[workspace] roots`, by default `~/projects` on both sides) and to the project's own
//! folder.

use crate::engine::{Actor, Engine};
use crate::workspace as ws;
use maw_core::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `Approval::kind` of a working-folder proposal.
pub const KIND: &str = "workspace";
/// There is no provider request behind it, so there is nothing to answer there.
pub const REQUEST_REF: &str = "maw:workspace";
const KV_KEY: &str = "work_dir";

/// The user's home folder as this process sees it.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

impl Engine {
    /// The working folder chosen for this project, if it is not the project's own folder.
    pub fn work_dir(&self) -> Option<PathBuf> {
        self.work_dir.read().ok().and_then(|g| g.clone())
    }

    /// Where agents work: the chosen folder, else the folder the project was started in.
    pub fn workspace_root(&self) -> PathBuf {
        self.work_dir()
            .unwrap_or_else(|| PathBuf::from(&self.project.path))
    }

    /// Folders the Leader may propose inside (besides the project's own folder).
    pub fn workspace_roots(&self) -> Vec<PathBuf> {
        self.workspace_roots
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn set_workspace_roots(&self, roots: Vec<PathBuf>) {
        if let Ok(mut g) = self.workspace_roots.write() {
            *g = roots;
        }
    }

    /// Choose the working folder (`None`: back to the project's own folder) and remember it.
    pub fn set_work_dir(&self, to: Option<&Path>) -> CoreResult<()> {
        let from = self.workspace_root();
        let value = to
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.store
            .write(|db| db.kv_set(&self.project.id, KV_KEY, &value))?;
        if let Ok(mut g) = self.work_dir.write() {
            *g = to.map(Path::to_path_buf);
        }
        self.emit_custom(
            EventKind::WorkspaceUpdated,
            None,
            None,
            json!({ "kind": "folder", "from": from, "folder": self.workspace_root() }),
        );
        Ok(())
    }

    /// Whether `p` is a place the Leader may propose.
    pub fn folder_allowed(&self, p: &Path) -> bool {
        let project = PathBuf::from(&self.project.path);
        ws::is_within(&project, p) || self.workspace_roots().iter().any(|r| ws::is_within(r, p))
    }

    /// Work that would be stranded by switching folders now: tasks assigned to someone or waiting
    /// for verification (their changes live in worktrees of the current folder).
    pub fn workspace_change_blockers(&self) -> CoreResult<Vec<String>> {
        Ok(self
            .list_tasks()?
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    TaskStatus::Assigned
                        | TaskStatus::Running
                        | TaskStatus::Verifying
                        | TaskStatus::InReview
                        | TaskStatus::Blocked
                )
            })
            .map(|t| format!("{} ({})", t.key, t.status.as_str()))
            .collect())
    }

    /// The proposal waiting for the user, if any.
    pub fn pending_workspace_proposal(&self) -> CoreResult<Option<Approval>> {
        Ok(self
            .pending_approvals()?
            .into_iter()
            .find(|a| a.kind == KIND))
    }

    /// The Leader asks to move the project's working folder. Nothing changes yet: this records a
    /// request for the user to approve.
    pub fn propose_workspace(
        &self,
        actor: &Actor,
        path: &str,
        reason: &str,
    ) -> CoreResult<Approval> {
        let Actor::Agent { id, role } = actor else {
            return Err(CoreError::Forbidden(
                "only the Leader proposes a working folder; the user sets it with `maw workspace folder`".into(),
            ));
        };
        if *role != Role::Leader {
            return Err(CoreError::Forbidden(
                "only the Leader may propose the working folder".into(),
            ));
        }
        let home = home_dir();
        let folder = ws::normalize_folder(path, home.as_deref(), ws::running_in_wsl())
            .map_err(CoreError::InvalidInput)?;
        if !self.folder_allowed(&folder) {
            let roots: Vec<String> = self
                .workspace_roots()
                .iter()
                .map(|r| r.display().to_string())
                .chain(std::iter::once(self.project.path.clone()))
                .collect();
            return Err(CoreError::InvalidInput(format!(
                "{} is outside the places you may use: {}. Pick a folder inside one of them (a Windows path such as C:\\Users\\me\\projects\\app works too).",
                folder.display(),
                roots.join(", ")
            )));
        }
        if folder.exists() && !folder.is_dir() {
            return Err(CoreError::InvalidInput(format!(
                "{} exists and is not a folder",
                folder.display()
            )));
        }
        let current = self.workspace_root();
        if folder == current {
            return Err(CoreError::InvalidInput(format!(
                "{} is already the working folder",
                folder.display()
            )));
        }
        if let Some(p) = self.pending_workspace_proposal()? {
            return Err(CoreError::Conflict(format!(
                "a proposal is already waiting for the user ({}); wait for the answer",
                p.summary
            )));
        }
        let blockers = self.workspace_change_blockers()?;
        if !blockers.is_empty() {
            return Err(CoreError::Precondition(format!(
                "work is still in flight ({}); finish, release or cancel those tasks first, then propose the folder",
                blockers.join(", ")
            )));
        }
        let reason: String = reason.trim().chars().take(500).collect();
        self.record_approval(
            id,
            KIND,
            &format!(
                "Leader proposes the project's working folder: {}",
                folder.display()
            ),
            (!reason.is_empty()).then_some(reason.clone()),
            json!({ "path": folder, "previous": current, "reason": reason }),
            REQUEST_REF,
        )
    }

    /// What `maw_workspace_get` returns.
    pub fn workspace_info(&self) -> CoreResult<Value> {
        let root = self.workspace_root();
        let blockers = self.workspace_change_blockers()?;
        let pending = self.pending_workspace_proposal()?;
        let mut roots: Vec<String> = self
            .workspace_roots()
            .iter()
            .map(|r| r.display().to_string())
            .collect();
        roots.push(self.project.path.clone());
        Ok(json!({
            "folder": root,
            "project_folder": self.project.path,
            "is_git_repository": ws::is_git_repo(&root),
            "you_may_propose_inside": roots,
            "can_change_now": blockers.is_empty() && pending.is_none(),
            "tasks_in_flight": blockers,
            "pending_proposal": pending.map(|a| json!({ "approval_id": a.id, "summary": a.summary })),
            "note": "Folders may be written as WSL paths (/home/me/projects/app, ~/projects/app) or Windows paths (C:\\Users\\me\\projects\\app). Only the user can approve a change.",
        }))
    }
}
