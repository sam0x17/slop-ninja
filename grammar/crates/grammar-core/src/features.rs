//! Versioned word-occurrence and grammatical features, with raw denominators.
//!
//! A family with zero opportunities is unavailable, even when its fixed metric
//! coordinates contain zeros. Consumers must not treat that case as an observed
//! zero rate or a successful match. Counts remain integers until comparison.

use crate::syntax::{Document, Token, dependency_depths, validate};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use unicode_categories::UnicodeCategories;
use unicode_normalization::UnicodeNormalization;

pub mod order;

pub const FEATURE_VERSION: &str = "grammar-features-v1;nfc-lowercase-letter-apostrophe-v1;alphabetic-parse-token-v1;syntax-families-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Features {
    pub schema: String,
    pub parser_identity: String,
    pub source_sha256: String,
    pub families: BTreeMap<String, Family>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Family {
    /// Mutually exclusive observations whose counts sum to opportunities.
    Distribution {
        counts: BTreeMap<String, u64>,
        opportunities: u64,
    },
    /// Occurrences per opportunity, without an upper bound of one. Several
    /// occurrences may share an opportunity; zero coordinates are explicit.
    Rates {
        counts: BTreeMap<String, u64>,
        opportunities: u64,
    },
    Metrics {
        values: BTreeMap<String, f64>,
        scale_floors: BTreeMap<String, f64>,
        opportunities: u64,
    },
}

/// An alternative extractor can define its own versioned feature vocabulary.
/// Its identity must change whenever tokenization, feature meaning, or a
/// denominator changes. Parser identity is retained independently in Features.
pub trait FeatureExtractor {
    fn identity(&self) -> &str;
    fn extract(&self, doc: &Document) -> Result<Features>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultExtractor;

impl FeatureExtractor for DefaultExtractor {
    fn identity(&self) -> &str {
        FEATURE_VERSION
    }

    fn extract(&self, doc: &Document) -> Result<Features> {
        default_extract(doc)
    }
}

/// Extract the default versioned families without loading a parser or model.
pub fn extract(doc: &Document) -> Result<Features> {
    DefaultExtractor.extract(doc)
}

/// NFC Unicode words: a letter followed by letters/combining marks, with an
/// internal apostrophe only before another letter. Curly and modifier-letter
/// apostrophes map to ASCII. Lowercasing is Unicode lowercase, not case-folding.
/// Digits, hyphens, and underscores separate words; ß is preserved. This lexical
/// definition is independent of a parser's contraction or punctuation splitting.
pub fn words(text: &str) -> Vec<String> {
    let normalized: String = text
        .nfc()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' | '\u{ff07}' => '\'',
            _ => ch,
        })
        .collect();
    let mut chars = normalized.chars().peekable();
    let mut result = Vec::new();
    let mut current = String::new();
    while let Some(ch) = chars.next() {
        let internal_apostrophe =
            ch == '\'' && !current.is_empty() && chars.peek().is_some_and(|next| next.is_letter());
        if ch.is_letter() || (ch.is_mark() && !current.is_empty()) || internal_apostrophe {
            current.push(ch);
        } else if !current.is_empty() {
            result.push(current.to_lowercase().nfc().collect());
            current.clear();
        }
    }
    if !current.is_empty() {
        result.push(current.to_lowercase().nfc().collect());
    }
    result
}

fn lexical(token: &Token) -> bool {
    !token.is_space && !token.is_punct && token.text.chars().any(|ch| ch.is_letter())
}

fn morphology(token: &Token) -> String {
    let bundle: Vec<String> = token
        .morph
        .iter()
        .map(|(key, values)| {
            let mut values = values.clone();
            values.sort_unstable();
            format!("{key}={}", values.join(","))
        })
        .collect();
    format!(
        "{}|{}",
        token.pos,
        if bundle.is_empty() {
            "_".into()
        } else {
            bundle.join("|")
        }
    )
}

fn ancestor_path(doc: &Document, token: &Token, max_arcs: usize) -> String {
    let mut path = token.pos.clone();
    let mut cursor = token.i;
    let mut arcs = 0;
    while doc.tokens[cursor].head != cursor && arcs < max_arcs {
        let current = &doc.tokens[cursor];
        let parent = &doc.tokens[current.head];
        path.push_str(&format!("-[{}]->{}", current.dep, parent.pos));
        cursor = current.head;
        arcs += 1;
    }
    path.push_str(if doc.tokens[cursor].head == cursor {
        "-[ROOT]"
    } else {
        "-[MORE]"
    });
    path
}

fn head_frame(doc: &Document, token: &Token, children: &[usize]) -> String {
    let describe = |index: &usize| {
        let child = &doc.tokens[*index];
        format!("{}:{}", child.dep, child.pos)
    };
    let left = children
        .iter()
        .filter(|&&i| i < token.i)
        .map(describe)
        .collect::<Vec<_>>();
    let right = children
        .iter()
        .filter(|&&i| i > token.i)
        .map(describe)
        .collect::<Vec<_>>();
    // Keep every lexical child in source order. There is no frame-size cap.
    format!("{}|L[{}]|R[{}]", token.pos, left.join(","), right.join(","))
}

fn clause_head(token: &Token) -> bool {
    token.head == token.i
        || matches!(
            token.dep.as_str(),
            "csubj"
                | "csubjpass"
                | "csubj:pass"
                | "ccomp"
                | "xcomp"
                | "advcl"
                | "acl"
                | "relcl"
                | "acl:relcl"
        )
        || (token.dep == "conj" && matches!(token.pos.as_str(), "VERB" | "AUX"))
}

fn distribution(features: impl IntoIterator<Item = String>) -> Family {
    let mut counts = BTreeMap::new();
    let mut opportunities = 0;
    for feature in features {
        *counts.entry(feature).or_default() += 1;
        opportunities += 1;
    }
    Family::Distribution {
        counts,
        opportunities,
    }
}

fn summary(values: &[usize]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().map(|&value| value as f64).sum::<f64>() / values.len() as f64;
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    (mean, sorted[(9 * sorted.len()).div_ceil(10) - 1] as f64)
}

fn metrics<'a>(
    opportunities: usize,
    series: impl IntoIterator<Item = (&'a str, &'a [usize], f64)>,
) -> Family {
    let mut values = BTreeMap::new();
    let mut scale_floors = BTreeMap::new();
    for (name, data, floor) in series {
        let (mean, p90) = summary(data);
        values.insert(format!("{name}_mean"), mean);
        values.insert(format!("{name}_p90"), p90);
        scale_floors.insert(format!("{name}_mean"), floor);
        scale_floors.insert(format!("{name}_p90"), floor);
    }
    Family::Metrics {
        values,
        scale_floors,
        opportunities: opportunities as u64,
    }
}

/// All fourteen default families are always present, including unavailable ones.
///
/// * `word`: NFC/lowercase word occurrences over the exact document text; see
///   `words` for the versioned lexical definition. `word_bigram`: adjacent words
///   within each parsed sentence, with opportunities equal to those pairs.
/// * `pos`, `pos_tag`, `dependency`, `morphology_bundle`, `dependency_path_2`,
///   `dependency_path_3`, `ordered_head_frame`: one observation per alphabetic
///   parsed token (excluding parser-marked punctuation/space). `pos_bigram` and
///   `pos_trigram`: adjacent lexical POS windows within parsed sentences.
/// * Dependency paths retain starting POS, each incoming dependency and ancestor
///   POS, and stop at the root or two/three arcs. ROOT marks a reached root; MORE
///   explicitly marks a depth-limited path. Frames retain the complete ordered
///   lexical direct-child list on each side of their head, without truncation.
/// * `clause_head_frame`: one observation per lexical root, clausal dependency
///   head (csubj, ccomp, xcomp, advcl, acl/relcl and passive/UD variants), or VERB/AUX
///   conjunct. Includes incoming dependency, morphology, and local head frame.
///   This syntactic proxy includes fragments and nonfinite clauses; it is not a
///   semantic clause count.
/// * `syntax_load`: all lexical tokens, roots included at depth/distance zero;
///   mean and nearest-rank p90 of ancestor depth (floor 1 arc), absolute head
///   distance (floor 2 parsed-token positions, punctuation/space positions
///   included), and lexical direct-child count (floor 1 child).
/// * `syntax_sentence_load`: sentences containing lexical tokens; mean and p90 of
///   lexical-token count (floor 2), clause-head count (floor 1), and maximum lexical
///   dependency depth (floor 1 arc). Empty opportunities retain fixed metric keys
///   and positive floors but do not constitute observations.
fn default_extract(doc: &Document) -> Result<Features> {
    validate(doc)?;
    let depths = dependency_depths(doc)?;
    let tokens: Vec<&Token> = doc.tokens.iter().filter(|token| lexical(token)).collect();
    let mut children = vec![Vec::new(); doc.tokens.len()];
    for token in &tokens {
        if token.head != token.i {
            children[token.head].push(token.i);
        }
    }
    let clause_heads: Vec<&Token> = tokens
        .iter()
        .copied()
        .filter(|token| clause_head(token))
        .collect();
    let mut families = BTreeMap::new();
    families.insert("word".into(), distribution(words(&doc.text)));
    let sentence_words: Vec<Vec<String>> = doc
        .sentences
        .iter()
        .map(|sentence| words(&doc.text[sentence.start_byte..sentence.end_byte]))
        .collect();
    families.insert(
        "word_bigram".into(),
        distribution(
            sentence_words
                .iter()
                .flat_map(|sentence| sentence.windows(2).map(|pair| pair.join(" "))),
        ),
    );
    families.insert(
        "pos".into(),
        distribution(tokens.iter().map(|token| token.pos.clone())),
    );
    families.insert(
        "pos_tag".into(),
        distribution(
            tokens
                .iter()
                .map(|token| format!("{}|{}", token.pos, token.tag)),
        ),
    );
    families.insert(
        "dependency".into(),
        distribution(tokens.iter().map(|token| {
            format!(
                "{}|{}|{}",
                if token.head == token.i {
                    "ROOT"
                } else {
                    &doc.tokens[token.head].pos
                },
                token.dep,
                token.pos
            )
        })),
    );
    let sentence_pos: Vec<Vec<&str>> = doc
        .sentences
        .iter()
        .map(|sentence| {
            doc.tokens[sentence.token_start..sentence.token_end]
                .iter()
                .filter(|token| lexical(token))
                .map(|token| token.pos.as_str())
                .collect()
        })
        .collect();
    for (name, size) in [("pos_bigram", 2), ("pos_trigram", 3)] {
        families.insert(
            name.into(),
            distribution(
                sentence_pos
                    .iter()
                    .flat_map(|sentence| sentence.windows(size).map(|window| window.join(" "))),
            ),
        );
    }
    families.insert(
        "morphology_bundle".into(),
        distribution(tokens.iter().map(|token| morphology(token))),
    );
    for arcs in [2, 3] {
        families.insert(
            format!("dependency_path_{arcs}"),
            distribution(tokens.iter().map(|token| ancestor_path(doc, token, arcs))),
        );
    }
    families.insert(
        "ordered_head_frame".into(),
        distribution(
            tokens
                .iter()
                .map(|token| head_frame(doc, token, &children[token.i])),
        ),
    );
    families.insert(
        "clause_head_frame".into(),
        distribution(clause_heads.iter().map(|token| {
            format!(
                "{}|{}|{}",
                if token.head == token.i {
                    "ROOT"
                } else {
                    &token.dep
                },
                morphology(token),
                head_frame(doc, token, &children[token.i])
            )
        })),
    );
    let lexical_depths: Vec<usize> = tokens.iter().map(|token| depths[token.i]).collect();
    let distances: Vec<usize> = tokens
        .iter()
        .map(|token| token.i.abs_diff(token.head))
        .collect();
    let child_counts: Vec<usize> = tokens.iter().map(|token| children[token.i].len()).collect();
    families.insert(
        "syntax_load".into(),
        metrics(
            tokens.len(),
            [
                ("dependency_depth", lexical_depths.as_slice(), 1.0),
                ("dependency_distance_tokens", distances.as_slice(), 2.0),
                ("lexical_children", child_counts.as_slice(), 1.0),
            ],
        ),
    );
    let mut sentence_tokens = Vec::new();
    let mut sentence_clauses = Vec::new();
    let mut sentence_depths = Vec::new();
    for sentence in &doc.sentences {
        let lexical: Vec<&Token> = doc.tokens[sentence.token_start..sentence.token_end]
            .iter()
            .filter(|token| lexical(token))
            .collect();
        if lexical.is_empty() {
            continue;
        }
        sentence_tokens.push(lexical.len());
        sentence_clauses.push(lexical.iter().filter(|token| clause_head(token)).count());
        sentence_depths.push(lexical.iter().map(|token| depths[token.i]).max().unwrap());
    }
    families.insert(
        "syntax_sentence_load".into(),
        metrics(
            sentence_tokens.len(),
            [
                ("lexical_tokens", sentence_tokens.as_slice(), 2.0),
                ("clause_heads", sentence_clauses.as_slice(), 1.0),
                ("max_dependency_depth", sentence_depths.as_slice(), 1.0),
            ],
        ),
    );
    Ok(Features {
        schema: FEATURE_VERSION.into(),
        parser_identity: doc.parser_identity.clone(),
        source_sha256: hex::encode(Sha256::digest(doc.text.as_bytes())),
        families,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::tests::fixture;

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

    fn count(family: &Family, key: &str) -> u64 {
        let Family::Distribution { counts, .. } = family else {
            panic!("Expected distribution")
        };
        counts.get(key).copied().unwrap_or(0)
    }

    fn opportunities(family: &Family) -> u64 {
        match family {
            Family::Distribution { opportunities, .. }
            | Family::Rates { opportunities, .. }
            | Family::Metrics { opportunities, .. } => *opportunities,
        }
    }

    fn metric(family: &Family, key: &str) -> f64 {
        let Family::Metrics { values, .. } = family else {
            panic!("Expected metrics")
        };
        values[key]
    }

    #[test]
    fn word_occurrences_are_first_class_and_do_not_depend_on_contraction_tokens() {
        let text = "CAFÉ cafe\u{301} isn’t unusual.";
        let doc = fixture(
            text,
            &[
                ("CAFÉ", "NOUN", "compound", 1),
                ("cafe\u{301}", "NOUN", "nsubj", 2),
                ("is", "AUX", "ROOT", 2),
                ("n’t", "PART", "neg", 2),
                ("unusual", "ADJ", "acomp", 2),
                (".", "PUNCT", "punct", 2),
            ],
        );
        let result = extract(&doc).unwrap();
        assert_eq!(count(&result.families["word"], "café"), 2);
        assert_eq!(count(&result.families["word"], "isn't"), 1);
        assert_eq!(opportunities(&result.families["word"]), 4);
        assert_eq!(opportunities(&result.families["pos"]), 5);
        assert_eq!(
            result.source_sha256,
            hex::encode(Sha256::digest(text.as_bytes()))
        );
        assert_eq!(doc.text, text);
        assert_eq!(
            words("'hello' 123 co-operate under_score Straße हिन्दी"),
            [
                "hello",
                "co",
                "operate",
                "under",
                "score",
                "straße",
                "हिन्दी"
            ]
        );
    }

    #[test]
    fn lexical_and_pos_windows_stop_at_sentence_boundaries() {
        let mut doc = fixture(
            "Birds fly. Fish swim.",
            &[
                ("Birds", "NOUN", "nsubj", 1),
                ("fly", "VERB", "ROOT", 1),
                (".", "PUNCT", "punct", 1),
                ("Fish", "NOUN", "nsubj", 4),
                ("swim", "VERB", "ROOT", 4),
                (".", "PUNCT", "punct", 4),
            ],
        );
        doc.sentences = vec![
            crate::syntax::Sentence {
                start_byte: 0,
                end_byte: 10,
                root: 1,
                token_start: 0,
                token_end: 3,
            },
            crate::syntax::Sentence {
                start_byte: 11,
                end_byte: 21,
                root: 4,
                token_start: 3,
                token_end: 6,
            },
        ];
        for token in &mut doc.tokens[3..] {
            token.sentence = 1;
        }
        let result = extract(&doc).unwrap();
        assert_eq!(opportunities(&result.families["word_bigram"]), 2);
        assert_eq!(count(&result.families["word_bigram"], "fly fish"), 0);
        assert_eq!(opportunities(&result.families["pos_bigram"]), 2);
        assert_eq!(count(&result.families["pos_bigram"], "VERB NOUN"), 0);
        assert_eq!(opportunities(&result.families["pos_trigram"]), 0);
    }

    #[test]
    fn paths_frames_and_metrics_use_complete_explicit_opportunities() {
        let doc = tree();
        let result = extract(&doc).unwrap();
        let families = &result.families;
        for name in [
            "morphology_bundle",
            "dependency_path_2",
            "dependency_path_3",
            "ordered_head_frame",
        ] {
            let Family::Distribution {
                counts,
                opportunities,
            } = &families[name]
            else {
                panic!()
            };
            assert_eq!(*opportunities, 6);
            assert_eq!(counts.values().sum::<u64>(), *opportunities);
        }
        assert_eq!(opportunities(&families["clause_head_frame"]), 2);
        assert_eq!(count(&families["dependency_path_2"], "VERB-[ROOT]"), 1);
        assert_eq!(
            count(
                &families["dependency_path_2"],
                "ADV-[advmod]->VERB-[relcl]->NOUN-[MORE]"
            ),
            1
        );
        assert_eq!(
            count(
                &families["dependency_path_3"],
                "ADV-[advmod]->VERB-[relcl]->NOUN-[dobj]->VERB-[ROOT]"
            ),
            1
        );
        assert_eq!(
            count(
                &families["ordered_head_frame"],
                "VERB|L[nsubj:PRON]|R[dobj:NOUN]"
            ),
            1
        );
        assert_eq!(opportunities(&families["syntax_load"]), 6);
        assert_eq!(
            metric(&families["syntax_load"], "dependency_depth_mean"),
            10.0 / 6.0
        );
        assert_eq!(
            metric(&families["syntax_load"], "dependency_depth_p90"),
            3.0
        );
        assert_eq!(
            metric(&families["syntax_load"], "dependency_distance_tokens_mean"),
            1.0
        );
        assert_eq!(opportunities(&families["syntax_sentence_load"]), 1);
        assert_eq!(
            metric(&families["syntax_sentence_load"], "clause_heads_mean"),
            2.0
        );
        let serialized = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<Features>(&serialized).unwrap(),
            result
        );
    }

    #[test]
    fn morphology_is_sorted_and_long_child_frames_are_not_capped() {
        let mut doc = tree();
        doc.tokens[1].morph = BTreeMap::from([
            ("VerbForm".into(), vec!["Part".into(), "Fin".into()]),
            ("Tense".into(), vec!["Past".into()]),
        ]);
        assert_eq!(
            morphology(&doc.tokens[1]),
            "VERB|Tense=Past|VerbForm=Fin,Part"
        );
        let text = "root a b c d e f g h i j k l m n o p q r s t";
        let items: Vec<(&str, &str, &str, usize)> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, word)| {
                (
                    word,
                    if i == 0 { "VERB" } else { "NOUN" },
                    if i == 0 { "ROOT" } else { "dobj" },
                    0,
                )
            })
            .collect();
        let result = extract(&fixture(text, &items)).unwrap();
        let Family::Distribution { counts, .. } = &result.families["ordered_head_frame"] else {
            panic!()
        };
        let frame = counts.keys().find(|key| key.starts_with("VERB|")).unwrap();
        assert_eq!(frame.matches("dobj:NOUN").count(), 20);
        assert_eq!(
            metric(&result.families["syntax_load"], "lexical_children_mean"),
            20.0 / 21.0
        );
    }

    #[test]
    fn absent_opportunities_remain_unavailable_with_fixed_finite_metrics() {
        for text in ["", "   ", "."] {
            let items = if text.is_empty() {
                vec![]
            } else {
                vec![(text, if text == "." { "PUNCT" } else { "SPACE" }, "ROOT", 0)]
            };
            let result = extract(&fixture(text, &items)).unwrap();
            assert_eq!(result.families.len(), 14);
            for family in result.families.values() {
                assert_eq!(opportunities(family), 0);
                match family {
                    Family::Distribution { counts, .. } | Family::Rates { counts, .. } => {
                        assert!(counts.is_empty())
                    }
                    Family::Metrics {
                        values,
                        scale_floors,
                        ..
                    } => {
                        assert!(values.values().all(|value| *value == 0.0));
                        assert_eq!(
                            values.keys().collect::<Vec<_>>(),
                            scale_floors.keys().collect::<Vec<_>>()
                        );
                        assert!(
                            scale_floors
                                .values()
                                .all(|value| value.is_finite() && *value > 0.0)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn extractor_interface_retains_independent_parser_and_feature_identities() {
        let doc = tree();
        let engine: &dyn FeatureExtractor = &DefaultExtractor;
        let result = engine.extract(&doc).unwrap();
        assert_eq!(engine.identity(), result.schema);
        assert_eq!(result.parser_identity, doc.parser_identity);
        let mut invalid = doc.clone();
        invalid.tokens[0].head = invalid.tokens.len();
        assert!(engine.extract(&invalid).is_err());
    }
}
