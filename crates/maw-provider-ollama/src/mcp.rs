//! Client side of MAW's own coordination MCP server.
//!
//! The local model has no MCP support of its own, so the session connects here with the agent's
//! bearer token (minted by MAW for this agent only), lists the tools the server offers that role,
//! and forwards the model's calls to it.

use maw_core::provider::McpEndpoint;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::ServiceExt;
use serde_json::{json, Value};

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// Result of one tool call: the text handed back to the model and whether the tool reported an error.
pub struct McpOutcome {
    pub text: String,
    pub is_error: bool,
}

pub struct McpConnection {
    client: Client,
    /// Tool definitions in Ollama's `tools` format.
    pub definitions: Vec<Value>,
}

/// Replace `{"$ref": "#/definitions/X"}` by the definition itself (and drop the definitions).
/// MCP tool schemas keep nested argument types in `definitions`; a model that is only shown the
/// dangling reference cannot see what a nested object (e.g. one evidence item) must contain.
pub fn inline_refs(root: &Value) -> Value {
    fn go(v: &Value, defs: &Value, depth: u8) -> Value {
        match v {
            Value::Object(m) => {
                if depth < 5 {
                    if let Some(Value::String(r)) = m.get("$ref") {
                        if let Some(d) = r.rsplit('/').next().and_then(|n| defs.get(n)) {
                            let mut out = go(d, defs, depth + 1);
                            if let Value::Object(o) = &mut out {
                                for (k, val) in m {
                                    if k != "$ref" {
                                        o.insert(k.clone(), go(val, defs, depth + 1));
                                    }
                                }
                            }
                            return out;
                        }
                    }
                }
                Value::Object(
                    m.iter()
                        .filter(|(k, _)| !matches!(k.as_str(), "definitions" | "$defs"))
                        .map(|(k, v)| (k.clone(), go(v, defs, depth)))
                        .collect(),
                )
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| go(x, defs, depth)).collect()),
            other => other.clone(),
        }
    }
    let defs = root
        .get("definitions")
        .or_else(|| root.get("$defs"))
        .cloned()
        .unwrap_or(Value::Null);
    go(root, &defs, 0)
}

/// Shrink a JSON schema for small context windows: drop decoration, cap descriptions.
pub fn compact_schema(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                if matches!(
                    k.as_str(),
                    "$schema"
                        | "title"
                        | "additionalProperties"
                        | "format"
                        | "$defs"
                        | "definitions"
                ) {
                    continue;
                }
                if k == "description" {
                    if let Some(s) = val.as_str() {
                        out.insert(k.clone(), Value::String(s.chars().take(110).collect()));
                    }
                    continue;
                }
                out.insert(k.clone(), compact_schema(val));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(compact_schema).collect()),
        other => other.clone(),
    }
}

/// Text of a tool result: structured content if present, else the text parts.
pub fn result_text(res: &Value) -> String {
    if let Some(s) = res.get("structuredContent").filter(|v| !v.is_null()) {
        return s.to_string();
    }
    let parts: Vec<&str> = res
        .get("content")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    parts.join("\n")
}

impl McpConnection {
    pub async fn connect(ep: &McpEndpoint) -> Result<Self, String> {
        let cfg = StreamableHttpClientTransportConfig::with_uri(ep.url.clone())
            .auth_header(ep.token.clone());
        let client = ()
            .serve(StreamableHttpClientTransport::from_config(cfg))
            .await
            .map_err(|e| format!("could not connect to MAW's coordination server: {e}"))?;
        let listed = client
            .list_all_tools()
            .await
            .map_err(|e| format!("could not list MAW tools: {e}"))?;
        let mut definitions = Vec::new();
        for t in listed {
            let v = serde_json::to_value(&t).unwrap_or(Value::Null);
            let Some(name) = v.get("name").and_then(Value::as_str) else {
                continue;
            };
            let desc: String = v
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(200)
                .collect();
            let params = v
                .get("inputSchema")
                .map(|s| compact_schema(&inline_refs(s)))
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            definitions.push(json!({"type":"function","function":{
                "name": name, "description": desc, "parameters": params
            }}));
        }
        Ok(McpConnection {
            client,
            definitions,
        })
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.definitions
            .iter()
            .any(|d| d["function"]["name"].as_str() == Some(name))
    }

    pub async fn call(&self, name: &str, args: &Value) -> Result<McpOutcome, String> {
        let params = CallToolRequestParams::new(name.to_string())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        let res = self
            .client
            .call_tool(params)
            .await
            .map_err(|e| format!("MCP call failed: {e}"))?;
        let v = serde_json::to_value(&res).unwrap_or(Value::Null);
        Ok(McpOutcome {
            text: result_text(&v),
            is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_compacted_for_small_contexts() {
        let s = json!({
            "$schema": "x", "title": "T", "type": "object", "additionalProperties": false,
            "properties": {"a": {"type": "string", "description": "d".repeat(300), "format": "uuid"}},
            "required": ["a"]
        });
        let c = compact_schema(&s);
        assert!(c.get("$schema").is_none() && c.get("title").is_none());
        assert_eq!(
            c["properties"]["a"]["description"].as_str().unwrap().len(),
            110
        );
        assert!(c["properties"]["a"].get("format").is_none());
        assert_eq!(c["required"], json!(["a"]));
    }

    #[test]
    fn nested_argument_shapes_survive_so_a_model_can_see_the_allowed_values() {
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "evidence": {"type": "array", "items": {"$ref": "#/definitions/EvidenceArg"}}
            },
            "required": ["summary"],
            "definitions": {"EvidenceArg": {
                "type": "object",
                "properties": {"kind": {"type": "string", "description": "file_changes | test_result | verification | note"}},
                "required": ["kind"]
            }}
        });
        let c = compact_schema(&inline_refs(&schema));
        let text = c.to_string();
        assert!(
            !text.contains("$ref") && !text.contains("definitions"),
            "{text}"
        );
        assert!(
            text.contains("file_changes | test_result"),
            "the valid kinds reach the model: {text}"
        );
        assert_eq!(
            c["properties"]["evidence"]["items"]["required"],
            json!(["kind"])
        );
        // a cycle cannot loop forever
        let cyc = json!({"properties": {"a": {"$ref": "#/definitions/N"}}, "definitions": {"N": {"properties": {"n": {"$ref": "#/definitions/N"}}}}});
        let _ = inline_refs(&cyc);
    }

    #[test]
    fn result_text_prefers_structured_content() {
        assert_eq!(
            result_text(
                &json!({"structuredContent":{"ok":true},"content":[{"type":"text","text":"x"}]})
            ),
            "{\"ok\":true}"
        );
        assert_eq!(
            result_text(
                &json!({"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]})
            ),
            "a\nb"
        );
        assert_eq!(result_text(&json!({})), "");
    }
}
