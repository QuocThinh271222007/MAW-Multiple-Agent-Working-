# Security model

MAW coordinates AI agents that can edit files and run commands, so its job is to *not add* authority and to keep a human in control. This document states what is enforced today and what is not.

## Trust boundaries

| Boundary | Treatment |
|---|---|
| Repository files, command output, agent text, MCP arguments | **Untrusted data.** Role prompts say so; authorization never depends on what an agent claims. |
| Agent identity | Assigned by MAW. Each session gets a random bearer token (`maw_tok_…`, 192-bit) minted per connect, held in memory only, revoked on disconnect/shutdown. The MCP layer maps token → agent → **role stored in the database**. Tool arguments cannot name the caller. |
| MCP network surface | Loopback only (`serve_http` refuses non-loopback binds). `Host` must be loopback (DNS-rebinding defense); any browser `Origin` is rejected; every request needs a valid bearer token (401 otherwise). Tests: `crates/maw-mcp/tests/http.rs`. |
| Providers | Authentication is the provider's own (see AUTHENTICATION.md). MAW never reads, stores or proxies provider credentials. |
| Filesystem | Workers run in per-agent git worktrees under `<repo>/.maw/worktrees`; artifact paths must be project-relative; `workspace::is_within` normalizes `..`. The user's checkout is never written by MAW; integration happens on `maw/integration`. |

## Authorization (enforced twice)

1. The MCP layer hides tools a role may not call (`tools/list` is role-filtered) and rejects calls with `forbidden`.
2. Each engine operation re-checks (Leader-only: create/update/assign/verify/goal/plan; owner-only: progress/submit/block; reviewers cannot be owners; instructions only from Leader/user; inbox/ack only for the recipient). Covered by `crates/maw-orchestrator/tests/engine_rules.rs`.

The MCP server **cannot execute shell commands or touch files**; it only mutates MAW's task/message state. Execution is controlled by the orchestrator through provider sessions.

## Permissions

Profiles map to provider-native approval/sandbox settings; MAW additionally records every provider approval request in the Approval Center (agent, operation, paths, reason) and waits for the user. A Leader request never auto-approves anything. `danger-full-access` / `--dangerously-skip-permissions` are never used. Approvals whose session vanished are marked `expired`, not silently granted.

**Known limit:** MAW can only gate what the provider surfaces as an approval request; it does not sandbox the provider processes itself.

## Data protection

- `maw_core::redact` strips common key/token shapes from logs, transcripts, evidence and messages before persistence (best effort, pattern based — not a guarantee).
- Logs rotate (3×5 MB) and are redacted at write time. No tokens in argv or on disk.
- The SQLite database contains goals, messages, task text and agent output summaries. It is created with the user's default file mode; treat it like source code.

## Abuse resistance

Bounded task count/depth per goal, message (32 KiB) and context (64 KiB) sizes, bounded provider channels/transcripts, bounded retries with backoff, lease timeouts, one live token per agent.

## Not yet covered (honest list)

- No OS-level sandboxing of providers beyond what Codex/Claude provide.
- Secret redaction is heuristic.
- Native Windows hardening (job objects, ACLs) is unvalidated; use WSL2.
- Streamable HTTP is plain HTTP on loopback; other local users on a multi-user host could connect to the port but cannot authenticate without a token.
