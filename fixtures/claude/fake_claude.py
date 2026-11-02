#!/usr/bin/env python3
"""Fake `claude` executable for contract tests.

It replays the REAL sanitized captures in this directory, so the adapter is exercised against
genuine stream-json bytes without spending provider quota. Behaviour is selected with
FAKE_CLAUDE_SCENARIO; FAKE_CLAUDE_FIXTURES points at this directory and FAKE_CLAUDE_RECORD at a
JSON-lines file where the fake records what it received (argv/env/stdin control messages).

This is a test double. It is not evidence that the real CLI behaves identically; see the
opt-in live test (MAW_LIVE_CLAUDE=1) for that.
"""
import json
import os
import signal
import sys
import time

argv = sys.argv[1:]
scenario = os.environ.get("FAKE_CLAUDE_SCENARIO", "two_turns")
fixtures = os.environ.get("FAKE_CLAUDE_FIXTURES", os.path.dirname(os.path.abspath(__file__)))
record_path = os.environ.get("FAKE_CLAUDE_RECORD")


def record(obj):
    if record_path:
        with open(record_path, "a") as f:
            f.write(json.dumps(obj) + "\n")


if argv[:1] == ["--version"]:
    print("2.1.309 (Claude Code)")
    sys.exit(0)
if argv[:2] == ["auth", "status"]:
    print(json.dumps({"loggedIn": True, "authMethod": "claude.ai", "apiProvider": "firstParty", "subscriptionType": "pro"}))
    sys.exit(0)

record({"kind": "start", "argv": argv, "cwd": os.getcwd(), "token_env": os.environ.get("MAW_MCP_TOKEN"), "pid": os.getpid()})


def opt(name):
    return argv[argv.index(name) + 1] if name in argv else None


session_id = opt("--session-id") or opt("--resume") or "00000000-0000-4000-8000-000000000000"


def load(name):
    """Split a capture into per-turn segments (each ends with a `result`)."""
    segs, cur = [], []
    with open(os.path.join(fixtures, name)) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            o = json.loads(line)
            if "session_id" in o:
                o["session_id"] = session_id
            cur.append(o)
            if o.get("type") == "result":
                segs.append(cur)
                cur = []
    if cur:
        segs.append(cur)
    return segs


def emit(o):
    sys.stdout.write((o if isinstance(o, str) else json.dumps(o)) + "\n")
    sys.stdout.flush()


def read_stdin_line():
    line = sys.stdin.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except Exception:
        return {"type": "garbage"}


def wait_for(pred):
    while True:
        m = read_stdin_line()
        if m is None:
            sys.exit(0)
        record({"kind": "stdin", "msg": m})
        if pred(m):
            return m


def next_user():
    return wait_for(lambda m: m.get("type") == "user")


if scenario == "stay_alive":
    # never reads its input; only dies to a signal. Used to test forced shutdown.
    while True:
        time.sleep(1)

if scenario == "stubborn":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True:
        time.sleep(1)

if scenario == "exit":
    next_user()
    sys.stderr.write("fatal: something broke with key sk-ant-api03-SECRETSECRETSECRETSECRET\n")
    sys.stderr.flush()
    sys.exit(3)

if scenario == "resume_fail" and "--resume" in argv:
    next_user()
    emit({"type": "result", "subtype": "error_during_execution", "is_error": True, "session_id": session_id,
          "errors": ["No conversation found with session ID: " + session_id], "terminal_reason": "error"})
    sys.stderr.write("No conversation found with session ID: " + session_id + "\n")
    sys.stderr.flush()
    sys.exit(1)

if scenario == "permission":
    seg = load("permission_request_denied.jsonl")[0]
    next_user()
    for o in seg:
        emit(o)
        if o.get("type") == "control_request":
            wait_for(lambda m: m.get("type") == "control_response")
    # stay alive for further turns
    while True:
        if next_user() is None:
            break
    sys.exit(0)

if scenario == "interrupt":
    segs = load("allow_interrupt_reuse.jsonl")
    # segs[1] is the interrupted turn, segs[2] the follow-up on the same process
    interrupted, alive = segs[1], segs[2]
    next_user()
    for o in interrupted[:-1]:
        emit(o)
    wait_for(lambda m: m.get("type") == "control_request" and m.get("request", {}).get("subtype") == "interrupt")
    emit({"type": "control_response", "response": {"subtype": "success", "request_id": "x", "response": {"still_queued": []}}})
    emit(interrupted[-1])
    next_user()
    for o in alive:
        emit(o)
    while next_user() is not None:
        pass
    sys.exit(0)

# two_turns / malformed / resume_fail(fresh) : replay segments, one per user message
segs = load("two_turns_mcp_tool.jsonl")
i = 0
while True:
    next_user()
    seg = segs[min(i, len(segs) - 1)]
    i += 1
    if scenario == "malformed":
        emit("this is not json")
        emit('{"type":"assistant","message":')
        emit('{"type":"totally_unknown","x":1}')
        emit("")
    for o in seg:
        emit(o)
