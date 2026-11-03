//! One lemma-bearing syntactic-role event per eligible function-word token.
//! This family adds lexical/role coupling; it is not a semantic-fidelity test.
use anyhow::{Context, Result};
use grammar_core::{
    features::Family,
    syntax::{self, Document},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::lexical_context::{lexical, normalized_lemma};

pub const FAMILY: &str = "function_word_role_v1";
pub const DEFINITION: &str = "One event per token satisfying the original Unicode-letter/nonspace/nonpunct predicate and UPOS ADP/AUX/CCONJ/DET/PART/PRON/SCONJ or exact dependency neg. JSON tuple [lemma_or_null, own_POS, incoming_dependency, governing_head_POS, L/R/SELF]. Lemma NFC then Unicode lowercase then NFC; empty or whitespace-only lemma is null with no surface fallback. Incoming labels remain unchanged. Side uses original token/head indices; self-head uses own POS and SELF. Denominator counts all eligible tokens, including missing-lemma events; no eligible tokens means zero opportunities/unavailable.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    L,
    R,
    #[serde(rename = "SELF")]
    SelfHead,
}

/// Token indices are audit metadata and do not enter the feature key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub token_index: usize,
    pub head_index: usize,
    pub token_byte_span: [usize; 2],
    pub head_byte_span: [usize; 2],
    pub lemma: Option<String>,
    pub own_pos: String,
    pub incoming_relation: String,
    pub head_pos: String,
    pub side: Side,
}

impl Event {
    /// A JSON tuple preserves field boundaries, escapes arbitrary labels and
    /// distinguishes a missing lemma from the literal string "null".
    pub fn key(&self) -> Result<String> {
        Ok(serde_json::to_string(&(
            &self.lemma,
            &self.own_pos,
            &self.incoming_relation,
            &self.head_pos,
            self.side,
        ))?)
    }
}

/// Validate the unchanged syntax document before trusting heads or positions.
pub fn document_events(doc: &Document) -> Result<Vec<Event>> {
    syntax::validate(doc)?;
    let mut events = Vec::new();
    for token in &doc.tokens {
        if !lexical(token)
            || !(matches!(
                token.pos.as_str(),
                "ADP" | "AUX" | "CCONJ" | "DET" | "PART" | "PRON" | "SCONJ"
            ) || token.dep == "neg")
        {
            continue;
        }
        let side = match token.i.cmp(&token.head) {
            std::cmp::Ordering::Less => Side::L,
            std::cmp::Ordering::Greater => Side::R,
            std::cmp::Ordering::Equal => Side::SelfHead,
        };
        let head = &doc.tokens[token.head];
        events.push(Event {
            token_index: token.i,
            head_index: head.i,
            token_byte_span: [token.start_byte, token.end_byte],
            head_byte_span: [head.start_byte, head.end_byte],
            lemma: normalized_lemma(token),
            own_pos: token.pos.clone(),
            incoming_relation: token.dep.clone(),
            head_pos: head.pos.clone(),
            side,
        });
    }
    Ok(events)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extraction {
    pub family: Family,
    pub missing_lemma_events: u64,
    pub events: Vec<Event>,
}

/// Optional traces retain token/head byte spans outside the author-style key.
pub fn extract(doc: &Document) -> Result<Extraction> {
    let mut counts = BTreeMap::<String, u64>::new();
    let mut opportunities = 0u64;
    let events = document_events(doc)?;
    let mut missing_lemma_events = 0u64;
    for event in &events {
        let value = counts.entry(event.key()?).or_default();
        *value = value
            .checked_add(1)
            .context("function-word role count overflow")?;
        opportunities = opportunities
            .checked_add(1)
            .context("function-word role denominator overflow")?;
        if event.lemma.is_none() {
            missing_lemma_events = missing_lemma_events
                .checked_add(1)
                .context("missing lemma count overflow")?;
        }
    }
    Ok(Extraction {
        family: Family::Distribution {
            counts,
            opportunities,
        },
        missing_lemma_events,
        events,
    })
}

pub fn document_family(doc: &Document) -> Result<Family> {
    Ok(extract(doc)?.family)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::{Sentence, Token};
    fn fixture(items: &[(&str, &str, &str, usize)]) -> Document {
        let text = items.iter().map(|x| x.0).collect::<Vec<_>>().join(" ");
        let mut offset = 0;
        let tokens: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, (word, pos, dep, head))| {
                let start = offset;
                offset += word.len() + 1;
                Token {
                    i,
                    start_byte: start,
                    end_byte: start + word.len(),
                    text: word.to_string(),
                    lemma: word.to_lowercase(),
                    pos: pos.to_string(),
                    tag: pos.to_string(),
                    dep: dep.to_string(),
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
                start_byte: 0,
                end_byte: text.len(),
                root: tokens.iter().find(|t| t.i == t.head).unwrap().i,
                token_start: 0,
                token_end: tokens.len(),
            }]
        };
        Document {
            text,
            parser_identity: "synthetic-function-roles-v1".into(),
            tokens,
            sentences,
        }
    }
    fn family(doc: &Document) -> (BTreeMap<String, u64>, u64) {
        let Family::Distribution {
            counts,
            opportunities,
        } = document_family(doc).unwrap()
        else {
            panic!("expected categorical family")
        };
        (counts, opportunities)
    }
    #[test]
    fn all_function_tags_and_negation_emit_once_with_exact_denominator() {
        let doc = fixture(&[
            ("leave", "VERB", "ROOT", 0),
            ("in", "ADP", "prep", 0),
            ("might", "AUX", "aux", 0),
            ("and", "CCONJ", "cc", 0),
            ("the", "DET", "det", 0),
            ("to", "PART", "aux", 0),
            ("we", "PRON", "nsubj", 0),
            ("because", "SCONJ", "mark", 0),
            ("not", "ADV", "neg", 0),
            ("softly", "ADV", "advmod", 0),
        ]);
        let (counts, n) = family(&doc);
        assert_eq!(n, 8);
        assert_eq!(counts.values().sum::<u64>(), n);
        let events = document_events(&doc).unwrap();
        assert_eq!(
            events.iter().map(|e| e.token_index).collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .all(|e| e.head_pos == "VERB" && e.side == Side::R)
        );
        let twice = fixture(&[("can", "AUX", "ROOT", 0), ("not", "PART", "neg", 0)]);
        assert_eq!(family(&twice).1, 2);
    }
    #[test]
    fn roles_head_pos_and_position_enter_the_key_without_head_lemmas() {
        let doc = fixture(&[
            ("We", "PRON", "nsubj", 1),
            ("see", "VERB", "ROOT", 1),
            ("them", "PRON", "dobj", 1),
        ]);
        let events = document_events(&doc).unwrap();
        assert_eq!(
            events[0].key().unwrap(),
            r#"["we","PRON","nsubj","VERB","L"]"#
        );
        assert_eq!(
            events[1].key().unwrap(),
            r#"["them","PRON","dobj","VERB","R"]"#
        );
        let mut changed = doc.clone();
        changed.tokens[1].lemma = "notice".into();
        assert_eq!(family(&doc), family(&changed));
        changed.tokens[1].pos = "NOUN".into();
        assert_ne!(family(&doc), family(&changed));
        let root = fixture(&[("We", "PRON", "ROOT", 0)]);
        let event = &document_events(&root).unwrap()[0];
        assert_eq!(event.side, Side::SelfHead);
        assert_eq!(event.head_pos, "PRON");
        assert_eq!(event.incoming_relation, "ROOT");
        assert_eq!(
            event.key().unwrap(),
            r#"["we","PRON","ROOT","PRON","SELF"]"#
        );
        assert_eq!(event.token_byte_span, [0, 2]);
        assert_eq!(event.head_byte_span, [0, 2]);
    }
    #[test]
    fn occurrence_multiplicity_and_missing_lemmas_are_observed_events() {
        let mut doc = fixture(&[
            ("do", "VERB", "ROOT", 0),
            ("it", "PRON", "dobj", 0),
            ("it", "PRON", "dobj", 0),
        ]);
        let (counts, n) = family(&doc);
        assert_eq!(n, 2);
        assert_eq!(counts.len(), 1);
        assert_eq!(*counts.values().next().unwrap(), 2);
        doc.tokens[1].lemma.clear();
        doc.tokens[2].lemma = " \t\n".into();
        let (counts, n) = family(&doc);
        assert_eq!(n, 2);
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[r#"[null,"PRON","dobj","VERB","R"]"#], 2);
        assert_eq!(extract(&doc).unwrap().missing_lemma_events, 2);
        doc.tokens[2].lemma = "null".into();
        assert_eq!(family(&doc).0.len(), 2);
    }
    #[test]
    fn key_encoding_is_collision_free_for_delimiters_quotes_and_missing_values() {
        let mut event = Event {
            token_index: 0,
            head_index: 1,
            token_byte_span: [0, 1],
            head_byte_span: [2, 3],
            lemma: Some("a|b,\"c\"".into()),
            own_pos: "PRON".into(),
            incoming_relation: "nsubj|x".into(),
            head_pos: "VERB".into(),
            side: Side::L,
        };
        let key = event.key().unwrap();
        event.token_index = 10;
        event.head_index = 11;
        event.token_byte_span = [100, 101];
        event.head_byte_span = [102, 103];
        assert_eq!(event.key().unwrap(), key);
        let tuple: (Option<String>, String, String, String, Side) =
            serde_json::from_str(&key).unwrap();
        assert_eq!(
            tuple,
            (
                event.lemma.clone(),
                event.own_pos.clone(),
                event.incoming_relation.clone(),
                event.head_pos.clone(),
                event.side
            )
        );
        event.lemma = None;
        let missing = event.key().unwrap();
        event.lemma = Some("null".into());
        assert_ne!(missing, event.key().unwrap());
    }
    #[test]
    fn empty_and_nonfunction_documents_remain_unavailable() {
        for doc in [
            fixture(&[]),
            fixture(&[("Birds", "NOUN", "nsubj", 1), ("fly", "VERB", "ROOT", 1)]),
            fixture(&[("!", "PUNCT", "ROOT", 0)]),
        ] {
            assert_eq!(family(&doc), (BTreeMap::new(), 0));
        }
        let mut token = fixture(&[("123", "PRON", "ROOT", 0)]);
        assert_eq!(family(&token).1, 0);
        token = fixture(&[("word", "PRON", "ROOT", 0)]);
        token.tokens[0].is_punct = true;
        assert_eq!(family(&token).1, 0);
    }
    #[test]
    fn malformed_heads_fail_before_any_event_is_returned() {
        let mut doc = fixture(&[("We", "PRON", "nsubj", 1), ("go", "VERB", "ROOT", 1)]);
        doc.tokens[0].head = 99;
        assert!(document_family(&doc).is_err());
        let mut doc = fixture(&[("We", "PRON", "nsubj", 1), ("go", "VERB", "ROOT", 1)]);
        doc.tokens[0].pos.clear();
        assert!(document_events(&doc).is_err());
    }
    #[test]
    fn installed_spacy_negation_and_function_roles_preserve_original_features() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let python = std::env::var_os("GRAMMAR_SPACY_PYTHON")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                ["../../../.venv/bin/python", "../../.venv/bin/python"]
                    .iter()
                    .map(|p| root.join(p))
                    .find(|p| p.exists())
            });
        let Some(python) = python else {
            eprintln!("spaCy integration unavailable; set GRAMMAR_SPACY_PYTHON");
            return;
        };
        let texts = [
            "We cannot leave because they are waiting for us.",
            "The book on the desk is mine.",
            "Birds fly.",
        ];
        for (text, parsed) in texts
            .iter()
            .zip(grammar_spacy::parse_batch(&texts, python.to_str().unwrap()).unwrap())
        {
            let doc = parsed.unwrap();
            assert_eq!(doc.text, *text);
            let old = grammar_core::features::extract(&doc).unwrap();
            let events = document_events(&doc).unwrap();
            let (_, n) = family(&doc);
            assert_eq!(n, events.len() as u64);
            assert_eq!(old, grammar_core::features::extract(&doc).unwrap());
            assert_eq!(old.families.len(), 14);
            for event in &events {
                let token = &doc.tokens[event.token_index];
                assert_eq!(event.lemma, normalized_lemma(token));
                assert_eq!(event.incoming_relation, token.dep);
            }
            if text.contains("cannot") {
                assert!(events.iter().any(|e| e.incoming_relation == "neg"));
                assert!(events.iter().any(|e| e.own_pos == "PRON"));
            }
            if *text == "Birds fly." {
                assert_eq!(n, 0);
            }
        }
    }
}
