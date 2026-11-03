//! Validated syntax IR with byte spans into the unchanged UTF-8 source.
//!
//! No parser or model is loaded by this module. Every transformation must validate
//! these public annotations before trusting their offsets or dependency tree.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub text: String,
    pub parser_identity: String,
    pub tokens: Vec<Token>,
    pub sentences: Vec<Sentence>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Token {
    pub i: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
    pub lemma: String,
    pub pos: String,
    pub tag: String,
    pub dep: String,
    pub head: usize,
    pub sentence: usize,
    pub morph: BTreeMap<String, Vec<String>>,
    pub is_punct: bool,
    pub is_space: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sentence {
    pub start_byte: usize,
    pub end_byte: usize,
    pub root: usize,
    pub token_start: usize,
    pub token_end: usize,
}

// Python's str.isspace additionally recognizes these four ASCII separators.
// Tokenizer gaps may contain whitespace, but must not hide source words.
fn whitespace(text: &str) -> bool {
    text.chars()
        .all(|ch| ch.is_whitespace() || ('\u{001c}'..='\u{001f}').contains(&ch))
}

/// Validate public fixtures or deserialized annotations before using offsets.
/// Every sentence partitions contiguous tokens and has one reachable root.
pub fn validate(doc: &Document) -> Result<()> {
    ensure!(
        !doc.parser_identity.is_empty(),
        "Missing syntax parser identity"
    );
    let mut previous_end = 0;
    for (index, token) in doc.tokens.iter().enumerate() {
        ensure!(
            token.i == index,
            "Syntax token indices are not contiguous at {index}"
        );
        ensure!(
            token.start_byte >= previous_end && token.start_byte < token.end_byte,
            "Syntax token spans overlap, reverse, or are empty at {index}"
        );
        let actual = doc
            .text
            .get(token.start_byte..token.end_byte)
            .with_context(|| format!("Invalid syntax UTF-8 token span at {index}"))?;
        ensure!(
            actual == token.text,
            "Syntax token text differs from its source span at {index}"
        );
        ensure!(
            whitespace(&doc.text[previous_end..token.start_byte]),
            "Syntax annotation omits non-whitespace source text before token {index}"
        );
        ensure!(
            token.head < doc.tokens.len(),
            "Syntax head is out of bounds at token {index}"
        );
        ensure!(
            token.sentence < doc.sentences.len(),
            "Syntax sentence is out of bounds at token {index}"
        );
        ensure!(
            !token.pos.is_empty() && !token.dep.is_empty(),
            "Syntax POS or dependency missing at token {index}"
        );
        for (key, values) in &token.morph {
            ensure!(
                !key.is_empty()
                    && !values.is_empty()
                    && values.iter().all(|value| !value.is_empty()),
                "Empty morphological annotation at token {index}"
            );
            let unique: std::collections::BTreeSet<_> = values.iter().collect();
            ensure!(
                unique.len() == values.len(),
                "Repeated morphological value at token {index}"
            );
        }
        previous_end = token.end_byte;
    }
    ensure!(
        whitespace(&doc.text[previous_end..]),
        "Syntax annotation omits trailing source text"
    );
    let mut next_token = 0;
    for (index, sentence) in doc.sentences.iter().enumerate() {
        ensure!(
            sentence.token_start == next_token
                && sentence.token_start < sentence.token_end
                && sentence.token_end <= doc.tokens.len(),
            "Syntax sentences do not partition the tokens at sentence {index}"
        );
        ensure!(
            (sentence.token_start..sentence.token_end).contains(&sentence.root),
            "Syntax sentence root is out of bounds at sentence {index}"
        );
        ensure!(
            sentence.start_byte == doc.tokens[sentence.token_start].start_byte
                && sentence.end_byte == doc.tokens[sentence.token_end - 1].end_byte,
            "Syntax sentence span disagrees with its tokens at sentence {index}"
        );
        let mut root_count = 0;
        for token in &doc.tokens[sentence.token_start..sentence.token_end] {
            ensure!(
                token.sentence == index,
                "Syntax token has the wrong sentence at token {}",
                token.i
            );
            ensure!(
                (sentence.token_start..sentence.token_end).contains(&token.head),
                "Syntax head crosses a sentence boundary at token {}",
                token.i
            );
            if token.head == token.i {
                root_count += 1;
                ensure!(
                    token.i == sentence.root,
                    "Syntax self-head disagrees with the sentence root"
                );
            }
        }
        ensure!(
            root_count == 1,
            "Syntax sentence {index} must have exactly one self-head root"
        );
        next_token = sentence.token_end;
    }
    ensure!(
        next_token == doc.tokens.len(),
        "Syntax sentences omit tokens"
    );
    dependency_depths(doc)?;
    Ok(())
}

/// Iterative, linear-time traversal also detects cycles without recursive stack
/// growth. Sentence and head bounds must be validated before calling this.
pub(crate) fn dependency_depths(doc: &Document) -> Result<Vec<usize>> {
    let mut depths = vec![None; doc.tokens.len()];
    let mut visiting = vec![false; doc.tokens.len()];
    for start in 0..doc.tokens.len() {
        if depths[start].is_some() {
            continue;
        }
        let mut path = Vec::new();
        let mut cursor = start;
        while depths[cursor].is_none() {
            ensure!(
                !visiting[cursor],
                "Syntax dependency cycle at token {cursor}"
            );
            if doc.tokens[cursor].head == cursor {
                depths[cursor] = Some(0);
                break;
            }
            visiting[cursor] = true;
            path.push(cursor);
            cursor = doc.tokens[cursor].head;
        }
        let mut depth = depths[cursor].unwrap();
        for index in path.into_iter().rev() {
            depth += 1;
            depths[index] = Some(depth);
            visiting[index] = false;
        }
    }
    Ok(depths.into_iter().map(Option::unwrap).collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deliberately synthetic annotations; callers may set multiple sentences
    /// before validation. This helper does not pretend to be a language parser.
    pub(crate) fn fixture(text: &str, items: &[(&str, &str, &str, usize)]) -> Document {
        let mut cursor = 0;
        let mut tokens = Vec::new();
        for (i, &(word, pos, dep, head)) in items.iter().enumerate() {
            let start_byte = text[cursor..].find(word).unwrap() + cursor;
            let end_byte = start_byte + word.len();
            tokens.push(Token {
                i,
                start_byte,
                end_byte,
                text: word.into(),
                lemma: word.to_lowercase(),
                pos: pos.into(),
                tag: pos.into(),
                dep: dep.into(),
                head,
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: pos == "PUNCT",
                is_space: pos == "SPACE",
            });
            cursor = end_byte;
        }
        let sentences = if tokens.is_empty() {
            Vec::new()
        } else {
            vec![Sentence {
                start_byte: tokens[0].start_byte,
                end_byte: tokens.last().unwrap().end_byte,
                root: items
                    .iter()
                    .enumerate()
                    .find(|(i, item)| *i == item.3)
                    .unwrap()
                    .0,
                token_start: 0,
                token_end: tokens.len(),
            }]
        };
        Document {
            text: text.into(),
            parser_identity: "synthetic-fixture-v1".into(),
            tokens,
            sentences,
        }
    }

    fn tree() -> Document {
        fixture(
            "We saw birds that flew home.",
            &[
                ("We", "PRON", "nsubj", 1),
                ("saw", "VERB", "ROOT", 1),
                ("birds", "NOUN", "dobj", 1),
                ("that", "PRON", "nsubj", 4),
                ("flew", "VERB", "relcl", 2),
                ("home", "ADV", "advmod", 4),
                (".", "PUNCT", "punct", 1),
            ],
        )
    }

    #[test]
    fn invalid_spans_cycles_and_roots_are_rejected_before_offsets_are_used() {
        let original = tree();
        validate(&original).unwrap();
        assert_eq!(dependency_depths(&original).unwrap(), [1, 0, 1, 3, 2, 3, 1]);
        let mut changed = original.clone();
        changed.tokens[2].start_byte = changed.tokens[1].start_byte;
        assert!(validate(&changed).is_err());
        let mut changed = original.clone();
        changed.tokens[0].head = 2;
        changed.tokens[2].head = 0;
        assert!(
            validate(&changed)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
        let mut changed = original.clone();
        changed.tokens[0].head = changed.tokens.len();
        assert!(
            validate(&changed)
                .unwrap_err()
                .to_string()
                .contains("out of bounds")
        );
        let mut changed = original.clone();
        changed.sentences[0].root = 0;
        assert!(validate(&changed).is_err());
        let mut changed = original.clone();
        changed.tokens[2].text = "fish".into();
        assert!(
            validate(&changed)
                .unwrap_err()
                .to_string()
                .contains("source span")
        );
        let mut changed = original;
        changed.tokens[0].sentence = 1;
        assert!(validate(&changed).is_err());
    }

    #[test]
    fn source_coverage_sentence_partition_and_cross_sentence_arcs_are_checked() {
        let doc = fixture(
            "We forgot words.",
            &[("We", "PRON", "ROOT", 0), (".", "PUNCT", "punct", 0)],
        );
        assert!(
            validate(&doc)
                .unwrap_err()
                .to_string()
                .contains("omits non-whitespace")
        );
        let mut doc = tree();
        doc.sentences = vec![
            Sentence {
                start_byte: 0,
                end_byte: doc.tokens[2].end_byte,
                root: 1,
                token_start: 0,
                token_end: 3,
            },
            Sentence {
                start_byte: doc.tokens[3].start_byte,
                end_byte: doc.text.len(),
                root: 4,
                token_start: 3,
                token_end: 7,
            },
        ];
        for token in &mut doc.tokens[3..] {
            token.sentence = 1;
        }
        doc.tokens[4].head = 4;
        // The terminal punctuation still points to the first sentence's root.
        assert!(
            validate(&doc)
                .unwrap_err()
                .to_string()
                .contains("crosses a sentence boundary")
        );
        doc.tokens[6].head = 4;
        validate(&doc).unwrap();
        doc.sentences[1].token_start = 4;
        assert!(
            validate(&doc)
                .unwrap_err()
                .to_string()
                .contains("partition")
        );
    }

    #[test]
    fn byte_offsets_preserve_original_unicode_and_reject_partial_scalars() {
        let text = "cafe\u{301} isn’t 🦊 ordinary.";
        let mut doc = fixture(
            text,
            &[
                ("cafe\u{301}", "NOUN", "nsubj", 1),
                ("is", "AUX", "ROOT", 1),
                ("n’t", "PART", "neg", 1),
                ("🦊", "PUNCT", "punct", 1),
                ("ordinary", "ADJ", "acomp", 1),
                (".", "PUNCT", "punct", 1),
            ],
        );
        validate(&doc).unwrap();
        assert_eq!(doc.tokens[0].end_byte, 6);
        assert_eq!(doc.tokens[3].end_byte - doc.tokens[3].start_byte, 4);
        let json = serde_json::to_string(&doc).unwrap();
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
        doc.tokens[3].end_byte -= 1;
        assert!(validate(&doc).unwrap_err().to_string().contains("UTF-8"));
    }

    #[test]
    fn long_dependency_chains_do_not_require_recursive_stack_growth() {
        let text = std::iter::repeat_n("word", 12_000)
            .collect::<Vec<_>>()
            .join(" ");
        let items: Vec<(&str, &str, &str, usize)> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, word)| {
                (
                    word,
                    "NOUN",
                    if i == 0 { "ROOT" } else { "dep" },
                    i.saturating_sub(1),
                )
            })
            .collect();
        let doc = fixture(&text, &items);
        validate(&doc).unwrap();
        assert_eq!(dependency_depths(&doc).unwrap().last(), Some(&11_999));
    }
}
