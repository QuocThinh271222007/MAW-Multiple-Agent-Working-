//! The coding tools a local model gets: read/list/search files, write/edit files, run commands.
//!
//! Everything is confined to the agent's workspace (its git worktree): paths are normalized and
//! checked after symlink resolution, `.git` is never writable, output is truncated and every
//! operation is bounded. **There is no OS sandbox here** (unlike Codex's): the permission profile
//! decides what needs the user's approval, and `run_command` can do anything the user account can.

use maw_core::agent::PermissionProfile;
use maw_core::provider::ApprovalKind;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MAX_READ_BYTES: usize = 48 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LIST: usize = 300;
const MAX_MATCHES: usize = 80;
const MAX_FILES_VISITED: usize = 5000;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".maw",
    "dist",
    "build",
    "__pycache__",
    ".venv",
];

pub const LOCAL_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "search_text",
    "write_file",
    "edit_file",
    "run_command",
];

pub fn is_local(name: &str) -> bool {
    LOCAL_TOOLS.contains(&name)
}

/// Tool definitions in Ollama's `tools` format.
pub fn definitions() -> Vec<Value> {
    let f = |name: &str, desc: &str, props: Value, required: &[&str]| {
        json!({"type":"function","function":{
            "name": name, "description": desc,
            "parameters": {"type":"object","properties": props, "required": required}
        }})
    };
    vec![
        f(
            "read_file",
            "Read a text file in the workspace (numbered lines).",
            json!({"path":{"type":"string"},"offset":{"type":"integer","description":"first line, 1-based"},"limit":{"type":"integer","description":"max lines (default 300)"}}),
            &["path"],
        ),
        f(
            "list_dir",
            "List a directory in the workspace (default: the workspace root).",
            json!({"path":{"type":"string"}}),
            &[],
        ),
        f(
            "search_text",
            "Case-insensitive text search across workspace files; returns path:line: text.",
            json!({"pattern":{"type":"string"},"path":{"type":"string","description":"directory or file, default ."}}),
            &["pattern"],
        ),
        f(
            "write_file",
            "Create or overwrite a file in the workspace with the full content.",
            json!({"path":{"type":"string"},"content":{"type":"string"}}),
            &["path", "content"],
        ),
        f(
            "edit_file",
            "Replace one exact occurrence of old_text with new_text in a file.",
            json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}}),
            &["path", "old_text", "new_text"],
        ),
        f(
            "run_command",
            "Run a shell command in the workspace and return its exit code and output.",
            json!({"command":{"type":"string"}}),
            &["command"],
        ),
    ]
}

/// Whether a tool call may run, needs the user's approval, or is refused outright.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Allow,
    Ask {
        kind: ApprovalKind,
        summary: String,
        paths: Vec<String>,
        /// Key for "approve for session".
        remember: String,
    },
    Deny(String),
}

#[derive(Debug)]
pub struct ToolOutput {
    pub text: String,
    /// Workspace-relative paths this call modified.
    pub changed: Vec<String>,
}

#[derive(Debug)]
pub struct CommandResult {
    pub exit_code: Option<i64>,
    pub output: String,
    pub timed_out: bool,
    pub cancelled: bool,
}

#[derive(Clone)]
pub struct LocalTools {
    root: PathBuf,
    profile: PermissionProfile,
    command_timeout: Duration,
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or non-string argument '{key}'"))
}

fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Heuristic list of commands that always need a human, even in the `autonomous` profile.
/// It is a guard rail, not a sandbox.
pub fn is_risky_command(cmd: &str) -> bool {
    const WORDS: &[&str] = &[
        "rm",
        "rmdir",
        "sudo",
        "su",
        "doas",
        "chmod",
        "chown",
        "chgrp",
        "mkfs",
        "dd",
        "shutdown",
        "reboot",
        "halt",
        "poweroff",
        "kill",
        "killall",
        "pkill",
        "curl",
        "wget",
        "ssh",
        "scp",
        "sftp",
        "rsync",
        "nc",
        "ncat",
        "telnet",
        "ftp",
        "del",
        "erase",
        "rd",
        "format",
        "reg",
        "powershell",
        "pwsh",
        "eval",
    ];
    const PHRASES: &[&str] = &[
        "git push",
        "git reset --hard",
        "git clean",
        "git checkout --",
        "git branch -d",
        "git branch -D",
        "| sh",
        "| bash",
        "|sh",
        "|bash",
        "> /",
        "pip install",
        "npm install",
        "npm i ",
        "cargo install",
        "apt ",
        "apt-get",
        "brew ",
        "--force",
        " -rf",
        ":(){",
    ];
    let lower = cmd.to_ascii_lowercase();
    if PHRASES
        .iter()
        .any(|p| lower.contains(&p.to_ascii_lowercase()))
    {
        return true;
    }
    lower
        .split(|c: char| c.is_whitespace() || ";|&()`$<>".contains(c))
        .any(|t| WORDS.contains(&t))
}

impl LocalTools {
    pub fn new(
        root: &Path,
        profile: PermissionProfile,
        command_timeout: Duration,
    ) -> std::io::Result<Self> {
        Ok(LocalTools {
            root: root.canonicalize()?,
            profile,
            command_timeout,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a model-supplied path to an absolute path inside the workspace.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, String> {
        if raw.contains('\0') {
            return Err("invalid path".into());
        }
        let p = Path::new(raw);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };
        let mut norm = PathBuf::new();
        for c in joined.components() {
            match c {
                Component::Prefix(_) | Component::RootDir => norm.push(c.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !norm.pop() {
                        return Err(format!("'{raw}' is outside the workspace"));
                    }
                }
                Component::Normal(x) => norm.push(x),
            }
        }
        // resolve symlinks of the deepest existing ancestor, then re-attach the rest
        let mut probe = norm.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        while !probe.exists() {
            match probe.file_name() {
                Some(n) => tail.push(n.to_owned()),
                None => break,
            }
            if !probe.pop() {
                break;
            }
        }
        let mut real = probe
            .canonicalize()
            .map_err(|e| format!("cannot resolve '{raw}': {e}"))?;
        if !real.starts_with(&self.root) {
            return Err(format!("'{raw}' is outside the workspace"));
        }
        for t in tail.into_iter().rev() {
            real.push(t);
        }
        Ok(real)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn writable(&self, raw: &str) -> Result<PathBuf, String> {
        let p = self.resolve(raw)?;
        let rel = p.strip_prefix(&self.root).unwrap_or(&p);
        if rel.components().any(|c| c.as_os_str() == ".git") {
            return Err(
                "the .git directory is managed by git and MAW; it cannot be written".into(),
            );
        }
        Ok(p)
    }

    pub fn gate(&self, name: &str, args: &Value) -> Gate {
        match name {
            "read_file" | "list_dir" | "search_text" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
                match self.resolve(path) {
                    Ok(_) => Gate::Allow,
                    Err(e) => Gate::Deny(e),
                }
            }
            "write_file" | "edit_file" => {
                let path = match arg_str(args, "path") {
                    Ok(p) => p,
                    Err(e) => return Gate::Deny(e),
                };
                let abs = match self.writable(path) {
                    Ok(p) => p,
                    Err(e) => return Gate::Deny(e),
                };
                match self.profile {
                    PermissionProfile::Safe => {
                        let rel = self.rel(&abs);
                        Gate::Ask {
                            kind: ApprovalKind::FileChange,
                            summary: format!("{name}: {rel}"),
                            paths: vec![rel],
                            remember: "files".into(),
                        }
                    }
                    _ => Gate::Allow,
                }
            }
            "run_command" => {
                let cmd = match arg_str(args, "command") {
                    Ok(c) if !c.trim().is_empty() => c,
                    _ => return Gate::Deny("missing 'command'".into()),
                };
                let risky = is_risky_command(cmd);
                if self.profile == PermissionProfile::Autonomous && !risky {
                    return Gate::Allow;
                }
                Gate::Ask {
                    kind: ApprovalKind::Command,
                    summary: if risky {
                        format!("{cmd}  (flagged as potentially destructive or networked)")
                    } else {
                        cmd.to_string()
                    },
                    paths: vec![],
                    remember: format!("cmd:{cmd}"),
                }
            }
            other => Gate::Deny(format!("unknown tool '{other}'")),
        }
    }

    /// Run a non-command tool. Blocking file I/O; call from `spawn_blocking`.
    pub fn run_file_tool(&self, name: &str, args: &Value) -> Result<ToolOutput, String> {
        match name {
            "read_file" => self.read_file(args),
            "list_dir" => self.list_dir(args),
            "search_text" => self.search_text(args),
            "write_file" => self.write_file(args),
            "edit_file" => self.edit_file(args),
            other => Err(format!("unknown tool '{other}'")),
        }
    }

    fn read_file(&self, args: &Value) -> Result<ToolOutput, String> {
        let p = self.resolve(arg_str(args, "path")?)?;
        let meta = std::fs::metadata(&p).map_err(|e| format!("cannot read: {e}"))?;
        if meta.is_dir() {
            return Err("that is a directory; use list_dir".into());
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "file is {} bytes (limit {MAX_FILE_BYTES}); use search_text or run_command with head/sed",
                meta.len()
            ));
        }
        let bytes = std::fs::read(&p).map_err(|e| format!("cannot read: {e}"))?;
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Err("binary file".into());
        }
        let text = String::from_utf8_lossy(&bytes);
        let offset = arg_u64(args, "offset").unwrap_or(1).max(1) as usize;
        let limit = arg_u64(args, "limit").unwrap_or(300).clamp(1, 2000) as usize;
        let mut out = String::new();
        let mut shown = 0usize;
        let mut next = offset;
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let l = format!("{}\t{}\n", i + 1, line);
            if out.len() + l.len() > MAX_READ_BYTES {
                break;
            }
            out.push_str(&l);
            shown += 1;
            next = i + 2;
        }
        let total = text.lines().count();
        if shown == 0 && total >= offset {
            return Err("a single line exceeds the output limit".into());
        }
        if next <= total {
            out.push_str(&format!(
                "[showing lines {offset}-{}; file has {total} lines; continue with offset={next}]\n",
                next - 1
            ));
        } else if out.is_empty() {
            out.push_str("[empty file]\n");
        }
        Ok(ToolOutput {
            text: out,
            changed: vec![],
        })
    }

    fn list_dir(&self, args: &Value) -> Result<ToolOutput, String> {
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let p = self.resolve(raw)?;
        let rd = std::fs::read_dir(&p).map_err(|e| format!("cannot list: {e}"))?;
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    format!("{n}/")
                } else {
                    n
                }
            })
            .collect();
        names.sort();
        let more = names.len().saturating_sub(MAX_LIST);
        names.truncate(MAX_LIST);
        let mut out = names.join("\n");
        if out.is_empty() {
            out = "[empty directory]".into();
        }
        if more > 0 {
            out.push_str(&format!("\n[{more} more entries not shown]"));
        }
        out.push('\n');
        Ok(ToolOutput {
            text: out,
            changed: vec![],
        })
    }

    fn search_text(&self, args: &Value) -> Result<ToolOutput, String> {
        let pattern = arg_str(args, "pattern")?.to_lowercase();
        if pattern.is_empty() {
            return Err("empty pattern".into());
        }
        let start = self.resolve(args.get("path").and_then(Value::as_str).unwrap_or("."))?;
        let mut stack = vec![(start, 0usize)];
        let (mut visited, mut hits) = (0usize, Vec::<String>::new());
        'walk: while let Some((p, depth)) = stack.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if depth > 12 {
                    continue;
                }
                let Ok(rd) = std::fs::read_dir(&p) else {
                    continue;
                };
                let mut kids: Vec<_> = rd.filter_map(|e| e.ok()).collect();
                kids.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
                for k in kids {
                    let n = k.file_name().to_string_lossy().into_owned();
                    if k.file_type().map(|t| t.is_dir()).unwrap_or(false)
                        && SKIP_DIRS.contains(&n.as_str())
                    {
                        continue;
                    }
                    stack.push((k.path(), depth + 1));
                }
            } else if meta.is_file() && meta.len() <= 1024 * 1024 {
                visited += 1;
                if visited > MAX_FILES_VISITED {
                    hits.push(format!("[stopped after {MAX_FILES_VISITED} files]"));
                    break 'walk;
                }
                let Ok(bytes) = std::fs::read(&p) else {
                    continue;
                };
                if bytes[..bytes.len().min(4096)].contains(&0) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                for (i, line) in text.lines().enumerate() {
                    if line.to_lowercase().contains(&pattern) {
                        hits.push(format!(
                            "{}:{}: {}",
                            self.rel(&p),
                            i + 1,
                            truncate_utf8(line.trim(), 200)
                        ));
                        if hits.len() >= MAX_MATCHES {
                            hits.push(format!("[stopped after {MAX_MATCHES} matches]"));
                            break 'walk;
                        }
                    }
                }
            }
        }
        if hits.is_empty() {
            hits.push("[no matches]".into());
        }
        Ok(ToolOutput {
            text: hits.join("\n") + "\n",
            changed: vec![],
        })
    }

    fn write_file(&self, args: &Value) -> Result<ToolOutput, String> {
        let p = self.writable(arg_str(args, "path")?)?;
        let content = arg_str(args, "content")?;
        if content.len() > MAX_WRITE_BYTES {
            return Err(format!("content is larger than {MAX_WRITE_BYTES} bytes"));
        }
        if p.is_dir() {
            return Err("that path is a directory".into());
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create directory: {e}"))?;
        }
        std::fs::write(&p, content).map_err(|e| format!("cannot write: {e}"))?;
        let rel = self.rel(&p);
        Ok(ToolOutput {
            text: format!("wrote {} bytes to {rel}\n", content.len()),
            changed: vec![rel],
        })
    }

    fn edit_file(&self, args: &Value) -> Result<ToolOutput, String> {
        let p = self.writable(arg_str(args, "path")?)?;
        let old = arg_str(args, "old_text")?;
        let new = arg_str(args, "new_text")?;
        if old.is_empty() {
            return Err("old_text must not be empty (use write_file to create a file)".into());
        }
        let meta = std::fs::metadata(&p).map_err(|e| format!("cannot read: {e}"))?;
        if meta.len() > MAX_FILE_BYTES {
            return Err("file too large to edit".into());
        }
        let text = std::fs::read_to_string(&p).map_err(|e| format!("cannot read as text: {e}"))?;
        let n = text.matches(old).count();
        if n == 0 {
            return Err("old_text was not found; read the file again and copy it exactly".into());
        }
        if n > 1 {
            return Err(format!(
                "old_text occurs {n} times; include more surrounding lines so it is unique"
            ));
        }
        let updated = text.replacen(old, new, 1);
        if updated.len() > MAX_WRITE_BYTES {
            return Err("result would be too large".into());
        }
        std::fs::write(&p, &updated).map_err(|e| format!("cannot write: {e}"))?;
        let rel = self.rel(&p);
        Ok(ToolOutput {
            text: format!("edited {rel}\n"),
            changed: vec![rel],
        })
    }

    /// Run a shell command in the workspace. Output chunks are sent to `live` (best effort) while
    /// the command runs; the full (head + tail truncated) output is returned.
    pub async fn run_command(
        &self,
        command: &str,
        cancel: &CancellationToken,
        live: mpsc::Sender<String>,
    ) -> CommandResult {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        };
        cmd.current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("PAGER", "cat");
        for (k, _) in std::env::vars() {
            if k.starts_with("MAW_") {
                cmd.env_remove(k);
            }
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return CommandResult {
                    exit_code: None,
                    output: format!("could not start the shell: {e}"),
                    timed_out: false,
                    cancelled: false,
                }
            }
        };
        let pid = child.id();
        let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
        let (otx, mut orx) = mpsc::channel::<String>(256);
        for pipe in [
            out.take()
                .map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            err.take()
                .map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let otx = otx.clone();
            tokio::spawn(async move {
                let mut pipe = pipe;
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = pipe.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if otx
                        .send(String::from_utf8_lossy(&buf[..n]).into_owned())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(otx);
        let mut acc = HeadTail::new(MAX_OUTPUT_BYTES / 2, MAX_OUTPUT_BYTES / 2);
        let deadline = tokio::time::sleep(self.command_timeout);
        tokio::pin!(deadline);
        let (mut timed_out, mut cancelled) = (false, false);
        let mut status = None;
        let mut pipes_open = true;
        loop {
            tokio::select! {
                chunk = orx.recv(), if pipes_open => match chunk {
                    Some(c) => { let _ = live.try_send(c.clone()); acc.push(&c); }
                    None => pipes_open = false,
                },
                s = child.wait(), if status.is_none() => { status = Some(s); }
                _ = &mut deadline => { timed_out = true; break; }
                _ = cancel.cancelled() => { cancelled = true; break; }
            }
            if status.is_some() {
                break;
            }
        }
        if status.is_some() && !timed_out && !cancelled {
            // the shell has exited: collect what the reader tasks still deliver (short grace)
            while let Ok(Some(c)) =
                tokio::time::timeout(Duration::from_millis(150), orx.recv()).await
            {
                let _ = live.try_send(c.clone());
                acc.push(&c);
            }
        }
        if timed_out || cancelled {
            kill_tree(pid, &mut child).await;
        }
        let exit_code = match status {
            Some(Ok(s)) => s.code().map(i64::from),
            _ => None,
        };
        CommandResult {
            exit_code,
            output: acc.render(),
            timed_out,
            cancelled,
        }
    }
}

async fn kill_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("kill")
            .args(["-s", "KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}

/// Keeps the first and last part of a long output.
struct HeadTail {
    head: String,
    tail: String,
    head_max: usize,
    tail_max: usize,
    dropped: usize,
}

impl HeadTail {
    fn new(head_max: usize, tail_max: usize) -> Self {
        HeadTail {
            head: String::new(),
            tail: String::new(),
            head_max,
            tail_max,
            dropped: 0,
        }
    }

    fn push(&mut self, s: &str) {
        let mut rest = s;
        if self.tail.is_empty() && self.head.len() < self.head_max {
            let room = self.head_max - self.head.len();
            let cut = truncate_utf8(rest, room).len();
            self.head.push_str(&rest[..cut]);
            rest = &rest[cut..];
        }
        if rest.is_empty() {
            return;
        }
        self.tail.push_str(rest);
        if self.tail.len() > self.tail_max {
            let mut cut = self.tail.len() - self.tail_max;
            while cut < self.tail.len() && !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.dropped += cut;
            self.tail.drain(..cut);
        }
    }

    fn render(&self) -> String {
        if self.dropped > 0 {
            format!(
                "{}\n[... {} bytes omitted ...]\n{}",
                self.head, self.dropped, self.tail
            )
        } else {
            format!("{}{}", self.head, self.tail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(profile: PermissionProfile) -> (tempfile::TempDir, LocalTools) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "fn a() {}\nfn Needle() {}\n").unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        let t = LocalTools::new(d.path(), profile, Duration::from_secs(5)).unwrap();
        (d, t)
    }

    #[test]
    fn paths_cannot_leave_the_workspace() {
        let (d, t) = tools(PermissionProfile::Balanced);
        assert!(t.resolve("src/a.rs").is_ok());
        assert!(
            t.resolve("new/dir/file.txt").is_ok(),
            "non-existent targets are fine"
        );
        for bad in ["../x", "src/../../x", "/etc/passwd", "a/../../b"] {
            assert!(t.resolve(bad).is_err(), "{bad} must be refused");
        }
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
            assert!(
                t.resolve("link/secret").is_err(),
                "symlinks out of the workspace are refused"
            );
        }
        let _ = d;
    }

    #[test]
    fn git_is_never_writable_and_reads_are_always_allowed() {
        let (_d, t) = tools(PermissionProfile::Autonomous);
        assert!(matches!(
            t.gate("write_file", &json!({"path":".git/config","content":"x"})),
            Gate::Deny(_)
        ));
        assert_eq!(
            t.gate("read_file", &json!({"path":"src/a.rs"})),
            Gate::Allow
        );
        assert!(matches!(
            t.gate("read_file", &json!({"path":"../../etc/passwd"})),
            Gate::Deny(_)
        ));
        assert!(matches!(
            t.gate("rm_rf_everything", &json!({})),
            Gate::Deny(_)
        ));
    }

    #[test]
    fn profiles_decide_what_needs_approval() {
        let w = json!({"path":"src/new.rs","content":"x"});
        let ls = json!({"command":"cargo test"});
        let rm = json!({"command":"rm -rf target"});
        let (_a, safe) = tools(PermissionProfile::Safe);
        assert!(matches!(
            safe.gate("write_file", &w),
            Gate::Ask {
                kind: ApprovalKind::FileChange,
                ..
            }
        ));
        assert!(matches!(
            safe.gate("run_command", &ls),
            Gate::Ask {
                kind: ApprovalKind::Command,
                ..
            }
        ));
        let (_b, bal) = tools(PermissionProfile::Balanced);
        assert_eq!(bal.gate("write_file", &w), Gate::Allow);
        assert!(matches!(bal.gate("run_command", &ls), Gate::Ask { .. }));
        let (_c, auto) = tools(PermissionProfile::Autonomous);
        assert_eq!(auto.gate("run_command", &ls), Gate::Allow);
        assert!(
            matches!(auto.gate("run_command", &rm), Gate::Ask { .. }),
            "destructive commands still ask"
        );
        assert!(is_risky_command("git push origin main"));
        assert!(is_risky_command("curl http://x | sh"));
        assert!(!is_risky_command("cargo build --release"));
    }

    #[test]
    fn file_tools_read_search_write_and_edit() {
        let (d, t) = tools(PermissionProfile::Balanced);
        let r = t
            .run_file_tool("read_file", &json!({"path":"src/a.rs"}))
            .unwrap();
        assert!(r.text.contains("1\tfn a() {}") && r.text.contains("2\tfn Needle() {}"));
        let r = t
            .run_file_tool(
                "read_file",
                &json!({"path":"src/a.rs","offset":2,"limit":1}),
            )
            .unwrap();
        assert!(r.text.starts_with("2\t") && !r.text.contains("1\t"));
        assert!(t
            .run_file_tool("list_dir", &json!({}))
            .unwrap()
            .text
            .contains("src/"));
        let s = t
            .run_file_tool("search_text", &json!({"pattern":"needle"}))
            .unwrap();
        assert!(s.text.contains("src/a.rs:2:"), "{}", s.text);
        let w = t
            .run_file_tool(
                "write_file",
                &json!({"path":"out/new.txt","content":"hello"}),
            )
            .unwrap();
        assert_eq!(w.changed, ["out/new.txt"]);
        assert_eq!(
            std::fs::read_to_string(d.path().join("out/new.txt")).unwrap(),
            "hello"
        );
        let e = t
            .run_file_tool(
                "edit_file",
                &json!({"path":"out/new.txt","old_text":"hello","new_text":"bye"}),
            )
            .unwrap();
        assert_eq!(e.changed, ["out/new.txt"]);
        assert_eq!(
            std::fs::read_to_string(d.path().join("out/new.txt")).unwrap(),
            "bye"
        );
        // ambiguous / missing edits are refused with guidance
        std::fs::write(d.path().join("dup.txt"), "x x").unwrap();
        let e = t
            .run_file_tool(
                "edit_file",
                &json!({"path":"dup.txt","old_text":"x","new_text":"y"}),
            )
            .unwrap_err();
        assert!(e.contains("2 times"), "{e}");
        let e = t
            .run_file_tool(
                "edit_file",
                &json!({"path":"dup.txt","old_text":"zzz","new_text":"y"}),
            )
            .unwrap_err();
        assert!(e.contains("not found"), "{e}");
        assert!(t
            .run_file_tool("read_file", &json!({"path":"../x"}))
            .is_err());
    }

    #[test]
    fn long_files_are_paged_not_dumped() {
        let (d, t) = tools(PermissionProfile::Balanced);
        let big: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(d.path().join("big.txt"), big).unwrap();
        let r = t
            .run_file_tool("read_file", &json!({"path":"big.txt"}))
            .unwrap();
        assert!(r.text.contains("300\tline 300") && !r.text.contains("301\tline 301"));
        assert!(r.text.contains("continue with offset=301"), "{}", r.text);
    }

    #[test]
    fn head_tail_keeps_both_ends_of_long_output() {
        let mut h = HeadTail::new(10, 10);
        h.push(&"a".repeat(10));
        h.push(&"b".repeat(100));
        h.push("END");
        let r = h.render();
        assert!(r.starts_with("aaaaaaaaaa") && r.ends_with("END") && r.contains("omitted"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn commands_run_in_the_workspace_and_report_exit_codes_and_timeouts() {
        let (d, t) = tools(PermissionProfile::Autonomous);
        let (tx, mut rx) = mpsc::channel(64);
        let c = CancellationToken::new();
        let r = t
            .run_command("pwd; echo out; echo err 1>&2; exit 3", &c, tx)
            .await;
        assert_eq!(r.exit_code, Some(3));
        assert!(r.output.contains("out") && r.output.contains("err"));
        assert!(r.output.contains(
            &d.path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()
        ));
        assert!(rx.try_recv().is_ok(), "output is streamed live");

        let quick = LocalTools::new(
            d.path(),
            PermissionProfile::Autonomous,
            Duration::from_millis(300),
        )
        .unwrap();
        let (tx, _rx) = mpsc::channel(8);
        let start = std::time::Instant::now();
        let r = quick.run_command("sleep 30", &c, tx).await;
        assert!(r.timed_out && start.elapsed() < Duration::from_secs(10));

        let (tx, _rx) = mpsc::channel(8);
        let c2 = CancellationToken::new();
        let c3 = c2.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            c3.cancel();
        });
        let r = t.run_command("sleep 30", &c2, tx).await;
        assert!(r.cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn maw_secrets_are_not_inherited_by_commands() {
        std::env::set_var("MAW_MCP_TOKEN", "maw_tok_should_not_leak");
        let (_d, t) = tools(PermissionProfile::Autonomous);
        let (tx, _rx) = mpsc::channel(8);
        let r = t.run_command("env", &CancellationToken::new(), tx).await;
        assert!(
            !r.output.contains("maw_tok_should_not_leak"),
            "{}",
            r.output
        );
        std::env::remove_var("MAW_MCP_TOKEN");
    }
}
