mod app;
mod config;
mod doctor;
mod headless;
mod logging;
mod platform;
mod providers;

use anyhow::{bail, Context, Result};
use app::{boot, open_utility_engine, resolve_project, Ctx, ProjectRef, Providers};
use clap::{Parser, Subcommand};
use maw_core::*;
use maw_orchestrator::mock::MockScript;
use maw_orchestrator::Actor;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "maw", version, about = "MAW — Multiple Agent Working: a terminal-native multi-agent coding team", long_about = None)]
struct Cli {
    /// Project name or path (default: the current directory)
    #[arg(short, long, global = true)]
    project: Option<String>,
    /// Log to stderr instead of the log file
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Register the current (or given) directory as a MAW project
    Init {
        path: Option<String>,
        /// `git init` and make an initial commit when the directory is not a repository
        #[arg(long)]
        git: bool,
    },
    /// Check the environment (git, provider CLIs, auth, terminal, WSL) and print repair steps
    Doctor,
    /// Manage projects
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Provider authentication (delegates to the official login flows)
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Manage the agents of a project
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// Run the team on a goal without the TUI
    Run {
        /// The objective for the Leader
        goal: String,
        /// Success criterion (repeatable)
        #[arg(long = "criterion")]
        criteria: Vec<String>,
        /// Give up after this many minutes
        #[arg(long)]
        timeout_mins: Option<u64>,
    },
    /// Show goal, progress, agents and tasks
    Status,
    /// Ask a running `maw run`/`maw resume` to stop gracefully
    Stop,
    /// Continue an existing goal headlessly (recovers interrupted work conservatively)
    Resume {
        #[arg(long)]
        timeout_mins: Option<u64>,
    },
    /// Inspect tasks
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// MCP coordination server utilities
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// Show recent logs or the event log
    Logs {
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Show the persisted project event log instead of the diagnostic log
        #[arg(long)]
        events: bool,
        #[arg(short, long)]
        follow: bool,
    },
    /// Show or edit configuration
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },
    /// Run a self-contained demo with scripted (mock) agents. Not evidence of real provider integration.
    Demo {
        /// Print events instead of opening the TUI
        #[arg(long)]
        headless: bool,
        /// Skip the artificial pauses between mock steps
        #[arg(long)]
        fast: bool,
    },
    /// Print the MCP tool reference
    Tools {
        #[arg(long)]
        markdown: bool,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    List,
    Add {
        path: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Open a project in the TUI
    Open {
        name: String,
    },
}

#[derive(Subcommand)]
enum AuthCmd {
    Status,
    /// Log in to Codex using its official flow
    Codex,
    /// Log in to Claude Code using its official flow
    Claude,
}

#[derive(Subcommand)]
enum AgentCmd {
    List,
    Add {
        #[arg(long)]
        name: String,
        /// claude | codex
        #[arg(long)]
        provider: String,
        /// leader | worker | reviewer | specialist
        #[arg(long, default_value = "worker")]
        role: String,
        #[arg(long)]
        model: Option<String>,
        /// safe | balanced | autonomous
        #[arg(long, default_value = "balanced")]
        permission: String,
    },
    Status {
        name: String,
    },
    Role {
        name: String,
        role: String,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    List {
        #[arg(long)]
        status: Option<String>,
    },
    Show {
        id: String,
    },
}

#[derive(Subcommand)]
enum McpCmd {
    /// Serve the coordination tools standalone (for inspection/debugging)
    Serve {
        /// Serve over stdio (default is loopback HTTP)
        #[arg(long)]
        stdio: bool,
        /// Identity to act as: `user` (operator) or an agent name
        #[arg(long = "as", default_value = "user")]
        identity: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    Status,
}

#[derive(Subcommand)]
enum ConfigCmd {
    Show,
    Path,
    /// Set a value, e.g. `maw config set orchestrator.max_concurrency=3`
    Set {
        assignment: String,
    },
}

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        // Behave like other CLI tools when piped into `head`: exit quietly on EPIPE instead of panicking.
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(real_main(cli))
}

fn anyhow_core<T>(r: CoreResult<T>) -> Result<T> {
    r.map_err(|e| anyhow::anyhow!("{e}"))
}

async fn real_main(cli: Cli) -> Result<()> {
    let ctx = Ctx::load()?;
    logging::init(&ctx.dirs.log_file(), cli.verbose);
    let project_arg = cli.project.clone();
    match cli.cmd {
        None => {
            let p = resolve_project(&ctx, project_arg.as_deref(), true)?;
            launch_tui(&ctx, &p, Providers::Real).await
        }
        Some(Cmd::Init { path, git }) => cmd_init(&ctx, path.or(project_arg), git),
        Some(Cmd::Doctor) => {
            let ok = doctor::run(&ctx).await;
            std::process::exit(if ok { 0 } else { 1 });
        }
        Some(Cmd::Project { cmd }) => match cmd {
            ProjectCmd::List => {
                let reg = config::Registry::load(&ctx.dirs);
                if reg.projects.is_empty() {
                    println!("No projects yet. Run `maw init` inside a repository.");
                }
                for (n, p) in reg.projects {
                    println!("{n:<24} {p}");
                }
                Ok(())
            }
            ProjectCmd::Add { path, name } => {
                let mut reg = config::Registry::load(&ctx.dirs);
                let p = std::fs::canonicalize(platform::normalize_user_path(&path, ctx.os))
                    .with_context(|| format!("{path} does not exist"))?;
                let n = reg.add(&p, name.as_deref());
                reg.save(&ctx.dirs)?;
                println!("Registered '{n}' -> {}", p.display());
                Ok(())
            }
            ProjectCmd::Open { name } => {
                let p = resolve_project(&ctx, Some(&name), true)?;
                launch_tui(&ctx, &p, Providers::Real).await
            }
        },
        Some(Cmd::Auth { cmd }) => cmd_auth(&ctx, cmd).await,
        Some(Cmd::Agent { cmd }) => cmd_agent(&ctx, project_arg.as_deref(), cmd),
        Some(Cmd::Run {
            goal,
            criteria,
            timeout_mins,
        }) => {
            let p = resolve_project(&ctx, project_arg.as_deref(), true)?;
            run_headless(
                &ctx,
                &p,
                Providers::Real,
                Some((goal, criteria)),
                timeout_mins.map(|m| Duration::from_secs(m * 60)),
            )
            .await
        }
        Some(Cmd::Resume { timeout_mins }) => {
            let p = resolve_project(&ctx, project_arg.as_deref(), true)?;
            run_headless(
                &ctx,
                &p,
                Providers::Real,
                None,
                timeout_mins.map(|m| Duration::from_secs(m * 60)),
            )
            .await
        }
        Some(Cmd::Status) => {
            let p = resolve_project(&ctx, project_arg.as_deref(), false)?;
            let eng = open_utility_engine(&ctx, &p)?;
            let snap = anyhow_core(eng.snapshot())?;
            println!("Project {} ({})", p.name, p.path.display());
            if let Some(info) = app::read_runtime_info(&ctx, &p) {
                println!(
                    "Running: pid {} since {}",
                    info["pid"],
                    info["started_at"].as_str().unwrap_or("?")
                );
            } else {
                println!("Not running.");
            }
            println!(
                "Scheduling: {}",
                if snap.paused { "PAUSED" } else { "active" }
            );
            headless::print_summary(&snap);
            println!();
            println!(
                "{:<16} {:<10} {:<9} {:<20} TASK",
                "AGENT", "ROLE", "PROVIDER", "STATE"
            );
            for a in &snap.agents {
                let t = a
                    .current_task_id
                    .as_ref()
                    .and_then(|t| snap.tasks.iter().find(|x| &x.id == t))
                    .map(|t| t.key.as_str())
                    .unwrap_or("-");
                println!(
                    "{:<16} {:<10} {:<9} {:<20} {}",
                    a.name, a.role, a.provider, a.state, t
                );
            }
            let pend = snap
                .approvals
                .iter()
                .filter(|a| a.status == ApprovalStatus::Pending)
                .count();
            if pend > 0 {
                println!("\n{pend} approval(s) pending: open the TUI (`maw`) to resolve them.");
            }
            Ok(())
        }
        Some(Cmd::Stop) => {
            let p = resolve_project(&ctx, project_arg.as_deref(), false)?;
            let eng = open_utility_engine(&ctx, &p)?;
            match app::read_runtime_info(&ctx, &p) {
                Some(_) => {
                    anyhow_core(eng.store().write(|db| {
                        db.kv_set(
                            eng.project_id(),
                            "stop_request",
                            &chrono::Utc::now().to_rfc3339(),
                        )
                    }))?;
                    println!("Stop requested; the running MAW process will shut down gracefully and keep all state.");
                }
                None => println!("No running MAW process found for '{}'.", p.name),
            }
            Ok(())
        }
        Some(Cmd::Task { cmd }) => cmd_task(&ctx, project_arg.as_deref(), cmd),
        Some(Cmd::Mcp { cmd }) => cmd_mcp(&ctx, project_arg.as_deref(), cmd).await,
        Some(Cmd::Logs {
            lines,
            events,
            follow,
        }) => cmd_logs(&ctx, project_arg.as_deref(), lines, events, follow).await,
        Some(Cmd::Config { cmd }) => cmd_config(&ctx, cmd),
        Some(Cmd::Demo { headless, fast }) => cmd_demo(headless, fast).await,
        Some(Cmd::Tools { markdown }) => {
            print_tools(markdown);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- init / auth

fn cmd_init(ctx: &Ctx, path: Option<String>, git: bool) -> Result<()> {
    let p = resolve_project(ctx, path.as_deref(), true)?;
    if !app::is_git_repo(&p.path) {
        if git {
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec!["add", "-A"],
                vec![
                    "-c",
                    "user.name=MAW",
                    "-c",
                    "user.email=maw@localhost",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "Initial commit (created by maw init)",
                ],
            ] {
                let st = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&p.path)
                    .args(&args)
                    .status()
                    .context("running git")?;
                if !st.success() {
                    bail!("git {} failed", args.join(" "));
                }
            }
            println!("Initialised a git repository with an initial commit.");
        } else {
            println!("Note: {} is not a git repository. MAW needs git for isolated worker worktrees; run `maw init --git` or `git init` + one commit.", p.path.display());
        }
    } else if !maw_orchestrator::workspace::has_commits(&p.path) {
        println!("Note: the repository has no commits yet; worktree isolation needs at least one commit.");
    }
    let eng = open_utility_engine(ctx, &p)?;
    let _ = eng;
    println!("Project '{}' ready at {}", p.name, p.path.display());
    println!("Next: `maw agent add --name lead --provider claude --role leader` and `maw agent add --name coder --provider codex`, then `maw`.");
    Ok(())
}

async fn cmd_auth(ctx: &Ctx, cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Status => {
            for (name, p) in [
                ("codex", providers::codex(ctx.cfg.codex.path.clone())),
                ("claude", providers::claude(ctx.cfg.claude.path.clone())),
            ] {
                let info = p.detect().await;
                match info {
                    Ok(i) if i.installed => {
                        let st = p.auth_status().await;
                        println!(
                            "{name:<7} {}  {}",
                            i.version.unwrap_or_default(),
                            match st {
                                Ok(maw_core::provider::AuthStatus::Authenticated {
                                    method,
                                    account,
                                    plan,
                                }) => format!(
                                    "authenticated via {method}{}{}",
                                    account.map(|a| format!(" ({a})")).unwrap_or_default(),
                                    plan.map(|p| format!(" [{p}]")).unwrap_or_default()
                                ),
                                Ok(maw_core::provider::AuthStatus::NotAuthenticated { hint }) =>
                                    format!("NOT authenticated: {hint}"),
                                Ok(maw_core::provider::AuthStatus::Unknown { reason }) =>
                                    format!("unknown: {reason}"),
                                Err(e) => format!("error: {e}"),
                            }
                        );
                    }
                    Ok(_) => println!("{name:<7} not installed"),
                    Err(e) => println!("{name:<7} error: {e}"),
                }
            }
            if ctx.os == platform::Os::Wsl {
                println!(
                    "\n(WSL: this reflects the Linux side; Windows-side logins are separate.)"
                );
            }
            Ok(())
        }
        AuthCmd::Codex => delegate_login("codex", ctx.cfg.codex.path.as_deref(), &["login"]),
        AuthCmd::Claude => {
            delegate_login("claude", ctx.cfg.claude.path.as_deref(), &["auth", "login"])
        }
    }
}

/// Hand the terminal to the provider's own login flow. MAW never sees credentials.
fn delegate_login(name: &str, path: Option<&str>, args: &[&str]) -> Result<()> {
    let exe = path.unwrap_or(name);
    if platform::find_executable(exe).is_none() {
        bail!("{name} is not installed (looked for '{exe}'); run `maw doctor` for install steps");
    }
    println!("Starting the official {name} login ({exe} {}). MAW does not read or store your credentials.\n", args.join(" "));
    let status = std::process::Command::new(exe)
        .args(args)
        .status()
        .with_context(|| format!("running {exe}"))?;
    if !status.success() {
        bail!("{name} login did not complete (exit {:?})", status.code());
    }
    println!("\nDone. Verify with `maw auth status`.");
    Ok(())
}

// ---------------------------------------------------------------- agents / tasks

fn cmd_agent(ctx: &Ctx, project: Option<&str>, cmd: AgentCmd) -> Result<()> {
    let p = resolve_project(ctx, project, true)?;
    let eng = open_utility_engine(ctx, &p)?;
    match cmd {
        AgentCmd::List => {
            println!(
                "{:<16} {:<10} {:<9} {:<11} {:<14} MODEL",
                "NAME", "ROLE", "PROVIDER", "PERMISSION", "STATE"
            );
            for a in anyhow_core(eng.list_agents())? {
                println!(
                    "{:<16} {:<10} {:<9} {:<11} {:<14} {}",
                    a.name,
                    a.role,
                    a.provider,
                    a.permission.as_str(),
                    a.state,
                    a.model.unwrap_or_else(|| "(default)".into())
                );
            }
        }
        AgentCmd::Add {
            name,
            provider,
            role,
            model,
            permission,
        } => {
            let provider: ProviderKind =
                provider.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let role: Role = role.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let permission: PermissionProfile =
                permission.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            if provider == ProviderKind::Mock {
                bail!("mock agents are only available through `maw demo`");
            }
            let a = anyhow_core(eng.add_agent(&name, provider, role, model, permission, None))?;
            println!(
                "Added {} ({} / {} / {})",
                a.name,
                a.provider,
                a.role,
                a.permission.as_str()
            );
        }
        AgentCmd::Status { name } => {
            let a = anyhow_core(eng.resolve_agent(&name))?;
            println!("{}", serde_json::to_string_pretty(&a)?);
        }
        AgentCmd::Role { name, role } => {
            let a = anyhow_core(eng.resolve_agent(&name))?;
            let role: Role = role.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let a = anyhow_core(eng.set_agent_role(&a.id, role))?;
            println!("{} is now {}", a.name, a.role);
        }
    }
    Ok(())
}

fn cmd_task(ctx: &Ctx, project: Option<&str>, cmd: TaskCmd) -> Result<()> {
    let p = resolve_project(ctx, project, false)?;
    let eng = open_utility_engine(ctx, &p)?;
    let agents = anyhow_core(eng.list_agents())?;
    match cmd {
        TaskCmd::List { status } => {
            let mut tasks = anyhow_core(eng.list_tasks())?;
            if let Some(s) = status {
                let s: TaskStatus = s.parse().map_err(|e: String| anyhow::anyhow!(e))?;
                tasks.retain(|t| t.status == s);
            }
            headless::print_tasks(&tasks, &agents);
        }
        TaskCmd::Show { id } => {
            let t = anyhow_core(eng.get_task(&id))?;
            let all = anyhow_core(eng.list_tasks())?;
            println!(
                "{} {}  [{}]  priority {}  weight {}",
                t.key,
                t.title,
                t.status,
                t.priority.as_str(),
                t.estimated_weight
            );
            println!(
                "owner: {}",
                t.owner_agent_id
                    .as_ref()
                    .and_then(|o| agents.iter().find(|a| &a.id == o))
                    .map(|a| a.name.as_str())
                    .unwrap_or("-")
            );
            println!(
                "deps: {}",
                t.dependencies
                    .iter()
                    .filter_map(|d| all.iter().find(|x| &x.id == d))
                    .map(|d| format!("{}({})", d.key, d.status))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!(
                "reported progress: {}% (unverified)   review: {}   attempts: {}",
                t.progress,
                t.review_status.as_str(),
                t.attempt_count
            );
            if let Some(r) = &t.blocked_reason {
                println!("blocked: {r}");
            }
            if !t.description.is_empty() {
                println!("\n{}", t.description);
            }
            for c in &t.acceptance_criteria {
                println!("  [ ] {c}");
            }
            println!("\nEvidence:");
            for e in &t.evidence {
                println!(
                    "  [{}] {} {}",
                    e.kind.as_str(),
                    if e.passed { "PASS" } else { "FAIL" },
                    e.summary
                );
            }
            println!("\nHistory:");
            for e in anyhow_core(eng.task_events(&id, 200))? {
                if !e.event_type.is_verbose() {
                    println!("  {}", headless::event_line(&e));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- run / tui / demo

async fn run_headless(
    ctx: &Ctx,
    p: &ProjectRef,
    providers: Providers,
    goal: Option<(String, Vec<String>)>,
    timeout: Option<Duration>,
) -> Result<()> {
    let rt = boot(ctx, p, providers, true).await?;
    app::write_runtime_info(ctx, p, &rt);
    let res = headless::drive(&rt, goal, timeout, false).await;
    let snap = rt.engine.snapshot().ok();
    app::clear_runtime_info(ctx, p);
    rt.shutdown().await;
    let outcome = res?;
    if let Some(s) = &snap {
        headless::print_summary(s);
    }
    match outcome {
        headless::Outcome::GoalCompleted => {
            println!("\nGoal completed and verified.");
            Ok(())
        }
        headless::Outcome::Stopped => {
            println!("\nStopped. All state is saved; `maw resume` continues.");
            Ok(())
        }
        headless::Outcome::TimedOut => bail!("timed out; state is saved, `maw resume` continues"),
        headless::Outcome::Stalled(why) => bail!("stalled: {why}"),
    }
}

async fn launch_tui(ctx: &Ctx, p: &ProjectRef, providers: Providers) -> Result<()> {
    let rt = boot(ctx, p, providers, true).await?;
    app::write_runtime_info(ctx, p, &rt);
    let opts = maw_tui::TuiOptions {
        monochrome: ctx.cfg.ui.monochrome || std::env::var_os("NO_COLOR").is_some(),
        mouse: ctx.cfg.ui.mouse,
        shutdown_on_quit: false,
        ..Default::default()
    };
    let res = maw_tui::run(rt.engine.clone(), rt.sup.clone(), opts).await;
    app::clear_runtime_info(ctx, p);
    rt.shutdown().await;
    res
}

async fn cmd_demo(headless_mode: bool, fast: bool) -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix("maw-demo-")
        .tempdir()?
        .keep();
    let repo = tmp.join("auth-demo");
    std::fs::create_dir_all(repo.join("src"))?;
    std::fs::write(
        repo.join("README.md"),
        "# auth-demo\n\nA tiny project for the MAW demo.\n",
    )?;
    std::fs::write(
        repo.join("src/lib.rs"),
        "// authentication module goes here\n",
    )?;
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=MAW",
            "-c",
            "user.email=maw@localhost",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .status()
            .context("git is required for the demo")?;
    }
    let dirs = config::Dirs {
        config: tmp.join("config"),
        data: tmp.join("data"),
    };
    let ctx = Ctx {
        dirs,
        cfg: config::MawConfig::default(),
        os: platform::detect_os(),
    };
    let p = ProjectRef {
        name: "maw-demo (mock agents)".into(),
        path: repo.clone(),
    };
    let script = MockScript {
        block_first_backend: true,
        reject_first_review: true,
        approval_demo: !headless_mode,
        step_delay: if fast {
            Duration::from_millis(15)
        } else {
            Duration::from_millis(900)
        },
        ..Default::default()
    };
    println!("MAW demo with SCRIPTED mock agents in {} — this exercises MAW itself, not Claude or Codex.", repo.display());
    let rt = boot(&ctx, &p, Providers::Mock(script), false).await?;
    for (name, role) in [
        ("claude-lead", Role::Leader),
        ("codex-backend", Role::Worker),
        ("codex-tests", Role::Worker),
        ("claude-review", Role::Reviewer),
    ] {
        anyhow_core(rt.engine.add_agent(
            name,
            ProviderKind::Mock,
            role,
            None,
            PermissionProfile::Balanced,
            None,
        ))?;
    }
    rt.sup.connect_all().await;
    let goal = "Implement a simple authentication module, including backend logic, validation, tests, and documentation.".to_string();
    if headless_mode {
        let outcome = headless::drive(
            &rt,
            Some((goal, vec!["tests pass".into(), "docs exist".into()])),
            Some(Duration::from_secs(120)),
            false,
        )
        .await;
        let snap = rt.engine.snapshot().ok();
        if let Some(s) = &snap {
            headless::print_summary(s);
        }
        let results = rt.sup.integrate_done_work().await;
        if let Ok(r) = results {
            println!("\nIntegration into maw/integration: {} branch(es)", r.len());
        }
        rt.shutdown().await;
        return match outcome? {
            headless::Outcome::GoalCompleted => Ok(()),
            _ => bail!("demo did not complete"),
        };
    }
    let engine = rt.engine.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let _ = engine.create_goal(&goal, vec!["tests pass".into(), "docs exist".into()]);
    });
    let opts = maw_tui::TuiOptions {
        monochrome: std::env::var_os("NO_COLOR").is_some(),
        mouse: true,
        shutdown_on_quit: false,
        ..Default::default()
    };
    let res = maw_tui::run(rt.engine.clone(), rt.sup.clone(), opts).await;
    rt.shutdown().await;
    println!("Demo data kept in {}", tmp.display());
    res
}

// ---------------------------------------------------------------- mcp / logs / config / tools

async fn cmd_mcp(ctx: &Ctx, project: Option<&str>, cmd: McpCmd) -> Result<()> {
    let p = resolve_project(ctx, project, true)?;
    match cmd {
        McpCmd::Status => {
            println!(
                "server: {} (tools: {})",
                maw_mcp::SERVER_NAME,
                maw_orchestrator::tool_specs().len()
            );
            match app::read_runtime_info(ctx, &p) {
                Some(i) => println!("running MAW process: pid {} at {}", i["pid"], i["mcp_url"].as_str().unwrap_or("?")),
                None => println!("no running MAW process for '{}' (the server starts with `maw`, `maw run` or `maw resume`)", p.name),
            }
            Ok(())
        }
        McpCmd::Serve {
            stdio,
            identity,
            port,
        } => {
            let eng = open_utility_engine(ctx, &p)?;
            let actor = if identity == "user" {
                Actor::User
            } else {
                let a = anyhow_core(eng.resolve_agent(&identity))?;
                Actor::Agent {
                    id: a.id,
                    role: a.role,
                }
            };
            if stdio {
                // stdout belongs to the protocol: logs already go to the file
                maw_mcp::serve_stdio(eng, actor).await
            } else {
                let Actor::Agent { id, .. } = &actor else {
                    bail!("HTTP mode needs `--as <agent name>`: HTTP clients authenticate with a per-agent token");
                };
                let token = eng.mint_token(id);
                let srv = maw_mcp::serve_http(eng, format!("127.0.0.1:{port}").parse()?).await?;
                println!("MCP server for agent '{identity}':\n  url:   {}\n  token: {token}   (send as `Authorization: Bearer <token>`)\nCtrl+C to stop. Note: standalone servers do not wake a running MAW scheduler.", srv.url);
                tokio::signal::ctrl_c().await?;
                srv.stop().await;
                Ok(())
            }
        }
    }
}

async fn cmd_logs(
    ctx: &Ctx,
    project: Option<&str>,
    lines: usize,
    events: bool,
    follow: bool,
) -> Result<()> {
    if events {
        let p = resolve_project(ctx, project, false)?;
        let eng = open_utility_engine(ctx, &p)?;
        let mut last = 0i64;
        let evs = anyhow_core(eng.read(|db| db.recent_events(eng.project_id(), lines)))?;
        for e in &evs {
            println!("{}", headless::event_line(e));
            last = e.sequence;
        }
        while follow && !follow_done() {
            tokio::time::sleep(Duration::from_millis(700)).await;
            for e in anyhow_core(eng.read(|db| db.events_since(eng.project_id(), last, 500)))? {
                println!("{}", headless::event_line(&e));
                last = e.sequence;
            }
        }
        return Ok(());
    }
    let path = ctx.dirs.log_file();
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no log file at {}", path.display()))?;
    let all: Vec<&str> = text.lines().collect();
    for l in &all[all.len().saturating_sub(lines)..] {
        println!("{l}");
    }
    let mut pos = text.len() as u64;
    while follow && !follow_done() {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let t = std::fs::read_to_string(&path).unwrap_or_default();
        if (t.len() as u64) < pos {
            pos = 0; // rotated
        }
        if (t.len() as u64) > pos {
            print!("{}", &t[pos as usize..]);
            pos = t.len() as u64;
        }
    }
    Ok(())
}

fn cmd_config(ctx: &Ctx, cmd: Option<ConfigCmd>) -> Result<()> {
    match cmd.unwrap_or(ConfigCmd::Show) {
        ConfigCmd::Path => {
            println!(
                "config:   {}\ndata:     {}\nlog:      {}",
                ctx.dirs.config_file().display(),
                ctx.dirs.data.display(),
                ctx.dirs.log_file().display()
            );
        }
        ConfigCmd::Show => print!("{}", toml::to_string_pretty(&ctx.cfg)?),
        ConfigCmd::Set { assignment } => {
            let (key, value) = assignment
                .split_once('=')
                .context("use KEY=VALUE, e.g. orchestrator.max_concurrency=3")?;
            let mut doc: toml::Value = toml::Value::try_from(&ctx.cfg)?;
            let parts: Vec<&str> = key.split('.').collect();
            let mut cur = &mut doc;
            for (i, part) in parts.iter().enumerate() {
                let tbl = cur.as_table_mut().context("not a table")?;
                if i == parts.len() - 1 {
                    let parsed = value
                        .parse::<toml::Value>()
                        .unwrap_or_else(|_| toml::Value::String(value.to_string()));
                    tbl.insert(part.to_string(), parsed);
                    break;
                }
                cur = tbl
                    .entry(part.to_string())
                    .or_insert_with(|| toml::Value::Table(Default::default()));
            }
            let new: config::MawConfig = doc
                .try_into()
                .context("that value is not valid for this setting")?;
            new.save(&ctx.dirs)?;
            println!("Saved {} = {}", key, value);
        }
    }
    Ok(())
}

fn print_tools(markdown: bool) {
    let specs = maw_orchestrator::tool_specs();
    if markdown {
        println!("# MAW MCP tool reference\n\nServer `maw-coordination` (exposed to agents as `maw`). {} tools. Generated by `maw tools --markdown`.\n", specs.len());
        for s in &specs {
            let roles = if s.roles.is_empty() {
                "any role".to_string()
            } else {
                s.roles
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!(
                "## `{}`\n\n{}\n\n- **Roles:** {}\n- **Read-only:** {}\n\n```json\n{}\n```\n",
                s.name,
                s.description,
                roles,
                s.read_only,
                serde_json::to_string_pretty(&(s.schema)()).unwrap_or_default()
            );
        }
    } else {
        for s in &specs {
            let roles = if s.roles.is_empty() {
                "any".to_string()
            } else {
                s.roles
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            };
            println!("{:<28} [{:<18}] {}", s.name, roles, s.description);
        }
    }
}

#[allow(dead_code)]
fn _unused(_: PathBuf) {}

/// `--follow` runs until the process is interrupted (Ctrl+C); this indirection keeps the loop explicit.
fn follow_done() -> bool {
    false
}
