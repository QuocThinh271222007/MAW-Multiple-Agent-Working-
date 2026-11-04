//! Models MAW suggests downloading, and the limits that keep a pull inside the user's hardware.
//!
//! The defaults assume 8 GB of VRAM: at most 8B parameters and a download of about 6.5 GB (an 8B
//! model at 4-bit is roughly 5 GB, and the context cache needs room on top). Sizes below were read
//! from the Ollama registry; tool support is as published in the Ollama library and is checked
//! again from the server once the download finishes.

use maw_core::provider::ModelChoice;

pub struct Suggested {
    pub name: &'static str,
    pub gb: f32,
    pub caps: &'static str,
    pub note: &'static str,
}

pub const SUGGESTED: &[Suggested] = &[
    Suggested {
        name: "qwen3:8b",
        gb: 5.2,
        caps: "tools, thinking",
        note: "best general choice that fits 8 GB",
    },
    Suggested {
        name: "qwen3:4b-instruct-2507-q4_K_M",
        gb: 2.5,
        caps: "tools",
        note: "4B that does NOT think: steadier for tool-heavy roles such as Leader",
    },
    Suggested {
        name: "qwen3:4b",
        gb: 2.5,
        caps: "tools, thinking",
        note: "light; leaves room for a large context",
    },
    Suggested {
        name: "qwen3:1.7b",
        gb: 1.4,
        caps: "tools, thinking",
        note: "tiny; fast but weak at multi-step work",
    },
    Suggested {
        name: "qwen2.5-coder:7b",
        gb: 4.7,
        caps: "tools",
        note: "code-focused, no thinking",
    },
    Suggested {
        name: "qwen2.5-coder:3b",
        gb: 1.9,
        caps: "tools",
        note: "small code model",
    },
    Suggested {
        name: "llama3.1:8b",
        gb: 4.9,
        caps: "tools",
        note: "general, widely used",
    },
    Suggested {
        name: "llama3.2:3b",
        gb: 2.0,
        caps: "tools",
        note: "small general model",
    },
    Suggested {
        name: "granite3.3:8b",
        gb: 4.9,
        caps: "tools",
        note: "IBM Granite, instruction following",
    },
    Suggested {
        name: "mistral:7b",
        gb: 4.4,
        caps: "tools",
        note: "general, older but solid",
    },
    Suggested {
        name: "phi4-mini",
        gb: 2.5,
        caps: "tools",
        note: "3.8B, strong for its size",
    },
    Suggested {
        name: "lfm3:1.7b",
        gb: 1.1,
        caps: "tools, thinking",
        note: "very small, newer and less tested",
    },
];

pub fn suggestions() -> Vec<ModelChoice> {
    SUGGESTED
        .iter()
        .map(|s| ModelChoice {
            id: s.name.to_string(),
            display_name: s.name.to_string(),
            description: format!("{:.1} GB · {} · {}", s.gb, s.caps, s.note),
            is_default: false,
            source: "catalog".into(),
            reasoning_efforts: vec![],
        })
        .collect()
}

/// Parameter count in billions read from the tag (`qwen3:8b` → 8.0, `x:1.7b-q4` → 1.7).
pub fn params_billions(name: &str) -> Option<f64> {
    let tag = name.rsplit_once(':').map(|(_, t)| t)?;
    let b = tag.to_ascii_lowercase();
    let bytes = b.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c != b'b' {
            continue;
        }
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphabetic();
        let after_ok = i + 1 == bytes.len() || !bytes[i + 1].is_ascii_alphabetic();
        if !(before_ok && after_ok) {
            continue;
        }
        let mut s = i;
        while s > 0 && (bytes[s - 1].is_ascii_digit() || bytes[s - 1] == b'.') {
            s -= 1;
        }
        if s < i {
            if let Ok(v) = b[s..i].parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

/// Validate a model reference typed or chosen for download.
pub fn check_pull_name(name: &str, max_params_b: f64) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("no model name given".into());
    }
    if n.len() > 120
        || n.starts_with('-')
        || n.contains("..")
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
    {
        return Err(format!(
            "'{n}' is not a valid model name (letters, digits and . _ : / - only)"
        ));
    }
    if let Some(p) = params_billions(n) {
        if p > max_params_b {
            return Err(format!(
                "'{n}' has about {p}B parameters; downloads are limited to {max_params_b}B so they fit your VRAM (change `max_pull_params_b` under [ollama] to allow more)"
            ));
        }
    }
    Ok(n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_counts_are_read_from_tags() {
        assert_eq!(params_billions("qwen3:8b"), Some(8.0));
        assert_eq!(params_billions("qwen3:1.7b"), Some(1.7));
        assert_eq!(params_billions("llama3.1:70b-instruct-q4_K_M"), Some(70.0));
        assert_eq!(params_billions("phi4-mini"), None);
        assert_eq!(
            params_billions("model:latest"),
            None,
            "'latest' contains no size"
        );
        assert_eq!(params_billions("x:q4_0"), None);
    }

    #[test]
    fn pull_names_are_checked_for_shape_and_size() {
        assert!(check_pull_name("qwen3:8b", 8.0).is_ok());
        assert!(check_pull_name(" phi4-mini ", 8.0).is_ok());
        assert!(check_pull_name("llama3.1:70b", 8.0)
            .unwrap_err()
            .contains("limited to 8B"));
        assert!(check_pull_name("qwen3:14b", 8.0).is_err());
        assert!(check_pull_name("qwen3:14b", 16.0).is_ok());
        for bad in ["", "-x", "a b", "a;rm -rf /", "../x\n"] {
            assert!(check_pull_name(bad, 8.0).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_suggestion_is_within_the_default_limit() {
        for s in SUGGESTED {
            assert!(check_pull_name(s.name, 8.0).is_ok(), "{}", s.name);
            assert!(s.gb <= 6.5, "{} is {} GB", s.name, s.gb);
        }
        assert_eq!(suggestions().len(), SUGGESTED.len());
    }
}
