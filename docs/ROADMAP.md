# Roadmap & status (updated at the end of session 1)

## Verified (WSL2 Ubuntu 24.04)
- ~230 automated tests passed at the last full run (core, storage, orchestrator incl. e2e + crash recovery, MCP via real `rmcp` client, Codex/Claude adapters against fakes + sanitized live captures, TUI render/reducer). Last full clippy/fmt/test pass was before the final edits listed under "Unverified since last full run".
- **Real providers, live:** Claude Code 2.1.309 Leader + Codex 0.166.0 Worker completed a 1-task goal end to end through MAW's MCP server (exit 0, verified, goal completed). A Claude Leader + two Codex Workers ran two tasks in parallel, exchanged direct messages, submitted evidence and were verified (100% verified progress), but the goal was left open (Leader idle) — fixed afterwards by the completion nudge + continuous integration.
- Measured (debug build, 3 live agents): `maw` ≈ 30 MB RSS, ≈ 2% CPU; each provider process 215–270 MB.

## Unverified since last full run (do these first)
1. `./scripts/wsl-cargo.sh clippy --workspace --all-targets && ... test --workspace` (orchestrator tests incl. the new nudge test passed; whole workspace not re-run).
2. Re-run the real 3-agent scenario (`~/real_auth2.sh` pattern): the last attempt was cut off by the **Claude 5-hour usage limit** (MAW correctly showed RATE_LIMITED and recorded the reset time), so the nudge/continuous-integration path has only been tested with the mock provider.
3. Commit state: see `git log`.

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
