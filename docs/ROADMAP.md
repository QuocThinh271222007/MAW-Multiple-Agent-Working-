# Roadmap & status (updated at the end of session 1)

## Verified (WSL2 Ubuntu 24.04)
- ~230 automated tests passed at the last full run (core, storage, orchestrator incl. e2e + crash recovery, MCP via real `rmcp` client, Codex/Claude adapters against fakes + sanitized live captures, TUI render/reducer). Last full clippy/fmt/test pass was before the final edits listed under "Unverified since last full run".
- **Real providers, live:** Claude Code 2.1.309 Leader + Codex 0.166.0 Worker completed a 1-task goal end to end through MAW's MCP server (exit 0, verified, goal completed). A Claude Leader + two Codex Workers ran two tasks in parallel, exchanged direct messages, submitted evidence and were verified (100% verified progress), but the goal was left open (Leader idle) — fixed afterwards by the completion nudge + continuous integration.
- Measured (debug build, 3 live agents): `maw` ≈ 30 MB RSS, ≈ 2% CPU; each provider process 215–270 MB.

## Unverified since last full run (do these first)
1. ~~Full gate~~ done after the model/effort work: fmt clean, clippy 0 warnings, 215 tests passed.
2. Re-run the real 3-agent scenario (`~/real_auth2.sh` pattern): the last attempt was cut off by the **Claude 5-hour usage limit** (MAW correctly showed RATE_LIMITED and recorded the reset time), so the nudge/continuous-integration path has only been tested with the mock provider.
3. Commit state: see `git log`.

## Added later in session 1
- Per-agent **model** and **effort** (migration `0002_agent_effort`), `maw models`, `maw agent model|effort|permission`, TUI picker, provider plumbing (Codex `turn/start.effort`, Claude `--effort`). Verified: unit/contract tests and live `maw models`; Claude ids `claude-haiku-5-5`, `claude-sonnet-5-5`, `claude-opus-5-5` were exercised live (`claude-fable-5-1` is accepted but the account was out of credits for it). **Not yet verified live:** that a non-default effort level actually changes behaviour in a running Codex/Claude agent session.
- TUI `!` shell, scrolling title/hint bars, working-folder display.
- Model/effort entry is pick-list based (TUI add-agent dialog, CLI numbered picker) and all typed values are validated against the provider's list (`maw_orchestrator::models`). Verified with the installed binary against the live Codex list.
- Add-agent login: `maw agent add` / TUI offer the provider's official login when it is not logged in (verified with a fake provider on a pty and by tests; **the TUI hand-over of the terminal has not been exercised in a real terminal**).
- Test totals after this work: 252 passed, 0 failed, 6 live tests ignored.

## Not done / known issues
- Native Windows build untested (use WSL2). TUI interactive feel not yet exercised in a real terminal (tests only). Mouse is scroll-only.
- Rate limit: MAW shows RATE_LIMITED with the provider's reset text but does not yet auto-resume at the reported reset time (retry_after_ms is None for Claude; parse "resets at unix N").
- Headless approvals: prompt on TTY, otherwise denied after 120 s (by design). The Leader burned minutes on denied compound shell commands; consider letting profiles allow read-only `cd && git/cat/ls` in worker worktrees.
- Codex workers cannot `git commit` in worktrees (read-only git metadata); MAW commits for them.
- Claude invoked with `--setting-sources ""` (user settings ignored on purpose).
- Not implemented: web dashboard, metrics export file, release packaging/CI artifacts, TypeScript bridge (not needed), benchmark doc (`docs/PERFORMANCE.md`), `maw approvals` CLI for other terminals, per-task branches (currently per-agent branches), reviewer role never exercised with real providers.

## Next steps
1. Run the verification list above; commit.
2. Real run with a Reviewer agent and a forced blocker; real crash/restart (`kill -9` mid-run, then `maw resume`).
3. Auto-resume after rate-limit reset; metrics export; packaging.

## Added: agent removal, token usage, conversation filters
- `Engine::remove_agent` / `Supervisor::remove_agent` / `maw agent remove` / TUI `X`: blocked by RUNNING / IN_REVIEW / VERIFYING tasks and by being the Leader of an active goal unless forced; force releases tasks; history, worktree and branch are kept; undelivered mail is closed.
- Token usage: `Engine::record_usage` stores per-report increments attributed to the current task; `maw usage [--json]`; TUI `$` view; `Tokens:` in the title bar. Verified against real recorded Claude/Codex runs (`maw usage` on `~/maw-real-runs/*`).
- Transcript kinds `Thinking` and `Protocol` (MAW's own `maw_*` tool calls and dispatch notices), filters `T`/`L`/`P`.
- **Finding:** with Claude Code 2.1.309 (Sonnet 5.5, effort high) the thinking block is empty text + encrypted signature; with Codex 0.166 (gpt-6.1-sol, summary detailed, effort high) the completed `reasoning` item has empty `summary` and `content`. The adapters forward reasoning when a provider sends it; today the Thinking filter therefore shows nothing for these models.
- Not done: per-agent context-window usage (Codex reports it, MAW does not surface it yet); token history is limited to the last ~3000 stored usage events.

## Added: local models (Ollama), model download, Codex reasoning summaries
- `maw-provider-ollama`: MAW-run agent loop over `/api/chat` with local file/command tools (workspace-confined, permission-gated), `maw_*` tools through an MCP client, approvals, interrupt, context trimming, usage from the server's counts. `ProviderKind::Ollama`.
- Autostart of `ollama serve` when an agent needs it (also the Windows build from WSL, bound to the WSL-reachable address). `maw models pull` and the TUI *model download* dialog (`POST /api/pull`), with parameter-count and size limits (defaults 8B / 6.5 GB).
- Codex: `model_reasoning_summary` is sent with each thread, which makes Codex stream reasoning summaries (short headings, not full reasoning). Claude Code in headless mode still returns empty thinking text (known upstream limitation); MAW does not try to work around it.
- TUI: `mock` is only offered in demo projects; in long pick-lists the arrow keys continue to the neighbouring field past either end.
- Verified live: one `qwen3:4b` turn using `write_file`. **Not verified with a real local model:** the other file tools, `run_command`, `maw_*` tools, a full Leader/Worker/verify cycle, and the TUI download dialog in a real terminal.
- Ollama effort: only `gpt-oss` has levels (`low/medium/high`, untested on a real gpt-oss); other models have none (`think: false` is unreliable). Fixed on the way: `maw agent add --effort` did not validate the level against the model, and the agent model dialog (`M`) built its own effort list instead of using the shared one.
- Known limits: no OS sandbox for local commands; small models follow the MAW protocol less reliably; `think: off` does not disable reasoning on `qwen3:4b`; autostarted servers keep running after MAW exits.
- Performance view (`%`): CPU/RAM (Windows host through PowerShell when under WSL, `/proc` on Linux), GPU/VRAM/temperature/power via `nvidia-smi`, generation speed and loaded models from Ollama, as line charts over 1/5/30 minutes. Sampled live on the dev machine (RTX 3050 6 GB laptop under WSL2); rendering checked with test frames, not yet by eye in a real terminal. NVIDIA only; history is in memory only.
