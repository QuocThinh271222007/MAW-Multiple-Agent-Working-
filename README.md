# MAW — Multiple Agent Working

A terminal-native control room that makes several independent coding agents (**Claude Code** and **OpenAI Codex**) work as one engineering team: a **Leader** plans and verifies, **Workers** implement in isolated git worktrees, optional **Reviewers** review — all coordinating through a built-in **MCP server**, with every decision, message and task transition visible in a keyboard-driven TUI and persisted in SQLite.

> Status: working vertical slice. See [`docs/ROADMAP.md`](docs/ROADMAP.md) for exactly what is implemented, what was verified against real providers, and what is not done. Nothing below is claimed beyond that.

## What it does

- **Real agents, no scraping.** Codex via its App Server (stdio JSON-RPC), Claude Code via its stream-json CLI. Provider events are normalized into one event stream; agents talk to MAW through typed MCP tools, never by reading terminal text.
- **Leader / Worker / Reviewer / Specialist roles**, assignable to either provider. Identity and role are minted by MAW (per-agent bearer token), never self-declared.
- **A real task engine**: dependency DAG (cycle-checked), atomic claim/assign, leases + bounded retries, review → verification gates. A task is **DONE only after verification with passing evidence** — an agent saying "done" or a process exiting never completes it.
- **Honest progress**: *verified* weight is shown separately from agent-reported, blocked, failed and unknown work. Adding scope visibly lowers the percentage and records the plan revision.
- **Isolation**: each Worker gets its own git worktree and branch (`.maw/worktrees/<agent>`, `maw/<agent>`); integration happens on `maw/integration` with conflict reporting. Your checkout is never touched.
- **Approval Center**: provider permission prompts surface in the TUI (approve once / for session / deny / abort turn). Headless runs never auto-approve.
- **Restart-safe**: state, events, plans, messages and provider session references persist (SQLite WAL). After a crash or restart, in-flight tasks are parked as BLOCKED for an explicit decision instead of being silently re-run.
- **TUI** (ratatui): dashboard, agent grid, agent detail (scroll/search/copy), task board, goal & plan history, event-derived data-flow graph, approvals, logs & diagnostics, config, command palette, monochrome fallback.

## Architecture in one picture

```
 TUI ──snapshots/commands──► Engine (validated ops + events in ONE SQLite transaction)
                               ▲        ▲
        MCP server (127.0.0.1) ┘        └── Supervisor ── provider sessions (codex app-server / claude -p)
        bearer token → agent identity          scheduling · dispatch · message delivery · recovery
```

Details: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/PROTOCOL.md`](docs/PROTOCOL.md), [`docs/TASK_LIFECYCLE.md`](docs/TASK_LIFECYCLE.md), [`docs/SECURITY.md`](docs/SECURITY.md).

## Requirements

- Linux or **WSL2 on Windows 11** (native Windows is not validated — see [`docs/WINDOWS_WSL.md`](docs/WINDOWS_WSL.md)).
- Rust ≥ 1.88 (to build), git ≥ 2.30.
- At least one of: Codex CLI (`codex`, tested 0.166.0), Claude Code (`claude`, tested 2.1.309), installed **in the same environment** and logged in.
- A UTF-8 terminal.

## Install

```bash
git clone <this repo> maw && cd maw
cargo install --path crates/maw-cli          # installs the `maw` binary
maw doctor                                   # checks git, providers, auth, terminal, loopback
```

## Authentication

MAW never touches credentials; it delegates to the providers' official flows:

```bash
maw auth status       # what codex / claude report
maw auth codex        # runs `codex login`
maw auth claude       # runs `claude auth login`
```
Adding an agent checks its provider right away: if it is not logged in, `maw agent add` (and the TUI's **Add agent**) offers the provider's official login on the spot (`--no-login` skips it); in the TUI the screen is handed to the login command and you return with Enter, after which the agent connects. `agent login` in the command palette repeats this later, and connecting an agent that needs a login starts it too.

See [`docs/AUTHENTICATION.md`](docs/AUTHENTICATION.md) (including the WSL-vs-Windows login note).

## First run (5 minutes)

```bash
cd ~/code/my-repo                  # a git repo with at least one commit
maw init                           # registers the project (use --git for a fresh dir)
maw agent add --name lead   --provider claude --role leader
maw agent add --name coder1 --provider codex  --role worker
maw agent add --name coder2 --provider codex  --role worker
maw                                # opens the TUI
```
In the TUI press `/` (command palette), type `goal`, and choose **set goal** (`?` shows every key; `g` is the Goal & plan view). Without a TUI:

```bash
maw run "Implement a simple authentication module with validation, tests and docs" \
        --criterion "tests pass" --timeout-mins 30
maw status ; maw task list ; maw logs --events -f
maw resume                         # continue after Ctrl+C / crash
```

### Models, effort and permissions

```bash
maw models                                   # what each provider offers (Codex: live from your account)
maw agent add --name lead --provider claude --role leader --model sonnet --effort high
maw agent model  coder1 gpt-6.1-sol          # or `default`
maw agent effort coder1 xhigh                # low | medium | high | xhigh | max (| ultra on some Codex models)
maw agent permission coder1 autonomous       # safe | balanced | autonomous
```
Nothing has to be typed from memory: in the TUI the **Add agent** dialog offers Model and Effort as pick-lists that follow the chosen provider (and the effort levels follow the chosen model); on the command line, `maw agent model <name>` / `maw agent effort <name>` without a value show a numbered list. Typed values are checked against the provider's list (Codex: live from your account) and rejected with the valid choices; Claude's list is a catalog, so newer `claude-…` ids are still accepted and a wrong one is reported by Claude when the session starts.

In the TUI: in an agent view type `/model` (or press `M` in browse mode) for a model picker, then an effort picker when the model has levels; `/permissions` sets the profile. A change applies immediately to an **idle** agent (it reconnects with the new settings); a busy agent keeps its turn and picks the change up on its next connect. `!` runs a shell command in the project folder and shows the output in a scrollable panel (60 s limit; it is *your* command, agents cannot trigger it).

On a narrow terminal the title bar and the key-hint bar scroll sideways: roll the mouse wheel (or Shift+wheel) over them; `‹` / `›` show that more is hidden.

### Removing agents, token usage, reading the conversation

```bash
maw agent remove coder1            # asks to confirm; refuses while it has unfinished work
maw agent remove coder1 --force    # releases its tasks back to the pool first
maw usage                          # tokens per agent and per task, exactly as the providers report them
maw usage --json
```
Removal stops the session, closes undelivered mail, keeps finished tasks' history and **leaves the agent's git worktree on disk**. In the TUI: `X` on the selected agent.

The **Tokens** view (`$`) and the `Tokens:` total in the title bar show what each agent has used: input / output / total, share of the project, tokens per minute over the last 5 minutes, a 30-minute sparkline, Claude's own cost estimate, and the tasks that used the most. MAW never estimates tokens; Codex reports tokens only, and neither provider exposes plan/quota limits.

In an agent's transcript, `T` / `L` / `P` hide or show **thinking**, **tool logs** (tool calls, commands, output, system lines) and **MAW protocol** lines (the agent's calls to `maw_*` tools such as message ack/inbox/progress, and MAW's dispatch notices). The agent's own messages, your messages and errors always stay. Palette: *transcript conversation only* / *transcript show all*. Note on thinking: MAW shows only what a provider itself streams. In testing (Claude Code 2.1.309 with Sonnet 5.5, Codex 0.166 with GPT-6.1), neither returned readable reasoning text (Claude sends an empty, signed thinking block; Codex a `reasoning` item with an empty summary), so those lines stay empty until a provider/model exposes them.

### Local models (Ollama)

```bash
maw models pull                                 # download a model (suggestions up to 8B)
maw agent add --name local --provider ollama --model qwen3:8b
```
MAW starts `ollama serve` by itself when an Ollama agent needs it, runs the agent loop (file tools, commands with approvals, and the `maw_*` coordination tools over MCP) and shows the model's reasoning when it streams one. There is no OS sandbox around commands. Details and limits: [docs/providers/ollama.md](docs/providers/ollama.md).

Try it with no providers at all (scripted *mock* agents; it exercises MAW itself, not Claude/Codex):

```bash
maw demo               # TUI
maw demo --headless    # prints the event flow and exits
```

### The three-agent workflow

1. The Leader reads the goal, inspects the repo, records a plan (`maw_plan_propose_revision`) and creates tasks with dependencies and acceptance criteria (`maw_task_create`).
2. MAW promotes tasks whose dependencies are DONE, assigns them to idle workers and dispatches them; independent tasks run in parallel in separate worktrees.
3. Workers report progress (renewing their lease), ask for help or block with a reason, and finish with `maw_task_submit_result` + evidence (tests actually run, changed files).
4. MAW commits the worktree, notifies the Leader (with the worktree path); a Reviewer may be dispatched; the Leader verifies with `maw_task_verify`. Failures route back to the owner with feedback.
5. When every task is verified, the Leader completes the goal; `Supervisor::integrate_done_work` merges worker branches into `maw/integration`.

## Configuration

`maw config path|show|set`, e.g. `maw config set orchestrator.max_concurrency=3`, `maw config set ui.monochrome=true`. Files: `~/.config/maw/config.toml`, data/logs in `~/.local/share/maw/` (override with `MAW_HOME`). Keys: [`docs/TUI.md`](docs/TUI.md) for UI, `OrchestratorConfig` for scheduling (`max_concurrency`, `lease_secs`, `max_attempts`, `max_tasks_per_goal`, …).

## Documentation

[`ARCHITECTURE`](docs/ARCHITECTURE.md) · [`PROTOCOL`](docs/PROTOCOL.md) · [`MCP_TOOLS`](docs/MCP_TOOLS.md) (36 tools, generated) · [`TASK_LIFECYCLE`](docs/TASK_LIFECYCLE.md) · [`SECURITY`](docs/SECURITY.md) · [`AUTHENTICATION`](docs/AUTHENTICATION.md) · [`PROVIDER_COMPATIBILITY`](docs/PROVIDER_COMPATIBILITY.md) · [`TUI`](docs/TUI.md) · [`WINDOWS_WSL`](docs/WINDOWS_WSL.md) · [`ROADMAP`](docs/ROADMAP.md) · [`AGENTS.md`](AGENTS.md)

## Known limitations

See ROADMAP “Known issues”. Highlights: Linux/WSL2 only; Codex's App Server is marked experimental by OpenAI; the TUI was verified with rendering/reducer tests and mock agents but its interactive feel has had limited real-terminal use; no web dashboard; no quota percentages (providers don't expose them); Workers under Codex's sandbox usually cannot `git commit` in a worktree, so MAW commits on their behalf.

## Troubleshooting

| Symptom | Fix |
|---|---|
| Agent shows `AUTH_REQUIRED` | `maw auth <provider>` inside the same environment (WSL ≠ Windows) |
| `codex`/`claude` not found | `maw doctor`; install in the same environment or set `codex.path` / `claude.path` in config |
| Tasks `BLOCKED` after restart | Expected: previous attempts' side effects are unknown. Leader/user releases or cancels them (TUI task board: `e`, `x`) |
| Headless run waits on an approval | No terminal → denied after 120 s by design; run in the TUI or a terminal to answer |
| Slow git/build on `/mnt/c/...` | Keep repos in the WSL filesystem |
| Garbled boxes | Use a UTF-8 terminal; `NO_COLOR=1` for monochrome |
| Where are logs? | `maw config path`; `maw logs -f`, `maw logs --events` |

## Development

```bash
./scripts/wsl-cargo.sh test --workspace      # see AGENTS.md for the WSL build note
./scripts/wsl-cargo.sh clippy --workspace --all-targets
```
Live-provider tests are opt-in (`MAW_LIVE_CODEX=1`, `MAW_LIVE_CLAUDE=1`, `-- --ignored`) because they spend real quota.

License: MIT.

### `maw refresh`

Re-checks the providers, the local Ollama server and its models and the agents of the project, and,
when a MAW is running for it, asks that process to reconnect the agents that are down (it starts the
local model server if it has to). Agents you stopped yourself stay stopped. In the TUI: `/refresh`.

### Where agents work: Windows disk or WSL disk

`maw workspace` shows where each agent's git worktree is created and `maw workspace project|wsl|windows`
changes it:

* `project` (default): next to the code, in `.maw/worktrees` of the project.
* `wsl`: on the Linux disk of WSL (`~/.local/share/maw/projects/<name>/worktrees`). Git is much faster
  there than on `/mnt/c`, but Windows Explorer does not show the folders conveniently.
* `windows`: on a Windows drive, in `orchestrator.windows_worktree_dir` (default: a `MAW` folder in your
  Windows profile, found through `cmd.exe`). Visible in Explorer.

New worktrees go to the chosen place at once. An agent that already has a worktree moves it the next time
it connects while it has no work in flight (its leftover changes are committed on its own branch first, so
nothing is lost). In the TUI the same switch is `/workspace project|wsl|windows` (for that session; use
the command above to make it permanent). Claude Code and Codex sessions are tied to a folder, so after a
move they may start a fresh conversation (MAW says so).
