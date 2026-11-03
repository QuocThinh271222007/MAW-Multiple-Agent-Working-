# Ollama provider (local models)

Run coding agents on models stored on your own machine. No account, API key or internet is needed
once a model is downloaded.

## How it differs from Codex and Claude Code

Codex and Claude Code are agent programs; MAW starts them and talks to them. **Ollama is only a
model server** (HTTP): it has no agent loop, no file tools and no MCP support. So for Ollama agents
MAW runs the loop itself (`crates/maw-provider-ollama`):

1. send the conversation and tool definitions to `POST /api/chat` (streamed);
2. run the tools the model asks for;
3. feed the results back and repeat until the model answers without a tool call.

Tools the model gets:

| Tool | Purpose | Permission |
|---|---|---|
| `read_file`, `list_dir`, `search_text` | inspect the workspace | always allowed (inside the workspace) |
| `write_file`, `edit_file` | change files | `safe`: asks; `balanced`/`autonomous`: allowed |
| `run_command` | shell command in the workspace | `safe`/`balanced`: asks; `autonomous`: allowed unless flagged risky |
| `maw_*` | MAW coordination (tasks, messages, results) | by role, through MAW's MCP server with the agent's own token |

The `maw_*` tools reach MAW through a small MCP **client** built into the provider; Ollama itself
never sees MCP. Third-party MCP servers are not supported for Ollama agents.

## Honest limits

* **No OS sandbox.** Unlike Codex, nothing confines `run_command`. File tools are confined to the
  agent's worktree (symlinks resolved, `.git` read-only) and approvals gate commands, but a command
  you approve can do anything your user account can. The "risky command" list
  (`rm`, `sudo`, `git push`, network tools, package installs, ...) is a heuristic guard rail, not a
  security boundary. Child processes do not inherit `MAW_*` environment variables.
* **Small models are weaker.** A 4B model may call tools wrongly or forget to report through
  `maw_task_submit_result`. Treat results as unverified; MAW's evidence rules still decide DONE.
* **Reasoning is shown only when the model streams it.** Thinking-capable models (Qwen3, gpt-oss, ...)
  send it in a separate field; it appears in the Thinking lines.
* **Effort** maps to Ollama's `think` option: `on`/`off` for thinking models, `low`/`medium`/`high`
  for `gpt-oss`, `off` only for models that cannot think. On some builds (observed with `qwen3:4b`)
  `off` does not stop the reasoning, it just stops separating it from the answer.
* **Context window** is `num_ctx` (default 8192). Bigger windows use more memory. Old tool output is
  trimmed from the history when the window fills up (token counts used for this are estimates and are
  never shown as usage).
* **Usage** is what Ollama reports per request (`prompt_eval_count`, `eval_count`), summed per
  session. Cached prompt tokens may not be counted by the server.
* Sessions are **not resumable**: history lives in memory.

## Server discovery and autostart

The server URL comes from `[ollama] url`, else `$OLLAMA_HOST`, else `127.0.0.1:11434`, else (WSL
only) the Windows host. An explicit `url` is the only address tried.

When an Ollama **agent** starts and nothing answers, MAW starts `ollama serve` itself (`autostart =
true`, on by default): the `ollama` on `PATH`, or `ollama.exe` in the usual Windows install folder
(also from WSL). Listing models or `maw doctor` never start anything. The server MAW starts keeps
running after MAW exits, like the Ollama desktop app; stop it yourself when you need the memory.

Under WSL2 with the default NAT networking, a Windows Ollama must listen on the Windows side of the
WSL network (MAW sets `OLLAMA_HOST` to that address when it starts it) or WSL cannot reach it.
`networkingMode=mirrored` in `.wslconfig` avoids the issue; MAW never edits your WSL configuration.

## Downloading models

```bash
maw models pull                 # suggestions with sizes; installed ones are marked
maw models pull qwen3:8b        # a specific model
maw models --provider ollama    # what is installed (with tool / thinking support)
```

In the TUI: palette (`/`) -> *model download*. This uses Ollama's documented `POST /api/pull`
(streamed progress). After a download MAW asks the server for the model's capabilities and warns if
it has no tool-calling support (such a model cannot run as an agent; MAW refuses to start one).

Limits (defaults assume ~8 GB of VRAM; change in `[ollama]`):

* `max_pull_params_b = 8.0`: a tag such as `:70b` is refused before anything is downloaded;
* `max_pull_gb = 6.5`: a download whose reported size exceeds this is cancelled.

These are about download size, not your actual VRAM: check it (`nvidia-smi`). Models larger than
VRAM still run if the rest fits in RAM, but slowly.

## Configuration

```toml
[ollama]
url = ""                 # empty = auto
num_ctx = 8192
keep_alive = "10m"
max_steps = 40           # model/tool round trips per turn
command_timeout_secs = 120
autostart = true
# path = "/path/to/ollama"
max_pull_params_b = 8.0
max_pull_gb = 6.5
```

## Watching the machine

`%` in the TUI shows CPU, RAM, GPU, VRAM, generation speed and the GPU/RAM split of the loaded
models while agents run (see `docs/TUI.md`). A model that only partly fits in VRAM runs partly from
RAM; the view shows the split so you can tell why it is slow.

## Testing

* Unit and integration tests use a scripted fake Ollama server with MAW's real engine and MCP server.
  They prove MAW's side of the protocol, not the behaviour of any model.
* Live: `MAW_LIVE_OLLAMA=1 cargo test -p maw-provider-ollama --test live -- --ignored --nocapture`
  (model from `MAW_LIVE_OLLAMA_MODEL`, default `qwen3:4b`). Verified once: `qwen3:4b` created a file
  with `write_file` and streamed its reasoning. Not yet verified with a real model: `read_file`,
  `edit_file`, `search_text`, `run_command`, the `maw_*` tools, and a full Leader/Worker/verify cycle.
