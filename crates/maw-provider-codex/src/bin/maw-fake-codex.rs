//! Test double for `codex app-server`: speaks the same newline-delimited JSON protocol and
//! plays a scenario chosen by the prompt text. Used only by this crate's contract tests.
//!
//! Scenarios (prompt prefix): OK, CMD, FILE, APPROVE, HANG, GARBAGE, EXIT, UNKNOWNREQ.
//! Every received line is appended to `$FAKE_CODEX_LOG` so tests can assert what MAW sent.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn out(v: &Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn raw(s: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{s}");
    let _ = o.flush();
}

fn log(line: &str) {
    if let Ok(path) = std::env::var("FAKE_CODEX_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn notify(method: &str, params: Value) {
    out(&json!({ "method": method, "params": params }));
}

fn read_msg(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Option<Value> {
    loop {
        let line = lines.next()?.ok()?;
        log(&line);
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            return Some(v);
        }
    }
}

const THREAD: &str = "thr_fake";

fn turn_started() {
    notify(
        "turn/started",
        json!({ "threadId": THREAD, "turn": { "id": "turn_1", "status": "inProgress", "items": [] } }),
    );
}

fn turn_completed(status: &str, error: Option<&str>) {
    let mut turn = json!({ "id": "turn_1", "status": status, "items": [] });
    if let Some(e) = error {
        turn["error"] = json!({ "message": e });
    }
    notify(
        "turn/completed",
        json!({ "threadId": THREAD, "turn": turn }),
    );
}

fn usage() {
    notify(
        "thread/tokenUsage/updated",
        json!({ "threadId": THREAD, "turnId": "turn_1", "tokenUsage": {
            "total": { "inputTokens": 120, "cachedInputTokens": 0, "cacheWriteInputTokens": 0, "outputTokens": 8, "reasoningOutputTokens": 0, "totalTokens": 128 },
            "last": { "inputTokens": 120, "cachedInputTokens": 0, "cacheWriteInputTokens": 0, "outputTokens": 8, "reasoningOutputTokens": 0, "totalTokens": 128 },
            "modelContextWindow": 200000 } }),
    );
}

fn main() {
    if std::env::var("MAW_MCP_TOKEN").is_ok() {
        log("env:MAW_MCP_TOKEN=present");
    }
    log(&format!(
        "argv:{}",
        std::env::args().skip(1).collect::<Vec<_>>().join(" ")
    ));
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut hanging = false;
    while let Some(msg) = read_msg(&mut lines) {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else { continue };
        match method.as_str() {
            "initialize" => out(
                &json!({ "id": id, "result": { "userAgent": "maw-fake-codex/0.0.0", "codexHome": "/tmp/fake-codex", "platformFamily": "unix", "platformOs": "linux" } }),
            ),
            "thread/start" => out(
                &json!({ "id": id, "result": { "thread": { "id": THREAD }, "model": "fake-model", "cwd": params.get("cwd") } }),
            ),
            "thread/resume" => {
                if params.get("threadId").and_then(Value::as_str) == Some("thr_missing") {
                    out(
                        &json!({ "id": id, "error": { "code": -32600, "message": "no rollout found for thread id thr_missing" } }),
                    );
                } else {
                    out(
                        &json!({ "id": id, "result": { "thread": { "id": params.get("threadId").cloned().unwrap_or(json!(THREAD)) } } }),
                    );
                }
            }
            "turn/interrupt" => {
                out(&json!({ "id": id, "result": {} }));
                if hanging {
                    hanging = false;
                    turn_completed("interrupted", None);
                }
            }
            "turn/steer" => out(&json!({ "id": id, "result": { "turnId": "turn_1" } })),
            "turn/start" => {
                let text = params
                    .pointer("/input/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                out(
                    &json!({ "id": id, "result": { "turn": { "id": "turn_1", "status": "inProgress", "items": [] } } }),
                );
                match text.split_whitespace().next().unwrap_or("") {
                    "OK" => {
                        turn_started();
                        notify(
                            "item/started",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "agentMessage", "id": "m1", "text": "" } }),
                        );
                        notify(
                            "item/agentMessage/delta",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "itemId": "m1", "delta": "O" }),
                        );
                        notify(
                            "item/agentMessage/delta",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "itemId": "m1", "delta": "K" }),
                        );
                        notify(
                            "item/completed",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "agentMessage", "id": "m1", "text": "OK" } }),
                        );
                        notify("warning", json!({ "message": "ignored by MAW" }));
                        notify("some/futureNotification", json!({ "x": 1 }));
                        // a notification for a different thread must be ignored
                        notify(
                            "item/agentMessage/delta",
                            json!({ "threadId": "thr_other", "turnId": "t", "itemId": "m9", "delta": "LEAK" }),
                        );
                        usage();
                        notify(
                            "account/rateLimits/updated",
                            json!({ "rateLimits": { "primary": { "usedPercent": 10, "resetsAt": null }, "rateLimitReachedType": null } }),
                        );
                        turn_completed("completed", None);
                    }
                    "CMD" => {
                        turn_started();
                        notify(
                            "item/started",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "commandExecution", "id": "c1", "command": "echo hi", "cwd": "/work", "status": "inProgress", "commandActions": [] } }),
                        );
                        notify(
                            "item/commandExecution/outputDelta",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "itemId": "c1", "delta": "hi\n" }),
                        );
                        notify(
                            "item/completed",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "commandExecution", "id": "c1", "command": "echo hi", "cwd": "/work", "status": "completed", "exitCode": 0, "aggregatedOutput": "hi\n" } }),
                        );
                        notify(
                            "item/started",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "mcpToolCall", "id": "t1", "server": "maw", "tool": "maw_goal_get", "arguments": {}, "status": "inProgress" } }),
                        );
                        notify(
                            "item/completed",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "mcpToolCall", "id": "t1", "server": "maw", "tool": "maw_goal_get", "arguments": {}, "status": "completed", "result": {} } }),
                        );
                        turn_completed("completed", None);
                    }
                    "FILE" => {
                        turn_started();
                        let item = json!({ "type": "fileChange", "id": "f1", "status": "completed", "changes": [{ "path": "src/a.rs", "kind": { "type": "add" }, "diff": "+x" }] });
                        notify(
                            "item/started",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "fileChange", "id": "f1", "status": "inProgress", "changes": [{ "path": "src/a.rs", "kind": { "type": "add" }, "diff": "+x" }] } }),
                        );
                        notify(
                            "item/completed",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": item }),
                        );
                        turn_completed("failed", Some("model overloaded"));
                    }
                    "APPROVE" => {
                        turn_started();
                        notify(
                            "item/started",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "commandExecution", "id": "c1", "command": "rm -rf build", "cwd": "/work", "status": "inProgress", "commandActions": [] } }),
                        );
                        out(
                            &json!({ "id": 900, "method": "item/commandExecution/requestApproval", "params": { "threadId": THREAD, "turnId": "turn_1", "itemId": "c1", "command": "rm -rf build", "cwd": "/work", "reason": "needs to delete build output", "startedAtMs": 1 } }),
                        );
                        let mut accepted = false;
                        while let Some(m) = read_msg(&mut lines) {
                            if m.get("id") == Some(&json!(900)) && m.get("method").is_none() {
                                let d = m
                                    .pointer("/result/decision")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?");
                                log(&format!("approval:{d}"));
                                accepted = d.starts_with("accept");
                                break;
                            }
                        }
                        notify(
                            "serverRequest/resolved",
                            json!({ "requestId": 900, "threadId": THREAD }),
                        );
                        notify(
                            "item/completed",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "commandExecution", "id": "c1", "command": "rm -rf build", "cwd": "/work", "status": if accepted { "completed" } else { "declined" }, "exitCode": if accepted { json!(0) } else { Value::Null } } }),
                        );
                        turn_completed("completed", None);
                    }
                    "UNKNOWNREQ" => {
                        turn_started();
                        out(
                            &json!({ "id": 901, "method": "item/tool/requestUserInput", "params": { "threadId": THREAD, "turnId": "turn_1", "itemId": "q", "questions": [] } }),
                        );
                        while let Some(m) = read_msg(&mut lines) {
                            if m.get("id") == Some(&json!(901)) && m.get("method").is_none() {
                                log(&format!(
                                    "unknown_request_answered_with_error:{}",
                                    m.get("error").is_some()
                                ));
                                break;
                            }
                        }
                        turn_completed("completed", None);
                    }
                    "GARBAGE" => {
                        raw("this is not json");
                        raw("{\"method\": \"item/agentMessage/delta\", \"params\": {broken");
                        raw("");
                        turn_started();
                        raw("[1,2,3]");
                        raw("{\"neither\":\"id nor method\"}");
                        notify(
                            "item/completed",
                            json!({ "threadId": THREAD, "turnId": "turn_1", "item": { "type": "agentMessage", "id": "m1", "text": "survived" } }),
                        );
                        turn_completed("completed", None);
                    }
                    "HANG" => {
                        turn_started();
                        hanging = true;
                    }
                    "EXIT" => {
                        turn_started();
                        eprintln!("fatal: simulated crash");
                        std::process::exit(3);
                    }
                    _ => turn_completed("completed", None),
                }
            }
            _ => out(
                &json!({ "id": id, "error": { "code": -32601, "message": format!("unknown method {method}") } }),
            ),
        }
    }
}
