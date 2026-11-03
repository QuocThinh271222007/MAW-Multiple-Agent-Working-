//! Keeping the conversation inside the model's context window.
//!
//! Small local models run with small windows. Before each request the history is trimmed: first
//! old tool output is elided, then the oldest whole tool round-trips are dropped. Token counts
//! here are rough estimates used only for this decision; they are never shown as usage.

use serde_json::{json, Value};

const ELIDED: &str = "[older tool output removed to save context]";

pub fn estimate_tokens(history: &[Value]) -> usize {
    history
        .iter()
        .map(|m| {
            let content = m["content"].as_str().map(str::len).unwrap_or(0);
            let calls = m
                .get("tool_calls")
                .map(|c| c.to_string().len())
                .unwrap_or(0);
            (content + calls) / 3 + 8
        })
        .sum()
}

/// Trim `history` (system prompt and tool definitions are accounted for in `reserved`) so that its
/// estimated size is at most `budget` tokens. Returns how many messages were changed or dropped.
pub fn compact(history: &mut Vec<Value>, budget: usize, reserved: usize) -> usize {
    let limit = budget.saturating_sub(reserved);
    let mut changed = 0;
    if estimate_tokens(history) <= limit {
        return 0;
    }
    // 1. elide old tool output, oldest first, keeping the last three
    let tool_idx: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| m["role"] == "tool")
        .map(|(i, _)| i)
        .collect();
    let keep_from = tool_idx.len().saturating_sub(3);
    for &i in &tool_idx[..keep_from] {
        if estimate_tokens(history) <= limit {
            return changed;
        }
        if history[i]["content"].as_str() != Some(ELIDED) {
            history[i]["content"] = json!(ELIDED);
            changed += 1;
        }
    }
    // 2. drop the oldest assistant(+tool results) groups, never the first user message or the tail
    while estimate_tokens(history) > limit && history.len() > 7 {
        let Some(start) = (1..history.len()).find(|&i| history[i]["role"] == "assistant") else {
            break;
        };
        let mut end = start + 1;
        while end < history.len() && history[end]["role"] == "tool" {
            end += 1;
        }
        if history.len() - (end - start) < 6 {
            break;
        }
        changed += end - start;
        history.drain(start..end);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(rounds: usize, size: usize) -> Vec<Value> {
        let mut h = vec![json!({"role":"user","content":"do the task"})];
        for i in 0..rounds {
            h.push(json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":format!("f{i}")}}}]}));
            h.push(json!({"role":"tool","tool_name":"read_file","content":"x".repeat(size)}));
        }
        h
    }

    #[test]
    fn nothing_changes_while_it_fits() {
        let mut h = conv(3, 100);
        assert_eq!(compact(&mut h, 10_000, 500), 0);
        assert_eq!(h.len(), 7);
    }

    #[test]
    fn old_tool_output_is_elided_before_anything_is_dropped() {
        let mut h = conv(10, 3000);
        let n = h.len();
        let c = compact(&mut h, 6000, 500);
        assert!(c > 0);
        assert_eq!(h.len(), n, "elision keeps the structure");
        assert_eq!(h[2]["content"], ELIDED);
        assert_ne!(
            h[n - 1]["content"],
            ELIDED,
            "the latest results stay intact"
        );
    }

    #[test]
    fn when_elision_is_not_enough_old_round_trips_go_and_pairing_is_kept() {
        let mut h = conv(30, 3000);
        compact(&mut h, 2500, 500);
        assert_eq!(h[0]["role"], "user", "the task statement is never dropped");
        assert!(h.len() >= 6);
        // every tool message still follows an assistant message that has tool calls
        for (i, m) in h.iter().enumerate() {
            if m["role"] == "tool" {
                let mut j = i;
                while h[j]["role"] == "tool" {
                    j -= 1;
                }
                assert!(
                    h[j].get("tool_calls").is_some(),
                    "orphan tool result at {i}"
                );
            }
        }
    }
}
