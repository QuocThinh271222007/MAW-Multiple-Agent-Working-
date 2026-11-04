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

## Small models and the MAW tools (measured)

MCP is MAW's way of handing a model its coordination tools; a local model has no MCP support of its
own, so the provider is the MCP client and shows the model the tools as ordinary function
definitions. What that means in practice, measured with `qwen3:4b` as a **Worker** on a one-file
task (live test `tests/live_worker.rs`, context 8192, one run per row, so treat as an anecdote):

| | before the fixes below | after |
|---|---|---|
| Did the work and verified it by reading the file back | yes | yes |
| Reported through `maw_task_submit_result` | after 4 rejected tries (guessed `kind` values) | first try |
| Time / tokens | 425 s / 51k | 180 s / 20k |
| Prompt size of the first request (role instructions + tool schemas + task) | ~2.9k tokens | ~3.0k tokens |

What went wrong and what was fixed (these help hosted models too):

* The provider used to drop the `definitions` section of tool schemas but keep the `$ref`s, so a
  model never saw the allowed `kind` values of an evidence item. References are now inlined.
* The engine's error for a bad evidence kind now lists the valid ones.
* After a **successful** `maw_task_submit_result`, `maw_task_block` or `maw_review_submit` the turn
  ends. Small models otherwise kept re-submitting or talking about the state.

Still true: a 4B model is clumsy. It can need several tries, can misread MAW's states, and was only
tried as a Worker on a trivial task. The Leader (planning, many tools, long context) and Reviewer
roles were not tried with a local model and `maw agent add` warns about the Leader. Use a local model
as a Worker and a hosted one as Leader; keep tasks small with explicit acceptance criteria.

## Starting the server by itself, and messages

* **After a reboot** Ollama is not running. MAW connects the agent, sees no server and starts it
  (`ollama serve`, or the Windows app from WSL). Earlier the connect step treated "no server" as a
  login problem and stopped before the start, so nothing was launched; a stopped server now counts as
  "unknown, will be started" while `autostart` is on. Turn it off in `[ollama]` to keep the old strictness.
* **Messages you type to an agent** reach a local model as a short brief: who wrote it, the text, and
  the exact `maw_message_ack` call with the message id to copy. Measured once with `qwen3:4b` (create
  a file, then acknowledge): with the new wording it wrote the file and acknowledged correctly on the
  first try (102 s, 12k tokens). The old wording (a bare hex id and "act using the maw_* tools") did
  not finish within 15 minutes in the one run I made. One run each, so an anecdote, not a statistic.

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
* **Effort** exists only where the model really has levels: the `gpt-oss` family takes `low` /
  `medium` / `high` (sent as Ollama's `think` level). Every other model has **no effort setting**:
  thinking models think whenever Ollama is asked to, and models that cannot think get no `think`
  field. A switch to turn thinking off is deliberately not offered: with `qwen3:4b` the request
  `think: false` did not stop the reasoning, it only moved the text from the separate field into
  the answer (and used more tokens). The CLI and dialogs refuse an effort for a model without
  levels; if the server cannot be reached to read the model list, no levels are offered either.
  A value stored by an earlier version (`on` / `off`) is ignored. The `gpt-oss` levels are
  implemented but not yet tried on a real `gpt-oss` model.
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
