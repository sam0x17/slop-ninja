//! Optional sentence-order observations. Default feature identities stay unchanged.
use super::{Family, Features, distribution, head_frame, lexical, morphology};
use crate::syntax::Document;
use anyhow::Result;
use std::collections::BTreeMap;

pub const VERSION: &str =
    "grammar-sentence-order-v1;quartiles-4;length-bins-8-16-32;blank-line-paragraphs";
pub const FACTORIZED_VERSION: &str = "grammar-sentence-order-v2;factorized-opening-pos-2-root-frame-morph-length;quartiles-4;length-bins-8-16-32;blank-line-paragraphs";

struct SentenceStyle {
    opening: String,
    root_frame: String,
    root_morph: String,
    length: &'static str,
}

fn paragraph_break(gap: &str) -> bool {
    let mut newline = false;
    for ch in gap.chars() {
        if ch == '\n' {
            if newline {
                return true;
            }
            newline = true;
        } else if !ch.is_whitespace() {
            newline = false;
        }
    }
    false
}

fn quartile(index: usize, count: usize) -> usize {
    ((index as u128 * 4) / count as u128) as usize
}

/// Three distributions, each with explicit opportunities:
/// - document position: one (sentence-index quartile, signature) per lexical sentence;
/// - paragraph position: the same within its paragraph;
/// - transitions: adjacent signatures within a paragraph, including START/END edges.
///
/// A signature contains the first three lexical POS tags, lexical root-child
/// frame, root morphology and a lexical-token-length bin (1..8, 9..16, 17..32,
/// 33+). It contains no content words. Paragraphs are separated by two LF
/// characters with only whitespace between them, including CRLF and blank lines
/// containing spaces. Only boundaries between parsed lexical sentences count.
/// Sentences without alphabetic tokens contribute no observations. Paragraph
/// boundaries inside one parser sentence cannot be represented by this view.
/// These are lossy syntactic proxies, not semantic discourse roles or quality.
pub fn extract(doc: &Document) -> Result<Features> {
    let (base, descriptions) = describe(doc)?;
    let paragraphs = descriptions
        .iter()
        .map(|paragraph| {
            paragraph
                .iter()
                .map(|s| {
                    format!(
                        "open[{}]|{}|{}|length[{}]",
                        s.opening, s.root_frame, s.root_morph, s.length
                    )
                })
                .collect()
        })
        .collect::<Vec<_>>();
    Ok(Features {
        schema: format!("{};{VERSION}", base.schema),
        parser_identity: base.parser_identity,
        source_sha256: base.source_sha256,
        families: order_families(&paragraphs, ""),
    })
}

/// Twelve distributions factor the v1 joint pattern into opening POS (first two),
/// root frame, root morphology, and length bin. Each descriptor has its own
/// document-position, paragraph-position and transition distribution with the
/// same opportunities as v1. A match no longer requires all descriptors to agree.
/// This reduces sparse exact-pattern matching, at the cost of their correlations.
pub fn extract_factorized(doc: &Document) -> Result<Features> {
    let (base, descriptions) = describe(doc)?;
    let mut families = BTreeMap::new();
    for descriptor in ["opening_pos", "root_frame", "root_morph", "length_bin"] {
        let paragraphs = descriptions
            .iter()
            .map(|paragraph| {
                paragraph
                    .iter()
                    .map(|s| match descriptor {
                        "opening_pos" => s
                            .opening
                            .split_whitespace()
                            .take(2)
                            .collect::<Vec<_>>()
                            .join(" "),
                        "root_frame" => s.root_frame.clone(),
                        "root_morph" => s.root_morph.clone(),
                        _ => s.length.to_owned(),
                    })
                    .collect()
            })
            .collect::<Vec<_>>();
        families.extend(order_families(&paragraphs, descriptor));
    }
    Ok(Features {
        schema: format!("{};{FACTORIZED_VERSION}", base.schema),
        parser_identity: base.parser_identity,
        source_sha256: base.source_sha256,
        families,
    })
}

fn describe(doc: &Document) -> Result<(Features, Vec<Vec<SentenceStyle>>)> {
    // The default extractor validates all source spans and dependency trees.
    let base = super::extract(doc)?;
    let mut children = vec![vec![]; doc.tokens.len()];
    for token in doc.tokens.iter().filter(|t| lexical(t)) {
        if token.i != token.head {
            children[token.head].push(token.i);
        }
    }
    let mut paragraphs: Vec<Vec<SentenceStyle>> = vec![];
    let mut previous_end = 0;
    for sentence in &doc.sentences {
        let tokens: Vec<_> = doc.tokens[sentence.token_start..sentence.token_end]
            .iter()
            .filter(|t| lexical(t))
            .collect();
        let Some(first) = tokens.first() else {
            continue;
        };
        if paragraphs.is_empty() || paragraph_break(&doc.text[previous_end..first.start_byte]) {
            paragraphs.push(vec![]);
        }
        previous_end = tokens.last().unwrap().end_byte;
        let root = &doc.tokens[sentence.root];
        let length = match tokens.len() {
            1..=8 => "1-8",
            9..=16 => "9-16",
            17..=32 => "17-32",
            _ => "33+",
        };
        let opening = tokens
            .iter()
            .take(3)
            .map(|t| t.pos.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        paragraphs.last_mut().unwrap().push(SentenceStyle {
            opening,
            root_frame: head_frame(doc, root, &children[root.i]),
            root_morph: morphology(root),
            length,
        });
    }
    Ok((base, paragraphs))
}

fn order_families(paragraphs: &[Vec<String>], descriptor: &str) -> BTreeMap<String, Family> {
    let count: usize = paragraphs.iter().map(Vec::len).sum();
    let document_position = paragraphs
        .iter()
        .flatten()
        .enumerate()
        .map(|(i, signature)| format!("q{}|{signature}", quartile(i, count)));
    let paragraph_position = paragraphs.iter().flat_map(|paragraph| {
        paragraph
            .iter()
            .enumerate()
            .map(move |(i, signature)| format!("q{}|{signature}", quartile(i, paragraph.len())))
    });
    let transitions = paragraphs.iter().flat_map(|paragraph| {
        let mut edges = vec![format!("START => {}", paragraph[0])];
        edges.extend(
            paragraph
                .windows(2)
                .map(|pair| format!("{} => {}", pair[0], pair[1])),
        );
        edges.push(format!("{} => END", paragraph.last().unwrap()));
        edges
    });
    let name = |prefix: &str, suffix: &str| {
        if descriptor.is_empty() {
            format!("{prefix}sentence_{suffix}_v1")
        } else {
            format!("{prefix}{descriptor}_{suffix}_v2")
        }
    };
    BTreeMap::from([
        (
            name("document_", "position"),
            distribution(document_position),
        ),
        (
            name("paragraph_", "position"),
            distribution(paragraph_position),
        ),
        (name("", "transition"), distribution(transitions)),
    ])
}

/// A separate augmented identity for consumers that want occurrence and order.
pub fn with_occurrences(doc: &Document) -> Result<Features> {
    let order = extract(doc)?;
    let mut base = super::extract(doc)?;
    base.schema = order.schema;
    base.schema.push_str(";with-occurrences");
    base.families.extend(order.families);
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::Family;
    use crate::syntax::{Sentence, Token};

    fn fixture(sentences: &[(&str, &str)], separator: &str) -> Document {
        let mut doc = Document {
            text: String::new(),
            parser_identity: "order-fixture-v1".into(),
            tokens: vec![],
            sentences: vec![],
        };
        for (word, pos) in sentences {
            if !doc.text.is_empty() {
                doc.text.push_str(separator);
            }
            let start = doc.text.len();
            doc.text.push_str(word);
            let end = doc.text.len();
            doc.text.push('.');
            let root = doc.tokens.len();
            let sentence = doc.sentences.len();
            doc.tokens.push(Token {
                i: root,
                start_byte: start,
                end_byte: end,
                text: (*word).into(),
                lemma: word.to_lowercase(),
                pos: (*pos).into(),
                tag: (*pos).into(),
                dep: "ROOT".into(),
                head: root,
                sentence,
                morph: BTreeMap::new(),
                is_punct: false,
                is_space: false,
            });
            doc.tokens.push(Token {
                i: root + 1,
                start_byte: end,
                end_byte: end + 1,
                text: ".".into(),
                lemma: ".".into(),
                pos: "PUNCT".into(),
                tag: ".".into(),
                dep: "punct".into(),
                head: root,
                sentence,
                morph: BTreeMap::new(),
                is_punct: true,
                is_space: false,
            });
            doc.sentences.push(Sentence {
                start_byte: start,
                end_byte: end + 1,
                root,
                token_start: root,
                token_end: root + 2,
            });
        }
        doc
    }

    #[test]
    fn order_changes_when_all_default_occurrences_are_identical() {
        let a = fixture(
            &[("Wait", "VERB"), ("Ready", "ADJ"), ("Shelter", "NOUN")],
            " ",
        );
        let b = fixture(
            &[("Ready", "ADJ"), ("Wait", "VERB"), ("Shelter", "NOUN")],
            " ",
        );
        let original = super::super::extract(&a).unwrap();
        assert_eq!(
            original.families,
            super::super::extract(&b).unwrap().families
        );
        assert_ne!(extract(&a).unwrap().families, extract(&b).unwrap().families);
        assert_ne!(
            extract_factorized(&a).unwrap().families,
            extract_factorized(&b).unwrap().families
        );
        assert_ne!(
            extract(&a).unwrap().schema,
            extract_factorized(&a).unwrap().schema
        );
        assert_eq!(extract_factorized(&a).unwrap().families.len(), 12);
        let augmented = with_occurrences(&a).unwrap();
        for (key, family) in &original.families {
            assert_eq!(&augmented.families[key], family);
        }
        assert_ne!(augmented.schema, original.schema);
        assert_ne!(extract(&a).unwrap().schema, augmented.schema);
    }

    #[test]
    fn boundaries_and_empty_documents_have_declared_denominators() {
        let sentences = [("Wait", "VERB"), ("Ready", "ADJ"), ("Shelter", "NOUN")];
        for (separator, expected) in [(" ", 4), ("\n", 4), ("\r\n \t\r\n", 6)] {
            let f = extract(&fixture(&sentences, separator)).unwrap();
            let Family::Distribution {
                counts,
                opportunities,
            } = &f.families["sentence_transition_v1"]
            else {
                panic!()
            };
            assert_eq!(*opportunities, expected);
            assert_eq!(counts.values().sum::<u64>(), expected);
            for (name, family) in extract_factorized(&fixture(&sentences, separator))
                .unwrap()
                .families
            {
                let Family::Distribution {
                    counts,
                    opportunities,
                } = family
                else {
                    panic!()
                };
                assert_eq!(
                    opportunities,
                    if name.ends_with("transition_v2") {
                        expected
                    } else {
                        3
                    }
                );
                assert_eq!(counts.values().sum::<u64>(), opportunities);
            }
        }
        for family in extract(&fixture(&[], " ")).unwrap().families.values() {
            let Family::Distribution {
                counts,
                opportunities,
            } = family
            else {
                panic!()
            };
            assert!(counts.is_empty());
            assert_eq!(*opportunities, 0);
        }
    }
}
