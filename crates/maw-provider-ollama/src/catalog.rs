//! Models MAW suggests downloading, and the limits that keep a pull inside the user's hardware.
//!
//! The defaults assume 8 GB of VRAM: at most 8B parameters and a download of about 6.5 GB (an 8B
//! model at 4-bit is roughly 5 GB, and the context cache needs room on top). Sizes and parameter
//! counts were read from the Ollama registry, makers and capabilities from the Ollama library pages;
//! tool support is checked again from the server once the download finishes.

use maw_core::provider::ModelChoice;

pub struct Suggested {
    pub name: &'static str,
    /// Download size read from the Ollama registry (sum of the layers).
    pub gb: f32,
    /// Parameters as the registry reports them (embeddings included, so an "E2B" can be 4.6B).
    pub params: &'static str,
    /// Who makes the model (the family's publisher; for a fine-tune, who published the tuned model).
    pub maker: &'static str,
    /// What the Ollama library lists for the family: tools, thinking, vision, audio. The server is
    /// asked again once the download finishes (a model without `tools` cannot run an agent).
    pub caps: &'static str,
    pub note: &'static str,
    /// Over the default download limit or too big for a 6 GB GPU: shown last, refused unless the
    /// limits in `[ollama]` are raised.
    pub large: bool,
}

/// Effort: only gpt-oss gets real levels (low/medium/high) from Ollama. Every other model has none
/// and MAW shows none for it - a thinking model thinks as much as it wants and `think:false` is not
/// honoured by all of them (measured: `qwen3:4b` writes its reasoning into the answer instead).
pub const SUGGESTED: &[Suggested] = &[
    Suggested {
        name: "qwen3:8b",
        gb: 5.2,
        params: "8B",
        maker: "Alibaba (Qwen)",
        caps: "tools, thinking",
        note: "best general choice that fits 8 GB",
        large: false,
    },
    Suggested {
        name: "qwen3.5:4b",
        gb: 3.3,
        params: "4.2B",
        maker: "Alibaba (Qwen)",
        caps: "tools, thinking, vision",
        note: "newer 4B, 256K context",
        large: false,
    },
    Suggested {
        name: "qwen3:4b-instruct-2507-q4_K_M",
        gb: 2.5,
        params: "4B",
        maker: "Alibaba (Qwen)",
        caps: "tools",
        note: "does NOT think: steadier for tool-heavy roles such as Leader",
        large: false,
    },
    Suggested {
        name: "qwen3:4b",
        gb: 2.5,
        params: "4B",
        maker: "Alibaba (Qwen)",
        caps: "tools, thinking",
        note: "always thinks (cannot be turned off), long reasoning fills the context",
        large: false,
    },
    Suggested {
        name: "qwen3:1.7b",
        gb: 1.4,
        params: "2B",
        maker: "Alibaba (Qwen)",
        caps: "tools, thinking",
        note: "tiny; fast but weak at multi-step work",
        large: false,
    },
    Suggested {
        name: "gemma4:e2b",
        gb: 4.6,
        params: "4.6B (2B effective)",
        maker: "Google",
        caps: "tools, thinking, vision, audio",
        note: "Gemma 4 edge model",
        large: false,
    },
    Suggested {
        name: "granite4.2:3b",
        gb: 2.2,
        params: "3.7B",
        maker: "IBM",
        caps: "tools, thinking",
        note: "small, instruction following, 128K context",
        large: false,
    },
    Suggested {
        name: "ministral-3:3b",
        gb: 3.0,
        params: "3.8B",
        maker: "Mistral AI",
        caps: "tools, vision",
        note: "edge model, does not think",
        large: false,
    },
    Suggested {
        name: "lfm3:1.7b",
        gb: 1.1,
        params: "1.7B",
        maker: "Liquid AI",
        caps: "tools, thinking",
        note: "tri-hybrid for local agents; very small",
        large: false,
    },
    Suggested {
        name: "bonsai-2:4b",
        gb: 1.6,
        params: "4.2B (2-bit)",
        maker: "Prism ML",
        caps: "tools, thinking, vision",
        note: "ternary weights (Q2): tiny download, quality below a 4-bit 4B",
        large: false,
    },
    Suggested {
        name: "qwen2.5-coder:7b",
        gb: 4.7,
        params: "7.6B",
        maker: "Alibaba (Qwen)",
        caps: "tools",
        note: "code-focused, does not think",
        large: false,
    },
    Suggested {
        name: "qwen2.5-coder:3b",
        gb: 1.9,
        params: "3.1B",
        maker: "Alibaba (Qwen)",
        caps: "tools",
        note: "small code model",
        large: false,
    },
    Suggested {
        name: "llama3.1:8b",
        gb: 4.9,
        params: "8B",
        maker: "Meta",
        caps: "tools",
        note: "general, widely used",
        large: false,
    },
    Suggested {
        name: "llama3.2:3b",
        gb: 2.0,
        params: "3.2B",
        maker: "Meta",
        caps: "tools",
        note: "small general model",
        large: false,
    },
    Suggested {
        name: "granite3.3:8b",
        gb: 4.9,
        params: "8.2B",
        maker: "IBM",
        caps: "tools",
        note: "instruction following",
        large: false,
    },
    Suggested {
        name: "mistral:7b",
        gb: 4.4,
        params: "7.2B",
        maker: "Mistral AI",
        caps: "tools",
        note: "general, older but solid",
        large: false,
    },
    Suggested {
        name: "phi4-mini",
        gb: 2.5,
        params: "3.8B",
        maker: "Microsoft",
        caps: "tools",
        note: "strong for its size",
        large: false,
    },
    Suggested {
        name: "nemotron-3.5-bolt:e4b",
        gb: 7.9,
        params: "7.5B (4B effective)",
        maker: "NVIDIA",
        caps: "tools, thinking, vision, audio",
        note: "built on the Gemma 4 architecture; does not fit 6 GB of VRAM",
        large: true,
    },
    Suggested {
        name: "mimo-v2.6:9b",
        gb: 6.6,
        params: "9B",
        maker: "Xiaomi MiMo",
        caps: "tools, thinking, vision",
        note: "MiMo-V2.6-Distill-Qwen-9B for code and agents; over 8B, does not fit 6 GB of VRAM",
        large: true,
    },
];

/// Models from Hugging Face, pulled through Ollama (`hf.co/<repo>:<quant>`). Every repo here was
/// checked on the Hugging Face API: it exists, is not gated, and has a Q4_K_M file of the size shown.
/// Whether the model can call tools depends on the chat template inside the file, so that is only
/// known once it is downloaded and the server is asked.
pub const HUGGING_FACE: &[Suggested] = &[
    Suggested {
        name: "hf.co/unsloth/Qwen3-4B-Instruct-2507-GGUF:Q4_K_M",
        gb: 2.5,
        params: "4B",
        maker: "Alibaba (Qwen), packaged by Unsloth",
        caps: "checked after download",
        note: "Hugging Face; the most downloaded GGUF of the non-thinking 4B",
        large: false,
    },
    Suggested {
        name: "hf.co/unsloth/Qwen3-1.7B-GGUF:Q4_K_M",
        gb: 1.1,
        params: "1.7B",
        maker: "Alibaba (Qwen), packaged by Unsloth",
        caps: "checked after download",
        note: "Hugging Face; tiny",
        large: false,
    },
    Suggested {
        name: "hf.co/bartowski/Qwen2.5-Coder-7B-Instruct-GGUF:Q4_K_M",
        gb: 4.7,
        params: "7B",
        maker: "Alibaba (Qwen), packaged by bartowski",
        caps: "checked after download",
        note: "Hugging Face; code model",
        large: false,
    },
    Suggested {
        name: "hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M",
        gb: 2.0,
        params: "3B",
        maker: "Meta, packaged by bartowski",
        caps: "checked after download",
        note: "Hugging Face",
        large: false,
    },
    Suggested {
        name: "hf.co/unsloth/gemma-3-4b-it-GGUF:Q4_K_M",
        gb: 2.5,
        params: "4B",
        maker: "Google, packaged by Unsloth",
        caps: "checked after download",
        note: "Hugging Face; Gemma 3",
        large: false,
    },
    Suggested {
        name: "hf.co/bartowski/Mistral-7B-Instruct-v0.3-GGUF:Q4_K_M",
        gb: 4.4,
        params: "7B",
        maker: "Mistral AI, packaged by bartowski",
        caps: "checked after download",
        note: "Hugging Face",
        large: false,
    },
];

/// Who makes a model, from its name (a fine-tune's publisher) or else from its architecture family.
/// `None` when it is not known: nothing is guessed.
pub fn maker_of(name: &str, family: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let by_name: &[(&str, &str)] = &[
        ("nemotron", "NVIDIA"),
        ("bonsai", "Prism ML"),
        ("mimo", "Xiaomi MiMo"),
        ("lfm", "Liquid AI"),
        ("granite", "IBM"),
        ("ministral", "Mistral AI"),
        ("mistral", "Mistral AI"),
        ("gemma", "Google"),
        ("gpt-oss", "OpenAI"),
        ("phi", "Microsoft"),
        ("llama", "Meta"),
        ("qwen", "Alibaba (Qwen)"),
        ("deepseek", "DeepSeek"),
    ];
    if let Some((_, m)) = by_name.iter().find(|(p, _)| {
        let base = n.rsplit('/').next().unwrap_or(&n);
        base.starts_with(p)
    }) {
        return Some(m);
    }
    let f = family.to_ascii_lowercase();
    by_name
        .iter()
        .find(|(p, _)| f.starts_with(p))
        .map(|(_, m)| *m)
}

pub fn suggestions() -> Vec<ModelChoice> {
    SUGGESTED
        .iter()
        .chain(HUGGING_FACE.iter())
        .map(|s| ModelChoice {
            id: s.name.to_string(),
            display_name: s.name.to_string(),
            description: format!(
                "{:.1} GB · {} · by {} · {} · no effort levels · {}{}",
                s.gb,
                s.params,
                s.maker,
                s.caps,
                if s.large { "OVER THE LIMIT: " } else { "" },
                s.note
            ),
            is_default: false,
            source: "catalog".into(),
            reasoning_efforts: vec![],
        })
        .collect()
}

/// Parameter count in billions from the REPOSITORY part of a Hugging Face name, where the tag is a
/// quantisation (`hf.co/unsloth/Qwen3-4B-Instruct-2507-GGUF:Q4_K_M` -> 4.0).
pub fn params_from_repo(name: &str) -> Option<f64> {
    let repo = name.split(':').next()?.rsplit('/').next()?;
    let bytes = repo.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if !(c == b'B' || c == b'b') {
            continue;
        }
        let after_ok = i + 1 == bytes.len() || matches!(bytes[i + 1], b'-' | b'_' | b'.');
        let mut s = i;
        while s > 0 && (bytes[s - 1].is_ascii_digit() || bytes[s - 1] == b'.') {
            s -= 1;
        }
        let before_ok = s == 0 || matches!(bytes[s - 1], b'-' | b'_' | b'.');
        if after_ok && before_ok && s < i {
            if let Ok(v) = repo[s..i].parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
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
    let size = params_billions(n).or_else(|| {
        // a Hugging Face name carries the size in the repository, not in the tag
        n.starts_with("hf.co/")
            .then(|| params_from_repo(n))
            .flatten()
    });
    if let Some(p) = size {
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
    fn suggestions_fit_the_default_limit_except_the_ones_marked_large() {
        for s in SUGGESTED {
            let ok = check_pull_name(s.name, 8.0).is_ok() && s.gb <= 6.5;
            assert_eq!(ok, !s.large, "{} ({} GB, large={})", s.name, s.gb, s.large);
            assert!(!s.maker.is_empty() && !s.params.is_empty() && !s.caps.is_empty());
        }
        assert_eq!(suggestions().len(), SUGGESTED.len() + HUGGING_FACE.len());
        // the models that were asked for are all there
        for want in [
            "qwen3.5:4b",
            "lfm3:1.7b",
            "granite4.2:3b",
            "gemma4:e2b",
            "ministral-3:3b",
            "nemotron-3.5-bolt:e4b",
            "bonsai-2:4b",
            "mimo-v2.6:9b",
        ] {
            assert!(SUGGESTED.iter().any(|s| s.name == want), "{want}");
        }
        // no model other than gpt-oss is offered effort levels
        assert!(suggestions().iter().all(|m| m.reasoning_efforts.is_empty()));
    }

    #[test]
    fn hugging_face_names_are_checked_like_the_others() {
        assert_eq!(
            params_from_repo("hf.co/unsloth/Qwen3-4B-Instruct-2507-GGUF:Q4_K_M"),
            Some(4.0)
        );
        assert_eq!(
            params_from_repo("hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"),
            Some(3.0)
        );
        assert_eq!(
            params_from_repo("hf.co/unsloth/Qwen3-1.7B-GGUF:Q4_K_M"),
            Some(1.7)
        );
        assert_eq!(params_from_repo("hf.co/x/NoSizeHere-GGUF:Q4_K_M"), None);
        for s in HUGGING_FACE {
            assert!(
                s.name.starts_with("hf.co/") && s.name.contains(':'),
                "{}",
                s.name
            );
            assert!(check_pull_name(s.name, 8.0).is_ok(), "{}", s.name);
            assert!(s.gb <= 6.5);
        }
        // a big repo is refused up front, before anything is downloaded
        assert!(
            check_pull_name("hf.co/someone/Llama-3.1-70B-Instruct-GGUF:Q4_K_M", 8.0)
                .unwrap_err()
                .contains("70")
        );
        assert_eq!(suggestions().len(), SUGGESTED.len() + HUGGING_FACE.len());
    }

    #[test]
    fn makers_come_from_the_name_first_and_the_family_second() {
        assert_eq!(maker_of("nemotron-3.5-bolt:e4b", "gemma4"), Some("NVIDIA"));
        assert_eq!(maker_of("bonsai-2:4b", "qwen35"), Some("Prism ML"));
        assert_eq!(maker_of("mimo-v2.6:9b", "qwen35"), Some("Xiaomi MiMo"));
        assert_eq!(maker_of("gemma4:e2b", "gemma4"), Some("Google"));
        assert_eq!(
            maker_of("hf.co/someone/some-model", "qwen3"),
            Some("Alibaba (Qwen)")
        );
        assert_eq!(maker_of("custom-thing:1b", "unknownarch"), None);
    }
}
