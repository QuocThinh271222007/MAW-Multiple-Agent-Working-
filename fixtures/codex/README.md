# Codex App Server fixtures

Raw server -> client lines captured from a **real** `codex app-server` session
(codex-cli **0.166.0**, ChatGPT login, Linux/WSL2) by the opt-in live tests
(`MAW_LIVE_CODEX=1 MAW_CODEX_CAPTURE=file cargo test -p maw-provider-codex --test live -- --ignored`).

| file | what it shows |
| --- | --- |
| `turn_ok_0.166.0.jsonl` | handshake, `thread/start`, a plain turn: deltas, final message, token usage, rate-limit snapshot, `turn/completed` |
| `mcp_tool_call_0.166.0.jsonl` | the same plus `mcpServer/startupStatus/updated` for the `maw` server and an `mcpToolCall` item (`maw/maw_goal_get`) |
| `mcp_elicitation_request_0.166.0.jsonl` | the verbatim `mcpServer/elicitation/request` codex sends (policy `on-request`) before running a **non-read-only** MCP tool when the server is not pre-approved: `_meta.codex_approval_kind = "mcp_tool_call"`, tool args in `_meta.tool_params`, `persist: ["session","always"]`. Captured from a live control run (only the request line, kept verbatim) |

Sanitized: home directory, hostname, installation id and temp directory names are replaced.
Secrets are redacted by the adapter at capture time. Re-capture (and bump the file names and the
pinned version in `tests/fixtures.rs`) when upgrading the tested Codex version.
