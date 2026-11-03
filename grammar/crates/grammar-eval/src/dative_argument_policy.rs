//! Role and movement assessments over exact source phrase evidence.
//! Supported means eligible for a structural test, never resolved reference or meaning.
use crate::lexical_context::{lexical, normalized_lemma};
use anyhow::{Result, ensure};
use grammar_core::syntax::{self, Document, Token};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentPolicy {
    #[default]
    LegacyV1,
    RoleAwareV1,
}
impl ArgumentPolicy {
    pub fn is_legacy(&self) -> bool {
        *self == Self::LegacyV1
    }
    pub fn schema(self) -> &'static str {
        match self {
            Self::LegacyV1 => super::SCHEMA,
            Self::RoleAwareV1 => "slopninja-dative-alternation-role-aware-v1",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Agent,
    Theme,
    Recipient,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Movement {
    Unchanged,
    Reordered,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    SupportedForStructuralTest,
    Unsupported,
    Unresolved,
}
#[derive(Clone, Debug, Serialize)]
pub struct PhraseEvidence {
    pub head_index: usize,
    pub token_indices: Vec<usize>,
    pub bounding_span: [usize; 2],
    pub contiguous_tokens: bool,
    pub tokens: Vec<Token>,
    pub normalized_lemmas: Vec<Option<String>>,
    pub internal_edges: Vec<Value>,
    pub head_pos: String,
    pub head_form: String,
    pub reference_features: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct Condition {
    pub condition: String,
    pub decision: Decision,
    pub evidence: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct ArgumentAssessment {
    pub role: Role,
    pub movement: Movement,
    pub decision: Decision,
    pub phrase: PhraseEvidence,
    pub findings: Vec<Condition>,
    pub reference_compatibility: String,
    pub semantic_equivalence_certified: bool,
    pub automatic_edit_license: bool,
}

fn form(token: &Token) -> String {
    crate::verbnet_resource::normalize_lemma(&token.text).unwrap_or_default()
}
const SUBJECTS: &[&str] = &["i", "we", "you", "he", "she", "it", "they"];
const OBLIQUES: &[&str] = &["me", "us", "you", "him", "her", "it", "them"];
const DEICTIC_POSSESSIVES: &[&str] = &["my", "our", "your"];
const ANAPHORIC_POSSESSIVES: &[&str] = &["his", "her", "hers", "its", "their", "theirs"];
const REFLEXIVES: &[&str] = &[
    "myself",
    "ourselves",
    "yourself",
    "yourselves",
    "himself",
    "herself",
    "itself",
    "themselves",
    "themself",
    "oneself",
];

pub fn describe(doc: &Document, head: usize) -> Result<PhraseEvidence> {
    syntax::validate(doc)?;
    ensure!(head < doc.tokens.len(), "Argument head is out of bounds");
    let mut children = vec![Vec::new(); doc.tokens.len()];
    for t in &doc.tokens {
        if t.head != t.i {
            children[t.head].push(t.i);
        }
    }
    let mut indices = vec![head];
    let mut at = 0;
    while at < indices.len() {
        indices.extend(&children[indices[at]]);
        at += 1;
    }
    indices.sort_unstable();
    let first = indices[0];
    let last = *indices.last().unwrap();
    let tokens: Vec<_> = indices.iter().map(|&i| doc.tokens[i].clone()).collect();
    let token = &doc.tokens[head];
    let head_form = form(token);
    Ok(PhraseEvidence {
        head_index: head,
        token_indices: indices.clone(),
        bounding_span: [doc.tokens[first].start_byte, doc.tokens[last].end_byte],
        contiguous_tokens: last - first + 1 == indices.len(),
        normalized_lemmas: tokens.iter().map(normalized_lemma).collect(),
        internal_edges: tokens
            .iter()
            .filter(|t| t.i != head)
            .map(|t| json!({"token_index":t.i,"head":t.head,"dependency":t.dep}))
            .collect(),
        reference_features: json!({"personal_subject_form":SUBJECTS.contains(&head_form.as_str()),
            "personal_oblique_form":OBLIQUES.contains(&head_form.as_str()),
            "deictic_possessive_indices":tokens.iter().filter(|t|DEICTIC_POSSESSIVES.contains(&form(t).as_str())).map(|t|t.i).collect::<Vec<_>>(),
            "reflexive_form_or_morphology_indices":tokens.iter().filter(|t|REFLEXIVES.contains(&form(t).as_str()) || t.morph.get("Reflex").is_some_and(|v|v.iter().any(|x|x=="Yes"))).map(|t|t.i).collect::<Vec<_>>(),
            "speaker_listener_or_coreference_resolved":false}),
        head_pos: token.pos.clone(),
        head_form,
        tokens,
    })
}
fn add(out: &mut ArgumentAssessment, name: &str, decision: Decision, evidence: Value) {
    out.findings.push(Condition {
        condition: name.into(),
        decision,
        evidence,
    });
}
fn check(out: &mut ArgumentAssessment, name: &str, okay: bool, evidence: Value) {
    add(
        out,
        name,
        if okay {
            Decision::SupportedForStructuralTest
        } else {
            Decision::Unsupported
        },
        evidence,
    );
}
fn personal(out: &mut ArgumentAssessment, t: &Token, subject: bool) {
    let supplied = form(t);
    let forms = if subject { SUBJECTS } else { OBLIQUES };
    let expected = if subject { "Nom" } else { "Acc" };
    check(
        out,
        "personal_pronoun_form_and_tag",
        t.pos == "PRON" && t.tag == "PRP" && forms.contains(&supplied.as_str()),
        json!({"token_index":t.i,"form":supplied,"pos":t.pos,"tag":t.tag,"subject":subject}),
    );
    if let Some(values) = t.morph.get("Case") {
        check(
            out,
            "supplied_pronoun_case",
            values.iter().all(|v| v == expected),
            json!({"token_index":t.i,"values":values,"expected":expected}),
        );
    } else {
        add(
            out,
            "case_from_closed_class_surface_form",
            Decision::SupportedForStructuralTest,
            json!({"token_index":t.i,"missing_case_morphology":true,"no_case_conversion":true}),
        );
    }
    check(
        out,
        "nonreflexive_personal_form",
        !REFLEXIVES.contains(&supplied.as_str())
            && !t
                .morph
                .get("Reflex")
                .is_some_and(|v| v.iter().any(|x| x == "Yes")),
        json!({"token_index":t.i}),
    );
}

pub fn assess(doc: &Document, role: Role, evidence: PhraseEvidence) -> Result<ArgumentAssessment> {
    ensure!(
        serde_json::to_value(describe(doc, evidence.head_index)?)?
            == serde_json::to_value(&evidence)?,
        "Phrase evidence differs from exact source annotation"
    );
    let movement = if role == Role::Agent {
        Movement::Unchanged
    } else {
        Movement::Reordered
    };
    let head = doc.tokens[evidence.head_index].clone();
    let mut out=ArgumentAssessment{role,movement,decision:Decision::SupportedForStructuralTest,phrase:evidence,findings:Vec::new(),reference_compatibility:"unresolved; no referent, speaker/listener identity, ownership, binding, selectional or discourse equivalence inferred".into(),semantic_equivalence_certified:false,automatic_edit_license:false};
    if out
        .phrase
        .tokens
        .iter()
        .any(|t| lexical(t) && normalized_lemma(t).is_none())
    {
        add(
            &mut out,
            "missing_lexical_lemma",
            Decision::Unresolved,
            json!({"no_surface_fallback":true}),
        );
    }
    if role == Role::Agent {
        check(
            &mut out,
            "unchanged_nominal_agent_head",
            matches!(head.pos.as_str(), "NOUN" | "PROPN" | "PRON"),
            json!({"pos":head.pos,"pronoun_or_quantifier_reference_resolved":false}),
        );
        add(
            &mut out,
            "unchanged_agent_subtree_contract",
            Decision::SupportedForStructuralTest,
            json!({"moved_np_modifier_gate_applied":false,"required_by_plan_and_compare":"All subtree tokens outside edit, preceding predicate; exact mapped subject and internal edges, tokens and morphology"}),
        );
    } else if role == Role::Recipient && head.pos == "PRON" {
        personal(&mut out, &head, false);
        let indices = out.phrase.token_indices.clone();
        check(
            &mut out,
            "singleton_oblique_recipient",
            indices.len() == 1,
            json!({"token_indices":indices}),
        );
    } else {
        let contiguous = out.phrase.contiguous_tokens;
        let indices = out.phrase.token_indices.clone();
        check(
            &mut out,
            "moved_nominal_head",
            matches!(head.pos.as_str(), "NOUN" | "PROPN"),
            json!({"pos":head.pos,"theme_pronouns_supported":false}),
        );
        check(
            &mut out,
            "contiguous_moved_subtree",
            contiguous,
            json!({"token_indices":indices}),
        );
        let tokens = out.phrase.tokens.clone();
        for t in &tokens {
            let surface = form(t);
            let lemma = normalized_lemma(t);
            let deictic = t.i != head.i
                && DEICTIC_POSSESSIVES.contains(&surface.as_str())
                && matches!(t.pos.as_str(), "PRON" | "DET")
                && matches!(t.dep.as_str(), "poss" | "nmod:poss")
                && t.head == head.i;
            let reflexive = REFLEXIVES.contains(&surface.as_str())
                || t.morph
                    .get("Reflex")
                    .is_some_and(|v| v.iter().any(|x| x == "Yes"));
            let poss = t.morph.contains_key("Poss");
            let deictic_morph = t
                .morph
                .get("Poss")
                .is_none_or(|v| v.iter().all(|x| x == "Yes"));
            check(
                &mut out,
                "moved_reference_and_lexical_scope",
                lexical(t)
                    && !reflexive
                    && !super::QUANTIFIERS.contains(&surface.as_str())
                    && !lemma
                        .as_ref()
                        .is_some_and(|l| super::QUANTIFIERS.contains(&l.as_str()))
                    && !ANAPHORIC_POSSESSIVES.contains(&surface.as_str())
                    && !t.morph.contains_key("NumType")
                    && (!poss || deictic)
                    && (!deictic || deictic_morph),
                json!({"token_index":t.i,"deictic_possessive":deictic,"reference_resolution_claimed":false}),
            );
            if t.i != head.i {
                let ordinary = match (t.pos.as_str(), t.dep.as_str()) {
                    ("DET", "det") => matches!(surface.as_str(), "a" | "an" | "the"),
                    ("ADJ", "amod") => true,
                    ("NOUN" | "PROPN", "compound" | "flat" | "flat:name") => true,
                    _ => false,
                };
                check(
                    &mut out,
                    "moved_nominal_composition",
                    ordinary || deictic,
                    json!({"token_index":t.i,"pos":t.pos,"dep":t.dep,"deictic_possessive":deictic}),
                );
            }
        }
        for pair in tokens.windows(2) {
            check(
                &mut out,
                "moved_nominal_spacing",
                doc.text.get(pair[0].end_byte..pair[1].start_byte) == Some(" "),
                json!({"left":pair[0].i,"right":pair[1].i}),
            );
        }
    }
    out.decision = if out
        .findings
        .iter()
        .any(|f| f.decision == Decision::Unsupported)
    {
        Decision::Unsupported
    } else if out
        .findings
        .iter()
        .any(|f| f.decision == Decision::Unresolved)
    {
        Decision::Unresolved
    } else {
        Decision::SupportedForStructuralTest
    };
    Ok(out)
}
