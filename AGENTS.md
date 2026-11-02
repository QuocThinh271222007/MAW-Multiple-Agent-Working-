# AGENTS.md — working on MAW

Guidance for AI coding agents (and humans) continuing this project. Read `docs/ROADMAP.md` first: it records what is done, verified, and next.

## Build & test (important environment note)

Development happens in **WSL2 Ubuntu** (Windows here has no MSVC/Rust). Sources may live on the Windows side (`/mnt/c/...`); keep build output on the Linux filesystem:

```bash
./scripts/wsl-cargo.sh build --workspace      # sets CARGO_TARGET_DIR=$HOME/maw-target
./scripts/wsl-cargo.sh test --workspace
./scripts/wsl-cargo.sh clippy --workspace --all-targets
./scripts/wsl-cargo.sh fmt --check
```

From Windows: `wsl -d Ubuntu-24.04 -e bash -lc 'cd /mnt/c/.../MAW && ./scripts/wsl-cargo.sh test --workspace'`.
Parallel builds by several agents: give each its own `CARGO_TARGET_DIR` to avoid lock contention.

Live provider tests are opt-in and spend real quota: `MAW_LIVE_CODEX=1`, `MAW_LIVE_CLAUDE=1` with `cargo test -- --ignored`. Never run them in CI by default.

## Layout & layering

```
maw-core          pure domain: ids, state machines, evidence rules, progress math, events, provider traits. NO I/O.
maw-storage       SQLite (WAL) + migrations (../../migrations). Db API is transactional via Store::write.
maw-orchestrator  Engine (validated ops + events), tools (MCP surface), Supervisor (sessions/scheduling),
                  recovery, git worktrees, mock provider
maw-mcp           transport + auth for the tools (rmcp). No business logic.
maw-provider-*    adapters; provider specifics stay here.
maw-tui           rendering + input; reads Engine::snapshot(), sends commands. Owns no processes/DB.
maw-cli           the `maw` binary, config, doctor, headless runner.
```

## Invariants — do not break

1. **Every mutation goes through `Engine`** (`Engine::tx`): state change + events in ONE transaction; events broadcast after commit. Never write tables from other crates.
2. **Identity is server-assigned.** MCP actors come from per-agent bearer tokens (`Engine::authenticate`), never tool arguments.
3. **A task becomes DONE only via `verify_task`** after passing evidence (`Task::completion_blockers`) — never because an agent says so, never because a turn/process ended.
4. **No silent re-execution after uncertainty**: lease expiry, owner disconnect and restart park tasks as BLOCKED for a Leader/user decision.
5. **Messages are persisted, then delivered by the Supervisor** (`delivered_at`), never injected mid-turn; `acknowledged_at` is set only by the recipient.
6. Bounded everything: provider event channels (`PROVIDER_EVENT_BUFFER`), transcripts, task counts/depth, retries.
7. Secrets: pass through `maw_core::redact::redact` before logging/persisting free text. Tokens never go to disk or argv.
8. Do not parse rendered terminal text for agent communication. Structured events only.

## Conventions

- Typed errors (`CoreError` has stable `code()`), no `unwrap()` in runtime paths, `tracing` for diagnostics.
- New events: add to `event_kinds!` in `maw-core/src/event.rs`; payload fields used by the data-flow view are `from/to/from_id/to_id/task_key`.
- New tools: add args struct + `spec!` + dispatch arm in `maw-orchestrator/src/tools.rs`, then regenerate docs: `maw tools --markdown > docs/MCP_TOOLS.md`. Add an engine-rule test.
- Schema changes: new numbered file in `migrations/` and register it in `maw-storage/src/lib.rs`; never edit applied migrations.
- Tests that need agents use `maw_orchestrator::mock` — it is deterministic and goes through the same `call_tool` path; it is NOT evidence that real providers work.
- Keep commits focused; do not push or publish without explicit user approval.

## Where state lives

`~/.local/share/maw/projects/<name>/maw.db` (override with `MAW_HOME`), logs in `~/.local/share/maw/logs/` (rotated, redacted), config `~/.config/maw/config.toml`. Worker worktrees: `<repo>/.maw/worktrees/<agent>` on branch `maw/<agent>`; integration branch `maw/integration`.
