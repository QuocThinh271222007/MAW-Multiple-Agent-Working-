# Authentication

**MAW never handles provider credentials.** It does not implement OAuth, read credential files, scrape browser sessions, or proxy subscription logins. It only (a) asks each provider's own CLI whether it is logged in, and (b) hands your terminal to the provider's official login command.

| Command | What it does |
|---|---|
| `maw auth status` | Asks `codex` / `claude` for their auth state and prints method, account and plan **as the provider reports it**. |
| `maw auth codex` | Runs `codex login` with your terminal (ChatGPT account / device-code flow as offered by Codex). |
| `maw auth claude` | Runs `claude auth login` (Claude subscription login or Console/API-key flow as offered by Claude Code). |
| `maw doctor` | Checks both providers and tells you exactly what to run. |

After logging in, start MAW; agents whose provider is not authenticated show `AUTH_REQUIRED` with the hint instead of failing silently.

## Per-environment note (WSL)

Authentication in Windows and in WSL2 is **separate**. Run `maw auth …` from the same environment MAW runs in (inside WSL if you run MAW there), and install `claude`/`codex` in that environment too. `maw doctor` prints this reminder under WSL.

## API-key mode

If you prefer API billing, use the provider's own documented mechanism (for Claude Code, its `ANTHROPIC_API_KEY`/Console login; for Codex, its API-key login) in the environment MAW is started from. MAW passes the process environment through to provider processes unchanged and never writes keys anywhere. Subscription and API usage are billed by the providers, not MAW; MAW displays only usage numbers the provider reports (tokens, and cost where Claude Code reports `total_cost_usd`, which Anthropic documents as a client-side estimate) and never invents quota percentages.

## Multiple profiles

Provider profiles are the providers' own (e.g. `CODEX_HOME`, `CLAUDE_CONFIG_DIR`). Start MAW with those variables set to use a different account; MAW stores nothing secret itself.

## What MAW stores

`~/.config/maw/config.toml` (UI and orchestrator settings, optional executable paths), `~/.config/maw/projects.json` (project registry), the per-project SQLite database (tasks, events, messages). Per-session MCP tokens exist only in memory.
