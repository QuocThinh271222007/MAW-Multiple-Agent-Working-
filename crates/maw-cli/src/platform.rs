//! Typed platform abstraction: OS/WSL detection, executables, path translation, terminal capabilities.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Wsl,
    Windows,
    Mac,
    Other,
}

impl Os {
    pub fn name(&self) -> &'static str {
        match self {
            Os::Linux => "Linux",
            Os::Wsl => "WSL2 (Linux)",
            Os::Windows => "Windows",
            Os::Mac => "macOS",
            Os::Other => "unknown",
        }
    }
}

pub fn detect_os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(target_os = "linux") {
        if is_wsl() {
            Os::Wsl
        } else {
            Os::Linux
        }
    } else {
        Os::Other
    }
}

fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .map(|v| v.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct TerminalCaps {
    pub is_tty: bool,
    pub color: bool,
    pub truecolor: bool,
    pub utf8: bool,
    pub term: String,
}

pub fn terminal_caps() -> TerminalCaps {
    use std::io::IsTerminal;
    let term = std::env::var("TERM").unwrap_or_default();
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    let lang = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_lowercase();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let is_tty = std::io::stdout().is_terminal();
    TerminalCaps {
        is_tty,
        color: is_tty && !no_color && term != "dumb",
        truecolor: colorterm == "truecolor" || colorterm == "24bit",
        utf8: lang.contains("utf-8") || lang.contains("utf8") || cfg!(windows),
        term,
    }
}

/// Locate an executable on PATH (or accept an explicit path).
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.exists().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into(), ".cmd".into(), ".bat".into(), String::new()]
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Run `exe args` and return trimmed stdout (first line) when it succeeds.
pub fn version_of(exe: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(exe).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    s.lines()
        .next()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
}

/// `C:\Users\me\repo` -> `/mnt/c/Users/me/repo` (for WSL users passing Windows paths).
pub fn windows_to_wsl_path(p: &str) -> Option<String> {
    let b = p.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        let drive = (b[0] as char).to_ascii_lowercase();
        let rest = p[3..].replace('\\', "/");
        return Some(
            format!("/mnt/{drive}/{rest}")
                .trim_end_matches('/')
                .to_string(),
        );
    }
    None
}

/// `/mnt/c/Users/me` -> `C:\Users\me`.
#[allow(dead_code)]
pub fn wsl_to_windows_path(p: &str) -> Option<String> {
    let rest = p.strip_prefix("/mnt/")?;
    let mut it = rest.splitn(2, '/');
    let drive = it.next()?;
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let tail = it.next().unwrap_or("").replace('/', "\\");
    Some(format!("{}:\\{}", drive.to_ascii_uppercase(), tail))
}

/// Normalise a user supplied path (translating Windows paths inside WSL).
/// The Windows user's profile folder as WSL sees it (`/mnt/c/Users/me`), asked from `cmd.exe`.
pub fn windows_home() -> Option<PathBuf> {
    let out = std::process::Command::new("cmd.exe")
        .args(["/C", "echo %USERPROFILE%"])
        .current_dir("/mnt/c") // cmd.exe refuses a Linux working directory
        .output()
        .ok()?;
    parse_cmd_path(&String::from_utf8_lossy(&out.stdout))
}

/// A Windows path printed by `cmd.exe` (`C:\Users\me`), as a WSL path. An unexpanded
/// `%USERPROFILE%` (no Windows side) is not a path.
pub fn parse_cmd_path(output: &str) -> Option<PathBuf> {
    let t = output.trim();
    if t.is_empty() || t.contains('%') {
        return None;
    }
    windows_to_wsl_path(t).map(PathBuf::from)
}

pub fn normalize_user_path(p: &str, os: Os) -> PathBuf {
    if os == Os::Wsl {
        if let Some(w) = windows_to_wsl_path(p) {
            return PathBuf::from(w);
        }
    }
    PathBuf::from(p)
}

/// Warn about the slow, semantically different Windows-mounted filesystems under WSL.
pub fn is_windows_mount(p: &Path, os: Os) -> bool {
    os == Os::Wsl && p.starts_with("/mnt/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_profile_folder_is_read_from_cmd_output() {
        assert_eq!(
            parse_cmd_path("C:\\Users\\Thinh\r\n"),
            Some(PathBuf::from("/mnt/c/Users/Thinh"))
        );
        // cmd.exe leaves the variable as is when it does not exist; empty output is nothing
        assert_eq!(parse_cmd_path("%USERPROFILE%\r\n"), None);
        assert_eq!(parse_cmd_path(""), None);
    }

    #[test]
    fn path_translation() {
        assert_eq!(
            windows_to_wsl_path(r"C:\Users\Me\repo").as_deref(),
            Some("/mnt/c/Users/Me/repo")
        );
        assert_eq!(
            windows_to_wsl_path("d:/x/y/").as_deref(),
            Some("/mnt/d/x/y")
        );
        assert_eq!(windows_to_wsl_path("/home/me"), None);
        assert_eq!(
            wsl_to_windows_path("/mnt/c/Users/Me").as_deref(),
            Some(r"C:\Users\Me")
        );
        assert_eq!(wsl_to_windows_path("/home/me"), None);
        assert_eq!(
            normalize_user_path(r"C:\a\b", Os::Wsl),
            PathBuf::from("/mnt/c/a/b")
        );
        assert_eq!(
            normalize_user_path(r"C:\a\b", Os::Linux),
            PathBuf::from(r"C:\a\b")
        );
    }

    #[test]
    fn finds_common_executables() {
        if cfg!(unix) {
            assert!(find_executable("sh").is_some());
            assert!(find_executable("definitely-not-a-real-binary-xyz").is_none());
        }
    }
}
