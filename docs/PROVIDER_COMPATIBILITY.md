# Provider compatibility

Integration decisions and the exact versions each was tested against. Per-provider detail: [`providers/codex.md`](providers/codex.md), [`providers/claude.md`](providers/claude.md).

Research sources consulted (2026-11): Codex App Server docs (`developers.openai.com/codex/app-server`, now served from `learn.chatgpt.com/docs/app-server`), the protocol schema **generated from the installed binary** (`codex app-server generate-json-schema`), Claude Code headless/Agent SDK docs (`code.claude.com/docs/en/headless`), `rmcp` 3.8.0 source (API read directly from the crate), MCP spec revision 2026-07-28.

## Tested matrix (WSL2 Ubuntu 24.04, x86_64)

| Component | Tested version | Status |
|---|---|---|
| Codex CLI | `codex-cli 0.166.0` | **Live-verified**: handshake, thread/turn, streamed events, usage, MCP over Streamable HTTP with bearer env var (against a stand-in server), real tool call. Approvals/interrupt/steer/resume verified against a protocol-faithful fake only. |
| Claude Code | `2.1.309` | **Live-verified**: stream-json multi-turn on one process, `system/init` MCP status, MCP tool call with bearer header, `can_use_tool` approvals (allow/deny/session), interrupt + reuse, resume (valid and invalid id). |
| `rmcp` | `3.8.0` | Server side tested with its own client (`crates/maw-mcp/tests/http.rs`). |
| Rust | 1.98.1 | — |
| Windows native | not tested | Use WSL2. |

Contract tests pin the provider versions (fixtures are tagged with the version they were captured from); an upgrade that changes event shapes fails `cargo test` explicitly instead of degrading silently.

## Codex — App Server (`codex app-server`, stdio JSONL)

- **Marked experimental by OpenAI.** Wire format: newline-delimited JSON-RPC *without* a `jsonrpc` field. We generate the schema from the installed binary to detect drift.
- Used: `initialize`/`initialized`, `thread/start|resume`, `turn/start|steer|interrupt`, server requests `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, notifications `turn/*`, `item/*`, `thread/tokenUsage/updated`, `account/rateLimits/updated`.
- Not used (deliberately): `account/login/*` (login is delegated to `codex login`), `fs/*`, `command/exec`, realtime, plugins.
- MCP registration (per thread, token via env var only): `{"mcp_servers":{"maw":{"url":"http://127.0.0.1:PORT/mcp","bearer_token_env_var":"MAW_MCP_TOKEN"}}}`.
- Unsupported on purpose: MCP elicitation / user-input requests are answered with an error so a turn never hangs.

## Claude Code — CLI stream-json (no TypeScript bridge)

- One long-lived `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages` per agent; multi-turn works on one process. The Agent SDK / TS bridge was **not** added: the CLI already provides interrupts, MCP, approvals (`--permission-prompt-tool stdio` + `control_request/can_use_tool`) and usage, so a Node dependency would add cost with no verified benefit.
- MCP registration: `--mcp-config '{"mcpServers":{"maw":{"type":"http","url":"…","headers":{"Authorization":"Bearer ${MAW_MCP_TOKEN}"}}}}'`; the server key **must** be `maw` (tools appear as `mcp__maw__*`). The token lives only in the child's environment.
- **`--setting-sources ""` is always passed.** Observed on the development machine: a user-level `permissions.allow:["Bash"]` silently bypasses every permission prompt, which would defeat MAW's profiles. MAW's profile is therefore the only policy source. (Consequence: user/project Claude settings, CLAUDE.md-style *settings*, are not loaded for MAW-managed agents.)
- `--permission-prompt-tool stdio` is not listed in `--help` but is referenced by the headless docs and works in 2.1.309; treated as version-pinned behavior.
- Never used: `--dangerously-skip-permissions`, `--bare`.
- Cost figures come from Claude Code's `total_cost_usd`, which Anthropic documents as a client-side estimate; MAW labels them as provider-reported, never as billing.

## Capability matrix (reported by sessions via `Capabilities`)

| | Codex | Claude Code | Mock |
|---|---|---|---|
| streaming | ✓ | ✓ | ✓ |
| interrupt | ✓ | ✓ | ✓ |
| resume | ✓ (falls back to fresh thread with a warning) | ✓ (falls back with a warning) | — |
| steer active turn | ✓ | — (CLI queues input) | — |
| interactive approvals | ✓ | ✓ | ✓ |
| MCP over HTTP | ✓ | ✓ | — (calls tools in-process) |
| provider-reported usage | ✓ (tokens, cumulative per thread) | ✓ (tokens + client-side cost estimate) | — |

## Known gaps

- No provider exposes subscription quota in a form MAW can show; MAW therefore shows rate-limit *events* only and never a quota percentage.
- Codex workers under `workspace-write` may be unable to `git commit` in a worktree (metadata outside the writable root). MAW commits on submission instead, so worker diffs are still captured.
- Claude's `ToolSearch` loads MCP tools lazily; the first coordination call in a session may take an extra step.
