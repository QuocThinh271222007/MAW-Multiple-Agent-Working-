# MAW architecture

MAW is a Rust workspace. Layers (spec A–F) map to crates; dependencies only point downward.

```
 A Terminal presentation    maw-tui ───────────────┐ reads snapshots / sends commands
 B Application core         maw-core (domain, no I/O)
 C Orchestrator             maw-orchestrator: Engine + Supervisor + tools        ◄── maw-cli (binary `maw`)
 D Provider adapters        maw-provider-codex, maw-provider-claude, mock (in orchestrator)
 E MCP coordination         maw-mcp (rmcp: Streamable HTTP + stdio) → Engine::call_tool
 F Events & persistence     maw-storage (SQLite WAL) + event log
```

## Process model

One `maw` process per project run owns everything:

```
                       ┌──────────────────────── maw process ────────────────────────┐
  user ── keys ──► TUI │  Engine ◄── call_tool(actor,…) ◄── MCP HTTP server (127.0.0.1)│◄── agents' MCP clients
                  ▲    │    │  tx(state + events)                 ▲ bearer token → actor │      (Claude/Codex)
      snapshot    │    │    ▼                                     │                      │
                  └────┤  SQLite (WAL)        Supervisor ─────────┘                      │
                       │    ▲                   │ spawn / events / dispatch / deliver    │
                       │    └─ events ◄─────────┴──► provider sessions (child processes) │
                       └────────────────────────────────────────────────────────────────┘
```

- **Engine** (`engine.rs`, `tasks.rs`, `comms.rs`): all validated domain operations. `Engine::tx` opens a transaction, runs the operation, persists the events it emitted **in the same transaction**, commits, then broadcasts them (tokio broadcast, lagging receivers are tolerated). The TUI, MCP, mock provider and tests use this one API.
- **Tools** (`tools.rs`): 36 `maw_*` tools, each with a JSON Schema generated from its argument struct, a role gate and a read-only flag. `Engine::call_tool(actor, name, args)` is transport independent.
- **Supervisor** (`supervisor.rs`): owns provider sessions. Per-session pump task turns `ProviderEvent`s into agent state, transcripts (bounded ring buffer, 2000 lines) and durable events. The scheduler loop is event-driven (woken by engine commits) with a safety tick: lease sweep → ready promotion → leader kickoff → auto-assign → dispatch → review dispatch → message delivery. Reconnect uses bounded exponential backoff and never reconnects an agent the user stopped.
- **Providers** implement `maw_core::provider::{AgentProvider, AgentSession}`: `start_session` returns a handle plus a bounded `mpsc` of `ProviderEvent`s; `send_input` starts a turn; capabilities are reported honestly.
- **Recovery** (`recovery.rs`): at startup, agents are reset to DISCONNECTED, pending approvals expire, RUNNING tasks are parked as BLOCKED and the Leader is told. Nothing is re-executed automatically.
- **Workspaces** (`workspace.rs`): per-agent git worktrees + branches, uncommitted worker changes are committed by MAW on submission (so diffs are accurate), `integrate` merges into `maw/integration` and reports conflicts instead of forcing.

## Event model

One envelope (`maw-core/src/event.rs`, schema v1): `{schema_version, event_id, sequence, timestamp, project_id, run_id, agent_id, task_id, correlation_id, event_type, payload}`. `sequence` is the SQLite rowid (strictly monotonic). 60+ typed kinds (`task.*`, `agent.*`, `message.*`, `handoff.*`, `permission.*`, `provider.*`, `mcp.*`, `session.recovered`…). High-volume kinds (agent messages, tool calls, MCP calls, usage) are flagged *verbose* and pruned by retention (`prune_verbose_events`); durable state transitions are never pruned. Token-level deltas are never persisted — they live only in the in-memory transcript.

State tables are authoritative; the event log is the audit trail and feeds the data-flow view. Payloads carry denormalized `from/to/task_key` so views need no joins.

## Backpressure & resource bounds

Provider→supervisor channels are bounded (`PROVIDER_EVENT_BUFFER` = 1024): a slow supervisor applies backpressure to adapters; a slow UI cannot stall providers because the UI reads snapshots on its own cadence and the broadcast channel drops (not blocks) for lagging subscribers. Transcripts, message sizes, task counts and retries are bounded (see SECURITY.md / TASK_LIFECYCLE.md).

## Persistence

`migrations/0001_init.sql` → projects, goals (+revisions), plan revisions, agents, tasks (+deps, evidence, artifacts, attempts), messages, events, context entries, workspaces, approvals, runs, kv. WAL + `synchronous=NORMAL`, foreign keys on, busy timeout 5 s. Migrations are applied transactionally and recorded.

## Why in-process MCP over HTTP

Agents must act through a single authoritative engine, and identity must be assigned by MAW. An in-process Streamable HTTP server on loopback with per-agent bearer tokens gives both without a second state owner. A stdio mode exists for standalone use (`maw mcp serve --stdio --as …`): it acts as an operator-chosen identity against the database but cannot wake a running scheduler.

## Status

See `docs/ROADMAP.md` for what is implemented, verified and planned.
