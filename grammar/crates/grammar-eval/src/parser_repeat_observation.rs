//! Occurrence-aligned diagnostics for repeated annotations of identical text.
//!
//! Equality hashes serialize the typed Document with serde_json; they do not
//! represent the bytes or formatting of an input annotation wrapper. Differences
//! describe supplied annotations without attributing their cause to an edit.

use crate::lexical_context::{lexical, normalized_lemma};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    features,
    syntax::{self, Document, Sentence, Token},
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-parser-repeat-observation-v1";

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn serialized<T: Serialize>(value: &T) -> Result<(Vec<u8>, String)> {
    let bytes = serde_json::to_vec(value)?;
    let hash = digest(&bytes);
    Ok((bytes, hash))
}

fn span(token: &Token) -> (usize, usize) {
    (token.start_byte, token.end_byte)
}

fn sentence_span(sentence: &Sentence) -> (usize, usize) {
    (sentence.start_byte, sentence.end_byte)
}

fn occurrence(token: &Token) -> Value {
    json!({"span":[token.start_byte,token.end_byte],"text":token.text})
}

fn sentence_evidence(doc: &Document, index: usize) -> Value {
    let sentence = &doc.sentences[index];
    json!({"index":index,"sentence":sentence,"root_occurrence":occurrence(&doc.tokens[sentence.root])})
}

/// Validate both annotations, then compare occurrences identified by exact byte
/// span and text. Changed tokenizations remain unmatched; surface words alone
/// are never used to align repeated occurrences or dependency heads.
pub fn compare(reference: &Document, observed: &Document) -> Result<Value> {
    syntax::validate(reference).context("Invalid reference annotation")?;
    syntax::validate(observed).context("Invalid observed annotation")?;
    ensure!(
        reference.text == observed.text,
        "Repeat comparison requires identical text bytes"
    );
    ensure!(
        reference.parser_identity == observed.parser_identity,
        "Repeat comparison requires identical parser identity"
    );
    let (reference_bytes, reference_sha) = serialized(reference)?;
    let (observed_bytes, observed_sha) = serialized(observed)?;
    let observed_by_span: BTreeMap<_, _> = observed.tokens.iter().map(|t| (span(t), t)).collect();
    let reference_spans: BTreeSet<_> = reference.tokens.iter().map(span).collect();
    let mut alignment = Vec::new();
    let mut reference_only = Vec::new();
    let mut annotations = Vec::new();
    let mut attachments = Vec::new();
    let mut positions = Vec::new();
    let mut memberships = Vec::new();
    let mut field_counts = BTreeMap::<&str, usize>::new();
    for r in &reference.tokens {
        let Some(&o) = observed_by_span.get(&span(r)) else {
            reference_only.push(r);
            continue;
        };
        // Validated spans in identical text must name identical source bytes.
        ensure!(r.text == o.text, "Matched occurrence text differs");
        alignment
            .push(json!({"occurrence":occurrence(r),"reference_index":r.i,"observed_index":o.i}));
        let r_lemma = normalized_lemma(r);
        let o_lemma = normalized_lemma(o);
        let comparisons = [
            ("pos", r.pos != o.pos),
            ("tag", r.tag != o.tag),
            ("raw_lemma", r.lemma != o.lemma),
            ("normalized_lemma", r_lemma != o_lemma),
            ("morphology", r.morph != o.morph),
            ("is_punct", r.is_punct != o.is_punct),
            ("is_space", r.is_space != o.is_space),
            ("lexical_eligibility", lexical(r) != lexical(o)),
        ];
        let fields: Vec<_> = comparisons
            .into_iter()
            .filter_map(|(field, changed)| changed.then_some(field))
            .collect();
        if !fields.is_empty() {
            for field in &fields {
                *field_counts.entry(field).or_default() += 1;
            }
            let morph_keys: BTreeSet<_> = r.morph.keys().chain(o.morph.keys()).collect();
            let morphology: Vec<_> = morph_keys.into_iter().filter(|key| r.morph.get(*key) != o.morph.get(*key))
                .map(|key| json!({"key":key,"reference":r.morph.get(key),"observed":o.morph.get(key)})).collect();
            annotations.push(json!({"occurrence":occurrence(r),"changed_fields":fields,
                "reference_token":r,"observed_token":o,
                "normalized_lemmas":{"reference":r_lemma,"observed":o_lemma},"morphology_differences":morphology}));
        }
        let rh = &reference.tokens[r.head];
        let oh = &observed.tokens[o.head];
        let head_changed = span(rh) != span(oh) || rh.text != oh.text;
        if r.dep != o.dep || head_changed {
            attachments.push(
                json!({"occurrence":occurrence(r),"reference_token":r,"observed_token":o,
                "dependency_changed":r.dep!=o.dep,"head_occurrence_changed":head_changed,
                "reference_head":occurrence(rh),"observed_head":occurrence(oh)}),
            );
        }
        let rs = &reference.sentences[r.sentence];
        let os = &observed.sentences[o.sentence];
        let membership_changed = sentence_span(rs) != sentence_span(os);
        if membership_changed {
            memberships.push(json!({"occurrence":occurrence(r),
                "reference_sentence":sentence_evidence(reference,r.sentence),"observed_sentence":sentence_evidence(observed,o.sentence)}));
        }
        if r.i != o.i || r.head != o.head || r.sentence != o.sentence {
            positions.push(json!({"occurrence":occurrence(r),
                "reference":{"index":r.i,"head_index":r.head,"sentence_index":r.sentence},
                "observed":{"index":o.i,"head_index":o.head,"sentence_index":o.sentence},
                "token_index_changed":r.i!=o.i,"head_index_changed":r.head!=o.head,"sentence_index_changed":r.sentence!=o.sentence,
                "head_index_changed_without_head_occurrence_change":r.head!=o.head && !head_changed,
                "sentence_index_changed_without_sentence_span_change":r.sentence!=o.sentence && !membership_changed}));
        }
    }
    let observed_only: Vec<_> = observed
        .tokens
        .iter()
        .filter(|t| !reference_spans.contains(&span(t)))
        .collect();
    let observed_sentences: BTreeMap<_, _> = observed
        .sentences
        .iter()
        .enumerate()
        .map(|(i, s)| (sentence_span(s), i))
        .collect();
    let reference_sentence_spans: BTreeSet<_> =
        reference.sentences.iter().map(sentence_span).collect();
    let mut matched_sentence_differences = Vec::new();
    let mut reference_only_sentences = Vec::new();
    for (i, r) in reference.sentences.iter().enumerate() {
        if let Some(&j) = observed_sentences.get(&sentence_span(r)) {
            let o = &observed.sentences[j];
            let root_changed =
                occurrence(&reference.tokens[r.root]) != occurrence(&observed.tokens[o.root]);
            if r != o || i != j {
                matched_sentence_differences.push(json!({"reference":sentence_evidence(reference,i),
                    "observed":sentence_evidence(observed,j),"root_occurrence_changed":root_changed,
                    "index_fields_changed":i!=j || r.root!=o.root || r.token_start!=o.token_start || r.token_end!=o.token_end}));
            }
        } else {
            reference_only_sentences.push(sentence_evidence(reference, i));
        }
    }
    let observed_only_sentences: Vec<_> = observed
        .sentences
        .iter()
        .enumerate()
        .filter(|(_, s)| !reference_sentence_spans.contains(&sentence_span(s)))
        .map(|(i, _)| sentence_evidence(observed, i))
        .collect();
    let exact_equal = reference_bytes == observed_bytes;
    let (feature_comparison, changed_family_count) = if exact_equal {
        (
            json!({"schema":features::FEATURE_VERSION,"status":"inferred_equal_from_exact_document","measured":false,"exact_equal":true,"changed_families":[]}),
            0,
        )
    } else {
        let rf = features::extract(reference).context("Reference default feature extraction")?;
        let of = features::extract(observed).context("Observed default feature extraction")?;
        let (rf_bytes, rf_sha) = serialized(&rf)?;
        let (of_bytes, of_sha) = serialized(&of)?;
        let families: BTreeSet<_> = rf.families.keys().chain(of.families.keys()).collect();
        let changed_families: Vec<_> = families.into_iter().filter(|name|rf.families.get(*name)!=of.families.get(*name))
            .map(|name|json!({"family":name,"reference":rf.families.get(name),"observed":of.families.get(name)})).collect();
        let count = changed_families.len();
        (
            json!({"schema":features::FEATURE_VERSION,"status":"measured","measured":true,"exact_equal":rf_bytes==of_bytes,"reference_sha256":rf_sha,"observed_sha256":of_sha,"changed_families":changed_families}),
            count,
        )
    };
    Ok(json!({
        "schema":SCHEMA,"parser_identity":reference.parser_identity,"source_sha256":digest(reference.text.as_bytes()),
        "exact_document_equal":exact_equal,
        "document_serialization":"serde_json::to_vec on typed grammar_core::syntax::Document; struct field order and BTreeMap keys; not original wrapper bytes",
        "reference_document_sha256":reference_sha,"observed_document_sha256":observed_sha,
        "tokenization_equal":reference_only.is_empty() && observed_only.is_empty(),
        "sentence_segmentation_equal":reference_only_sentences.is_empty() && observed_only_sentences.is_empty(),
        "counts":{"reference_tokens":reference.tokens.len(),"observed_tokens":observed.tokens.len(),
            "matched_token_occurrences":alignment.len(),"reference_only_token_occurrences":reference_only.len(),"observed_only_token_occurrences":observed_only.len(),
            "token_annotation_differences":annotations.len(),"token_field_differences":field_counts,
            "attachment_differences":attachments.len(),"positional_index_differences":positions.len(),"sentence_membership_differences":memberships.len(),
            "reference_sentences":reference.sentences.len(),"observed_sentences":observed.sentences.len(),
            "reference_only_sentence_spans":reference_only_sentences.len(),"observed_only_sentence_spans":observed_only_sentences.len(),
            "matched_sentence_record_differences":matched_sentence_differences.len(),"changed_feature_families":changed_family_count},
        "token_occurrence_alignment":alignment,"reference_only_tokens":reference_only,"observed_only_tokens":observed_only,
        "token_annotation_differences":annotations,"attachment_differences":attachments,
        "positional_index_differences":positions,"sentence_membership_differences":memberships,
        "sentence_differences":{"reference_only":reference_only_sentences,"observed_only":observed_only_sentences,"matched_span_differences":matched_sentence_differences},
        "default_features":feature_comparison,
        "interpretation":{"cause_assigned":false,"semantic_equivalence_certified":false,"automatic_edit_license":false,
            "unmatched_occurrence_fields_compared":false,"missing_morphology_equals_supplied_value":false,
            "counts_note":"Field, token, attachment and sentence counts overlap; sentence index changes are separate from byte-span segmentation changes"}
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(text: &str, items: &[(&str, &str, &str, usize)]) -> Document {
        let mut cursor = 0;
        let tokens: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, (word, pos, dep, head))| {
                let start_byte = cursor + text[cursor..].find(word).unwrap();
                let end_byte = start_byte + word.len();
                cursor = end_byte;
                Token {
                    i,
                    start_byte,
                    end_byte,
                    text: (*word).into(),
                    lemma: word.to_lowercase(),
                    pos: (*pos).into(),
                    tag: (*pos).into(),
                    dep: (*dep).into(),
                    head: *head,
                    sentence: 0,
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: *pos == "SPACE",
                }
            })
            .collect();
        let sentences = if tokens.is_empty() {
            vec![]
        } else {
            vec![Sentence {
                start_byte: tokens[0].start_byte,
                end_byte: tokens.last().unwrap().end_byte,
                root: tokens.iter().find(|t| t.head == t.i).unwrap().i,
                token_start: 0,
                token_end: tokens.len(),
            }]
        };
        let doc = Document {
            text: text.into(),
            parser_identity: "synthetic-repeat-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&doc).unwrap();
        doc
    }

    fn basic() -> Document {
        fixture(
            "Mira sees Mira.",
            &[
                ("Mira", "PROPN", "nsubj", 1),
                ("sees", "VERB", "ROOT", 1),
                ("Mira", "PROPN", "dobj", 1),
                (".", "PUNCT", "punct", 1),
            ],
        )
    }

    #[test]
    fn exact_document_and_features_have_equal_hashes() {
        let d = basic();
        let r = compare(&d, &d).unwrap();
        assert_eq!(r["exact_document_equal"], true);
        assert_eq!(
            r["reference_document_sha256"],
            r["observed_document_sha256"]
        );
        assert_eq!(r["counts"]["matched_token_occurrences"], 4);
        assert_eq!(r["counts"]["token_annotation_differences"], 0);
        assert_eq!(r["default_features"]["exact_equal"], true);
        assert_eq!(r["default_features"]["measured"], false);
        assert_eq!(
            r["default_features"]["status"],
            "inferred_equal_from_exact_document"
        );
        assert!(r["default_features"].get("reference_sha256").is_none());
    }

    #[test]
    fn missing_morphology_is_distinct_from_present_and_conflicting_values() {
        let a = basic();
        let mut b = a.clone();
        b.tokens[2].morph.insert("Case".into(), vec!["Acc".into()]);
        let r = compare(&a, &b).unwrap();
        assert_eq!(r["counts"]["token_annotation_differences"], 1);
        assert_eq!(
            r["token_annotation_differences"][0]["morphology_differences"][0],
            json!({"key":"Case","reference":null,"observed":["Acc"]})
        );
        let mut c = b.clone();
        c.tokens[2].morph.insert("Case".into(), vec!["Nom".into()]);
        let r = compare(&b, &c).unwrap();
        assert_eq!(
            r["token_annotation_differences"][0]["morphology_differences"][0]["reference"],
            json!(["Acc"])
        );
        assert_eq!(
            r["token_annotation_differences"][0]["morphology_differences"][0]["observed"],
            json!(["Nom"])
        );
    }

    #[test]
    fn raw_lemma_normalization_and_missingness_remain_separate() {
        let mut a = basic();
        let mut b = a.clone();
        a.tokens[0].lemma = "E\u{301}LAN".into();
        b.tokens[0].lemma = "élan".into();
        let r = compare(&a, &b).unwrap();
        assert_eq!(
            r["token_annotation_differences"][0]["changed_fields"],
            json!(["raw_lemma"])
        );
        b.tokens[0].lemma.clear();
        let r = compare(&a, &b).unwrap();
        assert_eq!(
            r["token_annotation_differences"][0]["normalized_lemmas"]["observed"],
            Value::Null
        );
        assert_eq!(
            r["counts"]["token_field_differences"]["normalized_lemma"],
            1
        );
    }

    #[test]
    fn repeated_surface_heads_are_compared_by_occurrence() {
        let a = fixture(
            "Mira sees Mira and Mira.",
            &[
                ("Mira", "PROPN", "nsubj", 1),
                ("sees", "VERB", "ROOT", 1),
                ("Mira", "PROPN", "dobj", 1),
                ("and", "CCONJ", "cc", 2),
                ("Mira", "PROPN", "conj", 2),
                (".", "PUNCT", "punct", 1),
            ],
        );
        let mut b = a.clone();
        b.tokens[4].head = 0;
        let r = compare(&a, &b).unwrap();
        assert_eq!(r["counts"]["attachment_differences"], 1);
        let e = &r["attachment_differences"][0];
        assert_eq!(e["head_occurrence_changed"], true);
        assert_eq!(e["reference_head"]["text"], e["observed_head"]["text"]);
        assert_ne!(e["reference_head"]["span"], e["observed_head"]["span"]);
    }

    #[test]
    fn tokenization_shifts_indices_without_changing_mapped_heads() {
        let a = fixture(
            "New York runs.",
            &[
                ("New York", "PROPN", "nsubj", 1),
                ("runs", "VERB", "ROOT", 1),
                (".", "PUNCT", "punct", 1),
            ],
        );
        let b = fixture(
            "New York runs.",
            &[
                ("New", "PROPN", "compound", 1),
                ("York", "PROPN", "nsubj", 2),
                ("runs", "VERB", "ROOT", 2),
                (".", "PUNCT", "punct", 2),
            ],
        );
        let r = compare(&a, &b).unwrap();
        assert_eq!(r["tokenization_equal"], false);
        assert_eq!(r["counts"]["matched_token_occurrences"], 2);
        assert_eq!(r["counts"]["reference_only_token_occurrences"], 1);
        assert_eq!(r["counts"]["observed_only_token_occurrences"], 2);
        assert_eq!(r["counts"]["attachment_differences"], 0);
        assert_eq!(r["sentence_segmentation_equal"], true);
        assert!(
            r["positional_index_differences"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["head_index_changed_without_head_occurrence_change"] == true)
        );
    }

    #[test]
    fn sentence_span_changes_are_distinct_from_later_index_offsets() {
        let a = fixture(
            "A runs. B runs. C runs.",
            &[
                ("A", "PROPN", "nsubj", 1),
                ("runs", "VERB", "ROOT", 1),
                (".", "PUNCT", "punct", 1),
                ("B", "PROPN", "nsubj", 4),
                ("runs", "VERB", "conj", 1),
                (".", "PUNCT", "punct", 4),
                ("C", "PROPN", "nsubj", 7),
                ("runs", "VERB", "conj", 1),
                (".", "PUNCT", "punct", 7),
            ],
        );
        let mut a = a;
        a.tokens[7].dep = "ROOT".into();
        a.tokens[7].head = 7;
        for t in &mut a.tokens[6..] {
            t.sentence = 1;
        }
        a.sentences = vec![
            Sentence {
                start_byte: 0,
                end_byte: 15,
                root: 1,
                token_start: 0,
                token_end: 6,
            },
            Sentence {
                start_byte: 16,
                end_byte: 23,
                root: 7,
                token_start: 6,
                token_end: 9,
            },
        ];
        let mut b = a.clone();
        b.tokens[4].head = 4;
        b.tokens[4].dep = "ROOT".into();
        for t in &mut b.tokens[3..6] {
            t.sentence = 1;
        }
        for t in &mut b.tokens[6..] {
            t.sentence = 2;
        }
        b.sentences = vec![
            Sentence {
                start_byte: 0,
                end_byte: 7,
                root: 1,
                token_start: 0,
                token_end: 3,
            },
            Sentence {
                start_byte: 8,
                end_byte: 15,
                root: 4,
                token_start: 3,
                token_end: 6,
            },
            Sentence {
                start_byte: 16,
                end_byte: 23,
                root: 7,
                token_start: 6,
                token_end: 9,
            },
        ];
        let r = compare(&a, &b).unwrap();
        assert_eq!(r["sentence_segmentation_equal"], false);
        assert_eq!(r["counts"]["sentence_membership_differences"], 6);
        assert_eq!(
            r["positional_index_differences"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["sentence_index_changed_without_sentence_span_change"] == true)
                .count(),
            3
        );
        assert_eq!(r["counts"]["attachment_differences"], 1);
    }

    #[test]
    fn changed_text_parser_and_invalid_annotations_are_rejected() {
        let a = basic();
        let mut b = a.clone();
        b.text.push(' ');
        assert!(compare(&a, &b).is_err());
        b = a.clone();
        b.parser_identity = "another-model".into();
        assert!(compare(&a, &b).is_err());
        b = a.clone();
        b.tokens[0].start_byte = 1;
        assert!(compare(&a, &b).is_err());
    }

    #[test]
    fn empty_valid_annotations_and_unchanged_words_with_changed_syntax() {
        let a = fixture("   ", &[]);
        assert_eq!(compare(&a, &a).unwrap()["exact_document_equal"], true);
        let a = basic();
        let mut b = a.clone();
        b.tokens[0].pos = "NOUN".into();
        let r = compare(&a, &b).unwrap();
        assert_eq!(r["exact_document_equal"], false);
        let changed = r["default_features"]["changed_families"]
            .as_array()
            .unwrap();
        assert!(!changed.is_empty());
        assert!(
            !changed
                .iter()
                .any(|f| f["family"] == "word" || f["family"] == "word_bigram")
        );
    }
}
