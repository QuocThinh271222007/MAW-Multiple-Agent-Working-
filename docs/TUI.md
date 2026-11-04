# MAW terminal UI (`maw-tui`)

Ratatui + crossterm. Keyboard first; mouse wheel scrolls when mouse capture is enabled.
The TUI owns no provider processes and no database connection: it reads
`Engine::snapshot()` and sends commands through `Engine` / `Supervisor`.

## Architecture

| Module | Role |
|---|---|
| `app` | Pure state (`App`) and the key reducer `handle_key(&mut App, KeyEvent) -> Vec<Action>`. No I/O. |
| `ui` | Pure rendering of `&App` (deterministic given `app.now`). One file per view. |
| `flow` | Data-flow graph derived from the stored event log (`derive_flow`). |
| `events` | One-line descriptions of events for the stream/logs. |
| `input` | Single-line text input with unicode-correct cursor and horizontal scrolling. |
| `runtime` | Terminal guard (RAII + panic hook), async loop, action execution, OSC 52 clipboard. |
| `testutil` | Synthetic snapshots and `TestBackend` render helpers (doc-hidden, used by tests). |

Public API:

```rust
maw_tui::run(engine: Arc<Engine>, supervisor: Arc<Supervisor>, opts: TuiOptions) -> anyhow::Result<()>
TuiOptions { monochrome, mouse, max_fps, shutdown_on_quit, on_quit }
```

`run` restores the terminal before returning, then (if `shutdown_on_quit`, default true)
calls `Supervisor::shutdown()` and the optional `on_quit` hook. Disable
`shutdown_on_quit` if the caller does its own shutdown.

### Event loop

Event driven, no polling: the loop waits on terminal input, the engine broadcast
(`Engine::subscribe`), `Supervisor::ui_updates` (live transcript changes) and a 1 s clock tick.
Refreshes (one `snapshot()` on `spawn_blocking`) and redraws are coalesced to `max_fps`
(default 15). Provider I/O only happens inside spawned action tasks, never on the render path.
Only the selected agent's full transcript is copied each refresh; grid cards get a 30-line tail.

### Terminal safety

`TerminalGuard` restores raw mode, the alternate screen, mouse capture and the cursor on drop,
and a panic hook performs the same restoration (idempotently) before the panic message prints.
Terminals smaller than 20x5 show a "too small" notice instead of a layout. Resize is handled.

## Views

| Key | View | Notes |
|---|---|---|
| `d` | Dashboard | Header (project, agents, Parallel/Serial, running/PAUSED), Goal panel with verified-progress bar, agent grid, task summary, event stream. Adapts from 80x24 up. |
| `s` | Agent grid | Cards (name, role, provider/model, state, state age, task, current tool/command, recent output, `⚠ APPROVAL` badge); compact rows when cards would not fit. |
| `Enter` | Agent detail | Full transcript with scrolling, `/` search (+`n`/`N`), copy (`y` last message, `Y` all, via OSC 52 + internal buffer), usage labelled "provider-reported" or "unavailable". |
| `t` | Task board | 9 columns (>=150 cols), 6 grouped columns (>=100), list mode below. Detail pane: owner, reported-vs-verified progress, review, attempts, dependencies, criteria, evidence (and a warning when there is none), artifacts, recent events. |
| `g` | Goal & plan | Objective, criteria, plan milestones derived from real task state, **verified / agent-reported / blocked / failed / unknown shown separately** (reported is explicitly "NOT counted"), goal and plan revision history. |
| `f` | Data flow | Leader → MCP/Orchestrator → workers/reviewers diagram plus edge table, task-dependency edges and timeline; everything derived from events. Narrower than ~60 columns (or `Tab` for list mode) shows the edge list only. |
| `o` | Approvals | Pending list with agent, operation, command, target paths, reason, age; history. |
| `l` | Logs | provider errors, rate limits, reconnects, recovery, disconnects, lease expiry, blockers; agent last errors. |
| `c` | Config | Concurrency, lease, auto-assign, limits, project/run/workspaces, providers & permission profiles, theme. |
| `?` | Help | Full key list. |

### Data-flow edges

Derived by `flow::derive_flow(agents, events, now, active_secs)`:

| Event | Edge |
|---|---|
| `task.created` | creator → MCP (`CREATE TASKS`) |
| `task.assigned` | MCP → worker (`ASSIGN`); Leader → MCP too when the Leader assigned |
| `task.claimed` | worker → MCP (`CLAIM`) |
| `message.sent` | sender → recipient (`MESSAGE` / `HANDOFF`) |
| `message.delivered` | MCP → recipient (`DELIVER`) |
| `handoff.accepted` | recipient → previous owner (`HANDOFF`) |
| `agent.help_requested` | worker → Leader (`HELP`) |
| `task.progress` | worker → MCP (`PROGRESS`) |
| `task.submitted` | worker → MCP (`RESULT`) |
| `task.blocked` | worker → MCP (`BLOCKER`) |
| `task.reviewed` | reviewer → MCP (`REVIEW`); rejected reviews also reviewer → owner (`FEEDBACK`) |
| `task.verified` | Leader → MCP (`VERIFY`) |

Edges seen within 45 s are "active" (●), older ones historical (○). Unknown ids (deleted agents,
pseudo senders) are ignored. With no events there are no edges.

## Key bindings

Global: `Tab`/`Shift+Tab` switch panel · `1`…`9` select agent · `d s t g f o l c` views ·
`/` `:` `Ctrl+P` command palette · `?` help · `Esc` close/back · `q` / `Ctrl+C` quit ·
`G` set or redirect the objective · `a` add agent · `p` pause · `r` resume.

Agents (dashboard / grid / detail): `C` connect · `D` disconnect · `i` interrupt · `m` message.
Agent detail: `↑↓ PgUp PgDn Home End` scroll · `/` search · `n N` next/previous match · `y` `Y` copy.
Tasks: `↑↓←→` select (`←→` jump status groups) · `Enter` focus detail · `R` requeue (FAILED/BLOCKED) ·
`e` release · `x` cancel · `v` verify (VERIFYING only).
Approvals: `y` approve · `Y` approve for session · `n` deny · `x` deny and abort the turn.
Config: `+`/`-` concurrency · `[`/`]` lease ±60 s · `u` auto-assign.
Flow: `Tab` graph/list · `↑↓` scroll.

Dialogs (goal, redirect, add agent, message, verify, release, cancel): `Tab`/`↓` next field,
`Shift+Tab`/`↑` previous, `←/→/Space` change a selector, `Enter` on the last field submits,
`Esc` cancels. Required fields are validated before an action is emitted.

### Command palette

Fuzzy-filtered list (subsequence scoring, name matches rank above description matches):
view switches, goal set/redirect, agent add/connect/connect all/disconnect/interrupt/message,
scheduling pause/resume, task requeue/release/cancel/verify, integrate, concurrency up/down,
toggle monochrome, copy transcript, help, quit.

## Themes

Dark/colour by default. Monochrome (`TuiOptions::monochrome`, or a non-empty `NO_COLOR`)
uses only bold/dim/reverse/underline, and state is always also spelled out in text.
A test asserts no colour cell is emitted in monochrome across the main views.

## What the UI never does

* It never invents progress: percentages are `Progress::compute` over real task state; agent
  reports are shown as "unverified". Token usage appears only when the provider reported it.
* It never approves anything on its own: approvals are resolved only by explicit keypresses.
* It never talks to providers on the render path.

## Tests

`cargo test -p maw-tui`: reducer/key tests, text input, flow derivation, event descriptions,
OSC 52/base64, render tests (`TestBackend`, sizes 0x0 … 300x80, content + monochrome + overlays),
and integration tests with a real `Engine`/`Supervisor` + mock provider (full team run rendered;
reducer actions driving the real engine).

Interactive demo (mock provider, not a real Claude/Codex run):
`cargo run -p maw-tui --example tui_demo`.

## Known gaps

* No mouse clicking (wheel scroll only); no text selection inside the TUI (use `y`/`Y`).
* OSC 52 depends on the terminal allowing clipboard writes; the internal buffer is the fallback.
* Transcript search is case-insensitive substring only, over wrapped display rows.
* Flow diagram supports one row of workers/reviewers (falls back to the edge list when they don't fit).
* The Integrate command reports a summary toast only; conflicts are visible via Logs/Leader messages.


## Added: model/effort, permissions, `!` shell, scrolling bars

- `M` or palette → **model**: model selector (provider catalog, loaded in the background; Codex is queried live), an optional *Custom model id* that overrides the selector, and an *Effort* selector (levels the model reports, else low…max). Idle agents reconnect immediately; busy agents apply it on their next connect.
- palette → **permissions**: `safe` / `balanced` / `autonomous`.
- `!` or palette → **shell**: runs a command in the project folder (`bash -lc`, 60 s limit, output capped at 64 KiB per stream and secret-redacted) and opens the result in a scrollable panel (`↑↓ PgUp PgDn Home End`, `y` copy, `Esc` close). This is a deliberate user action.
- The title bar always lists every view key and ends with `dir: <project folder>`. When it does not fit, roll the mouse wheel (or Shift+wheel / horizontal wheel) over the bar to scroll it; the key-hint bar at the bottom works the same way.

- **Add agent** (`a`): Model and Effort are pick-lists (←/→). Changing Provider swaps in that provider's models; changing Model swaps in the effort levels it supports. If a provider's list is unavailable the only choice is `default`. The lists are loaded in the background at startup (Claude instantly, Codex via `model/list`).
- In the model dialog the *Custom model id (advanced)* field is validated against the provider's list before anything is saved.
- **Long pick-lists** (4+ options, e.g. models) are drawn as a vertical list under the field label with each option's description, a `(n/total)` counter and `▲`/`▼` markers. `↑↓` choose, `PgUp/PgDn` jump 5, `Home/End` first/last, `Tab`/`Enter` move to the next field. On a short terminal the spacer rows disappear and the list becomes a 6-row scrolling window that always keeps the selection visible.
- `maw demo` also loads the real Codex/Claude model lists (for listing only; those providers can never start an agent there).
- **Login on add / connect**: after **Add agent**, or when connecting an agent whose provider is `AUTH_REQUIRED`, the TUI leaves the alternate screen, runs the provider's own login (`codex login` / `claude auth login`) in your terminal, waits for Enter, redraws and connects the agent. `agent login` in the palette does the same for the selected agent. MAW never reads the credentials; an *unknown* login state does not trigger it (the session start reports real auth errors).

## Added: tokens, conversation filters, removing agents

- **Tokens view** (`$`, palette *view tokens*): per-agent input / output / total, share bar, tokens per minute (last 5 min), 30-minute sparkline (wide terminals), Claude's client-side cost estimate, and "tokens by task". The title bar always shows `Tokens: <project total>`. Figures are the providers' own cumulative reports; the rate, sparkline and per-task numbers come from the increments MAW recorded with each report (`agent.usage` events with `payload.delta`, attributed to the agent's current task). Agents that never reported show "no usage reported yet", not zero.
- **Conversation filters** (agent views): `T` thinking, `L` tool logs (tool calls, commands, output, system), `P` MAW protocol (calls to `maw_*` tools and dispatch notices). The panel title shows the state (`T thinking ✓ · L tool logs ✗ · …`). Messages, your messages and errors are never hidden. The same filters apply to the grid cards, search and copy. Palette: *transcript thinking / tool logs / protocol / conversation only / show all*.
- **Remove agent** (`X`, palette *agent remove*): confirmation dialog (default is *no*) with a choice of refusing or releasing unfinished tasks. The worktree stays on disk.

## Added: performance view

`%` (palette *view performance*) shows what running local models costs the machine: line charts of
**generation speed** (tokens/s; the dim line is prompt processing), **VRAM**, **GPU load**, **RAM**
and **CPU** over a window you cycle with `w` (1 min / 5 min / 30 min), the models currently held in
memory and how much of each sits on the GPU (the rest runs from system RAM, which is slower), and
GPU temperature and power. Sampling starts when the view is first opened or when the project has an
Ollama agent, every 2 s, and keeps the last 30 minutes in memory only (nothing is written to disk).

Where the figures come from (the view names them):

* CPU and RAM: under WSL these are the **Windows host's**, read through one long-lived
  `powershell.exe` (it ends by itself after 4 h and is restarted); on plain Linux they come from
  `/proc`. If PowerShell cannot be started under WSL, the view says it shows the WSL virtual machine
  only.
* GPU: `nvidia-smi` (the whole GPU, including other programs). Without it the GPU charts say so;
  AMD/Intel GPUs are not supported.
* Models in memory and generation speed: the local-model provider (Ollama `/api/ps` and the timings
  of the most recent request).

A panel with no source shows "no data", never a zero. Charts turn yellow at 85% and red at 95%.

## Added: message box, command menu, pickers, top bar

* **Agent view = message box.** Opening an agent (`Enter` on the grid) puts the cursor in a box at the
  bottom, like Claude Code / Codex: just type; `Enter` sends the message to that agent. `/` opens a
  command menu above the box (type to filter, `↑↓`, `Enter` runs: `/model`, `/permissions`,
  `/download model`, `/transcript ...`, `/interrupt`, ...); `!cmd` runs a shell command in the project
  folder. While typing, letters are text, so the one-key shortcuts are off; `Tab` switches to **browse
  mode** where they work (`y` copy, `/` search, `n/N`, `T L P` filters, `1-9` other agent, `d s t ...`
  views) and `Tab` or `Enter` returns to the box. `Esc` clears the text, then leaves the view. `↑↓`
  and `PgUp/PgDn` scroll the transcript even while typing; `Home/End` too when the box is empty.
* **Pickers instead of forms** for `/model`, `/permissions` and `/download model`: one decision per
  screen, a filter line (type to narrow, or type any model name), `↑↓`, `Enter`, `Esc`. Choosing a
  model that has effort levels opens a second picker for the effort; models without levels apply at
  once. The current value is marked `●`. The Add-agent form is unchanged.
* **Top bar.** The view keys come first (`d s t g f o l $ %  c`, with names when they fit), then
  running/paused, then project, agents, tokens and the folder. The performance view is `%`, tokens
  `$`. The mouse wheel still scrolls the bar sideways where the terminal passes it on (verified in a
  pseudo-terminal; some terminals keep wheel events for themselves).

## Added: saved conversations, /thinking on|off, Alt+arrows

* Conversations are stored in the project database and restored when you open the agent view.
* In the message box `/thinking on|off`, `/tool logs on|off` and `/protocol on|off` set the transcript
  filters (the T / L / P keys are letters while you type). Without `on`/`off` they toggle.
* `Alt+←` / `Alt+→` scroll the title bar. Some terminals (cmd) turn the mouse wheel into arrow keys and
  never report the pointer position, so the wheel cannot tell the bar from the body there; `?` shows how
  many mouse events MAW has received (0 = the terminal is not passing the mouse on).
