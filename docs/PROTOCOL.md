# MAW protocols

## 1. MCP coordination server (`maw-coordination`)

- **Transport (primary):** Streamable HTTP at `http://127.0.0.1:<ephemeral>/mcp`, served by `rmcp` 3.8 inside the MAW process. Legacy session mode is enabled (clients negotiating revisions before 2026-07-28 get `mcp-session-id` sessions); clients negotiating 2026-07-28 are served statelessly by `rmcp`. Not every client supports the newest revision, so MAW does not require it.
- **Transport (secondary):** stdio, `maw mcp serve --stdio --as <user|agent>`.
- **Auth:** `Authorization: Bearer <token>` on every request. Tokens are minted by MAW per agent session (`Engine::mint_token`) and injected into the provider's MCP configuration (as an env-var reference or in-memory header — never on disk). Missing/forged/revoked → HTTP 401.
- **Origin/host:** non-loopback `Host` and any `Origin` header are rejected.
- **Capabilities:** `tools` only. (MAW's persistent task board is deliberately *not* the optional MCP Tasks extension.)
- **Tool visibility:** `tools/list` returns only tools the caller's role may use.
- **Results:** success → `structuredContent` JSON; failure → `isError: true` with
  `{"error": {"code": "<stable_code>", "message": "..."}}`. Codes: `not_found`, `forbidden`, `invalid_input`, `invalid_transition`, `dependency_cycle`, `conflict`, `limit_exceeded`, `precondition_failed`, `unsupported`, `storage_error`, `internal_error`.
- **Idempotency:** `maw_task_create` and `maw_message_send` accept `idempotency_key`; retries return the original record.

The complete, generated tool reference (schemas, roles, read-only flags) is [`MCP_TOOLS.md`](MCP_TOOLS.md) (`maw tools --markdown`).

Tool groups: goal/plan (5), tasks (13 incl. `maw_task_verify`, `maw_review_submit`), progress (5), communication (5), agents (4), shared context (4).

## 2. Event envelope

```json
{
  "schema_version": 1,
  "event_id": "evt_…", "sequence": 123, "timestamp": "2026-11-02T14:31:07.120Z",
  "project_id": "prj_…", "run_id": "run_…", "agent_id": "agt_…", "task_id": "tsk_…",
  "correlation_id": null,
  "event_type": "task.progress",
  "payload": { "task_key": "T-3", "percent": 40, "note": "…" }
}
```

Categories: `agent.*`, `goal.*`, `plan.revised`, `task.*`, `artifact.registered`, `message.*`, `handoff.*`, `agent.help_requested`, `context.published`, `decision.recorded`, `mcp.tool_called|completed`, `permission.requested|resolved`, `provider.rate_limited|reconnecting|error`, `session.recovered`, `recovery.required`, `workspace.updated`, `scheduler.paused|resumed`, `run.started|stopped`. Authoritative list: `EventKind` in `crates/maw-core/src/event.rs` (a test enforces unique, round-trippable names).

## 3. Messages

`{message_id, sender_agent_id, recipient_agent_id, project_id, task_id, message_type, subject, content, created_at, delivered_at, acknowledged_at, correlation_id}`, types `INSTRUCTION | QUESTION | ANSWER | STATUS | RESULT | BLOCKER | REVIEW_REQUEST | REVIEW_FEEDBACK | HANDOFF`.

Lifecycle: **created** (tool call returns, `delivered:false`) → **delivered** (Supervisor starts a turn on the recipient's session containing it; `message.delivered`) → **acknowledged** (recipient calls `maw_message_ack`). Delivery waits for the recipient to be idle — an agent mid-turn is not interrupted — and the Leader's inbox is debounced so bursts of reports become one turn. Only the Leader/user may send `INSTRUCTION`.

Handoffs: `maw_handoff_create` sends a `HANDOFF` message; ownership moves only when the recipient calls `maw_handoff_accept`.

## 4. Provider adapter contract

`crates/maw-core/src/provider.rs`. Adapters translate native protocols into `ProviderEvent`: `Connected`, `TurnStarted`, `MessageDelta/Message`, `ToolStarted/Completed`, `CommandStarted/Output/Completed`, `FileChanged`, `ApprovalRequested{request_id,…}`, `TurnCompleted{outcome}`, `Usage` (provider-reported only), `RateLimited`, `Error{fatal}`, `Disconnected`. Answering approvals goes back through `AgentSession::resolve_approval`. See `docs/providers/` and `PROVIDER_COMPATIBILITY.md` for native protocol details and tested versions.
