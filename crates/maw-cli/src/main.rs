mod app;
mod config;
mod doctor;
mod headless;
mod lab;
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
    /// Re-check providers and local models, and ask a running MAW to reconnect agents that are down
    Refresh,
    /// Learn how language models work: train a tiny Transformer on your own text and watch it in 3D
    Lab {
        #[command(subcommand)]
        cmd: Option<lab::LabCmd>,
    },
    /// Where agent worktrees live: project (next to the code), wsl (Linux disk, fast git) or windows
    Workspace {
        /// project | wsl | windows (without it: show the current choice); or `allow` / `deny` to
        /// let agents also write to a folder outside their worktree (give the folder next)
        mode: Option<String>,
        /// The folder for `allow` / `deny`, e.g. /mnt/c/Users/me/download or C:/Users/me/download
        dir: Option<String>,
    },
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
    /// List the models each provider offers (Codex: queried live from your account)
    Models {
        /// claude | codex | ollama (default: all)
        #[arg(long)]
        provider: Option<String>,
        #[command(subcommand)]
        cmd: Option<ModelsCmd>,
    },
    /// Token usage per agent and task, as reported by the providers
    Usage {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
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
        /// claude | codex | ollama (a local model run by MAW)
        #[arg(long)]
        provider: String,
        /// leader | worker | reviewer | specialist
        #[arg(long, default_value = "worker")]
        role: String,
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort, e.g. low | medium | high | xhigh | max (provider/model specific; see `maw models`)
        #[arg(long)]
        effort: Option<String>,
        /// Do not offer to log the provider in after adding the agent
        #[arg(long)]
        no_login: bool,
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
    /// Set the model used the next time the agent connects. Without a value, pick from the list.
    Model {
        name: String,
        /// a model id from `maw models`, or `default`
        model: Option<String>,
    },
    /// Set the reasoning effort used the next time the agent connects. Without a value, pick from the list.
    Effort {
        name: String,
        /// e.g. low | medium | high | xhigh | max, or `default`
        level: Option<String>,
    },
    /// Remove an agent from the project (its git worktree is left on disk)
    Remove {
        name: String,
        /// Release the agent's unfinished tasks instead of refusing
        #[arg(long)]
        force: bool,
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Set the permission profile used the next time the agent connects: safe | balanced | autonomous
    Permission {
        name: String,
        profile: String,
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
        Some(Cmd::Agent { cmd }) => cmd_agent(&ctx, project_arg.as_deref(), cmd).await,
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
                    a.name,
                    a.role.as_str(),
                    a.provider.as_str(),
                    a.state.as_str(),
                    t
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
        Some(Cmd::Refresh) => cmd_refresh(&ctx, project_arg.as_deref()).await,
        Some(Cmd::Lab { cmd }) => lab::run(cmd),
        Some(Cmd::Workspace { mode, dir }) => {
            cmd_workspace(&ctx, project_arg.as_deref(), mode, dir)
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
        Some(Cmd::Models {
            cmd: Some(ModelsCmd::Pull { name }),
            ..
        }) => cmd_models_pull(&ctx, name).await,
        Some(Cmd::Models {
            provider,
            cmd: None,
        }) => cmd_models(&ctx, provider).await,
        Some(Cmd::Usage { json }) => cmd_usage(&ctx, project_arg.as_deref(), json),
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
                ("ollama", providers::ollama(&ctx.cfg.ollama, None)),
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

async fn cmd_agent(ctx: &Ctx, project: Option<&str>, cmd: AgentCmd) -> Result<()> {
    let p = resolve_project(ctx, project, true)?;
    let eng = open_utility_engine(ctx, &p)?;
    match cmd {
        AgentCmd::List => {
            println!(
                "{:<16} {:<10} {:<9} {:<11} {:<14} {:<22} EFFORT",
                "NAME", "ROLE", "PROVIDER", "PERMISSION", "STATE", "MODEL"
            );
            for a in anyhow_core(eng.list_agents())? {
                println!(
                    "{:<16} {:<10} {:<9} {:<11} {:<14} {:<22} {}",
                    a.name,
                    a.role.as_str(),
                    a.provider.as_str(),
                    a.permission.as_str(),
                    a.state.as_str(),
                    a.model.unwrap_or_else(|| "(default)".into()),
                    a.effort.unwrap_or_else(|| "(default)".into())
                );
            }
        }
        AgentCmd::Add {
            name,
            provider,
            role,
            model,
            effort,
            no_login,
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
            if let Some(m) = &model {
                let list = models_for(ctx, provider).await;
                anyhow_core(maw_orchestrator::models::check_model(&list, provider, m))?;
            }
            if let Some(e) = &effort {
                let norm = maw_core::normalize_effort(e).map_err(|e| anyhow::anyhow!(e))?;
                if let Some(level) = norm {
                    let list = models_for(ctx, provider).await;
                    let allowed = maw_orchestrator::models::efforts_for_provider(
                        provider,
                        &list,
                        model.as_deref(),
                    );
                    if !allowed.contains(&level) {
                        bail!(
                            "{}",
                            no_such_effort(provider, model.as_deref(), &level, &allowed)
                        );
                    }
                }
            }
            let mut a = anyhow_core(eng.add_agent(&name, provider, role, model, permission, None))?;
            if let Some(e) = effort {
                a = anyhow_core(eng.set_agent_effort(&a.id, &e))?;
            }
            println!(
                "Added {} ({} / {} / {}; model {}, effort {})",
                a.name,
                a.provider,
                a.role,
                a.permission.as_str(),
                a.model.as_deref().unwrap_or("default"),
                a.effort.as_deref().unwrap_or("default")
            );
            if provider == ProviderKind::Ollama && role == Role::Leader {
                println!("Note: the Leader needs many MAW tools and long plans; small local models usually cannot do that reliably. A local model works best as a Worker (a hosted model as Leader).");
            }
            if !no_login {
                offer_login(ctx, provider).await?;
            }
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
        AgentCmd::Model { name, model } => {
            let a = anyhow_core(eng.resolve_agent(&name))?;
            let list = models_for(ctx, a.provider).await;
            let chosen = match model {
                Some(m) => m,
                None => {
                    let mut opts = vec![(
                        "default".to_string(),
                        "the provider's own default".to_string(),
                    )];
                    opts.extend(list.iter().map(|m| {
                        (
                            m.id.clone(),
                            format!(
                                "{}{}",
                                m.description,
                                if m.is_default {
                                    " (provider default)"
                                } else {
                                    ""
                                }
                            ),
                        )
                    }));
                    pick(
                        &format!("Model for {} ({}):", a.name, a.provider),
                        &opts,
                        a.model.as_deref().unwrap_or("default"),
                    )?
                }
            };
            anyhow_core(maw_orchestrator::models::check_model(
                &list, a.provider, &chosen,
            ))?;
            let a = anyhow_core(eng.set_agent_model(&a.id, Some(chosen)))?;
            println!("{} model = {} (applies the next time it connects; in the TUI use `M` to apply it now to an idle agent)", a.name, a.model.as_deref().unwrap_or("(provider default)"));
        }
        AgentCmd::Effort { name, level } => {
            let a = anyhow_core(eng.resolve_agent(&name))?;
            let list = models_for(ctx, a.provider).await;
            let allowed = maw_orchestrator::models::efforts_for_provider(
                a.provider,
                &list,
                a.model.as_deref(),
            );
            let chosen = match level {
                Some(l) => l,
                None => {
                    let mut opts =
                        vec![("default".to_string(), "the model's own default".to_string())];
                    opts.extend(allowed.iter().map(|e| (e.clone(), String::new())));
                    pick(
                        &format!(
                            "Effort for {} (model {}):",
                            a.name,
                            a.model.as_deref().unwrap_or("default")
                        ),
                        &opts,
                        a.effort.as_deref().unwrap_or("default"),
                    )?
                }
            };
            let norm = maw_core::normalize_effort(&chosen).map_err(|e| anyhow::anyhow!(e))?;
            if let Some(l) = &norm {
                if !allowed.contains(l) {
                    bail!(
                        "{}",
                        no_such_effort(a.provider, a.model.as_deref(), l, &allowed)
                    );
                }
            }
            let a = anyhow_core(eng.set_agent_effort(&a.id, &chosen))?;
            println!(
                "{} effort = {} (applies the next time it connects)",
                a.name,
                a.effort.as_deref().unwrap_or("provider default")
            );
        }
        AgentCmd::Remove { name, force, yes } => {
            use std::io::{BufRead, IsTerminal, Write};
            let a = anyhow_core(eng.resolve_agent(&name))?;
            if app::read_runtime_info(ctx, &p).is_some() {
                bail!("MAW is running for this project: remove '{}' from its TUI (press X on the agent) or stop MAW first, so the live session is stopped cleanly", a.name);
            }
            let blockers = anyhow_core(eng.removal_blockers(&a.id))?;
            if !blockers.is_empty() && !force {
                bail!(
                    "{} has unfinished work: {}. Re-run with --force to release its tasks",
                    a.name,
                    blockers.join("; ")
                );
            }
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("refusing to remove without confirmation: pass --yes");
                }
                print!("Remove {} ({} / {})? [y/N] ", a.name, a.provider, a.role);
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("Kept {}.", a.name);
                    return Ok(());
                }
            }
            let r = anyhow_core(eng.remove_agent(&a.id, force))?;
            println!("Removed {}.", r.name);
            if !r.released_tasks.is_empty() {
                println!("Released tasks: {}", r.released_tasks.join(", "));
            }
            if let Some(w) = r.worktree {
                println!("Its git worktree was left in place: {w}  (delete it yourself if you no longer need it)");
            }
        }
        AgentCmd::Permission { name, profile } => {
            let a = anyhow_core(eng.resolve_agent(&name))?;
            let profile: PermissionProfile =
                profile.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let a = anyhow_core(eng.set_agent_permission(&a.id, profile))?;
            println!(
                "{} permission = {} (applies the next time it connects)",
                a.name,
                a.permission.as_str()
            );
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
        on_board: Some(std::sync::Arc::new(platform::board_console)),
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
        on_board: Some(std::sync::Arc::new(platform::board_console)),
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

#[derive(Subcommand)]
enum ModelsCmd {
    /// Download a model for local agents (Ollama). Without a name, pick from suggestions.
    Pull {
        /// e.g. qwen3:8b (limited to 8B parameters / ~6.5 GB by default; see [ollama] in the config)
        name: Option<String>,
    },
}

async fn cmd_models_pull(ctx: &Ctx, name: Option<String>) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let p = providers::ollama(&ctx.cfg.ollama, None);
    let name = match name {
        Some(n) => n,
        None => {
            if !std::io::stdin().is_terminal() {
                bail!("give a model name, e.g. `maw models pull qwen3:8b` (see `maw models pull` in a terminal for suggestions)");
            }
            let installed: Vec<String> = p
                .list_models()
                .await
                .map(|l| l.into_iter().map(|m| m.id).collect())
                .unwrap_or_default();
            let sugg = p.pull_suggestions();
            let limit = |v: f64, unit: &str| {
                if v > 0.0 {
                    format!("{v}{unit}")
                } else {
                    "no limit".to_string()
                }
            };
            println!(
                "Download limits: parameters {}, size {} (installed ones are marked):",
                limit(ctx.cfg.ollama.max_pull_params_b, "B"),
                limit(ctx.cfg.ollama.max_pull_gb, " GB")
            );
            for (i, m) in sugg.iter().enumerate() {
                let mark = if installed.contains(&m.id) {
                    "installed"
                } else {
                    ""
                };
                println!("  {:>2}) {:<18} {} {}", i + 1, m.id, m.description, mark);
            }
            print!("Number, or type a model name (Enter = cancel): ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            let t = line.trim();
            if t.is_empty() {
                return Ok(());
            }
            match t.parse::<usize>() {
                Ok(n) => sugg
                    .get(n.wrapping_sub(1))
                    .map(|m| m.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("{n} is not in the list"))?,
                Err(_) => t.to_string(),
            }
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<maw_core::provider::PullProgress>(64);
    let printer = tokio::spawn(async move {
        let mut last = String::new();
        while let Some(pr) = rx.recv().await {
            let line = match (pr.completed * 100).checked_div(pr.total) {
                Some(pct) => format!(
                    "  {:>3}%  {:.2} / {:.2} GB  {}",
                    pct,
                    pr.completed as f64 / 1e9,
                    pr.total as f64 / 1e9,
                    pr.status.split(' ').next().unwrap_or("")
                ),
                None => format!("  {}", pr.status),
            };
            if line != last {
                print!("\r{line:<70}");
                let _ = std::io::stdout().flush();
                last = line;
            }
        }
        println!();
    });
    println!("Downloading {name} ...");
    let res = p.pull_model(&name, tx).await;
    let _ = printer.await;
    match res {
        Ok(msg) => {
            println!("✓ {msg}");
            println!("  use it: maw agent add --name local --provider ollama --model {name}");
            Ok(())
        }
        Err(e) => bail!("{e}"),
    }
}

fn no_such_effort(
    provider: ProviderKind,
    model: Option<&str>,
    level: &str,
    allowed: &[String],
) -> String {
    let model = model.unwrap_or("default");
    if allowed.is_empty() {
        format!("model '{model}' ({provider}) has no effort setting; leave effort at `default`")
    } else {
        format!(
            "'{level}' is not an effort level for {provider} (model {model}). Choose one of: default, {}",
            allowed.join(", ")
        )
    }
}

async fn cmd_models(ctx: &Ctx, provider: Option<String>) -> Result<()> {
    let want: Vec<ProviderKind> = match provider {
        Some(p) => vec![p
            .parse::<ProviderKind>()
            .map_err(|e: String| anyhow::anyhow!(e))?],
        None => vec![
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Ollama,
        ],
    };
    for kind in want {
        let p = match kind {
            ProviderKind::Claude => providers::claude(ctx.cfg.claude.path.clone()),
            ProviderKind::Codex => providers::codex(ctx.cfg.codex.path.clone()),
            ProviderKind::Ollama => providers::ollama(&ctx.cfg.ollama, None),
            ProviderKind::Mock => continue,
        };
        println!("{kind}:");
        match tokio::time::timeout(Duration::from_secs(45), p.list_models()).await {
            Ok(Ok(models)) => {
                for m in models {
                    let eff = if m.reasoning_efforts.is_empty()
                        || m.reasoning_efforts == [maw_orchestrator::models::NO_EFFORT]
                    {
                        String::new()
                    } else {
                        format!("  [effort: {}]", m.reasoning_efforts.join("/"))
                    };
                    println!(
                        "  {:<22}{} {}{}",
                        m.id,
                        if m.is_default { " (default)" } else { "" },
                        m.description,
                        eff
                    );
                }
                println!("  use: maw agent model <name> <id>   (or `default`)");
            }
            Ok(Err(e)) => println!("  unavailable: {e}"),
            Err(_) => println!("  timed out"),
        }
    }
    Ok(())
}

/// The provider's model list (empty when the provider cannot be queried).
async fn models_for(ctx: &Ctx, kind: ProviderKind) -> Vec<maw_core::provider::ModelChoice> {
    let p = match kind {
        ProviderKind::Claude => providers::claude(ctx.cfg.claude.path.clone()),
        ProviderKind::Codex => providers::codex(ctx.cfg.codex.path.clone()),
        ProviderKind::Ollama => providers::ollama(&ctx.cfg.ollama, None),
        ProviderKind::Mock => return vec![],
    };
    match tokio::time::timeout(Duration::from_secs(45), p.list_models()).await {
        Ok(Ok(list)) => list,
        _ => vec![],
    }
}

/// Numbered pick-list on the terminal, so nothing has to be typed from memory.
fn pick(title: &str, options: &[(String, String)], current: &str) -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!("no value given and no terminal to ask on; pass it explicitly (see `maw models`)");
    }
    println!("{title}");
    for (i, (id, desc)) in options.iter().enumerate() {
        let mark = if id == current { "*" } else { " " };
        println!("  {mark}{:>2}) {:<22} {}", i + 1, id, desc);
    }
    print!("Number (Enter = keep current): ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let t = line.trim();
    if t.is_empty() {
        return Ok(current.to_string());
    }
    let n: usize = t
        .parse()
        .map_err(|_| anyhow::anyhow!("'{t}' is not a number from the list"))?;
    options
        .get(n.wrapping_sub(1))
        .map(|(id, _)| id.clone())
        .ok_or_else(|| anyhow::anyhow!("{n} is not in the list (1..={})", options.len()))
}

/// After adding an agent: if its provider is not logged in, offer the provider's official login
/// right away (interactive terminals only). Credentials never pass through MAW.
async fn offer_login(ctx: &Ctx, kind: ProviderKind) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let p = match kind {
        ProviderKind::Claude => providers::claude(ctx.cfg.claude.path.clone()),
        ProviderKind::Codex => providers::codex(ctx.cfg.codex.path.clone()),
        ProviderKind::Ollama => return Ok(()), // local server: nothing to log in to
        ProviderKind::Mock => return Ok(()),
    };
    match p.auth_status().await {
        Ok(a) if a.is_authenticated() => {
            println!("✓ {kind} is already logged in.");
            return Ok(());
        }
        Ok(maw_core::provider::AuthStatus::Unknown { reason }) => {
            println!("{kind}: login state unknown ({reason}); check with `maw auth status`.");
            return Ok(());
        }
        _ => {}
    }
    let Some(cmd) = p.login_command() else {
        println!("{kind} is not logged in; run `maw auth {kind}`.");
        return Ok(());
    };
    if !std::io::stdin().is_terminal() {
        println!("{kind} is not logged in; run `maw auth {kind}`.");
        return Ok(());
    }
    print!("{kind} is not logged in. Log in now? [Y/n] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no") {
        println!("Skipped. Run `maw auth {kind}` before connecting this agent.");
        return Ok(());
    }
    println!(
        "Starting the official login ({}). MAW does not read or store your credentials.\n",
        cmd.join(" ")
    );
    let status = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .with_context(|| format!("running {}", cmd[0]))?;
    match (status.success(), p.auth_status().await) {
        (true, Ok(a)) if a.is_authenticated() => println!("\n✓ {kind} is now logged in."),
        (true, _) => println!("\nLogin finished; verify with `maw auth status`."),
        (false, _) => println!(
            "\nLogin did not complete (exit {:?}); retry with `maw auth {kind}`.",
            status.code()
        ),
    }
    Ok(())
}

fn cmd_usage(ctx: &Ctx, project: Option<&str>, json: bool) -> Result<()> {
    use maw_tui::usage::{derive_usage, fmt_cost, fmt_tokens};
    let p = resolve_project(ctx, project, false)?;
    let eng = open_utility_engine(ctx, &p)?;
    let snap = anyhow_core(eng.snapshot())?;
    let events = anyhow_core(
        eng.read(|db| db.events_of_kind(eng.project_id(), EventKind::AgentUsage, 5000)),
    )?;
    let r = derive_usage(&snap.agents, &snap.tasks, &events, chrono::Utc::now());
    if json {
        let agents: Vec<_> = r
            .agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "agent": a.name, "provider": a.provider.as_str(), "model": a.model, "effort": a.effort,
                    "input_tokens": a.input, "output_tokens": a.output, "total_tokens": a.total,
                    "cost_usd_client_estimate": a.cost_usd, "share": a.share,
                    "tokens_per_minute_5m": a.rate_per_min, "seconds_since_last_report": a.last_report_secs,
                })
            })
            .collect();
        let tasks: Vec<_> = r
            .tasks
            .iter()
            .map(|t| serde_json::json!({ "task": t.key, "title": t.title, "tokens": t.tokens }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "source": "reported by the providers; never estimated by MAW",
                "total_tokens": r.total_tokens, "input_tokens": r.total_input, "output_tokens": r.total_output,
                "cost_usd_client_estimate": r.total_cost_usd, "agents_reporting": r.agents_reporting, "agents_total": r.agents_total,
                "unattributed_tokens": r.unattributed_tokens, "agents": agents, "tasks": tasks,
            }))?
        );
        return Ok(());
    }
    println!(
        "Token usage for '{}' (as reported by the providers; MAW never estimates tokens)\n",
        p.name
    );
    println!(
        "{:<16} {:<8} {:<20} {:>8} {:>8} {:>8} {:>6} {:>9}",
        "AGENT", "PROVIDER", "MODEL", "INPUT", "OUTPUT", "TOTAL", "SHARE", "COST"
    );
    for a in &r.agents {
        let n = |v: Option<u64>| v.map(fmt_tokens).unwrap_or_else(|| "-".into());
        println!(
            "{:<16} {:<8} {:<20} {:>8} {:>8} {:>8} {:>5.0}% {:>9}",
            a.name,
            a.provider.as_str(),
            a.model.as_deref().unwrap_or("default"),
            n(a.input),
            n(a.output),
            n(a.total),
            a.share * 100.0,
            a.cost_usd.map(fmt_cost).unwrap_or_else(|| "-".into())
        );
    }
    println!(
        "\nTotal: {} tokens (in {} / out {}){}  ·  {}/{} agents have reported usage",
        fmt_tokens(r.total_tokens),
        fmt_tokens(r.total_input),
        fmt_tokens(r.total_output),
        r.total_cost_usd
            .map(|c| format!(
                "  ·  cost {} (Claude Code's client-side estimate)",
                fmt_cost(c)
            ))
            .unwrap_or_default(),
        r.agents_reporting,
        r.agents_total
    );
    if !r.tasks.is_empty() {
        println!("\nBy task (increments recorded while the task was the agent's current one):");
        for t in r.tasks.iter().take(10) {
            println!("  {:<6} {:>8}  {}", t.key, fmt_tokens(t.tokens), t.title);
        }
        if r.unattributed_tokens > 0 {
            println!(
                "  (+ {} tokens outside any task)",
                fmt_tokens(r.unattributed_tokens)
            );
        }
    }
    println!("\nNote: totals are each provider's cumulative figure for its current session. Codex reports tokens only; provider plan/quota limits are not exposed.");
    Ok(())
}

/// `maw refresh`: look at the environment again (providers, the Ollama server and its models, the
/// agents of this project) and, when a MAW is running for the project, ask it to reconnect the
/// agents that are down. Nothing is started from here; starting is the running MAW's job.
async fn cmd_refresh(ctx: &Ctx, project_arg: Option<&str>) -> Result<()> {
    let p = resolve_project(ctx, project_arg, false)?;
    println!("Providers:");
    for (name, prov) in [
        ("claude", providers::claude(ctx.cfg.claude.path.clone())),
        ("codex", providers::codex(ctx.cfg.codex.path.clone())),
        ("ollama", providers::ollama(&ctx.cfg.ollama, None)),
    ] {
        match tokio::time::timeout(Duration::from_secs(15), prov.detect()).await {
            Ok(Ok(i)) if i.installed => println!(
                "  {name:<7} ready{}",
                i.version
                    .map(|v| format!(" (version {v})"))
                    .unwrap_or_default()
            ),
            Ok(Ok(i)) => println!(
                "  {name:<7} not available{}",
                i.notes
                    .first()
                    .map(|n| format!(": {n}"))
                    .unwrap_or_default()
            ),
            Ok(Err(e)) => println!("  {name:<7} error: {e}"),
            Err(_) => println!("  {name:<7} did not answer in time"),
        }
    }
    let local = providers::ollama(&ctx.cfg.ollama, None);
    match tokio::time::timeout(Duration::from_secs(15), local.list_models()).await {
        Ok(Ok(models)) if !models.is_empty() => println!(
            "Local models: {}",
            models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Ok(Ok(_)) => println!("Local models: none installed (maw models pull)"),
        _ => println!("Local models: server not reachable"),
    }

    if !ctx.dirs.db_path(&p.name).exists() {
        println!(
            "{} is not a MAW project yet (maw init), so there are no agents to refresh.",
            p.path.display()
        );
        return Ok(());
    }
    let eng = open_utility_engine(ctx, &p)?;
    println!("Agents of '{}':", p.name);
    for a in eng.list_agents().map_err(|e| anyhow::anyhow!("{e}"))? {
        println!(
            "  {:<14} {:<7} {:<18}{}",
            a.name,
            a.provider,
            a.state,
            a.last_error.map(|e| format!(" ! {e}")).unwrap_or_default()
        );
    }
    match app::read_runtime_info(ctx, &p) {
        Some(_) => {
            anyhow_core(eng.store().write(|db| {
                db.kv_set(
                    eng.project_id(),
                    "refresh_request",
                    &chrono::Utc::now().to_rfc3339(),
                )
            }))?;
            println!("Asked the running MAW to reconnect agents that are down (it starts the local model server if it has to; agents you stopped yourself stay stopped).");
        }
        None => println!(
            "No MAW is running for '{}': agents connect when you open it.",
            p.name
        ),
    }
    Ok(())
}

/// `maw workspace [project|wsl|windows]`: show or change where agent worktrees are created.
fn cmd_workspace(
    ctx: &Ctx,
    project_arg: Option<&str>,
    mode: Option<String>,
    dir: Option<String>,
) -> Result<()> {
    let p = resolve_project(ctx, project_arg, false)?;
    // folder: the folder the project's agents work in (the user decides directly; no approval needed)
    if mode.as_deref().map(str::to_lowercase).as_deref() == Some("folder") {
        let eng = open_utility_engine(ctx, &p)?;
        let Some(raw) = dir else {
            bail!("give the folder, or `reset` to use the project folder again: maw workspace folder <folder>");
        };
        let blockers = anyhow_core(eng.workspace_change_blockers())?;
        if !blockers.is_empty() {
            bail!(
                "work is still in flight ({}); finish, release or cancel those tasks first",
                blockers.join(", ")
            );
        }
        if raw.trim().eq_ignore_ascii_case("reset") {
            anyhow_core(eng.set_work_dir(None))?;
            println!(
                "Agents work in the project folder again: {}",
                p.path.display()
            );
        } else {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let folder = maw_orchestrator::workspace::normalize_folder(
                &raw,
                home.as_deref(),
                matches!(ctx.os, platform::Os::Wsl),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let note = maw_orchestrator::workspace::prepare_folder(&folder)
                .map_err(|e| anyhow::anyhow!(e))?;
            anyhow_core(eng.set_work_dir(Some(&folder)))?;
            println!("Agents now work in {}", folder.display());
            println!("  ({note})");
        }
        match app::read_runtime_info(ctx, &p) {
            Some(_) => println!("A MAW is running for this project: restart it so its agents start in the new folder."),
            None => println!("It applies the next time MAW starts."),
        }
        return Ok(());
    }
    // allow / deny: the folders agents may write to besides their worktree
    if let Some(verb) = mode.as_deref().map(str::to_lowercase) {
        if verb == "allow" || verb == "deny" {
            let Some(raw) = dir else {
                bail!("give the folder: maw workspace {verb} <folder>");
            };
            let norm = platform::normalize_user_path(&raw, ctx.os);
            let mut cfg = ctx.cfg.clone();
            if verb == "allow" {
                let home = std::env::var_os("HOME").map(PathBuf::from);
                let real =
                    app::check_extra_dir(&norm, home.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
                let s = real.to_string_lossy().to_string();
                if !cfg.orchestrator.extra_dirs.contains(&s) {
                    cfg.orchestrator.extra_dirs.push(s.clone());
                }
                cfg.save(&ctx.dirs)?;
                println!("Agents that do work (Workers, Specialists) may now also write to {s}.");
                println!("It applies the next time each agent connects (restart MAW or /refresh after stopping the agent). Claude Code gets --add-dir, Codex gets it as a writable root of its sandbox, local models write there after you approve.");
            } else {
                let want = std::fs::canonicalize(&norm).unwrap_or(norm.clone());
                let before = cfg.orchestrator.extra_dirs.len();
                cfg.orchestrator.extra_dirs.retain(|d| {
                    let d = platform::normalize_user_path(d, ctx.os);
                    d != norm && std::fs::canonicalize(&d).map(|c| c != want).unwrap_or(true)
                });
                if cfg.orchestrator.extra_dirs.len() == before {
                    bail!("{} is not in the list of allowed folders", norm.display());
                }
                cfg.save(&ctx.dirs)?;
                println!(
                    "{} is no longer allowed (applies the next time each agent connects).",
                    norm.display()
                );
            }
            return Ok(());
        }
    }
    let (wsl, windows) = app::worktree_bases(ctx, &p);
    let describe = |r: &Result<PathBuf, String>| match r {
        Ok(d) => d.display().to_string(),
        Err(why) => format!("not available: {why}"),
    };
    if let Some(m) = mode {
        let m = m.trim().to_lowercase();
        let ok = match m.as_str() {
            "project" => Ok(()),
            "wsl" => wsl.as_ref().map(|_| ()).map_err(|e| e.clone()),
            "windows" => windows.as_ref().map(|_| ()).map_err(|e| e.clone()),
            other => bail!("unknown location '{other}': use project, wsl or windows"),
        };
        if let Err(why) = ok {
            bail!("cannot use '{m}': {why}");
        }
        let mut cfg = ctx.cfg.clone();
        cfg.orchestrator.worktree_location = m.clone();
        cfg.save(&ctx.dirs)?;
        println!("Worktrees of new work now go to: {m}");
        println!("Agents that already have a worktree move it the next time they connect with no work in flight (their changes are committed on their own branch first). A running MAW needs a restart, or /workspace {m} in its message box.");
    }
    let current = ctx.cfg.orchestrator.worktree_location.clone();
    let shown = if current.is_empty() {
        "project".to_string()
    } else {
        current
    };
    println!("Project '{}' ({})", p.name, p.path.display());
    println!(
        "  project  {}",
        p.path.join(".maw").join("worktrees").display()
    );
    println!("  wsl      {}", describe(&wsl));
    println!("  windows  {}", describe(&windows));
    println!("Saved choice: {shown} (set with `maw workspace <project|wsl|windows>`)");
    if let Ok(eng) = open_utility_engine(ctx, &p) {
        match eng.work_dir() {
            Some(d) => println!("Working folder: {} (the project was started in {}; `maw workspace folder reset` goes back)", d.display(), p.path.display()),
            None => println!("Working folder: {} (the folder MAW was started in; `maw workspace folder <folder>` or the Leader's proposal changes it)", p.path.display()),
        }
    }
    let roots = app::workspace_roots(ctx);
    if !roots.is_empty() {
        println!("The Leader may propose a project folder inside (you approve each one):");
        for r in &roots {
            println!("  {}", r.display());
        }
    }
    let (allowed, refused) = app::allowed_extra_dirs(ctx);
    if allowed.is_empty() && refused.is_empty() {
        println!("Folders agents may also write to: none (add one with `maw workspace allow <folder>`; only you can).");
    } else {
        println!("Folders agents may also write to:");
        for d in &allowed {
            println!("  {}", d.display());
        }
        for why in &refused {
            println!("  (ignored) {why}");
        }
    }
    Ok(())
}
