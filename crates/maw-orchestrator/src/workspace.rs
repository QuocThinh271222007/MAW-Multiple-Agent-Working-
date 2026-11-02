//! Git worktree isolation. Each Worker gets its own worktree + branch under `.maw/worktrees`,
//! and completed work is integrated on a dedicated `maw/integration` branch, never on the
//! user's checked-out branch. Uncommitted user changes are never touched.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub base_revision: String,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntegrationOutcome {
    Merged { files: Vec<String> },
    NothingToMerge,
    Conflict { files: Vec<String> },
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("failed to run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

pub fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    s.trim_matches('-').to_string()
}

pub fn is_git_repo(path: &Path) -> bool {
    git(path, &["rev-parse", "--is-inside-work-tree"]).map(|s| s == "true").unwrap_or(false)
}

pub fn has_commits(path: &Path) -> bool {
    git(path, &["rev-parse", "--verify", "HEAD"]).is_ok()
}

pub fn head_revision(path: &Path) -> Option<String> {
    git(path, &["rev-parse", "HEAD"]).ok()
}

pub fn current_branch(path: &Path) -> Option<String> {
    git(path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()
}

/// Keep MAW's scratch directory out of `git status` without modifying tracked files.
fn exclude_maw_dir(repo: &Path) {
    if let Ok(p) = git(repo, &["rev-parse", "--git-path", "info/exclude"]) {
        let path = if Path::new(&p).is_absolute() { PathBuf::from(p) } else { repo.join(p) };
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.lines().any(|l| l.trim() == ".maw/") {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, format!("{existing}{}.maw/\n", if existing.ends_with('\n') || existing.is_empty() { "" } else { "\n" }));
        }
    }
}

/// Create (or reuse) the agent's worktree on branch `maw/<agent>`.
pub fn ensure_worktree(repo: &Path, agent_name: &str) -> Result<WorktreeInfo, String> {
    if !is_git_repo(repo) {
        return Err("project is not a git repository".into());
    }
    if !has_commits(repo) {
        return Err("repository has no commits yet; create an initial commit so MAW can branch worktrees from it".into());
    }
    exclude_maw_dir(repo);
    let s = slug(agent_name);
    let path = repo.join(".maw").join("worktrees").join(&s);
    let branch = format!("maw/{s}");
    let base = head_revision(repo).ok_or("cannot resolve HEAD")?;
    if path.join(".git").exists() {
        return Ok(WorktreeInfo { path, branch, base_revision: base, created: false });
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    // -B resets the branch only when the worktree is created fresh; existing agent branches keep their history.
    let branch_exists = git(repo, &["rev-parse", "--verify", &format!("refs/heads/{branch}")]).is_ok();
    let path_s = path.to_string_lossy().to_string();
    if branch_exists {
        git(repo, &["worktree", "add", &path_s, &branch])?;
    } else {
        git(repo, &["worktree", "add", "-b", &branch, &path_s, &base])?;
    }
    Ok(WorktreeInfo { path, branch, base_revision: base, created: true })
}

/// Files changed in the worktree relative to `base` (committed + uncommitted + untracked).
pub fn changed_files(worktree: &Path, base: &str) -> Vec<String> {
    let mut files = std::collections::BTreeSet::new();
    if let Ok(out) = git(worktree, &["diff", "--name-only", &format!("{base}..HEAD")]) {
        files.extend(out.lines().filter(|l| !l.is_empty()).map(String::from));
    }
    if let Ok(out) = git(worktree, &["status", "--porcelain"]) {
        files.extend(out.lines().filter_map(|l| l.get(3..)).map(|s| s.trim().to_string()));
    }
    files.into_iter().collect()
}

pub fn has_uncommitted(worktree: &Path) -> bool {
    git(worktree, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)
}

/// Commit everything in a MAW-owned worktree (used when a worker forgot to commit).
pub fn commit_all(worktree: &Path, message: &str) -> Result<bool, String> {
    if !has_uncommitted(worktree) {
        return Ok(false);
    }
    git(worktree, &["add", "-A"])?;
    git(worktree, &["-c", "user.name=MAW", "-c", "user.email=maw@localhost", "commit", "-q", "-m", message])?;
    Ok(true)
}

/// Merge `branch` into the integration worktree. Conflicts abort the merge and are reported.
pub fn integrate(repo: &Path, branch: &str) -> Result<IntegrationOutcome, String> {
    exclude_maw_dir(repo);
    let int_path = repo.join(".maw").join("worktrees").join("_integration");
    let int_branch = "maw/integration";
    if !int_path.join(".git").exists() {
        std::fs::create_dir_all(int_path.parent().unwrap()).map_err(|e| e.to_string())?;
        let exists = git(repo, &["rev-parse", "--verify", &format!("refs/heads/{int_branch}")]).is_ok();
        let p = int_path.to_string_lossy().to_string();
        if exists {
            git(repo, &["worktree", "add", &p, int_branch])?;
        } else {
            let base = head_revision(repo).ok_or("cannot resolve HEAD")?;
            git(repo, &["worktree", "add", "-b", int_branch, &p, &base])?;
        }
    }
    if has_uncommitted(&int_path) {
        return Err("integration worktree has uncommitted changes; refusing to merge".into());
    }
    // Already contained?
    if git(&int_path, &["merge-base", "--is-ancestor", branch, "HEAD"]).is_ok() {
        return Ok(IntegrationOutcome::NothingToMerge);
    }
    let merge = Command::new("git")
        .arg("-C")
        .arg(&int_path)
        .args(["-c", "user.name=MAW", "-c", "user.email=maw@localhost", "merge", "--no-ff", "-m", &format!("MAW: integrate {branch}"), branch])
        .output()
        .map_err(|e| e.to_string())?;
    if merge.status.success() {
        let files = git(&int_path, &["diff", "--name-only", "HEAD~1..HEAD"]).unwrap_or_default().lines().map(String::from).collect();
        Ok(IntegrationOutcome::Merged { files })
    } else {
        let files: Vec<String> = git(&int_path, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default().lines().map(String::from).collect();
        let _ = git(&int_path, &["merge", "--abort"]);
        Ok(IntegrationOutcome::Conflict { files })
    }
}

pub fn remove_worktree(repo: &Path, path: &Path) -> Result<(), String> {
    git(repo, &["worktree", "remove", "--force", &path.to_string_lossy()]).map(|_| ())
}

/// True when `path` resolves inside `root` (after normalising `..`). Used for boundary checks.
pub fn is_within(root: &Path, path: &Path) -> bool {
    fn norm(p: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
    let r = norm(root);
    let p = if path.is_absolute() { norm(path) } else { norm(&root.join(path)) };
    p.starts_with(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        git(dir, &["add", "-A"]).unwrap();
        git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]).unwrap();
    }

    #[test]
    fn path_boundary_checks() {
        let root = Path::new("/work/repo");
        assert!(is_within(root, Path::new("/work/repo/src/a.rs")));
        assert!(is_within(root, Path::new("src/a.rs")));
        assert!(!is_within(root, Path::new("/work/other")));
        assert!(!is_within(root, Path::new("../../etc/passwd")));
        assert!(!is_within(root, Path::new("/work/repo/../secret")));
    }

    #[test]
    fn worktrees_are_isolated_and_integrate_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_repo(repo);
        let a = ensure_worktree(repo, "Codex-1").unwrap();
        let b = ensure_worktree(repo, "Codex-2").unwrap();
        assert!(a.created && b.created);
        assert_ne!(a.path, b.path);
        // reuse
        assert!(!ensure_worktree(repo, "Codex-1").unwrap().created);
        std::fs::write(a.path.join("a.txt"), "from a\n").unwrap();
        std::fs::write(b.path.join("b.txt"), "from b\n").unwrap();
        // neither change leaks into the user's checkout or each other
        assert!(!repo.join("a.txt").exists() && !a.path.join("b.txt").exists());
        assert_eq!(changed_files(&a.path, &a.base_revision), vec!["a.txt"]);
        assert!(commit_all(&a.path, "[T-1] a").unwrap());
        assert!(commit_all(&b.path, "[T-2] b").unwrap());
        assert!(matches!(integrate(repo, &a.branch).unwrap(), IntegrationOutcome::Merged { .. }));
        assert!(matches!(integrate(repo, &b.branch).unwrap(), IntegrationOutcome::Merged { .. }));
        assert_eq!(integrate(repo, &a.branch).unwrap(), IntegrationOutcome::NothingToMerge);
        let int = repo.join(".maw/worktrees/_integration");
        assert!(int.join("a.txt").exists() && int.join("b.txt").exists());
        // user's working tree untouched and .maw not reported as untracked
        assert_eq!(git(repo, &["status", "--porcelain"]).unwrap(), "");
    }

    #[test]
    fn conflicts_are_reported_not_forced() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        init_repo(repo);
        let a = ensure_worktree(repo, "w1").unwrap();
        let b = ensure_worktree(repo, "w2").unwrap();
        std::fs::write(a.path.join("README.md"), "A version\n").unwrap();
        std::fs::write(b.path.join("README.md"), "B version\n").unwrap();
        commit_all(&a.path, "a").unwrap();
        commit_all(&b.path, "b").unwrap();
        assert!(matches!(integrate(repo, &a.branch).unwrap(), IntegrationOutcome::Merged { .. }));
        match integrate(repo, &b.branch).unwrap() {
            IntegrationOutcome::Conflict { files } => assert_eq!(files, vec!["README.md"]),
            o => panic!("expected conflict, got {o:?}"),
        }
        // integration worktree left clean (merge aborted)
        assert!(!has_uncommitted(&repo.join(".maw/worktrees/_integration")));
    }

    #[test]
    fn refuses_repo_without_commits() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]).unwrap();
        assert!(ensure_worktree(dir.path(), "x").unwrap_err().contains("no commits"));
    }
}
