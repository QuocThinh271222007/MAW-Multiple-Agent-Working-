//! Turning text into numbers and back: a character tokenizer, and byte-pair encoding (BPE) built on
//! top of it by repeatedly merging the most frequent neighbouring pair, which is how the tokenizers of
//! real language models are made (only at a vastly bigger scale).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokenizer {
    /// The base symbols: id `i` is `chars[i]`.
    pub chars: Vec<char>,
    /// Merge rules in the order they were learned: the pair `(a, b)` becomes id `chars.len() + index`.
    pub merges: Vec<(u32, u32)>,
    #[serde(skip)]
    expansion: Vec<String>,
}

impl Tokenizer {
    /// One token per distinct character of `text`, in sorted order.
    pub fn char_level(text: &str) -> Tokenizer {
        let mut chars: Vec<char> = text.chars().collect();
        chars.sort_unstable();
        chars.dedup();
        let mut t = Tokenizer {
            chars,
            merges: vec![],
            expansion: vec![],
        };
        t.rebuild();
        t
    }

    /// Characters plus up to `n_merges` learned merges (stops early when no pair repeats).
    pub fn bpe(text: &str, n_merges: usize) -> Tokenizer {
        let mut t = Tokenizer::char_level(text);
        let mut ids = t.encode(text);
        for _ in 0..n_merges {
            let mut counts: HashMap<(u32, u32), usize> = HashMap::new();
            for w in ids.windows(2) {
                *counts.entry((w[0], w[1])).or_default() += 1;
            }
            // the most frequent pair; ties go to the smaller pair so the result is reproducible
            let best = counts
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                .map(|(p, c)| (*p, *c));
            let Some((pair, count)) = best else { break };
            if count < 2 {
                break;
            }
            let new_id = (t.chars.len() + t.merges.len()) as u32;
            t.merges.push(pair);
            ids = merge_pair(&ids, pair, new_id);
        }
        t.rebuild();
        t
    }

    /// Recompute the text of every token (needed after loading from a file).
    pub fn rebuild(&mut self) {
        let mut exp: Vec<String> = self.chars.iter().map(|c| c.to_string()).collect();
        for &(a, b) in &self.merges {
            let s = format!("{}{}", exp[a as usize], exp[b as usize]);
            exp.push(s);
        }
        self.expansion = exp;
    }

    pub fn vocab_size(&self) -> usize {
        self.chars.len() + self.merges.len()
    }

    /// The text a token stands for.
    pub fn token_str(&self, id: u32) -> &str {
        self.expansion
            .get(id as usize)
            .map(String::as_str)
            .unwrap_or("?")
    }

    /// Characters the tokenizer has never seen (they cannot be encoded).
    pub fn unknown_chars(&self, text: &str) -> Vec<char> {
        let mut u: Vec<char> = text
            .chars()
            .filter(|c| self.chars.binary_search(c).is_err())
            .collect();
        u.sort_unstable();
        u.dedup();
        u
    }

    /// Text to token ids. Characters outside the vocabulary are skipped.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut ids: Vec<u32> = text
            .chars()
            .filter_map(|c| self.chars.binary_search(&c).ok().map(|i| i as u32))
            .collect();
        for (i, &pair) in self.merges.iter().enumerate() {
            ids = merge_pair(&ids, pair, (self.chars.len() + i) as u32);
        }
        ids
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        ids.iter().map(|&i| self.token_str(i)).collect()
    }

    /// The text cut into its tokens: what a student sees when they ask "how was this split?".
    pub fn pieces(&self, text: &str) -> Vec<(String, u32)> {
        self.encode(text)
            .into_iter()
            .map(|id| (self.token_str(id).to_string(), id))
            .collect()
    }
}

fn merge_pair(ids: &[u32], pair: (u32, u32), new_id: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(ids.len());
    let mut i = 0;
    while i < ids.len() {
        if i + 1 < ids.len() && ids[i] == pair.0 && ids[i + 1] == pair.1 {
            out.push(new_id);
            i += 2;
        } else {
            out.push(ids[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_round_trip_and_unknown_ones_are_reported() {
        let t = Tokenizer::char_level("hello world");
        assert_eq!(t.vocab_size(), 8); // ' ', d, e, h, l, o, r, w
        let ids = t.encode("hello");
        assert_eq!(t.decode(&ids), "hello");
        assert_eq!(t.unknown_chars("héllo!"), vec!['!', 'é']);
        assert_eq!(
            t.decode(&t.encode("héllo")),
            "hllo",
            "unknown characters are dropped"
        );
    }

    #[test]
    fn bpe_learns_the_frequent_pairs_first_and_shortens_the_text() {
        let text = "ab ab ab ab abc abc";
        let t = Tokenizer::bpe(text, 5);
        assert!(!t.merges.is_empty());
        let first = t.merges[0];
        // 'a','b' is the commonest neighbouring pair
        assert_eq!((t.token_str(first.0), t.token_str(first.1)), ("a", "b"));
        let plain = Tokenizer::char_level(text).encode(text).len();
        assert!(t.encode(text).len() < plain, "merges make the text shorter");
        assert_eq!(t.decode(&t.encode(text)), text, "and nothing is lost");
        let pieces = t.pieces("abc");
        assert_eq!(
            pieces.iter().map(|p| p.0.as_str()).collect::<String>(),
            "abc"
        );
    }

    #[test]
    fn bpe_stops_when_nothing_repeats_and_is_reproducible() {
        let t = Tokenizer::bpe("abcdefg", 10);
        assert!(t.merges.is_empty());
        let a = Tokenizer::bpe("the cat sat on the mat. the end.", 12);
        let b = Tokenizer::bpe("the cat sat on the mat. the end.", 12);
        assert_eq!(a, b);
        // round trip through JSON (what a checkpoint does) keeps working after rebuild()
        let mut c: Tokenizer = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        c.rebuild();
        assert_eq!(c.decode(&c.encode("the cat")), "the cat");
    }
}
