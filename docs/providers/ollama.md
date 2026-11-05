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

Set either to `0` for no limit (your machine, your call): `max_pull_gb = 0` lets any size download.

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

## Conversations are kept

* **What you see.** Every agent's conversation (your messages, its replies, tool lines) is written
  to the project database (`transcript_lines`, migration 0003, up to 2000 lines per agent, secrets
  redacted) and loaded again when the agent view opens. This applies to all providers, not only Ollama.
* **What the model remembers.** Ollama keeps no conversations, so MAW saves the model's side itself in
  `<project data dir>/ollama-sessions/<id>.json` (mode 0600, redacted) at the end of each turn and on
  disconnect. When MAW reconnects the agent with its saved session id the model gets the earlier turns
  again, like `--resume` for Claude Code and Codex. If the file is gone the agent starts fresh and the
  transcript says so. Long histories are still trimmed to the context window as before.
* **Not saved:** a line still streaming when the process is killed (a normal quit and a finished turn
  write it); approvals that were pending.

## Two local agents talking to each other

A Leader on a small model works, but three things went wrong in a real project and were fixed:

* The Leader invented worker names (`worker1`) because it never listed the agents. The Leader
  instructions and the brief that accompanies a user's message now tell it to call `maw_agent_list`
  first, and an unknown assignee is rejected with the names that exist.
* `maw_task_create` failed with "no active goal" and the model did not know what to do. The message now
  says that a Leader can create the goal with `maw_goal_create`.
* Some models write the tool call as text (`tool_call {"name": ...}`) instead of using the tool
  interface, so nothing ran. MAW now reads such a call, but only when it names a real tool.

### Measured: user -> Leader -> worker -> verified goal, both local

One run through the real supervisor (`tests/live_team.rs`, Leader `qwen3:4b`, worker `qwen3:1.7b`, a git
repo in a temp folder). The user asked the Leader to give the worker `tester` a task: create `hello.txt`.

| | before the fixes above | after |
|---|---|---|
| Goal created by the Leader | yes (after 170 s of thinking) | yes (105-160 s) |
| Tasks | T-1 and a duplicate T-2 | one task |
| Worker | did T-1; got T-2 and kept reporting T-1 | did T-1, submitted on the second try (forgot the `task` argument once) |
| Leader's verification | T-1 verified at 495 s; T-2 BLOCKED, goal could not finish | blocked once without file evidence (an earlier run), then, with MAW recording the files it saw changed, verified and completed the goal at about 415 s |

One run each, so an anecdote. It shows the cycle can complete with two 1.7B-4B models on a trivial task;
it does not show they cope with real multi-file work. Three further changes came from these runs:
duplicate open tasks are returned instead of created twice, a worker who names its old task is told
which one is running, and MAW records the files it observed changed in the worker's worktree as
`file_changes` evidence when the worker did not list them.

## A Leader that repeats itself and forgets (qwen3:4b)

Seen in a real project: asked to hand out a task, the Leader listed the agents three times, then ended
its turn with "I am checking the list, please wait", and kept doing so when asked again. Measured:

* The Leader's first request is about **4,230 tokens** (instructions and the many MAW tool schemas), a
  worker's about 3,000, before any conversation, in a window of 8,192.
* `qwen3:4b` (the default tag) is a **thinking-only** model here: `think:false` makes it write its reasoning
  into the answer (1,645 tokens for "say hello"), and `/no_think` is ignored. Its reasoning is thousands
  of tokens per step.
* When prompt plus reply no longer fit the window, Ollama silently drops the START of the prompt, which is
  the instructions and tool list. The model then re-reads the conversation, forgets its role and loops.

What MAW does about it now:

* The Leader gets a larger window: `ollama.num_ctx_leader` (default 12288; `num_ctx` stays 8192 for
  workers). It costs memory: for a 4B model about 1 GB more than 8192. On a 6 GB GPU 12288 fits; if the
  model gets pushed to the CPU, lower it, or raise it only if you have the room (`maw config set
  ollama.num_ctx_leader=16384`).
* A warning appears in the agent's conversation when the instructions and tools already take more than
  45% of the window, and once when a request fills 92% of it ("context window is full ...").
* A read-only call repeated with the same arguments in one turn (`maw_agent_list`, `read_file`, ...) is
  answered with "you already called this, use the result" instead of running again; any write, command
  or MAW action lifts that.

Options if it still loops: a Leader on a model that does not think, for example
`qwen3:4b-instruct-2507-q4_K_M` (2.5 GB, now in the download list) or a 7-8B instruct model, with the local
model as Worker only. The instruct tag was found in the Ollama registry but has **not been tried** with
MAW's Leader role here.

## The download list: who makes each model, what it can do, and effort

`/download model` lists models with sizes and parameter counts read from the Ollama registry, the maker
from the library page, and the capabilities the library lists for the family (tools, thinking, vision,
audio). The server is asked again after the download: a model without tool calling cannot run an agent.

**Effort is only real where Ollama offers it.** Only `gpt-oss` has levels (low/medium/high). Every other
model, thinking or not, shows "no effort levels" and the effort picker is not offered for it. A thinking
model thinks as much as it wants, and switching thinking off is not reliable: measured on `qwen3:4b`, it
writes its reasoning into the answer instead (1,645 tokens for "say hello"), and `/no_think` is ignored.
The other thinking models in the list are untested for that.

| model | size | maker | tools | thinking | note |
|---|---|---|---|---|---|
| qwen3.5:4b | 3.3 GB | Alibaba (Qwen) | yes | yes | vision, 256K |
| qwen3:4b-instruct-2507-q4_K_M | 2.5 GB | Alibaba (Qwen) | yes | no | steadier as Leader |
| qwen3:4b | 2.5 GB | Alibaba (Qwen) | yes | always | cannot be turned off |
| gemma4:e2b | 4.6 GB | Google | yes | yes | 4.6B total, 2B effective |
| granite4.2:3b | 2.2 GB | IBM | yes | yes | 128K |
| ministral-3:3b | 3.0 GB | Mistral AI | yes | no | vision |
| lfm3:1.7b | 1.1 GB | Liquid AI | yes | yes | tri-hybrid, for local agents |
| bonsai-2:4b | 1.6 GB | Prism ML | yes | yes | 2-bit weights: small, lower quality |
| nemotron-3.5-bolt:e4b | 7.9 GB | NVIDIA | yes | yes | over the default limit, does not fit 6 GB |
| mimo-v2.6:9b | 6.6 GB | Xiaomi MiMo | yes | yes | 9B, over the default limit, does not fit 6 GB |

The last two are shown at the end marked "OVER THE LIMIT" and are refused until `max_pull_gb` /
`max_pull_params_b` under `[ollama]` are raised. Capabilities are per family as the library lists them
(not checked per tag); installed models show what the server itself reports, plus the model's real
maximum context and its effort levels.

## One system prompt, not one per message

The instructions are sent once, as the system prompt. Messages from you now reach the agent as your own
words, with no header and no repeated instructions (before, every message carried "You have N new
messages ... the user reads your plain-text reply ..." and, for the Leader, a paragraph on handing out
work; they piled up in the history, used context and contradicted the system prompt). How input reaches
an agent (your plain text, a TASK block, mail from another agent) is said once in the role instructions.

## Letting an agent write outside its worktree

An agent normally writes only inside its own worktree: that is the sandbox of Codex, the working
directory of Claude Code, and the folder our own tools of a local model are confined to. Moving a file
to a Windows folder was therefore refused (measured with Codex: it asks for permission, and without it
the file is not created). Only **you** can open a folder:

    maw workspace allow /mnt/c/Users/me/download     (or C:/Users/me/download)
    maw workspace deny  /mnt/c/Users/me/download
    maw workspace                                      (shows the list)

Applies to Workers and Specialists, the next time they connect. Claude Code gets `--add-dir`, Codex gets
it as a writable root of its workspace-write sandbox (still never full access), a local model's tools may
write there after you approve each kind of write (autonomous: without asking). Disks, system folders,
your home folder itself and credential folders (`.ssh`, `.aws`, ...) are refused. With Codex, the same
file is created when the folder is allowed and not created when it is not (live test, two short turns).
