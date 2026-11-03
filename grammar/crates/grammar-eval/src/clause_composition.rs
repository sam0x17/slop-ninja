//! A versioned clause-child projection alongside the unchanged v1 families.
use anyhow::{Context, Result, ensure};
use grammar_core::{
    features::{
        DefaultExtractor, FEATURE_VERSION as LEGACY_VERSION, Family, FeatureExtractor, Features,
    },
    syntax::{self, Document, Token},
};
use std::collections::BTreeMap;
use unicode_categories::UnicodeCategories;

pub const FAMILY: &str = "clause_child_backoff_v1";
pub const FEATURE_VERSION: &str = "grammar-features-v2;nfc-lowercase-letter-apostrophe-v1;alphabetic-parse-token-v1;syntax-families-v1;clause-child-backoff-v1";

#[derive(Clone, Copy, Debug, Default)]
pub struct CompositionalExtractor;

impl FeatureExtractor for CompositionalExtractor {
    fn identity(&self) -> &str {
        FEATURE_VERSION
    }

    fn extract(&self, doc: &Document) -> Result<Features> {
        let legacy = DefaultExtractor.extract(doc)?;
        let direct = document_family(doc)?;
        let mut extended = project_legacy(&legacy)?;
        ensure!(
            extended.families[FAMILY] == direct,
            "Document-derived clause slots disagree with the exact v1 frame projection"
        );
        extended.families.insert(FAMILY.into(), direct);
        Ok(extended)
    }
}

fn lexical(token: &Token) -> bool {
    !token.is_space && !token.is_punct && token.text.chars().any(|ch| ch.is_letter())
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

fn label(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && !value.chars().any(|c| matches!(c, '|' | '[' | ']' | ',')),
        "Empty or reserved delimiter in a syntax label"
    );
    Ok(())
}

fn pos(value: &str) -> Result<()> {
    ensure!(
        matches!(
            value,
            "ADJ"
                | "ADP"
                | "ADV"
                | "AUX"
                | "CCONJ"
                | "DET"
                | "INTJ"
                | "NOUN"
                | "NUM"
                | "PART"
                | "PRON"
                | "PROPN"
                | "PUNCT"
                | "SCONJ"
                | "SYM"
                | "VERB"
                | "X"
        ),
        "Unknown or ambiguous universal POS label"
    );
    Ok(())
}

fn count(counts: &mut BTreeMap<String, u64>, key: String, n: u64) -> Result<()> {
    let value = counts.entry(key).or_default();
    *value = value.checked_add(n).context("Clause slot count overflow")?;
    Ok(())
}

fn distribution(counts: BTreeMap<String, u64>) -> Result<Family> {
    let opportunities = counts
        .values()
        .try_fold(0_u64, |sum, n| sum.checked_add(*n))
        .context("Clause slot opportunity overflow")?;
    Ok(Family::Distribution {
        counts,
        opportunities,
    })
}

/// Each direct lexical child emits an event, preserving multiplicity. A head
/// with no lexical children emits one EMPTY event. The denominator counts these
/// events, not clause heads. No eligible heads means zero opportunities.
pub fn document_family(doc: &Document) -> Result<Family> {
    syntax::validate(doc)?;
    let mut children = vec![Vec::new(); doc.tokens.len()];
    for token in doc.tokens.iter().filter(|token| lexical(token)) {
        if token.head != token.i {
            children[token.head].push(token);
        }
    }
    let mut counts = BTreeMap::new();
    for head in doc
        .tokens
        .iter()
        .filter(|token| lexical(token) && clause_head(token))
    {
        let incoming = if head.head == head.i {
            "ROOT"
        } else {
            &head.dep
        };
        label(incoming)?;
        pos(&head.pos)?;
        if children[head.i].is_empty() {
            count(&mut counts, format!("{incoming}|{}|EMPTY", head.pos), 1)?;
        } else {
            for child in &children[head.i] {
                label(&child.dep)?;
                pos(&child.pos)?;
                let side = if child.i < head.i { "L" } else { "R" };
                count(
                    &mut counts,
                    format!("{incoming}|{}|{side}|{}:{}", head.pos, child.dep, child.pos),
                    1,
                )?;
            }
        }
    }
    distribution(counts)
}

fn encoded_events(key: &str) -> Result<Vec<String>> {
    let (prefix, lists) = key
        .split_once("|L[")
        .context("Malformed v1 frame: no left child list")?;
    let parts: Vec<&str> = prefix.split('|').collect();
    ensure!(
        parts.len() >= 4
            && parts[1] == parts[parts.len() - 1]
            && parts.iter().all(|p| !p.is_empty()),
        "Malformed v1 clause morphology/head prefix"
    );
    let incoming = parts[0];
    let head_pos = parts[1];
    label(incoming)?;
    pos(head_pos)?;
    let morph = &parts[2..parts.len() - 1];
    ensure!(
        morph == ["_"]
            || morph.iter().all(|value| value
                .split_once('=')
                .is_some_and(|(k, v)| !k.is_empty() && !v.is_empty())),
        "Malformed v1 morphology bundle"
    );
    let (left, right) = lists
        .split_once("]|R[")
        .context("Malformed v1 frame: no right child list")?;
    let right = right
        .strip_suffix(']')
        .context("Malformed v1 frame: missing closing bracket")?;
    let mut events = Vec::new();
    for (side, children) in [("L", left), ("R", right)] {
        if children.is_empty() {
            continue;
        }
        for child in children.split(',') {
            let (dep, child_pos) = child
                .rsplit_once(':')
                .context("Malformed v1 child relation/POS")?;
            label(dep)?;
            pos(child_pos)?;
            events.push(format!("{incoming}|{head_pos}|{side}|{dep}:{child_pos}"));
        }
    }
    if events.is_empty() {
        events.push(format!("{incoming}|{head_pos}|EMPTY"));
    }
    Ok(events)
}

/// Exact count projection for the declared legacy schema. Other schemas must
/// get their own adapter; callers must bind cached input bytes before using it.
pub fn project_legacy(legacy: &Features) -> Result<Features> {
    ensure!(
        legacy.schema == LEGACY_VERSION,
        "Unsupported legacy feature schema"
    );
    ensure!(
        !legacy.parser_identity.is_empty()
            && legacy.source_sha256.len() == 64
            && legacy.source_sha256.bytes().all(|c| c.is_ascii_hexdigit()),
        "Missing parser/source identity"
    );
    let expected = [
        "clause_head_frame",
        "dependency",
        "dependency_path_2",
        "dependency_path_3",
        "morphology_bundle",
        "ordered_head_frame",
        "pos",
        "pos_bigram",
        "pos_tag",
        "pos_trigram",
        "syntax_load",
        "syntax_sentence_load",
        "word",
        "word_bigram",
    ];
    ensure!(
        legacy.families.keys().map(String::as_str).eq(expected),
        "Expected exactly the fourteen legacy families"
    );
    let Some(Family::Distribution {
        counts: frames,
        opportunities,
    }) = legacy.families.get("clause_head_frame")
    else {
        anyhow::bail!("Missing legacy clause-frame distribution");
    };
    let total = frames
        .values()
        .try_fold(0_u64, |sum, n| sum.checked_add(*n))
        .context("Legacy frame count overflow")?;
    ensure!(
        total == *opportunities && frames.values().all(|n| *n > 0),
        "Legacy clause-frame denominator/count mismatch"
    );
    let mut counts = BTreeMap::new();
    for (key, n) in frames {
        for event in encoded_events(key)? {
            count(&mut counts, event, *n)?;
        }
    }
    let mut extended = legacy.clone();
    extended.schema = FEATURE_VERSION.into();
    extended
        .families
        .insert(FAMILY.into(), distribution(counts)?);
    Ok(extended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    fn fixture(items: &[(&str, &str, &str, usize)]) -> Document {
        let text = items.iter().map(|x| x.0).collect::<Vec<_>>().join(" ");
        let mut offset = 0;
        let tokens: Vec<Token> = items
            .iter()
            .enumerate()
            .map(|(i, (word, pos, dep, head))| {
                let start = offset;
                offset += word.len() + 1;
                Token {
                    i,
                    start_byte: start,
                    end_byte: start + word.len(),
                    text: (*word).into(),
                    lemma: word.to_lowercase(),
                    pos: (*pos).into(),
                    tag: (*pos).into(),
                    dep: (*dep).into(),
                    head: *head,
                    sentence: 0,
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect();
        let sentences = if items.is_empty() {
            vec![]
        } else {
            vec![Sentence {
                start_byte: 0,
                end_byte: text.len(),
                root: items.iter().enumerate().find(|(i, x)| *i == x.3).unwrap().0,
                token_start: 0,
                token_end: items.len(),
            }]
        };
        Document {
            text,
            parser_identity: "synthetic-annotation-v1".into(),
            tokens,
            sentences,
        }
    }
    fn family(doc: &Document) -> (BTreeMap<String, u64>, u64) {
        let extended = CompositionalExtractor.extract(doc).unwrap();
        match extended.families[FAMILY].clone() {
            Family::Distribution {
                counts,
                opportunities,
            } => (counts, opportunities),
            _ => panic!(),
        }
    }

    #[test]
    fn preserves_legacy_and_cache_projection_with_repeated_children() {
        let doc = fixture(&[
            ("We", "PRON", "nsubj", 2),
            ("might", "AUX", "aux", 2),
            ("leave", "VERB", "ROOT", 2),
            ("quietly", "ADV", "advmod", 2),
            ("tomorrow", "ADV", "advmod", 2),
        ]);
        let original = DefaultExtractor.extract(&doc).unwrap();
        let extended = CompositionalExtractor.extract(&doc).unwrap();
        for (name, value) in &original.families {
            assert_eq!(
                serde_json::to_vec(value).unwrap(),
                serde_json::to_vec(&extended.families[name]).unwrap()
            );
        }
        assert_eq!(extended, project_legacy(&original).unwrap());
        let (counts, n) = family(&doc);
        assert_eq!(n, 4);
        assert_eq!(counts["ROOT|VERB|R|advmod:ADV"], 2);
        assert_eq!(extended.families.len(), 15);
        assert_ne!(extended.schema, original.schema);
    }

    #[test]
    fn side_and_clause_relation_survive_but_within_side_order_backs_off() {
        let a = fixture(&[
            ("We", "PRON", "nsubj", 1),
            ("leave", "VERB", "ROOT", 1),
            ("quietly", "ADV", "advmod", 1),
            ("home", "NOUN", "dobj", 1),
        ]);
        let b = fixture(&[
            ("We", "PRON", "nsubj", 1),
            ("leave", "VERB", "ROOT", 1),
            ("home", "NOUN", "dobj", 1),
            ("quietly", "ADV", "advmod", 1),
        ]);
        assert_eq!(family(&a), family(&b));
        assert_ne!(
            DefaultExtractor.extract(&a).unwrap().families["clause_head_frame"],
            DefaultExtractor.extract(&b).unwrap().families["clause_head_frame"]
        );
        let c = fixture(&[
            ("quietly", "ADV", "advmod", 2),
            ("We", "PRON", "nsubj", 2),
            ("leave", "VERB", "ROOT", 2),
            ("home", "NOUN", "dobj", 2),
        ]);
        assert_ne!(family(&a), family(&c));
        let d = fixture(&[
            ("We", "PRON", "nsubj", 1),
            ("want", "VERB", "ROOT", 1),
            ("leave", "VERB", "xcomp", 1),
        ]);
        assert!(family(&d).0.contains_key("xcomp|VERB|EMPTY"));
    }

    #[test]
    fn empty_documents_punctuation_and_childless_fragments_are_distinct() {
        assert_eq!(family(&fixture(&[])), (BTreeMap::new(), 0));
        assert_eq!(
            family(&fixture(&[("!", "PUNCT", "ROOT", 0)])),
            (BTreeMap::new(), 0)
        );
        let (counts, n) = family(&fixture(&[
            ("Hello", "INTJ", "ROOT", 0),
            ("!", "PUNCT", "punct", 0),
        ]));
        assert_eq!(n, 1);
        assert_eq!(counts["ROOT|INTJ|EMPTY"], 1);
    }

    #[test]
    fn unicode_and_nonfinite_clause_variants_match_legacy() {
        let mut doc = fixture(&[
            ("Élan", "NOUN", "ROOT", 0),
            ("coming", "VERB", "acl:relcl", 0),
            ("cálmly", "ADV", "advmod", 1),
        ]);
        doc.tokens[1]
            .morph
            .insert("VerbForm".into(), vec!["Part".into()]);
        let extended = CompositionalExtractor.extract(&doc).unwrap();
        assert_eq!(
            extended,
            project_legacy(&DefaultExtractor.extract(&doc).unwrap()).unwrap()
        );
        assert_eq!(family(&doc).0["acl:relcl|VERB|R|advmod:ADV"], 1);
        // v1 considers U+02BC a letter even though the lexical word tokenizer
        // maps it to an apostrophe and drops a bare occurrence.
        assert_eq!(
            family(&fixture(&[("ʼ", "X", "ROOT", 0)])).0["ROOT|X|EMPTY"],
            1
        );
    }

    #[test]
    fn malformed_documents_frames_and_opportunities_are_rejected() {
        let doc = fixture(&[("Go", "VERB", "ROOT", 0)]);
        let base = DefaultExtractor.extract(&doc).unwrap();
        let mut bad = doc.clone();
        bad.tokens[0].head = 99;
        assert!(CompositionalExtractor.extract(&bad).is_err());
        let mut bad = doc.clone();
        bad.tokens[0].pos = "VERB|L[".into();
        assert!(CompositionalExtractor.extract(&bad).is_err());
        for key in [
            "ROOT|VERB|_|NOUN|L[]|R[]",
            "ROOT|VERB|_|VERB|L[x]|R[]",
            "ROOT|VERB|_|VERB|L[]|R[aux:AUX]junk",
        ] {
            let mut bad = base.clone();
            bad.families.insert(
                "clause_head_frame".into(),
                Family::Distribution {
                    counts: BTreeMap::from([(key.into(), 1)]),
                    opportunities: 1,
                },
            );
            assert!(project_legacy(&bad).is_err());
        }
        let mut bad = base.clone();
        bad.schema = "other-v1".into();
        assert!(project_legacy(&bad).is_err());
        let mut bad = base;
        bad.families.insert(
            "clause_head_frame".into(),
            Family::Distribution {
                counts: BTreeMap::new(),
                opportunities: 1,
            },
        );
        assert!(project_legacy(&bad).is_err());
    }

    #[test]
    fn installed_spacy_preserves_legacy_families_and_matches_cache_projection() {
        let python = std::env::var_os("GRAMMAR_SPACY_PYTHON")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.venv/bin/python")
            });
        if !python.exists() {
            eprintln!("spaCy integration skipped: install local .venv or set GRAMMAR_SPACY_PYTHON");
            return;
        }
        let texts = [
            "We might not leave quietly tomorrow.",
            "The team that arrived early can stay.",
            "Hello!",
            "We have been waiting, but they did not return.",
        ];
        let documents = grammar_spacy::parse_batch(&texts, python.to_str().unwrap()).unwrap();
        for doc in documents {
            let doc = doc.unwrap();
            let legacy = DefaultExtractor.extract(&doc).unwrap();
            let extended = CompositionalExtractor.extract(&doc).unwrap();
            assert_eq!(extended, project_legacy(&legacy).unwrap());
            assert_eq!(extended.parser_identity, doc.parser_identity);
            assert!(
                extended
                    .parser_identity
                    .starts_with("grammar-spacy-annotation-v1;spacy=")
            );
            for (name, family) in legacy.families {
                assert_eq!(
                    serde_json::to_vec(&family).unwrap(),
                    serde_json::to_vec(&extended.families[&name]).unwrap()
                );
            }
        }
    }
}
