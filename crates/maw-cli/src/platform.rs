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
            parse_cmd_path("C:\\Users\\Someone\r\n"),
            Some(PathBuf::from("/mnt/c/Users/Someone"))
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

// ---------------------------------------------------------------- console mouse (WSL under cmd)

/// UTF-16LE + base64, the form `powershell -EncodedCommand` takes.
fn encode_powershell(script: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The PowerShell that sets the QuickEdit flag of the console MAW runs in (`quick_edit` true: on,
/// false: off) and prints what it was (`was=1` / `was=0`) or why it could not (`noconsole` ...).
fn quick_edit_script(quick_edit: bool) -> String {
    quick_edit_script_for(if quick_edit { 1 } else { 0 })
}

/// `want`: 0 QuickEdit off, 1 on, 2 only report.
fn quick_edit_script_for(want: u8) -> String {
    format!(
        r#"$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class MawConsole {{
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
  static extern IntPtr CreateFileW(string n, uint a, uint s, IntPtr p, uint d, uint f, IntPtr t);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetConsoleMode(IntPtr h, out uint m);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetConsoleMode(IntPtr h, uint m);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  public static string Run(int want) {{
    IntPtr h = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    if (h == new IntPtr(-1)) return "noconsole";
    uint m;
    if (!GetConsoleMode(h, out m)) {{ CloseHandle(h); return "nomode"; }}
    bool was = (m & 0x40) != 0;
    uint n = m;
    if (want == 1) n = m | 0x80 | 0x40;
    else if (want == 0) n = (m | 0x80) & ~0x40u;
    if (n != m && !SetConsoleMode(h, n)) {{ CloseHandle(h); return "setfailed"; }}
    CloseHandle(h);
    return was ? "was=1" : "was=0";
  }}
}}
"@
[MawConsole]::Run({})
"#,
        want
    )
}

/// QuickEdit state before MAW turned it off: -1 not touched, 0 it was off, 1 it was on.
static QUICK_EDIT_WAS: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

/// Make the console deliver mouse clicks while the board is shown (`on`), and give it back as it was
/// when the board is left. Under WSL started from `cmd`, the console's QuickEdit mode keeps clicks
/// for itself (to select text) and MAW never sees them; this clears that flag for the console MAW
/// runs in and restores it afterwards. Returns a note for the user when there is something to say.
/// Elsewhere (Windows Terminal reports the mouse regardless, Linux, macOS) it does nothing.
pub fn board_console(on: bool) -> Option<String> {
    use std::sync::atomic::Ordering;
    if detect_os() != Os::Wsl {
        return None;
    }
    if !on && QUICK_EDIT_WAS.load(Ordering::SeqCst) != 1 {
        return None; // nothing was changed, nothing to give back
    }
    let out = match run_powershell(&quick_edit_script(!on), std::time::Duration::from_secs(12)) {
        Ok(o) => o,
        Err(e) => {
            return on.then(|| {
                format!("board: could not reach the Windows console ({e}); use Windows Terminal or untick QuickEdit in the cmd window's Properties")
            })
        }
    };
    let out = out.trim();
    if on {
        match out {
            "was=1" => {
                // read it back from a separate process: the change only counts if it stuck, that is
                // if the helper really shares the console MAW is shown in
                let stuck = run_powershell(
                    &quick_edit_script_for(2),
                    std::time::Duration::from_secs(12),
                )
                .map(|o| o.trim() == "was=0")
                .unwrap_or(false);
                if stuck {
                    QUICK_EDIT_WAS.store(1, Ordering::SeqCst);
                    Some("board: console text selection (QuickEdit) switched off so clicks reach MAW; it comes back when you leave the board".into())
                } else {
                    Some("board: the Windows console could not be changed from here (the helper got a console of its own); use Windows Terminal or untick QuickEdit in the cmd window's Properties".into())
                }
            }
            "was=0" => {
                QUICK_EDIT_WAS.store(0, Ordering::SeqCst);
                None
            }
            other => Some(format!(
                "board: could not change the console mouse mode ({other}); use Windows Terminal or untick QuickEdit in the cmd window's Properties"
            )),
        }
    } else {
        QUICK_EDIT_WAS.store(-1, Ordering::SeqCst);
        None
    }
}

fn run_powershell(script: &str, limit: std::time::Duration) -> Result<String, String> {
    use std::io::Read;
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encode_powershell(script),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("powershell.exe: {e}"))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (mut o, mut e) = (String::new(), String::new());
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut o);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut e);
                }
                return if status.success() {
                    Ok(o)
                } else {
                    Err(e.lines().next().unwrap_or("failed").trim().to_string())
                };
            }
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                return Err("timed out".into());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod console_tests {
    use super::*;

    #[test]
    fn powershell_commands_are_utf16_base64() {
        // "Hi" in UTF-16LE is 48 00 69 00 -> SABpAA==
        assert_eq!(encode_powershell("Hi"), "SABpAA==");
        assert_eq!(encode_powershell(""), "");
        assert_eq!(encode_powershell("A"), "QQA=");
    }

    #[test]
    fn the_script_asks_for_the_right_state() {
        assert!(quick_edit_script(false).contains("Run(0)"));
        assert!(quick_edit_script(true).contains("Run(1)"));
        assert!(quick_edit_script(true).contains("CONIN$"));
    }

    /// Needs WSL with Windows interop. Run by hand: it compiles and runs the real script. With no
    /// console attached (as under a test runner) the honest answer is `noconsole`.
    #[test]
    #[ignore = "needs WSL + powershell.exe"]
    fn the_real_script_runs_under_powershell() {
        let run = |want: u8| {
            let out = run_powershell(
                &quick_edit_script_for(want),
                std::time::Duration::from_secs(30),
            )
            .expect("powershell ran");
            let out = out.trim().to_string();
            eprintln!("want={want}: {out}");
            assert!(
                ["noconsole", "nomode", "was=0", "was=1", "setfailed"].contains(&out.as_str()),
                "{out}"
            );
            out
        };
        let before = run(2);
        run(0);
        let after_off = run(2); // does the change stay for the next process?
        run(1);
        let after_on = run(2);
        eprintln!("before={before} after_off={after_off} after_on={after_on}");
    }

    #[test]
    fn nothing_happens_outside_wsl_or_when_there_is_nothing_to_restore() {
        // restoring something that was never changed does not touch the console
        assert_eq!(board_console(false), None);
    }
}
