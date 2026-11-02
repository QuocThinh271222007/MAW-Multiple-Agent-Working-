//! Best-effort secret redaction for logs, diagnostics and persisted output.

const MARKERS: &[&str] = &[
    "sk-ant-",
    "sk-proj-",
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "maw_tok_",
];

/// Replace likely credentials with `[REDACTED]`.
pub fn redact(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    'outer: while !rest.is_empty() {
        // Bearer tokens
        for prefix in [
            "Bearer ",
            "bearer ",
            "Authorization: ",
            "api_key=",
            "apikey=",
            "token=",
            "password=",
        ] {
            if rest.starts_with(prefix) {
                out.push_str(prefix);
                let tail = &rest[prefix.len()..];
                let end = tail
                    .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '&' | ';'))
                    .unwrap_or(tail.len());
                if end > 0 {
                    out.push_str("[REDACTED]");
                }
                rest = &tail[end..];
                continue 'outer;
            }
        }
        for m in MARKERS {
            if rest.starts_with(m) {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                    .unwrap_or(rest.len());
                if end >= m.len() + 8 {
                    out.push_str("[REDACTED]");
                    rest = &rest[end..];
                    continue 'outer;
                }
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_shapes() {
        let s = redact("key sk-ant-api03-abcdefghijklmnop and Bearer abc.def.ghi next");
        assert!(!s.contains("abcdefghijklmnop"));
        assert!(!s.contains("abc.def.ghi"));
        assert!(s.contains("[REDACTED]"));
        assert_eq!(redact("plain text"), "plain text");
        assert_eq!(redact("maw_tok_0123456789abcdef"), "[REDACTED]");
    }

    #[test]
    fn keeps_short_lookalikes() {
        assert_eq!(redact("sk-1"), "sk-1");
        assert_eq!(redact("héllo wörld ✓"), "héllo wörld ✓");
    }
}
