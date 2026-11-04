//! The model's side of a conversation, kept on disk so a local agent can pick up where it left
//! off after MAW is closed (what `--resume` does for Claude Code and Codex). Ollama itself keeps
//! no conversations: the history is whatever MAW sends with each request.
//!
//! One JSON file per session id in the state directory, readable only by the user. Text goes
//! through the shared redactor before it is written; MCP tokens never appear in a history.

use maw_core::redact::redact;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Histories larger than this are not written (they could not fit a context window anyway).
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Session ids are `ollama-` plus 32 hex digits; anything else is refused so an id coming from the
/// database can never point outside the state directory.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("ollama-")
        .map(|h| h.len() == 32 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or(false)
}

pub fn new_id() -> String {
    format!("ollama-{}", uuid::Uuid::new_v4().simple())
}

fn file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), redact_value(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The saved history of a session, or `None` when there is none (or it is unreadable).
pub fn load(dir: &Path, id: &str) -> Option<Vec<Value>> {
    if !valid_id(id) {
        return None;
    }
    let bytes = std::fs::read(file(dir, id)).ok()?;
    if bytes.len() > MAX_FILE_BYTES {
        return None;
    }
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let msgs = v.get("messages")?.as_array()?;
    // only well-formed chat messages come back
    msgs.iter()
        .all(|m| m.get("role").and_then(Value::as_str).is_some())
        .then(|| msgs.clone())
}

/// Write the history atomically. Errors are returned for logging; a failed save never stops a turn.
pub fn save(dir: &Path, id: &str, history: &[Value]) -> Result<(), String> {
    if !valid_id(id) {
        return Err("invalid session id".into());
    }
    let body = serde_json::json!({
        "version": 1,
        "messages": history.iter().map(redact_value).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "history is {} bytes, too large to save",
            bytes.len()
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{id}.json.tmp"));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
        f.write_all(&bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, file(dir, id)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_history_round_trips_redacted_and_bad_ids_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let id = new_id();
        assert!(valid_id(&id));
        let h = vec![
            json!({"role":"user","content":"hi"}),
            json!({"role":"assistant","content":"key sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAA","tool_calls":[]}),
        ];
        save(dir.path(), &id, &h).unwrap();
        let back = load(dir.path(), &id).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], h[0]);
        assert!(
            !back[1].to_string().contains("AAAAAAAAAAAAAAAA"),
            "{}",
            back[1]
        );
        // ids from the database cannot escape the directory or name other files
        for bad in [
            "../x",
            "ollama-../../etc/passwd",
            "ollama-zz",
            "",
            "other-0123",
        ] {
            assert!(!valid_id(bad), "{bad}");
            assert!(load(dir.path(), bad).is_none());
            assert!(save(dir.path(), bad, &h).is_err());
        }
        // garbage on disk is "no history", not a crash
        std::fs::write(file(dir.path(), &id), b"{not json").unwrap();
        assert!(load(dir.path(), &id).is_none());
        std::fs::write(file(dir.path(), &id), br#"{"messages":[{"no_role":1}]}"#).unwrap();
        assert!(load(dir.path(), &id).is_none());
    }
}
