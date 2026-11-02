# Claude Code provider (`maw-provider-claude`)

Status: **implemented and verified live** against Claude Code **2.1.309** (WSL2 Ubuntu 24.04,
claude.ai subscription login, model alias `haiku`). Other versions are allowed; `detect()` reports a
note when the installed version differs from the tested one.

## Transport decision

MAW drives the **official CLI structured-streaming interface**, one long-lived process per agent:

```
claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages ...
```

Verified empirically (not assumed):

| Question | Result |
|---|---|
| Multi-turn on one process? | Yes. User messages are written as JSON lines; a `system/init` + `result` pair frames every turn. A second turn on the same PID worked (also after an interrupt). So the per-turn `--resume` fallback is **not needed**. |
| Approvals without the SDK? | Yes: `--permission-prompt-tool stdio` makes the CLI emit `control_request` / `can_use_tool` on stdout and honour a `control_response` on stdin. The flag is hidden from `--help` but is what the Agent SDK itself passes, and the headless docs reference `--permission-prompt-tool`. |
| Interrupt? | Yes: `control_request{subtype:"interrupt"}` ends the turn (`result` with `subtype:"error_during_execution"`, `terminal_reason:"aborted_streaming"`); the process stays usable. |
| Resume | `--resume <uuid>` keeps the session id. A missing session produces `error_during_execution`, stderr `No conversation found with session ID: …`, exit 1 (handled, see below). |
| HTTP MCP | `--mcp-config` with `{"type":"http"}` works; `system/init.mcp_servers[]` reports `connected`. |

**No TypeScript bridge.** The Agent SDK would add a Node runtime, but everything MAW needs
(multi-turn, interrupts, permission callbacks, MCP, usage) is available from the CLI protocol. No
`bridges/claude-ts/` was created.

## Launch arguments

| Flag | Why |
|---|---|
| `-p --input-format stream-json --output-format stream-json --verbose --include-partial-messages` | structured streaming, text deltas |
| `--setting-sources ""` | **Do not load user/project/local settings.** Found live: the test machine's `~/.claude/settings.json` has `permissions.allow: ["Bash"]`, which silently bypassed every prompt. Isolation makes MAW's profile the only policy. |
| `--permission-prompt-tool stdio` | route prompts to MAW (Approval Center) instead of auto-denying |
| `--permission-mode`, `--allowedTools=…`, `--disallowedTools=…` | profile mapping below (`=` form because the flags are variadic) |
| `--strict-mcp-config --mcp-config=<json>` | only MAW's server; no ambient MCP servers |
| `--append-system-prompt <instructions>` | role instructions |
| `--model <m>` | when configured |
| `--session-id <uuid>` / `--resume <uuid>` | MAW picks the id, so it is known before the first event and can be persisted for recovery |

Never used: `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--bare`
(bare mode disables OAuth), any credential flag. Working directory = `SessionConfig.cwd`.

### MCP configuration (exact shape, verified)

```json
{"mcpServers":{"maw":{"type":"http","url":"http://127.0.0.1:PORT/mcp",
 "headers":{"Authorization":"Bearer ${MAW_MCP_TOKEN}"}}}}
```

* Server key must be **`maw`**: Claude Code names tools `mcp__maw__<tool>` (e.g.
  `mcp__maw__maw_task_create`); the allowlist rule is `mcp__maw__*`.
* `${MAW_MCP_TOKEN}` is expanded by Claude Code inside the header (verified: the server saw
  `Bearer <token from env>`). The token is passed in the child's **environment only**, so it never
  appears in `argv`/`ps`. Unit + contract tests assert this.
* Notes for the MAW MCP server: the client sends `Accept: application/json, text/event-stream`, a
  `server/discover` request *before* `initialize` (answer with JSON-RPC `-32601`; Claude then
  initializes normally), `notifications/initialized` (expects 202), then `tools/list`.
  MCP tools are loaded lazily through Claude's `ToolSearch` tool, which shows up as an extra tool
  call in the transcript.

## Permission model

MAW `PermissionProfile` → Claude Code flags (`args::permission_flags`):

| Profile | `--permission-mode` | Allowed without asking | Denied outright | Everything else |
|---|---|---|---|---|
| Safe | `default` | `Read Grep Glob LS`, `mcp__maw__*`, the CLI's built-in read-only shell set | `Edit MultiEdit Write NotebookEdit` | prompt → MAW approval |
| Balanced | `acceptEdits` | above + in-workspace edits (CLI rule) + `git status/diff/log/show/add/commit`, `cargo test/build/check/fmt/clippy`, `npm test/run test/run build`, `pytest`, `go test` | – | prompt → MAW approval |
| Autonomous | `acceptEdits` | above + `Bash` (any command) | – | prompt (paths outside the workspace, network, …) |

All profiles deny `git push`, `sudo` and `rm -rf /*`, `rm -rf ~*`: external side effects and
integration are the operator's/MAW's job. `Autonomous` is *broader*, not unrestricted; the CLI's
own sandbox/path checks still apply and no bypass flag is ever passed.

### What is approvable interactively

Everything that is neither allowed nor denied by the table raises a `can_use_tool` control request,
surfaced as `ProviderEvent::ApprovalRequested` (kind `Command` for Bash, `FileChange` for
Edit/Write family, `Other` for the rest) with the tool input, the CLI's description and blocked
path. `resolve_approval` maps decisions to the documented response shape:

| MAW decision | `control_response.response` |
|---|---|
| `Approve` | `{"behavior":"allow","updatedInput":<original input>}` (verified live) |
| `ApproveForSession` | same + `updatedPermissions` = the CLI's `addRules` suggestions re-targeted to `destination:"session"` (never `localSettings`, so nothing is written to the user's settings files) |
| `Deny` | `{"behavior":"deny","message":…}` (verified: file was not created) |
| `Abort` | deny with `interrupt:true`, plus an `interrupt` control request |

Not approvable interactively: anything in the deny list (hard denials produce a failing
`tool_result`, no prompt), and unsupported control-request subtypes (answered with an error
response so the CLI never waits forever). A pending approval is invalidated when its turn ends or
is interrupted; answering it afterwards returns a clear `Protocol` error.

## Event mapping

| Claude stream-json | `ProviderEvent` |
|---|---|
| `system/init` (one per turn) | `TurnStarted`; `Connected{id}` only if the id changed; `Error` (non-fatal, once) for MCP servers whose status is not `connected`/`pending` |
| `stream_event` `text_delta` | `MessageDelta` (thinking deltas are deliberately not forwarded; sub-agent streams skipped) |
| `assistant` text block | `Message` |
| `assistant` `tool_use` `Bash` | `CommandStarted{command}` |
| `assistant` `tool_use` other | `ToolStarted{name, detail}` (MCP tools keep the full `mcp__maw__…` name) |
| `user` `tool_result` for Bash | `CommandOutput` (≤8 KiB) + `CommandCompleted{exit_code}` (0 on success, parsed `Exit code N` on error, else `None`) |
| `user` `tool_result` for Edit/Write/MultiEdit/NotebookEdit | `ToolCompleted` + `FileChanged{paths}` when successful |
| `user` `tool_result` other | `ToolCompleted{ok}` |
| `control_request` `can_use_tool` | `ApprovalRequested` |
| `result` | `Usage` then `TurnCompleted`: `Completed`; `Interrupted` (`terminal_reason=aborted_streaming`, or an interrupt was requested and the result errored); `Failed{error}` otherwise |
| `result` error text mentioning login/auth | additionally `Error{fatal:true}` ("run `claude auth login`") |
| `system/api_retry` (`rate_limit`/`overloaded`) | `RateLimited{retry_after_ms}`; other categories → non-fatal `Error` |
| `rate_limit_event` with `status` ≠ allowed | `RateLimited` (plan windows with `allowed` are not surfaced) |
| process exit / stdout EOF | `Disconnected{reason}` (exit status + redacted stderr tail) |
| anything else, malformed lines | ignored (debug log), never fatal |

Usage: taken from `modelUsage` (conversation-cumulative; falls back to per-turn `usage`) with
`reported_by_provider = true`. `input_tokens` includes cache reads/writes (all tokens the model
read), `cost_usd` is the CLI's `total_cost_usd` client-side estimate. Subscription quota percentages
are not invented.

## Lifecycle

* **Capabilities**: streaming, interrupt, resume, approvals, `mcp_http`, usage reporting, model
  selection = true. `steer_active_turn = false`: extra input during a turn is *queued* by the CLI
  (allowed with `steer_if_active`), it does not redirect the running turn. A plain second
  `send_input` while a turn is active is rejected.
* **Resume**: a valid UUID in `resume_native_id` → `--resume`. If the conversation is gone (checked
  lazily: no `init` + stderr `No conversation found`), the adapter starts a fresh session, emits a
  non-fatal `Error` ("earlier conversation context is lost") and `Connected{new id}`, and
  re-delivers the user message; the bogus failed turn is never emitted.
* **Shutdown**: interrupt if busy → close stdin (4 s grace) → `SIGTERM` (Claude Code tears down
  its own process tree) → `SIGKILL`. `kill_on_drop` and `Drop` abort protect against leaks.
* **Back-pressure**: bounded event channel (`PROVIDER_EVENT_BUFFER`); a slow consumer stalls
  stdout reading instead of growing memory. Lines over 16 MiB are dropped.
* **Secrets**: stderr tail, disconnect reasons and logged lines pass through `maw_core::redact`.

## Authentication

Official login only. `auth_status()` runs `claude auth status --json` and parses
`loggedIn/authMethod/subscriptionType/email`. MAW never reads credential files, never implements
OAuth, never handles session tokens. To sign in: `claude auth login` (or `maw auth claude`, which
must delegate to that command). `ANTHROPIC_API_KEY`, if the user configured it, is inherited by the
child like any other environment variable; MAW does not set or manage it. Windows-native and WSL
logins are independent; the adapter checks whichever `claude` it launches.

## Tests

* `src/args.rs`, `src/mapper.rs`, `src/lib.rs`: 17 unit tests.
* `tests/contract.rs` (15): sanitized **real captures** in `fixtures/claude/*.jsonl` replayed through
  the mapper, plus the full process layer against `fixtures/claude/fake_claude.py`, a fake `claude`
  that replays those captures (handshake, two turns, argv/env assertions, busy-turn rejection,
  approve/approve-for-session/deny/abort payloads, interrupt, process exit + redaction, malformed
  lines, resume fallback, forced shutdown without leaked PIDs, missing binary, detect/auth).
* `tests/live.rs` (1, `#[ignore]` **and** requires `MAW_LIVE_CLAUDE=1`): real CLI, three `haiku`
  turns (≈ $0.015): init/MCP connected, MCP tool call authorised by the allowlist with the bearer
  token verified at an in-process HTTP stand-in, real approval round-trip.

```
MAW_LIVE_CLAUDE=1 cargo test -p maw-provider-claude --test live -- --ignored --nocapture
```

Captures: `two_turns_mcp_tool.jsonl`, `permission_request_denied.jsonl`,
`allow_interrupt_reuse.jsonl` (paths/skills/sockets stripped; no account data).

## Known limitations

* The stand-in MCP server used in tests is not the real `maw-mcp` server; integration with it is
  pending (the contract above is what it must satisfy).
* `--permission-prompt-tool stdio` is not listed in `--help`; if a future release removes it the
  adapter would see prompts auto-denied (visible as failing tool results). Pinned/tested: 2.1.309.
* The resume-failure detection relies on the stderr text `No conversation found`.
* Rate-limit *windows* (`utilization`) from `rate_limit_event` are parsed but not yet exposed because
  `ProviderEvent` has no variant for them; only rejections surface as `RateLimited`.
* Sub-agent (Task tool) internals are not mapped beyond their tool calls.
* `Safe` allows the CLI's built-in read-only shell commands; that set is defined by Claude Code, not
  MAW.
* Windows-native process management (`SIGTERM` step) falls back to kill; only Linux/WSL was tested.
