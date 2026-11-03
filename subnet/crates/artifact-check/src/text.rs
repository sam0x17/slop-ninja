//! Canonical text profile `slop_ninja_a_text_profile_v1`: how the scorer
//! turns a document into model input today, read from
//! `inference/a-container-serve` (`engine.rs` `Model::score`, `main.rs`
//! `predict`) and the pinned `tokenizer.json`, and compared with the trainer
//! (`grammar/crates/grammar-eval/python/full_a.py` `hf_tokenize`).
//!
//! The profile describes; it does not choose. Where the trainer differs, the
//! difference is recorded in [`FINDINGS`] and the profile states the scorer's
//! behaviour.
//!
//! - **Bytes.** A document is a JSON string, so it is valid UTF-8 by
//!   construction. Invalid UTF-8 or a lone-surrogate escape fails JSON
//!   parsing of the whole score request, not one document.
//! - **Digest.** `text_sha256` is the SHA-256 of the exact UTF-8 bytes
//!   received, before anything below.
//! - **Empty.** `text.trim().is_empty()` (Rust: Unicode `White_Space`) returns
//!   status `unsupported_input`, reason `empty_text`, and no prediction.
//! - **Unicode normalization.** NFC, by the tokenizer's normalizer, inside
//!   tokenization only. The text and its digest are not rewritten.
//! - **Whitespace.** Kept: no stripping, collapsing or newline conversion.
//!   The byte-level pre-tokenizer encodes every byte; runs of spaces can match
//!   the tokenizer's whitespace added tokens.
//! - **Invisible characters.** Kept: zero-width characters, joiners, bidi
//!   controls, the BOM and soft hyphens pass NFC unchanged and become byte
//!   tokens. Nothing is removed.
//! - **Added and special tokens.** The tokenizer splits its added tokens out
//!   of the raw text before BPE, special ones included: a literal `[SEP]` in a
//!   document becomes the SEP id.
//! - **Frame.** `[CLS]` + tokens + `[SEP]` (the tokenizer's post-processor).
//! - **Over-length.** At most 2,048 tokens including the frame. A longer
//!   document returns `unsupported_input`, reason `unsupported_length`, with
//!   its token count and no prediction. Nothing is truncated or windowed.

use serde_json::{Value, json};

use crate::spec;

pub const PROFILE_ID: &str = "slop_ninja_a_text_profile_v1";

/// Largest input, in tokens including `[CLS]` and `[SEP]`.
pub const MAX_TOKENS_INCLUDING_SPECIALS: u64 = 2048;

/// The scorer's empty-text rule.
pub fn scorer_is_empty(text: &str) -> bool {
    text.trim().is_empty()
}

/// The trainer's empty-text rule: Python `str.strip`, whose whitespace is
/// Unicode `White_Space` plus U+001C..U+001F.
pub fn trainer_is_empty(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Places where the scorer and the trainer (or the evaluation data path)
/// disagree today. Recorded, not resolved.
pub const FINDINGS: [&str; 3] = [
    "empty text: the scorer tests Rust str::trim (Unicode White_Space); the trainer tests Python \
     str.strip, which also strips U+001C..U+001F. A document of only those separators (with or \
     without White_Space) is refused by the trainer and scored by the scorer.",
    "failure shape: the trainer raises on empty and over-length text (training rows are filtered \
     before the trainer sees them); the scorer returns per-document unsupported_input statuses. \
     Both count [CLS] and [SEP] and neither truncates.",
    "evaluation data: the coordinator trims leading and trailing White_Space from generated text \
     (eval/textgen.rs common_checks) but passes human pool text as stored \
     (tools/a-testnet-eval-manifest export_human_pool). Edge whitespace can therefore differ by \
     origin; whether stored human rows carry it is not measured here.",
];

/// The profile as a JSON record; its digest is [`profile_sha256`].
pub fn profile_json() -> Value {
    crate::sorted_keys(json!({
        "schema": PROFILE_ID,
        "tokenizer_sha256": spec::TOKENIZER_SHA256,
        "input": "JSON string (valid UTF-8); a malformed request fails as a whole",
        "text_sha256": "SHA-256 of the exact UTF-8 bytes received",
        "empty": {"rule": "rust_str_trim_is_empty", "whitespace": "Unicode White_Space",
            "result": "unsupported_input/empty_text"},
        "unicode_normalization": {"form": "NFC", "applied_by": "tokenizer normalizer",
            "text_rewritten": false},
        "whitespace": "kept",
        "invisible_characters": "kept",
        "added_tokens": "matched in raw text, special tokens included",
        "frame": ["[CLS]", "[SEP]"],
        "over_length": {"max_tokens_including_specials": MAX_TOKENS_INCLUDING_SPECIALS,
            "truncation": false, "result": "unsupported_input/unsupported_length"},
        "findings": FINDINGS,
    }))
}

/// SHA-256 of the compact serialization of [`profile_json`].
pub fn profile_sha256() -> String {
    crate::sha256_hex(&serde_json::to_vec(&profile_json()).expect("profile serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Code points `c` with `chr(c).strip() == ""`, recorded from CPython
    /// 3.12.15 (Unicode 15.0.0): the trainer's empty-text rule, measured
    /// rather than restated.
    const CPYTHON_STRIP_EMPTY: [u32; 29] = [
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x85, 0xa0, 0x1680, 0x2000,
        0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028,
        0x2029, 0x202f, 0x205f, 0x3000,
    ];

    #[test]
    fn the_trainer_rule_is_cpythons_strip() {
        let ours: Vec<u32> = (0u32..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|c| trainer_is_empty(&c.to_string()) && !c.to_string().is_empty())
            .map(u32::from)
            .collect();
        assert_eq!(ours, CPYTHON_STRIP_EMPTY);
        let scorer: Vec<u32> = (0u32..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|c| scorer_is_empty(&c.to_string()))
            .map(u32::from)
            .collect();
        let differ: Vec<u32> = CPYTHON_STRIP_EMPTY
            .iter()
            .copied()
            .filter(|c| !scorer.contains(c))
            .collect();
        assert_eq!(differ, vec![0x1c, 0x1d, 0x1e, 0x1f]);
        assert!(scorer.iter().all(|c| CPYTHON_STRIP_EMPTY.contains(c)));
    }

    #[test]
    fn scorer_and_trainer_empty_rules_differ_exactly_on_the_information_separators() {
        let differ: Vec<u32> = (0u32..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|c| {
                let s = c.to_string();
                scorer_is_empty(&s) != trainer_is_empty(&s)
            })
            .map(u32::from)
            .collect();
        assert_eq!(differ, vec![0x1c, 0x1d, 0x1e, 0x1f]);
        // The 25 Rust White_Space characters are empty to both.
        let both = (0u32..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|c| scorer_is_empty(&c.to_string()))
            .count();
        assert_eq!(both, 25);
        assert!(!scorer_is_empty("\u{200b}") && !trainer_is_empty("\u{200b}"));
        assert!(scorer_is_empty("") && trainer_is_empty(""));
        assert!(!scorer_is_empty("\u{1c} \n") && trainer_is_empty("\u{1c} \n"));
    }

    #[test]
    fn profile_matches_the_pinned_contract_limits() {
        assert_eq!(
            MAX_TOKENS_INCLUDING_SPECIALS,
            spec::contract::MAX_TOKENS_INCLUDING_SPECIALS
        );
        assert_eq!(profile_sha256().len(), 64);
    }
}
