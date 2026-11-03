//! `maw doctor`: environment diagnostics with concrete repair instructions.

use crate::app::Ctx;
use crate::platform::{self, Os};
use maw_core::provider::AuthStatus;

struct Line {
    ok: Option<bool>, // None = informational/warning
    text: String,
    fix: Option<String>,
}

fn ok(text: impl Into<String>) -> Line {
    Line {
        ok: Some(true),
        text: text.into(),
        fix: None,
    }
}
fn bad(text: impl Into<String>, fix: impl Into<String>) -> Line {
    Line {
        ok: Some(false),
        text: text.into(),
        fix: Some(fix.into()),
    }
}
fn info(text: impl Into<String>) -> Line {
    Line {
        ok: None,
        text: text.into(),
        fix: None,
    }
}

pub async fn run(ctx: &Ctx) -> bool {
    let mut lines: Vec<Line> = vec![];
    let caps = platform::terminal_caps();
    lines.push(info(format!(
        "MAW {} on {}",
        env!("CARGO_PKG_VERSION"),
        ctx.os.name()
    )));
    lines.push(info(format!(
        "config: {}   data: {}",
        ctx.dirs.config.display(),
        ctx.dirs.data.display()
    )));
    lines.push(info(format!(
        "terminal: TERM={} tty={} color={} truecolor={} utf8={}",
        caps.term, caps.is_tty, caps.color, caps.truecolor, caps.utf8
    )));
    if !caps.utf8 {
        lines.push(bad(
            "terminal locale is not UTF-8; box-drawing characters may render incorrectly",
            "export LANG=C.UTF-8 (or en_US.UTF-8)",
        ));
    }
    if ctx.os == Os::Windows {
        lines.push(bad(
            "native Windows is not validated for MAW yet",
            "run MAW inside WSL2 (wsl --install -d Ubuntu) and install claude/codex there; see docs/WINDOWS_WSL.md",
        ));
    }
    if let Ok(cwd) = std::env::current_dir() {
        if platform::is_windows_mount(&cwd, ctx.os) {
            lines.push(info("working directory is on a Windows mount (/mnt/...): git and builds are much slower there; consider cloning inside the WSL filesystem"));
        }
    }

    // git
    match platform::version_of("git", &["--version"]) {
        Some(v) => lines.push(ok(v)),
        None => lines.push(bad(
            "git not found (needed for worktree isolation)",
            "install git: sudo apt install git",
        )),
    }
    // node (only needed for the optional TypeScript bridge)
    match platform::version_of("node", &["--version"]) {
        Some(v) => lines.push(info(format!("node {v} (optional)"))),
        None => lines.push(info("node not found (optional; only needed for the TypeScript Claude bridge, which is not required)")),
    }

    // providers
    for (name, exe, cfgpath) in [
        ("codex", "codex", ctx.cfg.codex.path.clone()),
        ("claude", "claude", ctx.cfg.claude.path.clone()),
    ] {
        let bin = cfgpath.unwrap_or_else(|| exe.to_string());
        match platform::find_executable(&bin) {
            None => lines.push(bad(
                format!("{name}: executable '{bin}' not found"),
                if name == "codex" { "install: npm i -g @openai/codex   (inside the same environment MAW runs in)" } else { "install: https://code.claude.com/docs/en/setup   (inside the same environment MAW runs in)" },
            )),
            Some(path) => {
                let v = platform::version_of(&bin, &["--version"]).unwrap_or_else(|| "version unknown".into());
                lines.push(ok(format!("{name}: {v} ({})", path.display())));
                let provider = if name == "codex" { crate::providers::codex(Some(bin.clone())) } else { crate::providers::claude(Some(bin.clone())) };
                match tokio::time::timeout(std::time::Duration::from_secs(20), provider.auth_status()).await {
                    Ok(Ok(AuthStatus::Authenticated { method, account, plan })) => {
                        lines.push(ok(format!("{name}: authenticated via {method}{}{}", account.map(|a| format!(" as {a}")).unwrap_or_default(), plan.map(|p| format!(" ({p})")).unwrap_or_default())))
                    }
                    Ok(Ok(AuthStatus::NotAuthenticated { hint })) => lines.push(bad(format!("{name}: not authenticated"), format!("run `maw auth {name}`  ({hint})"))),
                    Ok(Ok(AuthStatus::Unknown { reason })) => lines.push(info(format!("{name}: auth status unknown ({reason})"))),
                    Ok(Err(e)) => lines.push(bad(format!("{name}: auth check failed: {e}"), format!("run `maw auth {name}` and `{bin} --help`"))),
                    Err(_) => lines.push(bad(format!("{name}: auth check timed out"), "check your network connection")),
                }
            }
        }
    }
    // local models (Ollama)
    {
        let p = crate::providers::ollama(&ctx.cfg.ollama);
        match p.detect().await {
            Ok(i) if i.installed => {
                lines.push(ok(format!(
                    "ollama: server {} at {}",
                    i.version.unwrap_or_default(),
                    i.executable.unwrap_or_default()
                )));
                match p.list_models().await {
                    Ok(m) if m.is_empty() => {
                        lines.push(info("ollama: no models installed (ollama pull <model>)"))
                    }
                    Ok(m) => {
                        let usable = m
                            .iter()
                            .filter(|x| !x.description.contains("NO tool support"))
                            .count();
                        lines.push(ok(format!(
                            "ollama: {} model(s), {usable} can run as agents (tool calling)",
                            m.len()
                        )));
                    }
                    Err(e) => lines.push(info(format!("ollama: could not list models: {e}"))),
                }
            }
            Ok(i) => {
                lines.push(info(format!(
                    "ollama: no server reachable (optional; only needed for local agents). {}",
                    i.notes.join(" ")
                )));
            }
            Err(e) => lines.push(info(format!("ollama: check failed: {e}"))),
        }
    }
    if ctx.os == Os::Wsl {
        lines.push(info("WSL note: provider authentication inside WSL is separate from Windows; log in from inside WSL (`maw auth codex`, `maw auth claude`)"));
    }

    // storage + MCP
    match std::fs::create_dir_all(&ctx.dirs.data)
        .and_then(|_| std::fs::create_dir_all(&ctx.dirs.config))
    {
        Ok(()) => lines.push(ok("data and config directories are writable")),
        Err(e) => lines.push(bad(
            format!("cannot create data/config directories: {e}"),
            "set MAW_HOME to a writable directory",
        )),
    }
    match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => lines.push(ok(format!(
            "loopback sockets available (MCP server can bind, e.g. {})",
            l.local_addr().map(|a| a.to_string()).unwrap_or_default()
        ))),
        Err(e) => lines.push(bad(
            format!("cannot bind a loopback socket: {e}"),
            "check firewall / sandbox settings",
        )),
    }

    let mut all_ok = true;
    for l in &lines {
        let mark = match l.ok {
            Some(true) => "✓",
            Some(false) => {
                all_ok = false;
                "✗"
            }
            None => "·",
        };
        println!("{mark} {}", l.text);
        if let Some(f) = &l.fix {
            println!("    fix: {f}");
        }
    }
    println!();
    println!(
        "{}",
        if all_ok {
            "All checks passed."
        } else {
            "Some checks failed; see the fixes above."
        }
    );
    all_ok
}
