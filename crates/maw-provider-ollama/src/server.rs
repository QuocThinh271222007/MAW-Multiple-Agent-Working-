//! Finding and starting the Ollama server, so using an Ollama agent never needs a manual step.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// An Ollama executable and where it lives.
#[derive(Debug, Clone, PartialEq)]
pub struct OllamaBinary {
    pub path: PathBuf,
    /// A Windows executable launched from WSL (it then needs `OLLAMA_HOST` handed across).
    pub windows_from_wsl: bool,
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

fn on_path(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for n in names {
            let c = dir.join(n);
            if is_file(&c) {
                return Some(c);
            }
        }
    }
    None
}

/// Locate `ollama`: explicit path, PATH, then the usual Windows install location
/// (also from WSL through `/mnt/c`).
pub fn find_binary(configured: Option<&str>) -> Option<OllamaBinary> {
    if let Some(c) = configured.filter(|c| !c.trim().is_empty()) {
        let p = PathBuf::from(c);
        let found = if is_file(&p) { Some(p) } else { on_path(&[c]) };
        return found.map(|path| OllamaBinary {
            windows_from_wsl: crate::api::is_wsl()
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
            path,
        });
    }
    if let Some(path) = on_path(&["ollama", "ollama.exe"]) {
        let exe = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
        return Some(OllamaBinary {
            windows_from_wsl: crate::api::is_wsl() && exe,
            path,
        });
    }
    if cfg!(windows) {
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            let p = PathBuf::from(l).join("Programs/Ollama/ollama.exe");
            if is_file(&p) {
                return Some(OllamaBinary {
                    path: p,
                    windows_from_wsl: false,
                });
            }
        }
    }
    if crate::api::is_wsl() {
        if let Ok(rd) = std::fs::read_dir("/mnt/c/Users") {
            for u in rd.flatten() {
                let p = u.path().join("AppData/Local/Programs/Ollama/ollama.exe");
                if is_file(&p) {
                    return Some(OllamaBinary {
                        path: p,
                        windows_from_wsl: true,
                    });
                }
            }
        }
    }
    None
}

/// Start `ollama serve` detached: it outlives MAW (like the Ollama desktop app does) and keeps
/// no handle to MAW's terminal. `host` is the `OLLAMA_HOST` value to bind, when one is needed.
pub fn spawn_serve(
    bin: &OllamaBinary,
    host: Option<&str>,
    models_dir: Option<&Path>,
) -> std::io::Result<()> {
    let mut cmd = Command::new(&bin.path);
    cmd.arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    // WSL only hands environment variables to Windows programs if they are listed in WSLENV; the
    // `/p` flag turns a WSL path into the Windows path on the way over
    let mut hand_over: Vec<&str> = vec![];
    if let Some(h) = host {
        cmd.env("OLLAMA_HOST", h);
        hand_over.push("OLLAMA_HOST");
    }
    if let Some(dir) = models_dir {
        // where downloaded models are kept; created here so the first download cannot fail on it
        std::fs::create_dir_all(dir)?;
        cmd.env("OLLAMA_MODELS", dir);
        hand_over.push(if bin.windows_from_wsl {
            "OLLAMA_MODELS/p"
        } else {
            "OLLAMA_MODELS"
        });
    }
    if bin.windows_from_wsl && !hand_over.is_empty() {
        let add = hand_over.join(":");
        let wslenv = match std::env::var("WSLENV") {
            Ok(v) if !v.is_empty() => format!("{v}:{add}"),
            _ => add,
        };
        cmd.env("WSLENV", wslenv);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn()?;
    // the server runs on its own; do not wait for it (and do not kill it when dropped)
    drop(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_wins_and_a_missing_one_is_not_invented() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("ollama");
        std::fs::write(&f, "").unwrap();
        let b = find_binary(Some(&f.to_string_lossy())).unwrap();
        assert_eq!(b.path, f);
        assert!(find_binary(Some("/definitely/not/here/ollama-xyz")).is_none());
    }
}
