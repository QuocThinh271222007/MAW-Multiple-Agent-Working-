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
and are answered later via `resolve_approval`. **MCP tool-call approvals** (`mcpServer/elicitation/request`
with `_meta.codex_approval_kind = "mcp_tool_call"`, which Codex sends before running any MCP tool that is
not `readOnlyHint`) become `ApprovalRequested{kind: Other, summary: "call tool <server>/<tool>"}` with the
tool arguments in `details.arguments` and the tool description as `reason`.

| `ApprovalDecision` | command / file change | permissions | MCP tool call (`Other`) |
| --- | --- | --- | --- |
| Approve | `accept` | requested permissions, scope `turn` | `{"action":"accept"}` |
| ApproveForSession | `acceptForSession` | requested permissions, scope `session` | `{"action":"accept","_meta":{"persist":"session"}}` (verified live: later calls don't prompt) |
| Deny | `decline` | `{}` | `{"action":"decline"}` (verified live: the call never reaches the server) |
| Abort | `cancel` | `{}` and the turn is interrupted | `{"action":"cancel"}` |

MAW never answers with `persist: "always"`: that would write a permanent grant into the user's Codex config.

Genuine form / URL elicitations (anything that is not a tool-call approval) cannot be shown by MAW, so they are
answered immediately with `{"action":"decline"}` (a valid result, not a hang) and surfaced as a non-fatal
`Error` naming the server. Every other server request (`item/tool/requestUserInput`, `item/tool/call`, auth
refresh, legacy `applyPatchApproval`/`execCommandApproval`, ...) is answered immediately with a JSON-RPC error
(`-32601`) so Codex never waits on MAW. Pending approvals are dropped when the turn ends (Codex cancels them too).

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
{ "mcp_servers": { "maw": { "url": "http://127.0.0.1:PORT/mcp", "bearer_token_env_var": "MAW_MCP_TOKEN",
                            "default_tools_approval_mode": "approve" } } }
```

**`default_tools_approval_mode = "approve"` is required for MAW to work.** Without it, with
`approvalPolicy` `on-request` or `untrusted`, Codex asks before every call to a tool that lacks
`readOnlyHint: true` (every MAW *write* tool: `maw_task_submit_result`, `maw_task_block`,
`maw_message_ack`, ...). Reproduced live on 0.166.0 with a stand-in server exposing a
`readOnlyHint: false` tool: Codex sent `mcpServer/elicitation/request`, and an adapter that rejected it made
every write tool fail ("tool actions were denied") while read-only tools worked. With the key set, the same
tool ran with **zero** server requests under both `on-request` and `untrusted`. The key is set only on the
server named `maw` (MAW's own loopback coordination server, authorized by MAW's role checks); every other
MCP server, including the user's own, still asks and goes through the Approval Center.
`CodexConfig::preapprove_maw_tools = false` removes the key (then each call surfaces as an approval request).
A per-tool form (`tools.<name>.approval_mode`) also exists in the binary but was not needed.

The per-agent bearer token is placed **only** in the child process environment as `MAW_MCP_TOKEN`
(never in argv, the config object, or on disk). Live probe results on 0.166.0: the server `maw`
reports `mcpServer/startupStatus/updated` `starting -> ready`, `mcpServerStatus/list` lists it, and a
stand-in streamable-HTTP server received `Authorization: Bearer <token>` on `initialize`,
`notifications/initialized`, `tools/list` and `tools/call`. A real turn then called
`maw/maw_goal_get` and used its result (live test `live_codex_calls_an_authenticated_mcp_tool`).

Equivalent per-process fallback (`McpInjection::CommandLine`): `codex app-server -c 'mcp_servers.maw.url="..."' -c 'mcp_servers.maw.bearer_token_env_var="MAW_MCP_TOKEN"' -c 'mcp_servers.maw.default_tools_approval_mode="approve"'`
(argv carries no secret; asserted by a contract test). The same keys are what `codex mcp add maw --url URL --bearer-token-env-var VAR` writes to `config.toml`.

The user's own `~/.codex/config.toml` MCP servers/plugins (e.g. the built-in `codex_apps`) still load alongside `maw`.

## Authentication

MAW never reads or stores Codex credentials. `auth_status` shells out to `codex login status`
("Logged in using ChatGPT" / API key). Logging in is delegated to the native `codex login` flow
(device-code/browser) run by the user; the app-server's `account/login/*` methods are not used.
Auth inside WSL is separate from Windows; log in inside the distro where agents run.

## Tests

* `src/protocol.rs`, `src/lib.rs` unit tests: permission mapping, token never in config, decision mapping, rate-limit gating, login/version parsing.
* `tests/contract.rs` (22): drives the real adapter against `maw-fake-codex` (handshake params incl. the pre-approval key, no token leak, event mapping, command/file/permission approval round trips incl. deny/session/abort, **MCP tool-call elicitation** accept/session(`persist=session`, never `always`)/deny/abort, form elicitations declined with a notice, pre-approval can be disabled, unknown server requests, interrupt, steer, process exit => `Disconnected` with stderr tail, malformed lines, resume fallback, shutdown reaping, missing binary).
* `tests/fixtures.rs` (5): sanitized captures from the **real** server in `fixtures/codex/`, version-pinned, including the verbatim tool-call elicitation request.
* `tests/live.rs` (5, `#[ignore]`, need `MAW_LIVE_CODEX=1`): a minimal real turn; a real authenticated MCP tool call; a **non-read-only** MAW tool that runs with no approval; the negative control (pre-approval off => `ApprovalRequested "call tool maw/maw_probe_write"`, approving lets the call through); a denied call that never reaches the server.
  `MAW_LIVE_CODEX=1 cargo test -p maw-provider-codex --test live -- --ignored --nocapture`.
  `MAW_CODEX_CAPTURE=file` appends the redacted raw server lines (how the fixtures were produced).

## Known limitations

* **Experimental protocol**: method/field names may change; unknown notifications are ignored, but renamed ones silently degrade (fixtures catch this on version bumps).
* **`codex` on PATH inside WSL**: a non-login `wsl -e` shell can resolve `codex` to the *Windows* npm shim (`/mnt/c/.../npm/codex`) which fails with `node: not found`. Use a login shell, put the Linux `codex` first on PATH, or set `MAW_CODEX_BIN` / `CodexConfig::binary`. `detect` reports the configured executable.
* **Git writes under `workspace-write`**: the sandbox's writable root is the agent's cwd; a git *worktree* keeps its metadata in the main repository's `.git`, outside that root (and Codex protects `.git`). Expect `git commit` inside a worker worktree to need approval or fail - not exercised live. MAW's supervisor already commits on submission; prompts should not rely on the worker committing.
* **Form / URL MCP elicitations and `item/tool/requestUserInput`** are not supported (no UI for them yet): elicitations are declined with a visible `Error`, user-input requests get a JSON-RPC error. A tool that needs one fails rather than hangs. (MCP *tool-call approvals* are fully supported, see above.)
* **Elicitation detection** relies on `_meta.codex_approval_kind == "mcp_tool_call"` and on the tool name being quoted in `message` (neither is a documented contract; both observed on 0.166.0 and pinned by a fixture test). If the marker disappears, requests would be declined with a notice rather than approved.
* **Tool name in the summary** is parsed from the human-readable message; if that fails the summary says `(unknown tool)` but `details` still carries the server name and arguments.
* **Token usage** is Codex's cumulative thread total, not per-turn.
* `account/read` details (email, plan) are not surfaced by `auth_status` (it only reports the method).
* Resume uses `thread/resume`; not exercised against the real server beyond the fake (fallback path covered).
* No Windows-native path was tested (no MSVC toolchain on the dev machine); WSL/Linux only.
