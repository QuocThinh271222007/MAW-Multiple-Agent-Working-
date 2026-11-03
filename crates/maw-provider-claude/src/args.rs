//! Command-line construction for the Claude Code CLI. Pure functions, unit tested.
//!
//! Security decisions encoded here (all verified against Claude Code 2.1.309):
//! * `--setting-sources ""` — ambient user/project/local settings are NOT loaded. A user's own
//!   `permissions.allow: ["Bash"]` would otherwise silently bypass MAW's permission profile.
//! * `--permission-prompt-tool stdio` — permission prompts are routed to MAW as
//!   `control_request/can_use_tool` messages instead of being auto-denied.
//! * `--strict-mcp-config` — only the MAW coordination server is attached.
//! * the MCP bearer token is passed through the child's environment (`${MAW_MCP_TOKEN}` is
//!   expanded by Claude Code inside the header), never on the command line where `ps` shows it.
//! * `--dangerously-skip-permissions` is never used.

use maw_core::agent::PermissionProfile;
use maw_core::provider::SessionConfig;
use serde_json::json;

/// MCP server key. Claude Code exposes its tools as `mcp__<key>__<tool>`.
pub const MCP_SERVER_NAME: &str = "maw";
/// Environment variable carrying the per-agent bearer token to the child process.
pub const MCP_TOKEN_ENV: &str = "MAW_MCP_TOKEN";

/// How the CLI session id is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionArg {
    /// Fresh session with an id MAW chooses (so it is known before the first event).
    New(String),
    /// Continue a persisted session.
    Resume(String),
}

impl SessionArg {
    pub fn id(&self) -> &str {
        match self {
            SessionArg::New(i) | SessionArg::Resume(i) => i,
        }
    }
}

/// Tools that only read; allowed in every profile.
const READ_TOOLS: &[&str] = &["Read", "Grep", "Glob", "LS"];
/// Tools that modify files.
const WRITE_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];
/// Development commands considered safe to run without asking in the Balanced profile.
const DEV_COMMANDS: &[&str] = &[
    "Bash(git status *)",
    "Bash(git diff *)",
    "Bash(git log *)",
    "Bash(git show *)",
    "Bash(git add *)",
    "Bash(git commit *)",
    "Bash(cargo test *)",
    "Bash(cargo build *)",
    "Bash(cargo check *)",
    "Bash(cargo fmt *)",
    "Bash(cargo clippy *)",
    "Bash(npm test *)",
    "Bash(npm run test *)",
    "Bash(npm run build *)",
    "Bash(pytest *)",
    "Bash(python -m pytest *)",
    "Bash(go test *)",
];
/// Always denied: external side effects belong to the user, not to an agent.
const ALWAYS_DENY: &[&str] = &[
    "Bash(git push *)",
    "Bash(sudo *)",
    "Bash(rm -rf /*)",
    "Bash(rm -rf ~*)",
];

/// Resolved CLI permission flags for a MAW profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionFlags {
    pub mode: &'static str,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// Map a MAW [`PermissionProfile`] to Claude Code flags. Anything not allowed here and not
/// denied becomes a `can_use_tool` request that the MAW operator answers in the Approval Center.
pub fn permission_flags(profile: PermissionProfile) -> PermissionFlags {
    let mcp = format!("mcp__{MCP_SERVER_NAME}__*");
    let mut allow: Vec<String> = READ_TOOLS.iter().map(|s| s.to_string()).collect();
    allow.push(mcp);
    let mut deny: Vec<String> = ALWAYS_DENY.iter().map(|s| s.to_string()).collect();
    match profile {
        PermissionProfile::Safe => {
            // Read-only: file modification is denied outright; non-read-only shell commands
            // (the CLI auto-allows a fixed read-only set) require operator approval.
            deny.extend(WRITE_TOOLS.iter().map(|s| s.to_string()));
            PermissionFlags {
                mode: "default",
                allow,
                deny,
            }
        }
        PermissionProfile::Balanced => {
            // In-workspace edits are accepted; build/test/git-commit tooling is pre-approved;
            // everything else (network, other commands, edits outside the workspace) asks.
            allow.extend(DEV_COMMANDS.iter().map(|s| s.to_string()));
            PermissionFlags {
                mode: "acceptEdits",
                allow,
                deny,
            }
        }
        PermissionProfile::Autonomous => {
            // Broad shell access inside the workspace. Deny rules above still apply and the
            // CLI's own checks (paths outside the workspace, network) still prompt.
            allow.push("Bash".to_string());
            PermissionFlags {
                mode: "acceptEdits",
                allow,
                deny,
            }
        }
    }
}

/// MCP config registering the MAW coordination server as a streamable-HTTP server.
/// The token is referenced via environment expansion so it never appears in `argv`.
pub fn mcp_config_json(url: &str) -> String {
    json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "type": "http",
                "url": url,
                "headers": { "Authorization": format!("Bearer ${{{MCP_TOKEN_ENV}}}") }
            }
        }
    })
    .to_string()
}

/// Build the full argument vector (excluding the program name).
pub fn build_args(cfg: &SessionConfig, session: &SessionArg) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--setting-sources".into(),
        String::new(),
        "--permission-prompt-tool".into(),
        "stdio".into(),
    ];
    let perms = permission_flags(cfg.permission);
    a.push("--permission-mode".into());
    a.push(perms.mode.into());
    // `=` form: these flags are variadic and would otherwise swallow following arguments.
    a.push(format!("--allowedTools={}", perms.allow.join(",")));
    a.push(format!("--disallowedTools={}", perms.deny.join(",")));
    if let Some(mcp) = &cfg.mcp {
        a.push("--strict-mcp-config".into());
        a.push(format!("--mcp-config={}", mcp_config_json(&mcp.url)));
    } else {
        a.push("--strict-mcp-config".into());
    }
    if let Some(m) = cfg.model.as_deref().filter(|m| !m.trim().is_empty()) {
        a.push("--model".into());
        a.push(m.to_string());
    }
    if let Some(e) = cfg.effort.as_deref().filter(|e| !e.trim().is_empty()) {
        a.push("--effort".into());
        a.push(e.to_string());
    }
    if !cfg.instructions.trim().is_empty() {
        a.push("--append-system-prompt".into());
        a.push(cfg.instructions.clone());
    }
    match session {
        SessionArg::New(id) => {
            a.push("--session-id".into());
            a.push(id.clone());
        }
        SessionArg::Resume(id) => {
            a.push("--resume".into());
            a.push(id.clone());
        }
    }
    a
}

/// True for strings shaped like a UUID (what `--session-id`/MAW persist).
pub fn looks_like_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use maw_core::agent::Role;
    use maw_core::ids::AgentId;
    use maw_core::provider::McpEndpoint;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn cfg(profile: PermissionProfile) -> SessionConfig {
        SessionConfig {
            agent_id: AgentId::new(),
            name: "w".into(),
            role: Role::Worker,
            cwd: PathBuf::from("/tmp"),
            model: Some("haiku".into()),
            permission: profile,
            mcp: Some(McpEndpoint {
                url: "http://127.0.0.1:7777/mcp".into(),
                token: "maw_tok_secret123456".into(),
            }),
            instructions: "be useful".into(),
            env: HashMap::new(),
            resume_native_id: None,
            effort: None,
        }
    }

    #[test]
    fn token_never_appears_in_argv() {
        let args = build_args(
            &cfg(PermissionProfile::Balanced),
            &SessionArg::New("11111111-1111-4111-8111-111111111111".into()),
        );
        assert!(
            args.iter().all(|a| !a.contains("maw_tok_secret")),
            "{args:?}"
        );
        assert!(args.iter().any(|a| a.contains("${MAW_MCP_TOKEN}")));
    }

    #[test]
    fn mcp_config_has_the_verified_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&mcp_config_json("http://127.0.0.1:1/mcp")).unwrap();
        let s = &v["mcpServers"]["maw"];
        assert_eq!(s["type"], "http");
        assert_eq!(s["url"], "http://127.0.0.1:1/mcp");
        assert_eq!(s["headers"]["Authorization"], "Bearer ${MAW_MCP_TOKEN}");
    }

    #[test]
    fn isolates_settings_and_routes_prompts_to_maw() {
        let args = build_args(&cfg(PermissionProfile::Safe), &SessionArg::New("x".into()));
        let i = args.iter().position(|a| a == "--setting-sources").unwrap();
        assert_eq!(args[i + 1], "", "ambient settings must not be loaded");
        let i = args
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .unwrap();
        assert_eq!(args[i + 1], "stdio");
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(!args.iter().any(|a| a.contains("dangerously")));
    }

    #[test]
    fn profiles_map_as_documented() {
        let safe = permission_flags(PermissionProfile::Safe);
        assert_eq!(safe.mode, "default");
        assert!(
            safe.deny.contains(&"Edit".to_string()) && safe.deny.contains(&"Write".to_string())
        );
        assert!(safe.allow.contains(&"mcp__maw__*".to_string()));
        assert!(!safe.allow.iter().any(|a| a.starts_with("Bash")));

        let bal = permission_flags(PermissionProfile::Balanced);
        assert_eq!(bal.mode, "acceptEdits");
        assert!(bal.allow.contains(&"Bash(cargo test *)".to_string()));
        assert!(
            !bal.allow.contains(&"Bash".to_string()),
            "no blanket shell in Balanced"
        );
        assert!(!bal.deny.contains(&"Edit".to_string()));

        let auto = permission_flags(PermissionProfile::Autonomous);
        assert!(auto.allow.contains(&"Bash".to_string()));
        for p in [&safe, &bal, &auto] {
            assert!(
                p.deny.contains(&"Bash(git push *)".to_string()),
                "pushes are never agent-initiated"
            );
        }
    }

    #[test]
    fn session_flags_and_optional_parts() {
        let mut c = cfg(PermissionProfile::Balanced);
        let new = build_args(&c, &SessionArg::New("abc".into()));
        let i = new.iter().position(|a| a == "--session-id").unwrap();
        assert_eq!(new[i + 1], "abc");
        let res = build_args(&c, &SessionArg::Resume("def".into()));
        assert!(res.windows(2).any(|w| w[0] == "--resume" && w[1] == "def"));
        assert!(!res.contains(&"--session-id".to_string()));
        c.model = None;
        c.instructions = String::new();
        c.mcp = None;
        let a = build_args(&c, &SessionArg::New("abc".into()));
        assert!(!a.contains(&"--model".to_string()));
        assert!(!a.contains(&"--append-system-prompt".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--mcp-config")));
        assert!(
            a.contains(&"--strict-mcp-config".to_string()),
            "never inherit ambient MCP servers"
        );
    }

    #[test]
    fn variadic_flags_use_equals_form() {
        let a = build_args(
            &cfg(PermissionProfile::Balanced),
            &SessionArg::New("x".into()),
        );
        assert!(a.iter().any(|x| x.starts_with("--allowedTools=")));
        assert!(a.iter().any(|x| x.starts_with("--disallowedTools=")));
        assert!(a.iter().any(|x| x.starts_with("--mcp-config=")));
    }

    #[test]
    fn effort_flag_is_passed_only_when_set() {
        let mut c = cfg(PermissionProfile::Balanced);
        c.effort = Some("high".into());
        let a = build_args(&c, &SessionArg::New("s".into()));
        let i = a
            .iter()
            .position(|x| x == "--effort")
            .expect("--effort present");
        assert_eq!(a[i + 1], "high");
        c.effort = None;
        assert!(!build_args(&c, &SessionArg::New("s".into())).contains(&"--effort".to_string()));
        c.effort = Some("  ".into());
        assert!(!build_args(&c, &SessionArg::New("s".into())).contains(&"--effort".to_string()));
    }
}
