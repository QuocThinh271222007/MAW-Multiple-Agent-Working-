//! Wire-level helpers for Ollama's HTTP API: endpoint discovery, chat-stream parsing, model details.
//!
//! Only documented endpoints are used: `GET /api/version`, `GET /api/tags`, `POST /api/show`,
//! `GET /api/ps` and `POST /api/chat` (NDJSON stream).

use serde_json::Value;
use std::net::Ipv4Addr;

pub const DEFAULT_PORT: u16 = 11434;

/// `OLLAMA_HOST` style values ("host", "host:port", "http://host:port", "0.0.0.0") → base URL.
pub fn normalize_url(raw: &str) -> Option<String> {
    let t = raw.trim().trim_end_matches('/');
    if t.is_empty() {
        return None;
    }
    let (scheme, rest) = match t.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("http".to_string(), t),
    };
    let rest = rest.split('/').next().unwrap_or(rest);
    if rest.is_empty() {
        return None;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() && !h.ends_with(':') => (h, p.to_string()),
        _ => (rest, DEFAULT_PORT.to_string()),
    };
    // a server bound to every interface is reached through loopback
    let host = if host == "0.0.0.0" || host == "[::]" || host == "::" {
        "127.0.0.1"
    } else {
        host
    };
    Some(format!("{scheme}://{host}:{port}"))
}

/// Default gateway from the contents of `/proc/net/route` (little-endian hex). Under WSL2 in NAT
/// mode this is the Windows host, where the Windows build of Ollama listens.
pub fn gateway_from_route(table: &str) -> Option<Ipv4Addr> {
    for line in table.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() > 2 && f[1] == "00000000" {
            if let Ok(g) = u32::from_str_radix(f[2], 16) {
                if g != 0 {
                    let b = g.to_le_bytes();
                    return Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]));
                }
            }
        }
    }
    None
}

pub fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::fs::read_to_string("/proc/version")
                .map(|v| v.to_ascii_lowercase().contains("microsoft"))
                .unwrap_or(false))
}

/// Base URLs to try: the explicit config value alone if there is one, else `$OLLAMA_HOST`,
/// loopback, then (WSL only) the Windows host.
pub fn candidate_urls(configured: Option<&str>) -> Vec<String> {
    // an address chosen explicitly in the config is the only one used
    if let Some(u) = configured.and_then(normalize_url) {
        return vec![u];
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |u: Option<String>| {
        if let Some(u) = u {
            if !out.contains(&u) {
                out.push(u);
            }
        }
    };
    push(configured.and_then(normalize_url));
    push(
        std::env::var("OLLAMA_HOST")
            .ok()
            .as_deref()
            .and_then(normalize_url),
    );
    push(normalize_url("127.0.0.1"));
    if is_wsl() {
        if let Ok(t) = std::fs::read_to_string("/proc/net/route") {
            if let Some(g) = gateway_from_route(&t) {
                push(normalize_url(&g.to_string()));
            }
        }
    }
    out
}

/// One decoded line of the `/api/chat` stream.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ChatChunk {
    pub content: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub done: bool,
    pub done_reason: Option<String>,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    /// Time Ollama spent generating (`eval_duration`) / reading the prompt, in nanoseconds.
    pub eval_ns: u64,
    pub prompt_eval_ns: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

/// Some small models write the tool call as ordinary text instead of using the structured field
/// (`tool_call {"name": ..., "arguments": {...}}`, `<tool_call>{...}</tool_call>`, a fenced JSON
/// block). Read those, but only for tools that really exist, so prose never becomes an action.
pub fn text_tool_calls(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<ToolCall> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() && out.len() < 8 {
        if text.as_bytes()[i] == b'{' {
            if let Some(end) = balanced_end(text, i) {
                if let Ok(v) = serde_json::from_str::<Value>(&text[i..=end]) {
                    if let Some(c) = as_text_call(&v, known, 0) {
                        out.push(c);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// Index of the `}` that closes the `{` at `start`, or `None`.
fn balanced_end(text: &str, start: usize) -> Option<usize> {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (off, &b) in text.as_bytes()[start..].iter().enumerate() {
        if in_str {
            match (esc, b) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + off);
                }
            }
            _ => {}
        }
    }
    None
}

fn as_text_call(v: &Value, known: &dyn Fn(&str) -> bool, depth: u8) -> Option<ToolCall> {
    if let Some(name) = v.get("name").and_then(Value::as_str) {
        if !known(name) {
            return None;
        }
        let args = ["arguments", "parameters", "args"]
            .iter()
            .find_map(|k| v.get(*k))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        // some models send the arguments as a JSON string
        let args = match &args {
            Value::String(s) => serde_json::from_str(s).ok()?,
            _ => args,
        };
        return args.is_object().then_some(ToolCall {
            id: None,
            name: name.to_string(),
            arguments: args,
        });
    }
    if depth == 0 {
        for k in ["tool_call", "function", "call"] {
            if let Some(inner) = v.get(k) {
                return as_text_call(inner, known, 1);
            }
        }
    }
    None
}

pub fn parse_chunk(line: &str) -> Result<ChatChunk, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad stream line: {e}"))?;
    let mut c = ChatChunk::default();
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        c.error = Some(e.to_string());
        return Ok(c);
    }
    if let Some(m) = v.get("message") {
        c.content = m
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        c.thinking = m
            .get("thinking")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let f = call.get("function").unwrap_or(call);
                let Some(name) = f.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let mut args = f.get("arguments").cloned().unwrap_or(Value::Null);
                // some servers send the arguments as a JSON string
                if let Value::String(s) = &args {
                    args = serde_json::from_str(s).unwrap_or(Value::Null);
                }
                c.tool_calls.push(ToolCall {
                    id: call.get("id").and_then(Value::as_str).map(String::from),
                    name: name.to_string(),
                    arguments: args,
                });
            }
        }
    }
    c.done = v.get("done").and_then(Value::as_bool).unwrap_or(false);
    c.done_reason = v
        .get("done_reason")
        .and_then(Value::as_str)
        .map(String::from);
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    c.prompt_tokens = n("prompt_eval_count");
    c.output_tokens = n("eval_count");
    c.eval_ns = n("eval_duration");
    c.prompt_eval_ns = n("prompt_eval_duration");
    Ok(c)
}

/// What `/api/show` says a model can do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelInfo {
    pub capabilities: Vec<String>,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
    pub family: Option<String>,
    /// Trained context length, when the server reports it.
    pub context_length: Option<u64>,
}

impl ModelInfo {
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

pub fn parse_show(v: &Value) -> ModelInfo {
    let caps = v
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let d = v.get("details");
    let s = |k: &str| {
        d.and_then(|d| d.get(k))
            .and_then(Value::as_str)
            .filter(|x| !x.is_empty())
            .map(String::from)
    };
    let family = s("family");
    let ctx = v
        .get("model_info")
        .and_then(Value::as_object)
        .and_then(|m| {
            m.iter()
                .find(|(k, _)| k.ends_with(".context_length"))
                .and_then(|(_, v)| v.as_u64())
        });
    ModelInfo {
        capabilities: caps,
        parameter_size: s("parameter_size"),
        quantization: s("quantization_level"),
        family,
        context_length: ctx,
    }
}

/// A model currently loaded in memory (`/api/ps`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LoadedModel {
    pub name: String,
    pub size_bytes: u64,
    pub vram_bytes: u64,
    pub context_length: Option<u64>,
}

pub fn parse_ps(v: &Value) -> Vec<LoadedModel> {
    v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(LoadedModel {
                        name: m
                            .get("name")
                            .or_else(|| m.get("model"))?
                            .as_str()?
                            .to_string(),
                        size_bytes: m.get("size").and_then(Value::as_u64).unwrap_or(0),
                        vram_bytes: m.get("size_vram").and_then(Value::as_u64).unwrap_or(0),
                        context_length: m.get("context_length").and_then(Value::as_u64),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Marker in [`maw_core::provider::ModelChoice::reasoning_efforts`] meaning "this model has no
/// effort setting" (an empty list would mean "use the common levels").
pub const NO_EFFORT: &str = "none";

/// Effort levels MAW offers for a model. Only the `gpt-oss` family takes real levels
/// (`low`/`medium`/`high`). Other models either cannot think or think whenever they are asked to;
/// turning thinking off is not offered because it is not reliable (with `qwen3:4b` the model keeps
/// reasoning and the text just ends up inside the answer).
pub fn efforts_for(model: &str, info: &ModelInfo) -> Vec<String> {
    if info.has("thinking") && model.starts_with("gpt-oss") {
        ["low", "medium", "high"].map(String::from).to_vec()
    } else {
        vec![NO_EFFORT.to_string()]
    }
}

/// The `think` request field. `None` = the model cannot think, so nothing is sent. A stale
/// `on`/`off` stored by an earlier version is ignored.
pub fn think_value(model: &str, info: &ModelInfo, effort: Option<&str>) -> Option<Value> {
    if !info.has("thinking") {
        return None;
    }
    match effort {
        Some(l @ ("low" | "medium" | "high")) if model.starts_with("gpt-oss") => {
            Some(Value::String(l.to_string()))
        }
        _ => Some(Value::Bool(true)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tool_call_written_as_text_is_read_only_for_real_tools() {
        let known = |n: &str| matches!(n, "maw_task_create" | "write_file");
        let text = "Sure, I will do it.\ntool_call {\n  \"name\": \"maw_task_create\",\n  \"arguments\": {\"title\": \"A {brace} \\\"q\\\"\", \"assignee\": \"tester\"}\n}\nDone.";
        let calls = text_tool_calls(text, &known);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].name, "maw_task_create");
        assert_eq!(calls[0].arguments["assignee"], "tester");
        assert_eq!(calls[0].arguments["title"], "A {brace} \"q\"");
        // tagged, fenced and wrapped forms; arguments given as a JSON string
        let tagged = r#"<tool_call>{"name":"write_file","arguments":{"path":"a.txt","content":"x"}}</tool_call>"#;
        assert_eq!(text_tool_calls(tagged, &known)[0].name, "write_file");
        let fenced = "```json\n{\"tool_call\": {\"name\": \"write_file\", \"parameters\": \"{\\\"path\\\": \\\"b\\\"}\"}}\n```";
        let c = text_tool_calls(fenced, &known);
        assert_eq!(
            (c[0].name.as_str(), c[0].arguments["path"].as_str()),
            ("write_file", Some("b"))
        );
        // two calls in one message
        let two = r#"{"name":"write_file","arguments":{"path":"1"}} then {"name":"write_file","arguments":{"path":"2"}}"#;
        assert_eq!(text_tool_calls(two, &known).len(), 2);
        // unknown tools, plain prose and broken JSON are not actions
        assert!(text_tool_calls(r#"{"name":"rm_everything","arguments":{}}"#, &known).is_empty());
        assert!(text_tool_calls("I would call write_file but will not.", &known).is_empty());
        assert!(text_tool_calls(r#"{"name":"write_file","arguments":{"path":"#, &known).is_empty());
        assert!(
            text_tool_calls(r#"{"name":"write_file","arguments":"not json"}"#, &known).is_empty()
        );
    }

    #[test]
    fn urls_are_normalized_like_ollama_host() {
        assert_eq!(
            normalize_url("127.0.0.1").unwrap(),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            normalize_url("0.0.0.0:9000").unwrap(),
            "http://127.0.0.1:9000"
        );
        assert_eq!(
            normalize_url("http://box:1234/").unwrap(),
            "http://box:1234"
        );
        assert_eq!(normalize_url("https://h").unwrap(), "https://h:11434");
        assert!(normalize_url("  ").is_none());
    }

    #[test]
    fn wsl_gateway_is_read_from_the_route_table() {
        let t = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
                 eth0\t00000000\t01505017\t0003\t0\t0\t0\t00000000\n\
                 eth0\t00505017\t00000000\t0001\t0\t0\t0\t00F0FFFF\n";
        assert_eq!(gateway_from_route(t).unwrap(), Ipv4Addr::new(23, 80, 80, 1));
        assert!(gateway_from_route("Iface\tDestination\tGateway\n").is_none());
    }

    #[test]
    fn stream_lines_decode_text_thinking_tools_and_usage() {
        let c = parse_chunk(
            r#"{"message":{"role":"assistant","content":"hi","thinking":"hm"},"done":false}"#,
        )
        .unwrap();
        assert_eq!(
            (c.content.as_str(), c.thinking.as_str(), c.done),
            ("hi", "hm", false)
        );
        let c = parse_chunk(
            r#"{"message":{"content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"a.rs"}}},{"id":"c9","function":{"name":"x","arguments":"{\"k\":1}"}}]},"done":false}"#,
        )
        .unwrap();
        assert_eq!(c.tool_calls.len(), 2);
        assert_eq!(c.tool_calls[0].arguments, json!({"path":"a.rs"}));
        assert_eq!(c.tool_calls[1].id.as_deref(), Some("c9"));
        assert_eq!(c.tool_calls[1].arguments, json!({"k":1}));
        let c = parse_chunk(
            r#"{"message":{"content":""},"done":true,"done_reason":"stop","prompt_eval_count":120,"eval_count":30,"eval_duration":1500000000}"#,
        )
        .unwrap();
        assert!(
            c.done && c.prompt_tokens == 120 && c.output_tokens == 30 && c.eval_ns == 1_500_000_000
        );
        assert_eq!(
            parse_chunk(r#"{"error":"model not found"}"#)
                .unwrap()
                .error
                .as_deref(),
            Some("model not found")
        );
        assert!(parse_chunk("not json").is_err());
    }

    #[test]
    fn show_and_ps_are_parsed_and_effort_follows_capabilities() {
        let info = parse_show(&json!({
            "capabilities":["completion","tools","thinking"],
            "details":{"family":"qwen3","parameter_size":"4.0B","quantization_level":"Q4_K_M"},
            "model_info":{"qwen3.context_length":40960}
        }));
        assert!(info.has("tools") && info.has("thinking"));
        assert_eq!(info.context_length, Some(40960));
        // no effort setting for ordinary models, real levels only for gpt-oss
        assert_eq!(efforts_for("qwen3:4b", &info), [NO_EFFORT]);
        assert_eq!(efforts_for("gpt-oss:20b", &info), ["low", "medium", "high"]);
        assert_eq!(think_value("qwen3:4b", &info, None), Some(json!(true)));
        // values stored by an earlier version (on/off) or typed by hand are ignored for qwen3
        assert_eq!(
            think_value("qwen3:4b", &info, Some("off")),
            Some(json!(true))
        );
        assert_eq!(
            think_value("qwen3:4b", &info, Some("high")),
            Some(json!(true))
        );
        assert_eq!(
            think_value("gpt-oss:20b", &info, Some("high")),
            Some(json!("high"))
        );
        assert_eq!(think_value("gpt-oss:20b", &info, None), Some(json!(true)));
        let plain = ModelInfo::default();
        assert!(
            think_value("llama3", &plain, None).is_none(),
            "no think field for models that cannot think"
        );
        assert_eq!(efforts_for("llama3", &plain), [NO_EFFORT]);
        assert_eq!(
            efforts_for("gpt-oss:20b", &plain),
            [NO_EFFORT],
            "levels need the thinking capability"
        );
        let ps = parse_ps(&json!({"models":[{"name":"qwen3:4b","size":3000,"size_vram":2000}]}));
        assert_eq!(
            ps,
            [LoadedModel {
                name: "qwen3:4b".into(),
                size_bytes: 3000,
                vram_bytes: 2000,
                context_length: None
            }]
        );
    }
}
