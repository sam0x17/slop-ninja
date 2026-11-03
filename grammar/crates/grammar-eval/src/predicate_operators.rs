//! Versioned, predicate-local operator observations over validated parser output.
//!
//! One event is emitted per lexical v1 clause head. Operator paths describe
//! dependency attachment, not semantic scope. JSON keys preserve field and list
//! boundaries even when parser labels or lemmas contain punctuation.

use crate::lexical_context::{lexical, normalized_lemma};
use anyhow::{Context, Result};
use grammar_core::{
    features::{DefaultExtractor, Family, FeatureExtractor, Features},
    syntax::{self, Document, Token},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FAMILY: &str = "predicate_operator_sequence_v1";
pub const FEATURE_VERSION: &str = "grammar-features-v2;nfc-lowercase-letter-apostrophe-v1;alphabetic-parse-token-v1;syntax-families-v1;predicate-operator-sequence-v1";

#[derive(Clone, Copy, Debug, Default)]
pub struct PredicateOperatorExtractor;

impl FeatureExtractor for PredicateOperatorExtractor {
    fn identity(&self) -> &str {
        FEATURE_VERSION
    }

    fn extract(&self, doc: &Document) -> Result<Features> {
        let mut features = DefaultExtractor.extract(doc)?;
        features.schema = FEATURE_VERSION.into();
        features
            .families
            .insert(FAMILY.into(), document_family(doc)?);
        Ok(features)
    }
}

// Exact legacy clause-head predicate. It includes fragments and nonfinite heads.
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperatorRole {
    HeadAux,
    Aux,
    PassiveAux,
    Cop,
    Neg,
}

fn role(dep: &str) -> Option<OperatorRole> {
    match dep {
        "aux" => Some(OperatorRole::Aux),
        "auxpass" | "aux:pass" => Some(OperatorRole::PassiveAux),
        "cop" => Some(OperatorRole::Cop),
        "neg" => Some(OperatorRole::Neg),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Side {
    Left,
    Right,
    #[serde(rename = "SELF")]
    SelfPosition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttachmentParent {
    Head,
    Operator { sequence_index: usize },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Operator {
    pub role: OperatorRole,
    /// None means an empty or whitespace-only parser lemma; no surface fallback.
    pub lemma: Option<String>,
    pub side: Side,
    /// The observed parent, retaining identity when auxiliary siblings share roles.
    pub attachment_parent: AttachmentParent,
    /// Canonical edge roles from this clause head; empty for the AUX head itself.
    pub attachment_path: Vec<OperatorRole>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperatorSequence {
    None,
    Present { items: Vec<Operator> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CarrierPosition {
    Head,
    Operator { sequence_index: usize },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CarrierMorphology {
    pub verb_form: Option<Vec<String>>,
    pub tense: Option<Vec<String>>,
    pub mood: Option<Vec<String>>,
    pub person: Option<Vec<String>>,
    pub number: Option<Vec<String>>,
}

fn forms(token: &Token, key: &str) -> Option<Vec<String>> {
    token.morph.get(key).map(|values| {
        let mut values = values.clone();
        values.sort_unstable();
        values
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FiniteCarrier {
    pub position: CarrierPosition,
    pub pos: String,
    pub tag: String,
    pub morphology: CarrierMorphology,
    pub verb_form_fin: bool,
    pub finite_tag: bool,
    /// Preserve conflicting or additional values without resolving the parser.
    pub other_verb_forms: Vec<String>,
}

fn finite_carrier(token: &Token, position: CarrierPosition) -> Option<FiniteCarrier> {
    if !matches!(token.pos.as_str(), "VERB" | "AUX") {
        return None;
    }
    let verb_form = forms(token, "VerbForm");
    let verb_form_fin = verb_form
        .as_ref()
        .is_some_and(|values| values.iter().any(|value| value == "Fin"));
    let finite_tag = matches!(token.tag.as_str(), "VBD" | "VBP" | "VBZ" | "MD");
    if !verb_form_fin && !finite_tag {
        return None;
    }
    let other_verb_forms = verb_form
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|value| value.as_str() != "Fin")
        .cloned()
        .collect();
    Some(FiniteCarrier {
        position,
        pos: token.pos.clone(),
        tag: token.tag.clone(),
        morphology: CarrierMorphology {
            verb_form,
            tense: forms(token, "Tense"),
            mood: forms(token, "Mood"),
            person: forms(token, "Person"),
            number: forms(token, "Number"),
        },
        verb_form_fin,
        finite_tag,
        other_verb_forms,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FiniteSignature {
    None,
    One { carrier: FiniteCarrier },
    Multiple { carriers: Vec<FiniteCarrier> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PredicateEvent {
    pub incoming_dependency: String,
    pub head_pos: String,
    pub operators: OperatorSequence,
    pub finite: FiniteSignature,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PredicateObservation {
    /// Source token index for alignment; deliberately excluded from family keys.
    pub head_index: usize,
    pub event: PredicateEvent,
}

fn event(doc: &Document, head: &Token, children: &[Vec<usize>]) -> PredicateEvent {
    let mut included = BTreeMap::<usize, (OperatorRole, Vec<OperatorRole>)>::new();
    if head.pos == "AUX" {
        included.insert(head.i, (OperatorRole::HeadAux, Vec::new()));
    }
    // The validated syntax is an acyclic tree. Iterate rather than recurse so
    // an unusually long auxiliary chain cannot exhaust the Rust call stack.
    let mut pending = vec![(head.i, Vec::new())];
    while let Some((parent, path)) = pending.pop() {
        for &index in &children[parent] {
            let child = &doc.tokens[index];
            let Some(kind) = role(&child.dep) else {
                continue;
            };
            let mut child_path = path.clone();
            child_path.push(kind);
            included.insert(index, (kind, child_path.clone()));
            if kind != OperatorRole::Neg {
                pending.push((index, child_path));
            }
        }
    }
    let mut carriers = BTreeMap::new();
    if let Some(carrier) = finite_carrier(head, CarrierPosition::Head) {
        carriers.insert(head.i, carrier);
    }
    let ordinals = included
        .keys()
        .enumerate()
        .map(|(ordinal, &index)| (index, ordinal))
        .collect::<BTreeMap<_, _>>();
    let operators = included
        .iter()
        .enumerate()
        .map(|(sequence_index, (&index, (kind, path)))| {
            let token = &doc.tokens[index];
            if index != head.i
                && *kind != OperatorRole::Neg
                && let Some(carrier) =
                    finite_carrier(token, CarrierPosition::Operator { sequence_index })
            {
                carriers.insert(index, carrier);
            }
            Operator {
                role: *kind,
                lemma: normalized_lemma(token),
                side: if index < head.i {
                    Side::Left
                } else if index > head.i {
                    Side::Right
                } else {
                    Side::SelfPosition
                },
                attachment_parent: if index == head.i || token.head == head.i {
                    AttachmentParent::Head
                } else {
                    AttachmentParent::Operator {
                        sequence_index: ordinals[&token.head],
                    }
                },
                attachment_path: path.clone(),
            }
        })
        .collect::<Vec<_>>();
    let mut carriers = carriers.into_values().collect::<Vec<_>>();
    let finite = match carriers.len() {
        0 => FiniteSignature::None,
        1 => FiniteSignature::One {
            carrier: carriers.remove(0),
        },
        _ => FiniteSignature::Multiple { carriers },
    };
    PredicateEvent {
        incoming_dependency: if head.head == head.i {
            "ROOT".into()
        } else {
            head.dep.clone()
        },
        head_pos: head.pos.clone(),
        operators: if operators.is_empty() {
            OperatorSequence::None
        } else {
            OperatorSequence::Present { items: operators }
        },
        finite,
    }
}

/// Return inspectable events in clause-head source order. Main predicate lemmas,
/// subject/object words and arbitrary descendants do not enter these events.
pub fn document_observations(doc: &Document) -> Result<Vec<PredicateObservation>> {
    syntax::validate(doc)?;
    let mut children = vec![Vec::new(); doc.tokens.len()];
    for token in doc.tokens.iter().filter(|token| lexical(token)) {
        if token.i != token.head {
            children[token.head].push(token.i);
        }
    }
    Ok(doc
        .tokens
        .iter()
        .filter(|token| lexical(token) && clause_head(token))
        .map(|head| PredicateObservation {
            head_index: head.i,
            event: event(doc, head, &children),
        })
        .collect())
}

/// A non-clausal/empty document has zero opportunities. A clause without any
/// recorded operator emits an explicit NONE sequence, with its finite signature.
pub fn document_family(doc: &Document) -> Result<Family> {
    let mut counts = BTreeMap::<String, u64>::new();
    let mut opportunities = 0_u64;
    for observation in document_observations(doc)? {
        let value = counts
            .entry(serde_json::to_string(&observation.event)?)
            .or_default();
        *value = value
            .checked_add(1)
            .context("Predicate event count overflow")?;
        opportunities = opportunities
            .checked_add(1)
            .context("Predicate opportunity overflow")?;
    }
    Ok(Family::Distribution {
        counts,
        opportunities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    fn fixture(items: &[(&str, &str, &str, usize)]) -> Document {
        let text = items.iter().map(|row| row.0).collect::<Vec<_>>().join(" ");
        let mut offset = 0;
        let tokens = items
            .iter()
            .enumerate()
            .map(|(i, (word, pos, dep, head))| {
                let start_byte = offset;
                offset += word.len() + 1;
                Token {
                    i,
                    start_byte,
                    end_byte: start_byte + word.len(),
                    text: (*word).into(),
                    lemma: word.to_lowercase(),
                    pos: (*pos).into(),
                    tag: String::new(),
                    dep: (*dep).into(),
                    head: *head,
                    sentence: 0,
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect::<Vec<_>>();
        let sentences = if items.is_empty() {
            vec![]
        } else {
            vec![Sentence {
                start_byte: 0,
                end_byte: text.len(),
                root: items
                    .iter()
                    .enumerate()
                    .find(|(i, row)| *i == row.3)
                    .unwrap()
                    .0,
                token_start: 0,
                token_end: items.len(),
            }]
        };
        Document {
            text,
            parser_identity: "synthetic-predicate-fixture-v1".into(),
            tokens,
            sentences,
        }
    }

    fn events(doc: &Document) -> Vec<PredicateEvent> {
        document_observations(doc)
            .unwrap()
            .into_iter()
            .map(|row| row.event)
            .collect()
    }

    fn operators(event: &PredicateEvent) -> &[Operator] {
        match &event.operators {
            OperatorSequence::Present { items } => items,
            OperatorSequence::None => &[],
        }
    }

    fn total(family: &Family) -> u64 {
        let Family::Distribution {
            counts,
            opportunities,
        } = family
        else {
            panic!()
        };
        assert_eq!(counts.values().sum::<u64>(), *opportunities);
        *opportunities
    }

    #[test]
    fn exact_legacy_eligibility_and_unchanged_fourteen_families() {
        let mut doc = fixture(&[
            ("ʼ", "X", "ROOT", 0),
            ("leave", "VERB", "xcomp", 0),
            ("stay", "VERB", "conj", 1),
            ("room", "NOUN", "conj", 0),
            ("whose", "PRON", "csubj:pass", 1),
        ]);
        doc.tokens[1]
            .morph
            .insert("VerbForm".into(), vec!["Inf".into()]);
        let old = DefaultExtractor.extract(&doc).unwrap();
        let new = PredicateOperatorExtractor.extract(&doc).unwrap();
        assert_ne!(old.schema, new.schema);
        assert_eq!(new.schema, PredicateOperatorExtractor.identity());
        assert_eq!(new.families.len(), old.families.len() + 1);
        for (name, family) in &old.families {
            assert_eq!(
                serde_json::to_vec(family).unwrap(),
                serde_json::to_vec(&new.families[name]).unwrap()
            );
        }
        assert_eq!(total(&new.families[FAMILY]), 4);
        assert_eq!(
            total(&new.families[FAMILY]),
            total(&old.families["clause_head_frame"])
        );
    }

    #[test]
    fn closure_preserves_order_duplicates_and_stops_at_lexical_clauses() {
        let doc = fixture(&[
            ("may", "AUX", "aux", 4),
            ("have", "AUX", "aux", 0),
            ("not", "PART", "neg", 1),
            ("have", "AUX", "aux", 1),
            ("tried", "VERB", "ROOT", 4),
            ("never", "ADV", "advmod", 4),
            ("to", "PART", "aux", 8),
            ("not", "PART", "neg", 8),
            ("leave", "VERB", "xcomp", 4),
        ]);
        let result = events(&doc);
        assert_eq!(result.len(), 2);
        let first = operators(&result[0]);
        assert_eq!(
            first
                .iter()
                .map(|op| op.lemma.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["may", "have", "not", "have"]
        );
        assert_eq!(
            first[2].attachment_path,
            [OperatorRole::Aux, OperatorRole::Aux, OperatorRole::Neg]
        );
        assert_eq!(
            first[2].attachment_parent,
            AttachmentParent::Operator { sequence_index: 1 }
        );
        assert_eq!(operators(&result[1]).len(), 2);
        assert_eq!(operators(&result[1])[0].lemma.as_deref(), Some("to"));
        assert_eq!(result[1].incoming_dependency, "xcomp");
    }

    #[test]
    fn exact_operator_parent_survives_same_role_siblings_and_passive_aliases() {
        let a = fixture(&[
            ("might", "AUX", "aux", 3),
            ("have", "AUX", "aux", 3),
            ("not", "PART", "neg", 0),
            ("left", "VERB", "ROOT", 3),
        ]);
        let mut b = a.clone();
        b.tokens[2].head = 1;
        let ea = events(&a);
        let eb = events(&b);
        assert_eq!(
            operators(&ea[0])[2].attachment_path,
            operators(&eb[0])[2].attachment_path
        );
        assert_ne!(ea, eb);
        let mut a = fixture(&[("been", "AUX", "auxpass", 1), ("seen", "VERB", "ROOT", 1)]);
        let old_alias = events(&a);
        a.tokens[0].dep = "aux:pass".into();
        assert_eq!(old_alias, events(&a));
        a.tokens[0].dep = "aux".into();
        assert_ne!(old_alias, events(&a));
    }

    #[test]
    fn finite_none_one_multiple_conflicts_and_aux_head_deduplication() {
        let mut doc = fixture(&[("might", "AUX", "aux", 1), ("leave", "VERB", "ROOT", 1)]);
        assert_eq!(events(&doc)[0].finite, FiniteSignature::None);
        doc.tokens[0].tag = "MD".into();
        doc.tokens[0]
            .morph
            .insert("VerbForm".into(), vec!["Inf".into()]);
        let FiniteSignature::One { carrier } = events(&doc).remove(0).finite else {
            panic!()
        };
        assert!(carrier.finite_tag);
        assert!(!carrier.verb_form_fin);
        assert_eq!(carrier.other_verb_forms, ["Inf"]);
        assert_eq!(
            carrier.position,
            CarrierPosition::Operator { sequence_index: 0 }
        );
        doc.tokens[1]
            .morph
            .insert("VerbForm".into(), vec!["Fin".into(), "Part".into()]);
        let FiniteSignature::Multiple { carriers } = events(&doc).remove(0).finite else {
            panic!()
        };
        assert_eq!(carriers.len(), 2);
        assert_eq!(carriers[1].position, CarrierPosition::Head);
        assert_eq!(carriers[1].other_verb_forms, ["Part"]);
        let mut aux_head = fixture(&[
            ("is", "AUX", "ROOT", 0),
            ("not", "PART", "neg", 0),
            ("ready", "ADJ", "acomp", 0),
        ]);
        aux_head.tokens[0].tag = "VBZ".into();
        aux_head.tokens[0].lemma = "be".into();
        let event = events(&aux_head).remove(0);
        assert_eq!(operators(&event)[0].role, OperatorRole::HeadAux);
        assert_eq!(operators(&event)[0].side, Side::SelfPosition);
        let FiniteSignature::One { carrier } = event.finite else {
            panic!()
        };
        assert_eq!(carrier.position, CarrierPosition::Head);
    }

    #[test]
    fn copulas_and_missing_morphology_do_not_infer_a_finite_carrier() {
        let mut doc = fixture(&[("is", "AUX", "cop", 1), ("ready", "ADJ", "ROOT", 1)]);
        assert_eq!(operators(&events(&doc)[0])[0].role, OperatorRole::Cop);
        assert_eq!(events(&doc)[0].finite, FiniteSignature::None);
        doc.tokens[0]
            .morph
            .insert("VerbForm".into(), vec!["Fin".into()]);
        let FiniteSignature::One { carrier } = events(&doc).remove(0).finite else {
            panic!()
        };
        assert!(carrier.verb_form_fin);
        assert!(!carrier.finite_tag);
        assert_eq!(carrier.morphology.tense, None);
        doc.tokens[0].pos = "NOUN".into();
        assert_eq!(events(&doc)[0].finite, FiniteSignature::None);
    }

    #[test]
    fn missing_lemmas_json_boundaries_and_morph_value_order_are_explicit() {
        let mut doc = fixture(&[("may", "AUX", "aux", 1), ("leave", "VERB", "ROOT", 1)]);
        doc.tokens[0].lemma = " \t".into();
        assert_eq!(operators(&events(&doc)[0])[0].lemma, None);
        doc.tokens[0].lemma = "[\"may\"], | END".into();
        doc.tokens[1].pos = "V|ERB[\"]".into();
        let event = events(&doc).remove(0);
        let key = serde_json::to_string(&event).unwrap();
        assert_eq!(event, serde_json::from_str::<PredicateEvent>(&key).unwrap());
        assert_eq!(
            operators(&event)[0].lemma.as_deref(),
            Some("[\"may\"], | end")
        );
        let mut doc = fixture(&[("left", "VERB", "ROOT", 0)]);
        doc.tokens[0].tag = "VBD".into();
        doc.tokens[0]
            .morph
            .insert("Person".into(), vec!["3".into(), "1".into()]);
        let before = events(&doc);
        doc.tokens[0].morph.get_mut("Person").unwrap().reverse();
        assert_eq!(before, events(&doc));
    }

    #[test]
    fn parser_lemma_contraction_invariant_does_not_certify_meaning() {
        let mut expanded = fixture(&[
            ("We", "PRON", "nsubj", 3),
            ("do", "AUX", "aux", 3),
            ("not", "PART", "neg", 3),
            ("leave", "VERB", "ROOT", 3),
        ]);
        let mut contracted = fixture(&[
            ("We", "PRON", "nsubj", 3),
            ("do", "AUX", "aux", 3),
            ("n't", "PART", "neg", 3),
            ("stay", "VERB", "ROOT", 3),
        ]);
        expanded.tokens[1].tag = "VBP".into();
        contracted.tokens[1].tag = "VBP".into();
        contracted.tokens[2].lemma = "not".into();
        assert_eq!(
            document_family(&expanded).unwrap(),
            document_family(&contracted).unwrap()
        );
        assert_ne!(
            DefaultExtractor.extract(&expanded).unwrap().families["word"],
            DefaultExtractor.extract(&contracted).unwrap().families["word"]
        );
        contracted.tokens[1].lemma = "must".into();
        assert_ne!(
            document_family(&expanded).unwrap(),
            document_family(&contracted).unwrap()
        );
    }

    #[test]
    fn zero_opportunities_are_distinct_from_a_head_without_operators_and_invalid_ir_fails() {
        assert_eq!(total(&document_family(&fixture(&[])).unwrap()), 0);
        assert_eq!(
            total(&document_family(&fixture(&[("!", "PUNCT", "ROOT", 0)])).unwrap()),
            0
        );
        let mut doc = fixture(&[("Hello", "INTJ", "ROOT", 0)]);
        assert_eq!(total(&document_family(&doc).unwrap()), 1);
        assert_eq!(events(&doc)[0].operators, OperatorSequence::None);
        assert_eq!(events(&doc)[0].finite, FiniteSignature::None);
        doc.tokens[0].head = 100;
        assert!(document_family(&doc).is_err());
    }

    #[test]
    fn installed_spacy_synthetic_probes_preserve_legacy_opportunities_and_families() {
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
            "We might not have been invited.",
            "She is not ready.",
            "We don't leave, but they do stay.",
            "We want them to leave.",
            "Hello!",
        ];
        let docs = grammar_spacy::parse_batch(&texts, python.to_str().unwrap()).unwrap();
        let mut seen_operators = 0;
        for result in docs {
            let doc = result.unwrap();
            let before = DefaultExtractor.extract(&doc).unwrap();
            let after = PredicateOperatorExtractor.extract(&doc).unwrap();
            assert_eq!(before.parser_identity, after.parser_identity);
            assert_eq!(
                total(&before.families["clause_head_frame"]),
                total(&after.families[FAMILY])
            );
            for (name, value) in &before.families {
                assert_eq!(
                    serde_json::to_vec(value).unwrap(),
                    serde_json::to_vec(&after.families[name]).unwrap()
                );
            }
            seen_operators += events(&doc)
                .iter()
                .map(|event| operators(event).len())
                .sum::<usize>();
        }
        assert!(seen_operators >= 8);
    }
}
