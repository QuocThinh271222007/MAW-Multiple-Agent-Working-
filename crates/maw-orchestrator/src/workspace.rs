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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.trim_matches('-').to_string()
}

pub fn is_git_repo(path: &Path) -> bool {
    git(path, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s == "true")
        .unwrap_or(false)
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
        let path = if Path::new(&p).is_absolute() {
            PathBuf::from(p)
        } else {
            repo.join(p)
        };
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.lines().any(|l| l.trim() == ".maw/") {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &path,
                format!(
                    "{existing}{}.maw/\n",
                    if existing.ends_with('\n') || existing.is_empty() {
                        ""
                    } else {
                        "\n"
                    }
                ),
            );
        }
    }
}

/// Where agent worktrees live unless the user chose another place: next to the project.
pub fn default_base(repo: &Path) -> PathBuf {
    repo.join(".maw").join("worktrees")
}

/// The directory where `branch` is checked out as a (linked) worktree, if it is.
pub fn worktree_of_branch(repo: &Path, branch: &str) -> Option<PathBuf> {
    let list = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let mut current: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(p));
        } else if line
            .strip_prefix("branch ")
            .map(|b| b.trim_start_matches("refs/heads/"))
            == Some(branch)
        {
            return current;
        }
    }
    None
}

pub fn ensure_worktree(repo: &Path, agent_name: &str) -> Result<WorktreeInfo, String> {
    ensure_worktree_in(repo, agent_name, &default_base(repo), false)
}

/// Create (or reuse) the agent's worktree on branch `maw/<agent>` under `base`.
///
/// When the branch is already checked out somewhere else (the user changed where worktrees go),
/// `allow_move` decides: `false` keeps using that place; `true` moves it - leftover changes are
/// committed on the agent's branch first, so nothing is lost, then the worktree is recreated under
/// `base`. The caller only allows a move for an agent that has no work in flight.
pub fn ensure_worktree_in(
    repo: &Path,
    agent_name: &str,
    base_dir: &Path,
    allow_move: bool,
) -> Result<WorktreeInfo, String> {
    if !is_git_repo(repo) {
        return Err("project is not a git repository".into());
    }
    if !has_commits(repo) {
        return Err("repository has no commits yet; create an initial commit so MAW can branch worktrees from it".into());
    }
    if base_dir.starts_with(repo) {
        exclude_maw_dir(repo);
    }
    let s = slug(agent_name);
    let path = base_dir.join(&s);
    let branch = format!("maw/{s}");
    let base = head_revision(repo).ok_or("cannot resolve HEAD")?;
    if path.join(".git").exists() {
        return Ok(WorktreeInfo {
            path,
            branch,
            base_revision: base,
            created: false,
        });
    }
    if let Some(old) = worktree_of_branch(repo, &branch).filter(|o| o != &path && o != repo) {
        if !allow_move {
            return Ok(WorktreeInfo {
                path: old,
                branch,
                base_revision: base,
                created: false,
            });
        }
        // nothing is lost: leftovers go onto the agent's own branch before the folder is removed
        commit_all(
            &old,
            &format!("[{branch}] changes saved before moving the worktree"),
        )?;
        remove_worktree(repo, &old)?;
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    // -B resets the branch only when the worktree is created fresh; existing agent branches keep their history.
    let branch_exists = git(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok();
    let path_s = path.to_string_lossy().to_string();
    if branch_exists {
        git(repo, &["worktree", "add", &path_s, &branch])?;
    } else {
        git(repo, &["worktree", "add", "-b", &branch, &path_s, &base])?;
    }
    Ok(WorktreeInfo {
        path,
        branch,
        base_revision: base,
        created: true,
    })
}

/// Files changed in the worktree relative to `base` (committed + uncommitted + untracked).
pub fn changed_files(worktree: &Path, base: &str) -> Vec<String> {
    let mut files = std::collections::BTreeSet::new();
    if let Ok(out) = git(worktree, &["diff", "--name-only", &format!("{base}..HEAD")]) {
        files.extend(out.lines().filter(|l| !l.is_empty()).map(String::from));
    }
    if let Ok(out) = git(worktree, &["status", "--porcelain"]) {
        files.extend(
            out.lines()
                .filter_map(|l| l.get(3..))
                .map(|s| s.trim().to_string()),
        );
    }
    files.into_iter().collect()
}

pub fn has_uncommitted(worktree: &Path) -> bool {
    git(worktree, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// Commit everything in a MAW-owned worktree (used when a worker forgot to commit).
pub fn commit_all(worktree: &Path, message: &str) -> Result<bool, String> {
    if !has_uncommitted(worktree) {
        return Ok(false);
    }
    git(worktree, &["add", "-A"])?;
    git(
        worktree,
        &[
            "-c",
            "user.name=MAW",
            "-c",
            "user.email=maw@localhost",
            "commit",
            "-q",
            "-m",
            message,
        ],
    )?;
    Ok(true)
}

/// Merge `branch` into the integration worktree. Conflicts abort the merge and are reported.
pub fn integrate(repo: &Path, branch: &str) -> Result<IntegrationOutcome, String> {
    integrate_in(repo, branch, &default_base(repo))
}

/// `integrate` with the integration worktree kept under `base_dir`.
pub fn integrate_in(
    repo: &Path,
    branch: &str,
    base_dir: &Path,
) -> Result<IntegrationOutcome, String> {
    if base_dir.starts_with(repo) {
        exclude_maw_dir(repo);
    }
    let int_path = base_dir.join("_integration");
    let int_branch = "maw/integration";
    if !int_path.join(".git").exists() {
        std::fs::create_dir_all(int_path.parent().unwrap()).map_err(|e| e.to_string())?;
        let exists = git(
            repo,
            &["rev-parse", "--verify", &format!("refs/heads/{int_branch}")],
        )
        .is_ok();
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
        .args([
            "-c",
            "user.name=MAW",
            "-c",
            "user.email=maw@localhost",
            "merge",
            "--no-ff",
            "-m",
            &format!("MAW: integrate {branch}"),
            branch,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if merge.status.success() {
        let files = git(&int_path, &["diff", "--name-only", "HEAD~1..HEAD"])
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        Ok(IntegrationOutcome::Merged { files })
    } else {
        let files: Vec<String> = git(&int_path, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let _ = git(&int_path, &["merge", "--abort"]);
        Ok(IntegrationOutcome::Conflict { files })
    }
}

pub fn remove_worktree(repo: &Path, path: &Path) -> Result<(), String> {
    git(
        repo,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )
    .map(|_| ())
}

// ---------------------------------------------------------------- the folder agents work in

/// Whether this process runs inside WSL (Windows drives are then under `/mnt/<letter>`).
pub fn running_in_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/version")
            .map(|v| v.to_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

/// Turn what the Leader wrote into an absolute folder path on THIS machine, or say why not.
///
/// Accepted: a WSL/Linux path (`/home/me/projects/app`, `~/projects/app`), a Windows path
/// (`C:\Users\me\projects\app` or `C:/Users/...`, which under WSL is `/mnt/c/Users/...`) and a WSL
/// path seen from Windows (`\\wsl$\Ubuntu\home\me\app`, `\\wsl.localhost\Ubuntu\...`). Relative
/// paths and `..` are refused: where they point depends on a directory the Leader cannot know.
pub fn normalize_folder(input: &str, home: Option<&Path>, wsl: bool) -> Result<PathBuf, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("the path is empty".into());
    }
    if raw.len() > 240 || raw.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err("the path is too long or contains control characters".into());
    }
    let b = raw.as_bytes();
    let drive = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/');
    let unc = raw.starts_with("\\\\wsl$\\")
        || raw.to_ascii_lowercase().starts_with("\\\\wsl.localhost\\");
    let unix: String = if drive {
        if wsl {
            let rest = raw[3..].replace('\\', "/");
            format!(
                "/mnt/{}/{}",
                (b[0] as char).to_ascii_lowercase(),
                rest.trim_start_matches('/')
            )
        } else if cfg!(windows) {
            return clean_components(Path::new(raw));
        } else {
            return Err("a Windows path can only be used from Windows or WSL".into());
        }
    } else if unc {
        if !wsl {
            return Err("a \\\\wsl$ path can only be used from WSL".into());
        }
        // \\wsl$\<distro>\home\me\app  ->  /home/me/app
        let parts: Vec<&str> = raw.split('\\').filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return Err(
                "give a folder inside the distribution, e.g. \\\\wsl$\\Ubuntu\\home\\me\\app"
                    .into(),
            );
        }
        format!("/{}", parts[2..].join("/"))
    } else if raw == "~" || raw.starts_with("~/") {
        if raw == "~" || raw.trim_end_matches('/') == "~" {
            return Err("give a folder inside your home folder, e.g. ~/projects/app".into());
        }
        let h = home.ok_or("no home folder is known on this machine")?;
        h.join(raw.trim_start_matches('~').trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    } else {
        raw.replace('\\', "/")
    };
    if !unix.starts_with('/') && !cfg!(windows) {
        return Err(format!(
            "'{raw}' is not an absolute path (start it with /, ~/ or a drive letter such as C:\\)"
        ));
    }
    let cleaned = clean_components(Path::new(&unix))?;
    // /mnt/<letter> alone is a whole Windows drive
    let comps: Vec<String> = cleaned
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if comps.len() == 3 && comps[1] == "mnt" && comps[2].len() == 1 {
        return Err("that is a whole Windows drive; pick a project folder inside it".into());
    }
    Ok(cleaned)
}

/// Drop `.` and refuse `..`; the result must have at least two components below the root.
fn clean_components(p: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    let mut depth = 0;
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => return Err("'..' is not allowed in the path".into()),
            std::path::Component::CurDir => {}
            std::path::Component::Normal(n) => {
                depth += 1;
                out.push(n);
            }
            other => out.push(other.as_os_str()),
        }
    }
    if depth < 2 {
        return Err("that is too close to the root of a drive; pick a project folder inside your own folders".into());
    }
    Ok(out)
}

/// Create the folder (and its parents) and make it a git repository with one starting commit, so
/// every agent can have its own worktree. A folder that already holds a repository is left alone.
/// Returns a note about what was done; git trouble is reported, not fatal (agents then share the folder).
pub fn prepare_folder(path: &Path) -> Result<String, String> {
    if path.exists() && !path.is_dir() {
        return Err(format!("{} exists and is not a folder", path.display()));
    }
    let existed = path.is_dir();
    std::fs::create_dir_all(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut note = if existed {
        format!("{} already existed", path.display())
    } else {
        format!("created {}", path.display())
    };
    if is_git_repo(path) {
        note.push_str("; it is already a git repository");
        return Ok(note);
    }
    match git(path, &["init", "-q", "-b", "main"]) {
        Ok(_) => {
            let committed = git(
                path,
                &[
                    "-c",
                    "user.name=MAW",
                    "-c",
                    "user.email=maw@localhost",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "maw: start of the project folder",
                ],
            );
            match committed {
                Ok(_) => note.push_str("; started a git repository so each agent can have its own worktree"),
                Err(e) => note.push_str(&format!("; git init worked but the first commit did not ({e}); agents will share the folder")),
            }
        }
        Err(e) => note.push_str(&format!(
            "; could not start a git repository ({e}); agents will share the folder"
        )),
    }
    Ok(note)
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
    let p = if path.is_absolute() {
        norm(path)
    } else {
        norm(&root.join(path))
    };
    p.starts_with(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_from_windows_and_wsl_become_one_kind_of_path() {
        let home = PathBuf::from("/home/me");
        let n = |s: &str| normalize_folder(s, Some(&home), true);
        assert_eq!(
            n("/home/me/projects/app").unwrap(),
            PathBuf::from("/home/me/projects/app")
        );
        assert_eq!(
            n("~/projects/app").unwrap(),
            PathBuf::from("/home/me/projects/app")
        );
        assert_eq!(
            n("C:\\Users\\Me\\projects\\app").unwrap(),
            PathBuf::from("/mnt/c/Users/Me/projects/app")
        );
        assert_eq!(n("d:/work/app").unwrap(), PathBuf::from("/mnt/d/work/app"));
        assert_eq!(
            n("\\\\wsl$\\Ubuntu\\home\\me\\app").unwrap(),
            PathBuf::from("/home/me/app")
        );
        assert_eq!(
            n("\\\\wsl.localhost\\Ubuntu-24.04\\home\\me\\app").unwrap(),
            PathBuf::from("/home/me/app")
        );
        assert_eq!(
            n("  /home/me/./projects//app/ ").unwrap(),
            PathBuf::from("/home/me/projects/app")
        );
        for bad in [
            "",
            "projects/app",
            "../x",
            "/home/me/../root/app",
            "/",
            "/home",
            "C:\\",
            "~",
            "/a\nb",
        ] {
            assert!(n(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(n(&"x/".repeat(200)).is_err());
        // outside WSL a Windows path is not usable
        if !cfg!(windows) {
            assert!(normalize_folder("C:\\Users\\Me\\app", Some(&home), false).is_err());
            assert!(
                normalize_folder("\\\\wsl$\\Ubuntu\\home\\me\\app", Some(&home), false).is_err()
            );
        }
    }

    #[test]
    fn a_new_project_folder_becomes_a_git_repo_with_a_first_commit() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("projects").join("radix");
        let note = prepare_folder(&p).unwrap();
        assert!(p.is_dir() && is_git_repo(&p), "{note}");
        assert!(
            has_commits(&p),
            "agents need a commit to branch worktrees from: {note}"
        );
        // running it again leaves it alone
        let again = prepare_folder(&p).unwrap();
        assert!(again.contains("already"), "{again}");
        // a file where the folder should be is refused
        let f = d.path().join("file");
        std::fs::write(&f, "x").unwrap();
        assert!(prepare_folder(&f).is_err());
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        git(dir, &["add", "-A"]).unwrap();
        git(
            dir,
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
        )
        .unwrap();
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
        assert!(matches!(
            integrate(repo, &a.branch).unwrap(),
            IntegrationOutcome::Merged { .. }
        ));
        assert!(matches!(
            integrate(repo, &b.branch).unwrap(),
            IntegrationOutcome::Merged { .. }
        ));
        assert_eq!(
            integrate(repo, &a.branch).unwrap(),
            IntegrationOutcome::NothingToMerge
        );
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
        assert!(matches!(
            integrate(repo, &a.branch).unwrap(),
            IntegrationOutcome::Merged { .. }
        ));
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
        assert!(ensure_worktree(dir.path(), "x")
            .unwrap_err()
            .contains("no commits"));
    }

    #[test]
    fn worktrees_can_live_elsewhere_and_move_without_losing_work() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        init_repo(&repo);
        let elsewhere = tempfile::tempdir().unwrap();
        let base = elsewhere.path().join("worktrees");
        // created outside the project, on the agent's usual branch
        let a = ensure_worktree_in(&repo, "Codex-1", &base, true).unwrap();
        assert!(a.created && a.path.starts_with(&base), "{:?}", a.path);
        assert!(
            !repo.join(".maw").exists(),
            "nothing was put next to the project"
        );
        std::fs::write(a.path.join("keep.txt"), "unsaved\n").unwrap();

        // the user switches to another place: with work in flight the agent stays where it is
        let other = elsewhere.path().join("moved");
        let stay = ensure_worktree_in(&repo, "Codex-1", &other, false).unwrap();
        assert_eq!((stay.path.clone(), stay.created), (a.path.clone(), false));
        assert!(a.path.join("keep.txt").exists());

        // idle agent: it moves, its unsaved file is committed on its own branch first
        let moved = ensure_worktree_in(&repo, "Codex-1", &other, true).unwrap();
        assert!(
            moved.created && moved.path.starts_with(&other),
            "{:?}",
            moved.path
        );
        assert!(!a.path.exists(), "the old folder is gone");
        assert_eq!(
            std::fs::read_to_string(moved.path.join("keep.txt")).unwrap(),
            "unsaved\n"
        );
        assert_eq!(
            worktree_of_branch(&repo, &moved.branch),
            Some(moved.path.clone())
        );
        // and asking again changes nothing
        assert!(
            !ensure_worktree_in(&repo, "Codex-1", &other, true)
                .unwrap()
                .created
        );

        // the integration worktree follows the same base
        integrate_in(&repo, &moved.branch, &other).unwrap();
        assert!(other.join("_integration").join("keep.txt").exists());
    }
}
