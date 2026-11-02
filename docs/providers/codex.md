# Codex provider (`maw-provider-codex`)

Status: **implemented and verified live** against `codex-cli 0.166.0` (WSL2 Ubuntu 24.04, ChatGPT login).
The Codex App Server is labelled **[experimental]** by the CLI itself (`codex app-server --help`), so
the wire protocol may change between releases. MAW pins the tested version in
`tests/fixtures.rs` (`PINNED_VERSION`) and fails loudly when a fixture/version no longer matches.

## How it works

One long-lived `codex app-server` child process per agent session (stdio, newline-delimited JSON,
**no `jsonrpc` field**). The process is spawned with `kill_on_drop`, reaped on shutdown/EOF and
never shared between agents.

Handshake: `initialize` (clientInfo `maw`, `experimentalApi: true`) -> `initialized` notification
-> `thread/start` (or `thread/resume` when `SessionConfig::resume_native_id` is set; a failed resume
emits a non-fatal `Error` and falls back to a fresh thread).

| MAW operation | Codex method |
| --- | --- |
| `send_input` | `turn/start` (`turn/steer` only when `steer_if_active` and a turn is running) |
| `interrupt` | `turn/interrupt` |
| `resolve_approval` | JSON-RPC response to the pending server request |
| `shutdown` | close stdin, wait 3s, kill; reader reaps the child and emits `Disconnected{"shutdown"}` |
| `detect` | `codex --version` |
| `auth_status` | `codex login status` (credentials files are never read) |

Only one turn is active per session; a second `send_input` is refused unless it is a steer.

### Event mapping

| Codex | `ProviderEvent` |
| --- | --- |
| `turn/started` | `TurnStarted` |
| `item/agentMessage/delta` | `MessageDelta` |
| `item/completed` agentMessage | `Message` |
| `item/started`/`completed` commandExecution | `CommandStarted` / `CommandCompleted{exit_code}` |
| `item/commandExecution/outputDelta` | `CommandOutput` |
| `item/started`/`completed` fileChange | `ToolStarted`/`ToolCompleted` (+ `FileChanged{paths}`) |
| `item/started`/`completed` mcpToolCall | `ToolStarted`/`ToolCompleted` named `server/tool` |
| `thread/tokenUsage/updated` | `Usage` (`reported_by_provider = true`; cumulative thread totals) |
| `account/rateLimits/updated` | `RateLimited` **only** when `rateLimitReachedType` is set |
| `error` | `Error{fatal:false}` (the turn outcome comes from `turn/completed`) |
| `mcpServer/startupStatus/updated` `failed` | `Error{fatal:false}` naming the server |
| `turn/completed` | `TurnCompleted{Completed|Interrupted|Failed, error}` |
| stdout EOF / process exit | `Disconnected{reason}` including the last stderr lines |

Notifications for other threads, unknown notifications and malformed lines are ignored (debug log).
Events go through a bounded channel (`PROVIDER_EVENT_BUFFER`), so a slow consumer applies backpressure.

### Server requests (approvals)

`item/commandExecution/requestApproval`, `item/fileChange/requestApproval` and
`item/permissions/requestApproval` become `ApprovalRequested` (`request_id` = JSON-RPC id as a string)
and are answered later via `resolve_approval`:

| `ApprovalDecision` | command / file change | permissions |
| --- | --- | --- |
| Approve | `accept` | requested permissions, scope `turn` |
| ApproveForSession | `acceptForSession` | requested permissions, scope `session` |
| Deny | `decline` | `{}` |
| Abort | `cancel` | `{}` and the turn is interrupted |

Every other server request (`item/tool/requestUserInput`, `mcpServer/elicitation/request`,
`item/tool/call`, auth refresh, legacy `applyPatchApproval`/`execCommandApproval`, ...) is answered
immediately with a JSON-RPC error (`-32601`) so Codex never waits on MAW. Pending approvals are dropped
when the turn ends (Codex cancels them too).

## Permission mapping

| MAW profile | `approvalPolicy` | `sandbox` |
| --- | --- | --- |
| Safe | `untrusted` | `read-only` |
| Balanced | `on-request` | `workspace-write` |
| Autonomous | `never` | `workspace-write` |

`danger-full-access` is never selected.

## MAW MCP server registration (verified live)

Passed in `thread/start.config` (the app-server accepts a nested config object):

```json
{ "mcp_servers": { "maw": { "url": "http://127.0.0.1:PORT/mcp", "bearer_token_env_var": "MAW_MCP_TOKEN" } } }
```

The per-agent bearer token is placed **only** in the child process environment as `MAW_MCP_TOKEN`
(never in argv, the config object, or on disk). Live probe results on 0.166.0: the server `maw`
reports `mcpServer/startupStatus/updated` `starting -> ready`, `mcpServerStatus/list` lists it, and a
stand-in streamable-HTTP server received `Authorization: Bearer <token>` on `initialize`,
`notifications/initialized`, `tools/list` and `tools/call`. A real turn then called
`maw/maw_goal_get` and used its result (live test `live_codex_calls_an_authenticated_mcp_tool`).

Equivalent per-process fallback (`McpInjection::CommandLine`): `codex app-server -c 'mcp_servers.maw.url="..."' -c 'mcp_servers.maw.bearer_token_env_var="MAW_MCP_TOKEN"'`
(argv carries no secret). The same keys are what `codex mcp add maw --url URL --bearer-token-env-var VAR` writes to `config.toml`.

The user's own `~/.codex/config.toml` MCP servers/plugins (e.g. the built-in `codex_apps`) still load alongside `maw`.

## Authentication

MAW never reads or stores Codex credentials. `auth_status` shells out to `codex login status`
("Logged in using ChatGPT" / API key). Logging in is delegated to the native `codex login` flow
(device-code/browser) run by the user; the app-server's `account/login/*` methods are not used.
Auth inside WSL is separate from Windows; log in inside the distro where agents run.

## Tests

* `src/protocol.rs`, `src/lib.rs` unit tests: permission mapping, token never in config, decision mapping, rate-limit gating, login/version parsing.
* `tests/contract.rs` (17): drives the real adapter against `maw-fake-codex` (handshake params, no token leak, event mapping, approval round trips incl. deny/session/abort, unknown server requests, interrupt, steer, process exit => `Disconnected` with stderr tail, malformed lines, resume fallback, shutdown reaping, missing binary).
* `tests/fixtures.rs` (4): sanitized captures from the **real** server in `fixtures/codex/`, version-pinned.
* `tests/live.rs` (2, `#[ignore]`, need `MAW_LIVE_CODEX=1`): a minimal real turn and a real authenticated MCP tool call.
  `MAW_LIVE_CODEX=1 cargo test -p maw-provider-codex --test live -- --ignored --nocapture`.
  `MAW_CODEX_CAPTURE=file` appends the redacted raw server lines (how the fixtures were produced).

## Known limitations

* **Experimental protocol**: method/field names may change; unknown notifications are ignored, but renamed ones silently degrade (fixtures catch this on version bumps).
* **`codex` on PATH inside WSL**: a non-login `wsl -e` shell can resolve `codex` to the *Windows* npm shim (`/mnt/c/.../npm/codex`) which fails with `node: not found`. Use a login shell, put the Linux `codex` first on PATH, or set `MAW_CODEX_BIN` / `CodexConfig::binary`. `detect` reports the configured executable.
* **Git writes under `workspace-write`**: the sandbox's writable root is the agent's cwd; a git *worktree* keeps its metadata in the main repository's `.git`, outside that root (and Codex protects `.git`). Expect `git commit` inside a worker worktree to need approval or fail - not exercised live. MAW's supervisor already commits on submission; prompts should not rely on the worker committing.
* **MCP elicitation / user-input requests** are rejected (no UI for them yet); a tool that needs one will fail rather than hang.
* **Token usage** is Codex's cumulative thread total, not per-turn.
* `account/read` details (email, plan) are not surfaced by `auth_status` (it only reports the method).
* Resume uses `thread/resume`; not exercised against the real server beyond the fake (fallback path covered).
* No Windows-native path was tested (no MSVC toolchain on the dev machine); WSL/Linux only.
