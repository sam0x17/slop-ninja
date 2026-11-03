//! A narrow local-tree comparison, never an edit or attribution certificate.
//!
//! Version 1 covers only a backward left-predicate ccomp edge to the right
//! predicate, witnessed by the unchanged period-to-semicolon join rule.
use crate::{lexical_context::normalized_lemma, predicate_operators};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Candidate, Edit, RuleIdentity},
    rules::{self, RewriteRule},
    syntax::{self, Document, Token},
};
use predicate_operators::{FiniteSignature, PredicateEvent};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-boundary-local-view-v1";
const ATTRIBUTION_NOTE: &str = "Local structural agreement cannot establish independent assertions or preserve attribution/valency. The raw ccomp may carry reporting or stance information; unknown predicates are not presumed non-reporting. No edit is licensed.";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    LocalStructureAgrees,
    Differs,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionStatus {
    Agrees,
    Differs,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConditionFinding {
    pub condition: String,
    pub status: ConditionStatus,
    pub details: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Anchor {
    pub token_index: usize,
    pub byte_span: [usize; 2],
    pub text: String,
    pub lemma: Option<String>,
    pub pos: String,
    pub tag: String,
    pub morphology: BTreeMap<String, Vec<String>>,
    pub raw_dependency: String,
    pub raw_head: usize,
    pub sentence_index: usize,
}
impl From<&Token> for Anchor {
    fn from(t: &Token) -> Self {
        Self {
            token_index: t.i,
            byte_span: [t.start_byte, t.end_byte],
            text: t.text.clone(),
            lemma: normalized_lemma(t),
            pos: t.pos.clone(),
            tag: t.tag.clone(),
            morphology: sorted_morph(t),
            raw_dependency: t.dep.clone(),
            raw_head: t.head,
            sentence_index: t.sentence,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenAlignment {
    pub period: Anchor,
    pub semicolon: Anchor,
    pub side: Side,
    pub exception: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RawEdge {
    pub child: Anchor,
    pub parent: Anchor,
    pub child_side: Side,
    pub parent_side: Side,
    pub dependency: String,
    pub punctuation: bool,
    pub space: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PredicatePair {
    pub side: Side,
    pub period: Anchor,
    pub semicolon: Anchor,
    pub period_subjects: Vec<Anchor>,
    pub semicolon_subjects: Vec<Anchor>,
    pub period_event: PredicateEvent,
    pub semicolon_event: PredicateEvent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResidualComparison {
    pub period: Anchor,
    pub semicolon: Anchor,
    pub side: Side,
    pub period_parent_in_semicolon: usize,
    pub raw_semicolon_parent: usize,
    pub view_semicolon_parent: usize,
    pub raw_semicolon_dependency: String,
    pub view_semicolon_dependency: String,
    pub adjustment: Option<String>,
    pub parent_agrees: bool,
    pub dependency_agrees: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BoundaryViewReport {
    pub schema: String,
    pub outcome: Outcome,
    pub structural_agreement: Option<bool>,
    pub edit_licensed: bool,
    pub automatic_edit_license: bool,
    pub semantic_equivalence_certified: bool,
    pub attribution_unresolved: bool,
    pub attribution_note: String,
    pub parser_identity: String,
    pub source_sha256: String,
    pub candidate_sha256: String,
    pub actual_edits: Vec<Edit>,
    pub period_is_source: Option<bool>,
    pub period_witnesses: Vec<Candidate>,
    pub period_join_edits: Vec<Edit>,
    /// Canonical merged separator-through-opener inverse; actual patches above
    /// may instead be the two exact mechanical inverse witness patches.
    pub semicolon_inverse_edits: Vec<Edit>,
    pub conditions: Vec<ConditionFinding>,
    pub alignment: Vec<TokenAlignment>,
    pub crossing_edges: Vec<RawEdge>,
    pub exceptional_edge: Option<RawEdge>,
    pub predicate_pairs: Vec<PredicatePair>,
    pub residual_comparisons: Vec<ResidualComparison>,
}

const CONDITIONS: &[&str] = &[
    "declared_utf8_application",
    "original_period_join_witness",
    "complete_two_clause_layout",
    "declared_boundary_patch_shape",
    "byte_slot_correspondence",
    "anchored_predicates_and_subjects",
    "single_backward_ccomp",
    "scope_exclusions",
    "token_evidence",
    "predicate_operator_and_finite_evidence",
    "residual_attachments",
    "two_connected_local_trees",
];
impl BoundaryViewReport {
    fn set(&mut self, condition: &str, status: ConditionStatus, details: Vec<String>) {
        let finding = self
            .conditions
            .iter_mut()
            .find(|c| c.condition == condition)
            .expect("Known versioned condition");
        finding.status = status;
        finding.details = details;
    }
    fn checked(&mut self, condition: &str, problems: Vec<String>) {
        self.set(
            condition,
            if problems.is_empty() {
                ConditionStatus::Agrees
            } else {
                ConditionStatus::Differs
            },
            problems,
        );
    }
    fn finish(mut self) -> Self {
        self.outcome = if self
            .conditions
            .iter()
            .any(|c| c.status == ConditionStatus::Differs)
        {
            Outcome::Differs
        } else if self
            .conditions
            .iter()
            .any(|c| c.status == ConditionStatus::Unavailable)
        {
            Outcome::Unavailable
        } else {
            Outcome::LocalStructureAgrees
        };
        self.structural_agreement = match self.outcome {
            Outcome::LocalStructureAgrees => Some(true),
            Outcome::Differs => Some(false),
            Outcome::Unavailable => None,
        };
        self
    }
}
fn sorted_morph(t: &Token) -> BTreeMap<String, Vec<String>> {
    t.morph
        .iter()
        .map(|(k, v)| {
            let mut v = v.clone();
            v.sort();
            (k.clone(), v)
        })
        .collect()
}
fn subject(t: &Token) -> bool {
    matches!(t.dep.as_str(), "nsubj" | "nsubjpass" | "nsubj:pass")
}
fn canonical_edits(edits: &[Edit]) -> Vec<Edit> {
    let mut v = edits.to_vec();
    v.sort_by_key(|e| (e.start_byte, e.end_byte));
    v
}
fn local_side(index: usize, right_start: usize) -> Side {
    if index < right_start {
        Side::Left
    } else {
        Side::Right
    }
}

/// Compare exact supplied annotations, retaining the raw exceptional relation.
/// This function neither calls nor changes the existing claim guard.
pub fn compare(
    source: &Document,
    candidate: &Document,
    actual_edits: &[Edit],
) -> Result<BoundaryViewReport> {
    syntax::validate(source)?;
    syntax::validate(candidate)?;
    ensure!(
        source.parser_identity == candidate.parser_identity,
        "Boundary-view parser identities differ"
    );
    ensure!(
        edits::apply(&source.text, actual_edits)? == candidate.text,
        "Declared edits do not produce candidate bytes"
    );
    let mut report = BoundaryViewReport {
        schema: SCHEMA.into(),
        outcome: Outcome::Unavailable,
        structural_agreement: None,
        edit_licensed: false,
        automatic_edit_license: false,
        semantic_equivalence_certified: false,
        attribution_unresolved: true,
        attribution_note: ATTRIBUTION_NOTE.into(),
        parser_identity: source.parser_identity.clone(),
        source_sha256: edits::digest(&source.text),
        candidate_sha256: edits::digest(&candidate.text),
        actual_edits: actual_edits.to_vec(),
        period_is_source: None,
        period_witnesses: Vec::new(),
        period_join_edits: Vec::new(),
        semicolon_inverse_edits: Vec::new(),
        conditions: CONDITIONS
            .iter()
            .map(|c| ConditionFinding {
                condition: (*c).into(),
                status: ConditionStatus::Unavailable,
                details: vec![
                    "A preceding certificate condition has not established the required anchors."
                        .into(),
                ],
            })
            .collect(),
        alignment: Vec::new(),
        crossing_edges: Vec::new(),
        exceptional_edge: None,
        predicate_pairs: Vec::new(),
        residual_comparisons: Vec::new(),
    };
    report.checked("declared_utf8_application", Vec::new());
    let rule = rules::CATALOG
        .iter()
        .find(|r| r.id == "join_independent_sentences")
        .context("Missing original join descriptor")?;
    ensure!(
        rule.version == "independent-sentence-join-v1",
        "Original join version changed"
    );
    let mut orientations = Vec::new();
    for (is_source, period, semicolon) in [(true, source, candidate), (false, candidate, source)] {
        for patches in rule.apply(period)? {
            let witness = edits::candidate(
                &period.text,
                RuleIdentity {
                    rule: rule.id,
                    rule_version: rule.version,
                    catalog_version: rules::CATALOG_VERSION,
                    parser_identity: &period.parser_identity,
                },
                patches,
                rule.preconditions,
                rule.risks,
            )?;
            if witness.text == semicolon.text {
                orientations.push(is_source);
                report.period_witnesses.push(witness);
            }
        }
    }
    if orientations.len() != 1 {
        report.set(
            "original_period_join_witness",
            ConditionStatus::Unavailable,
            vec![format!(
                "Expected one exact full-text join witness; observed {}.",
                orientations.len()
            )],
        );
        return Ok(report.finish());
    }
    report.checked("original_period_join_witness", Vec::new());
    let period_is_source = orientations[0];
    report.period_is_source = Some(period_is_source);
    let (period, semicolon) = if period_is_source {
        (source, candidate)
    } else {
        (candidate, source)
    };
    let witness = report.period_witnesses[0].clone();
    report.period_join_edits = witness.edits.clone();
    if period.sentences.len() != 2 || semicolon.sentences.len() != 1 {
        report.set(
            "complete_two_clause_layout",
            ConditionStatus::Unavailable,
            vec![format!(
                "Expected two period sentences and one semicolon sentence; observed {} and {}.",
                period.sentences.len(),
                semicolon.sentences.len()
            )],
        );
        return Ok(report.finish());
    }
    let left = &period.sentences[0];
    let right = &period.sentences[1];
    let boundary = &period.tokens[left.token_end - 1];
    let opener = &period.tokens[right.token_start];
    let layout = boundary.text == "."
        && period.text[boundary.end_byte..opener.start_byte] == *" "
        && period.tokens.iter().all(|t| t.text != ";")
        && semicolon.tokens.iter().filter(|t| t.text == ";").count() == 1
        && period.text.len() == semicolon.text.len()
        && !period.text.contains(['\n', '\r'])
        && left.start_byte == 0
        && right.end_byte == period.text.len();
    if !layout {
        report.set("complete_two_clause_layout",ConditionStatus::Unavailable,vec!["Require complete two-clause texts, one literal boundary, exactly one ASCII gap, and no paragraph/outer text.".into()]);
        return Ok(report.finish());
    }
    report.checked("complete_two_clause_layout", Vec::new());
    let patches = &witness.edits;
    let witness_shape = patches.len() == 2
        && patches[0].start_byte == boundary.start_byte
        && patches[0].end_byte == boundary.end_byte
        && patches[0].expected == "."
        && patches[0].replacement == ";"
        && patches[1].start_byte == opener.start_byte
        && patches[1].end_byte == opener.end_byte
        && patches[1].expected == opener.text
        && patches
            .iter()
            .all(|e| e.expected.len() == e.replacement.len());
    if !witness_shape {
        report.set(
            "declared_boundary_patch_shape",
            ConditionStatus::Unavailable,
            vec![
                "Original witness does not have the fixed v1 punctuation/opener patch shape."
                    .into(),
            ],
        );
        return Ok(report.finish());
    }
    let mechanical_inverse = patches
        .iter()
        .map(|e| Edit {
            start_byte: e.start_byte,
            end_byte: e.end_byte,
            expected: e.replacement.clone(),
            replacement: e.expected.clone(),
        })
        .collect::<Vec<_>>();
    let merged_inverse = vec![Edit {
        start_byte: boundary.start_byte,
        end_byte: opener.end_byte,
        expected: semicolon.text[boundary.start_byte..opener.end_byte].into(),
        replacement: period.text[boundary.start_byte..opener.end_byte].into(),
    }];
    ensure!(
        edits::apply(&semicolon.text, &merged_inverse)? == period.text
            && edits::apply(&semicolon.text, &mechanical_inverse)? == period.text,
        "Witness inverse byte restoration failed"
    );
    report.semicolon_inverse_edits = merged_inverse.clone();
    let declared = canonical_edits(actual_edits);
    let declared_shape = if period_is_source {
        declared == *patches
    } else {
        declared == merged_inverse || declared == mechanical_inverse
    };
    if !declared_shape {
        report.set("declared_boundary_patch_shape",ConditionStatus::Unavailable,vec!["Equal final text is insufficient: declared edits are not the witness or its fixed one/two-patch inverse.".into()]);
        return Ok(report.finish());
    }
    report.checked("declared_boundary_patch_shape", Vec::new());
    let slots = semicolon
        .tokens
        .iter()
        .map(|t| ((t.start_byte, t.end_byte), t.i))
        .collect::<BTreeMap<_, _>>();
    let mapping = period
        .tokens
        .iter()
        .map(|t| slots.get(&(t.start_byte, t.end_byte)).copied())
        .collect::<Option<Vec<_>>>();
    let Some(mapping) = mapping.filter(|m| m.len() == semicolon.tokens.len()) else {
        report.set(
            "byte_slot_correspondence",
            ConditionStatus::Unavailable,
            vec!["Token boundaries differ; no lemma-based alignment is guessed.".into()],
        );
        return Ok(report.finish());
    };
    report.checked("byte_slot_correspondence", Vec::new());
    let mut reverse = vec![0; mapping.len()];
    for (p, &s) in mapping.iter().enumerate() {
        reverse[s] = p;
    }
    for (p, &s) in period.tokens.iter().zip(&mapping) {
        report.alignment.push(TokenAlignment {
            period: p.into(),
            semicolon: (&semicolon.tokens[s]).into(),
            side: local_side(p.i, right.token_start),
            exception: if p.i == boundary.i {
                Some("edited_boundary_punctuation".into())
            } else if p.i == opener.i && p.text != semicolon.tokens[s].text {
                Some("original_join_opener_case".into())
            } else {
                None
            },
        });
    }
    let lp = &period.tokens[left.root];
    let rp = &period.tokens[right.root];
    let ls = &semicolon.tokens[mapping[lp.i]];
    let rs = &semicolon.tokens[mapping[rp.i]];
    let mut anchors = Vec::new();
    if lp.head != lp.i || rp.head != rp.i || lp.dep != "ROOT" || rp.dep != "ROOT" {
        anchors.push("Period predicate anchors are not exact self-headed ROOT tokens.".into());
    }
    let period_events = predicate_operators::document_observations(period)?;
    let semicolon_events = predicate_operators::document_observations(semicolon)?;
    let expected_period = BTreeSet::from([lp.i, rp.i]);
    let expected_semicolon = BTreeSet::from([ls.i, rs.i]);
    if period_events
        .iter()
        .map(|e| e.head_index)
        .collect::<BTreeSet<_>>()
        != expected_period
        || semicolon_events
            .iter()
            .map(|e| e.head_index)
            .collect::<BTreeSet<_>>()
            != expected_semicolon
    {
        anchors
            .push("Clause-head inventory is not exactly the two aligned predicate anchors.".into());
    }
    let mut event_issues = Vec::new();
    let mut evidence_missing = false;
    let mut event_differs = false;
    for (side, p, s) in [(Side::Left, lp, ls), (Side::Right, rp, rs)] {
        let ps = period
            .tokens
            .iter()
            .filter(|t| t.head == p.i && t.i != p.i && subject(t))
            .collect::<Vec<_>>();
        let ss = semicolon
            .tokens
            .iter()
            .filter(|t| t.head == s.i && t.i != s.i && subject(t))
            .collect::<Vec<_>>();
        if ps.is_empty()
            || ps.iter().any(|t| t.i >= p.i)
            || ss.iter().any(|t| t.i >= s.i)
            || ps.iter().map(|t| mapping[t.i]).collect::<BTreeSet<_>>()
                != ss.iter().map(|t| t.i).collect::<BTreeSet<_>>()
        {
            anchors.push(format!(
                "{side:?} own preceding subjects differ or are missing."
            ));
        }
        let pe = period_events.iter().find(|e| e.head_index == p.i);
        let se = semicolon_events.iter().find(|e| e.head_index == s.i);
        if let (Some(pe), Some(se)) = (pe, se) {
            let finite_missing = matches!(pe.event.finite, FiniteSignature::None)
                || matches!(se.event.finite, FiniteSignature::None);
            if finite_missing {
                evidence_missing = true;
                event_issues.push(format!("{side:?} finite evidence unavailable."));
            }
            let incoming = if side == Side::Left {
                pe.event.incoming_dependency == "ROOT" && se.event.incoming_dependency == "ccomp"
            } else {
                pe.event.incoming_dependency == se.event.incoming_dependency
            };
            if !incoming
                || pe.event.head_pos != se.event.head_pos
                || (!finite_missing && pe.event.finite != se.event.finite)
                || pe.event.operators != se.event.operators
            {
                event_differs = true;
                event_issues.push(format!("{side:?} operator/finite/head observations differ outside the exact left ROOT/ccomp exception."));
            }
            report.predicate_pairs.push(PredicatePair {
                side,
                period: p.into(),
                semicolon: s.into(),
                period_subjects: ps.into_iter().map(Into::into).collect(),
                semicolon_subjects: ss.into_iter().map(Into::into).collect(),
                period_event: pe.event.clone(),
                semicolon_event: se.event.clone(),
            });
        } else {
            evidence_missing = true;
            event_issues.push(format!("{side:?} predicate event unavailable."));
        }
    }
    report.checked("anchored_predicates_and_subjects", anchors);
    if event_differs {
        report.set(
            "predicate_operator_and_finite_evidence",
            ConditionStatus::Differs,
            event_issues,
        );
    } else if evidence_missing {
        report.set(
            "predicate_operator_and_finite_evidence",
            ConditionStatus::Unavailable,
            event_issues,
        );
    } else {
        report.checked("predicate_operator_and_finite_evidence", event_issues);
    }
    for t in &semicolon.tokens {
        let child_side = local_side(reverse[t.i], right.token_start);
        let parent = &semicolon.tokens[t.head];
        let parent_side = local_side(reverse[parent.i], right.token_start);
        if child_side != parent_side {
            report.crossing_edges.push(RawEdge {
                child: t.into(),
                parent: parent.into(),
                child_side,
                parent_side,
                dependency: t.dep.clone(),
                punctuation: t.is_punct,
                space: t.is_space,
            });
        }
    }
    let content_edges = report
        .crossing_edges
        .iter()
        .filter(|e| !e.punctuation && !e.space)
        .collect::<Vec<_>>();
    let special = content_edges.len() == 1
        && content_edges[0].child.token_index == ls.i
        && content_edges[0].parent.token_index == rs.i
        && content_edges[0].dependency == "ccomp"
        && ls.dep == "ccomp"
        && ls.head == rs.i
        && rs.head == rs.i
        && rs.dep == "ROOT"
        && semicolon.sentences[0].root == rs.i;
    if special {
        report.exceptional_edge = Some(content_edges[0].clone());
    }
    report.checked("single_backward_ccomp",if special{Vec::new()}else{vec![format!("Expected only left ccomp -> right ROOT; observed {} content crossings with the retained raw endpoints.",content_edges.len())]});
    let mut scope = Vec::new();
    for t in &semicolon.tokens {
        let dep = t.dep.as_str();
        let forbidden = matches!(
            dep,
            "ccomp"
                | "xcomp"
                | "advcl"
                | "mark"
                | "csubj"
                | "csubjpass"
                | "csubj:pass"
                | "relcl"
                | "acl"
                | "acl:relcl"
                | "cc"
                | "conj"
                | "parataxis"
                | "neg"
        ) && !(special && t.i == ls.i && dep == "ccomp");
        if forbidden
            || t.tag == "MD"
            || t.pos == "CCONJ"
            || t.morph
                .get("Polarity")
                .is_some_and(|v| v.iter().any(|x| x == "Neg"))
        {
            scope.push(format!(
                "Excluded clause/scope/operator observation at semicolon token {} ({}/{}/{}).",
                t.i, t.dep, t.pos, t.tag
            ));
        }
    }
    if semicolon
        .text
        .contains(['`', '{', '}', '[', ']', '\\', '=', '<', '>', '\n', '\r'])
        || semicolon.tokens.iter().any(|t| {
            matches!(
                t.text.as_str(),
                "\"" | "'" | "“" | "”" | "‘" | "’" | "``" | "''"
            )
        })
    {
        scope.push("Recognized quote/code/paragraph marker remains excluded.".into());
    }
    report.checked("scope_exclusions", scope);
    let mut evidence = Vec::new();
    let mut missing = false;
    let mut token_differs = false;
    for (p, &s_index) in period.tokens.iter().zip(&mapping) {
        let s = &semicolon.tokens[s_index];
        if p.i == boundary.i {
            if !p.is_punct
                || !s.is_punct
                || p.pos != "PUNCT"
                || s.pos != "PUNCT"
                || p.dep != "punct"
                || s.dep != "punct"
                || s.text != ";"
            {
                token_differs = true;
                evidence.push(
                    "Edited boundary is not a punctuation observation in both parses.".into(),
                );
            }
            continue;
        }
        let lemma_p = normalized_lemma(p);
        let lemma_s = normalized_lemma(s);
        if !p.is_punct && !p.is_space && (lemma_p.is_none() || lemma_s.is_none()) {
            missing = true;
            evidence.push(format!(
                "Missing lexical lemma in byte slot {}..{}.",
                p.start_byte, p.end_byte
            ));
        }
        let lemma_differs = matches!((&lemma_p, &lemma_s), (Some(a), Some(b)) if a != b);
        if lemma_differs
            || p.pos != s.pos
            || p.tag != s.tag
            || sorted_morph(p) != sorted_morph(s)
            || p.is_punct != s.is_punct
            || p.is_space != s.is_space
        {
            token_differs = true;
            evidence.push(format!(
                "Token evidence differs in byte slot {}..{}.",
                p.start_byte, p.end_byte
            ));
        }
        let expected = if p.i == opener.i {
            &patches[1].replacement
        } else {
            &p.text
        };
        if &s.text != expected {
            token_differs = true;
            evidence.push(format!("Unlicensed surface difference at token {}.", p.i));
        }
    }
    if token_differs {
        report.set("token_evidence", ConditionStatus::Differs, evidence);
    } else if missing {
        report.set("token_evidence", ConditionStatus::Unavailable, evidence);
    } else {
        report.checked("token_evidence", evidence);
    }
    // No residual normalization is attempted for an unverified crossing edge.
    if !special {
        return Ok(report.finish());
    }
    let mut effective = semicolon.tokens.iter().map(|t| t.head).collect::<Vec<_>>();
    effective[ls.i] = ls.i;
    effective[mapping[boundary.i]] = mapping[boundary.head];
    let mut attachments = Vec::new();
    for (p, &s_index) in period.tokens.iter().zip(&mapping) {
        let s = &semicolon.tokens[s_index];
        let adjusted_dep = if s.i == ls.i { "ROOT" } else { &s.dep };
        let parent_agrees = effective[s.i] == mapping[p.head];
        let dependency_agrees = p.dep == adjusted_dep;
        if !parent_agrees || !dependency_agrees {
            attachments.push(format!(
                "Residual parent/label differs at period token {} and semicolon token {}.",
                p.i, s.i
            ));
        }
        report.residual_comparisons.push(ResidualComparison {
            period: p.into(),
            semicolon: s.into(),
            side: local_side(p.i, right.token_start),
            period_parent_in_semicolon: mapping[p.head],
            raw_semicolon_parent: s.head,
            view_semicolon_parent: effective[s.i],
            raw_semicolon_dependency: s.dep.clone(),
            view_semicolon_dependency: adjusted_dep.into(),
            adjustment: if s.i == ls.i {
                Some("recorded_left_ccomp_self_head".into())
            } else if p.i == boundary.i {
                Some("edited_boundary_punctuation_period_parent".into())
            } else {
                None
            },
            parent_agrees,
            dependency_agrees,
        });
    }
    report.checked("residual_attachments", attachments);
    let mut trees = Vec::new();
    for t in &semicolon.tokens {
        let side = local_side(reverse[t.i], right.token_start);
        let root = if side == Side::Left { ls.i } else { rs.i };
        let mut cursor = t.i;
        let mut seen = BTreeSet::new();
        loop {
            if local_side(reverse[cursor], right.token_start) != side {
                trees.push(format!("Residual edge from token {} leaves its side.", t.i));
                break;
            }
            if !seen.insert(cursor) {
                trees.push(format!("Residual cycle reachable from token {}.", t.i));
                break;
            }
            if effective[cursor] == cursor {
                if cursor != root {
                    trees.push(format!("Token {} reaches a different local root.", t.i));
                }
                break;
            }
            cursor = effective[cursor];
        }
    }
    report.checked("two_connected_local_trees", trees);
    Ok(report.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    fn pair(right_verb: &str) -> (Document, Document) {
        let period_text = format!("The clerk filed forms. The guard {right_verb}.");
        let words = [
            ("The", "DET", "det", 1, "DT"),
            ("clerk", "NOUN", "nsubj", 2, "NN"),
            ("filed", "VERB", "ROOT", 2, "VBD"),
            ("forms", "NOUN", "dobj", 2, "NNS"),
            (".", "PUNCT", "punct", 2, "."),
            ("The", "DET", "det", 6, "DT"),
            ("guard", "NOUN", "nsubj", 7, "NN"),
            (right_verb, "VERB", "ROOT", 7, "VBD"),
            (".", "PUNCT", "punct", 7, "."),
        ];
        let mut cursor = 0;
        let mut tokens = Vec::new();
        for (i, (word, pos, dep, head, tag)) in words.into_iter().enumerate() {
            let start_byte = cursor + period_text[cursor..].find(word).unwrap();
            let end_byte = start_byte + word.len();
            cursor = end_byte;
            tokens.push(Token {
                i,
                start_byte,
                end_byte,
                text: word.into(),
                lemma: word.to_lowercase(),
                pos: pos.into(),
                tag: tag.into(),
                dep: dep.into(),
                head,
                sentence: usize::from(i >= 5),
                morph: BTreeMap::new(),
                is_punct: pos == "PUNCT",
                is_space: false,
            });
        }
        let period = Document {
            text: period_text,
            parser_identity: "boundary-view-synthetic-v1".into(),
            sentences: vec![
                Sentence {
                    start_byte: 0,
                    end_byte: tokens[4].end_byte,
                    root: 2,
                    token_start: 0,
                    token_end: 5,
                },
                Sentence {
                    start_byte: tokens[5].start_byte,
                    end_byte: tokens[8].end_byte,
                    root: 7,
                    token_start: 5,
                    token_end: 9,
                },
            ],
            tokens,
        };
        let mut semicolon = period.clone();
        semicolon.text = period.text.replace(". The guard", "; the guard");
        semicolon.tokens[4].text = ";".into();
        semicolon.tokens[4].lemma = ";".into();
        semicolon.tokens[4].tag = ":".into();
        semicolon.tokens[4].head = 7;
        semicolon.tokens[5].text = "the".into();
        semicolon.tokens[2].dep = "ccomp".into();
        semicolon.tokens[2].head = 7;
        for t in &mut semicolon.tokens {
            t.sentence = 0;
        }
        semicolon.sentences = vec![Sentence {
            start_byte: 0,
            end_byte: semicolon.text.len(),
            root: 7,
            token_start: 0,
            token_end: 9,
        }];
        syntax::validate(&period).unwrap();
        syntax::validate(&semicolon).unwrap();
        (period, semicolon)
    }
    fn join(period: &Document) -> Vec<Edit> {
        rules::CATALOG
            .iter()
            .find(|r| r.id == "join_independent_sentences")
            .unwrap()
            .apply(period)
            .unwrap()
            .remove(0)
    }
    fn split(period: &Document, semicolon: &Document) -> Vec<Edit> {
        let start = period.tokens[4].start_byte;
        let end = period.tokens[5].end_byte;
        vec![Edit {
            start_byte: start,
            end_byte: end,
            expected: semicolon.text[start..end].into(),
            replacement: period.text[start..end].into(),
        }]
    }
    fn condition(report: &BoundaryViewReport, name: &str) -> ConditionStatus {
        report
            .conditions
            .iter()
            .find(|c| c.condition == name)
            .unwrap()
            .status
    }
    fn assert_never_licenses(report: &BoundaryViewReport) {
        assert!(!report.edit_licensed);
        assert!(!report.automatic_edit_license);
        assert!(!report.semantic_equivalence_certified);
        assert!(report.attribution_unresolved);
    }

    #[test]
    fn symmetric_local_agreement_retains_raw_ccomp_and_punctuation_adjustment() {
        let (p, s) = pair("waited");
        let originals = (p.clone(), s.clone());
        let forward = compare(&p, &s, &join(&p)).unwrap();
        let reverse = compare(&s, &p, &split(&p, &s)).unwrap();
        for r in [&forward, &reverse] {
            assert_eq!(r.outcome, Outcome::LocalStructureAgrees);
            assert_eq!(r.structural_agreement, Some(true));
            assert_never_licenses(r);
            assert_eq!(r.conditions.len(), CONDITIONS.len());
            assert!(
                r.conditions
                    .iter()
                    .all(|c| c.status == ConditionStatus::Agrees)
            );
            let edge = r.exceptional_edge.as_ref().unwrap();
            assert_eq!(edge.dependency, "ccomp");
            assert_eq!(edge.child.token_index, 2);
            assert_eq!(edge.parent.token_index, 7);
            assert_eq!(r.crossing_edges.len(), 2);
            assert_eq!(
                r.predicate_pairs[0].semicolon_event.incoming_dependency,
                "ccomp"
            );
            assert_eq!(r.residual_comparisons[2].raw_semicolon_parent, 7);
            assert_eq!(r.residual_comparisons[2].view_semicolon_parent, 2);
            assert_eq!(r.residual_comparisons[4].raw_semicolon_parent, 7);
            assert_eq!(r.residual_comparisons[4].view_semicolon_parent, 2);
        }
        assert_eq!(forward.period_is_source, Some(true));
        assert_eq!(reverse.period_is_source, Some(false));
        assert_eq!((p, s), originals);
        let roundtrip: BoundaryViewReport =
            serde_json::from_str(&serde_json::to_string(&forward).unwrap()).unwrap();
        assert_eq!(roundtrip, forward);
    }

    #[test]
    fn reporting_tail_can_agree_but_attribution_remains_unresolved() {
        let (p, s) = pair("said");
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::LocalStructureAgrees);
        assert_never_licenses(&r);
        assert!(r.attribution_note.contains("unknown predicates"));
        assert_eq!(r.exceptional_edge.unwrap().dependency, "ccomp");
    }

    #[test]
    fn declared_whole_text_replacement_is_not_a_boundary_witness() {
        let (p, s) = pair("waited");
        let broad = Edit {
            start_byte: 0,
            end_byte: p.text.len(),
            expected: p.text.clone(),
            replacement: s.text.clone(),
        };
        let r = compare(&p, &s, &[broad]).unwrap();
        assert_eq!(r.outcome, Outcome::Unavailable);
        assert_eq!(r.period_witnesses.len(), 1);
        assert_eq!(
            condition(&r, "declared_boundary_patch_shape"),
            ConditionStatus::Unavailable
        );
        assert!(r.alignment.is_empty());
        assert_never_licenses(&r);
        let inverse = join(&p)
            .iter()
            .map(|e| Edit {
                start_byte: e.start_byte,
                end_byte: e.end_byte,
                expected: e.replacement.clone(),
                replacement: e.expected.clone(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            compare(&s, &p, &inverse).unwrap().outcome,
            Outcome::LocalStructureAgrees
        );
    }

    #[test]
    fn subject_and_object_swaps_are_not_hidden_by_equal_token_bags() {
        let (p, mut s) = pair("waited");
        s.tokens[1].head = 7;
        s.tokens[6].head = 2;
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(
            condition(&r, "anchored_predicates_and_subjects"),
            ConditionStatus::Differs
        );
        assert_eq!(
            condition(&r, "single_backward_ccomp"),
            ConditionStatus::Differs
        );
        assert!(r.exceptional_edge.is_none());
        assert_never_licenses(&r);
        let (p, mut s) = pair("waited");
        s.tokens[3].head = 7;
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(
            r.crossing_edges.iter().filter(|e| !e.punctuation).count(),
            2
        );
    }

    #[test]
    fn different_edge_direction_and_extra_embedded_head_remain_differences() {
        let (p, mut s) = pair("waited");
        s.tokens[2].head = 2;
        s.tokens[2].dep = "ROOT".into();
        s.tokens[7].head = 2;
        s.tokens[7].dep = "ccomp".into();
        s.sentences[0].root = 2;
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert!(r.exceptional_edge.is_none());
        let (p, mut s) = pair("waited");
        s.tokens[3].dep = "ccomp".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(condition(&r, "scope_exclusions"), ConditionStatus::Differs);
        assert_eq!(
            condition(&r, "anchored_predicates_and_subjects"),
            ConditionStatus::Differs
        );
    }

    #[test]
    fn every_nonboundary_attachment_and_lexical_finite_field_remains_checked() {
        let (p, mut s) = pair("waited");
        s.tokens[8].head = 2;
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(
            condition(&r, "single_backward_ccomp"),
            ConditionStatus::Agrees
        );
        assert_eq!(
            condition(&r, "residual_attachments"),
            ConditionStatus::Differs
        );
        assert_eq!(
            condition(&r, "two_connected_local_trees"),
            ConditionStatus::Differs
        );
        let (p, mut s) = pair("waited");
        s.tokens[3].lemma = "receipts".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(condition(&r, "token_evidence"), ConditionStatus::Differs);
        assert_eq!(r.outcome, Outcome::Differs);
        let (p, mut s) = pair("waited");
        s.tokens[2].tag = "VBP".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(
            condition(&r, "predicate_operator_and_finite_evidence"),
            ConditionStatus::Differs
        );
        assert_eq!(r.outcome, Outcome::Differs);
    }

    #[test]
    fn missing_lemmas_are_unavailable_and_scope_observations_are_not_excused() {
        let (p, mut s) = pair("waited");
        s.tokens[3].lemma = " ".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Unavailable);
        assert_eq!(
            condition(&r, "token_evidence"),
            ConditionStatus::Unavailable
        );
        s.tokens[1].tag = "NNS".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(condition(&r, "token_evidence"), ConditionStatus::Differs);
        let evidence = r
            .conditions
            .iter()
            .find(|finding| finding.condition == "token_evidence")
            .unwrap();
        assert!(
            evidence
                .details
                .iter()
                .any(|detail| detail.contains("Missing lexical lemma"))
        );
        assert!(
            evidence
                .details
                .iter()
                .any(|detail| detail.contains("Token evidence differs"))
        );
        for dep in ["neg", "mark", "advcl", "cc"] {
            let (p, mut s) = pair("waited");
            s.tokens[3].dep = dep.into();
            let r = compare(&p, &s, &join(&p)).unwrap();
            assert_eq!(r.outcome, Outcome::Differs);
            assert_eq!(condition(&r, "scope_exclusions"), ConditionStatus::Differs);
        }
        let (p, mut s) = pair("waited");
        s.tokens[7].tag = "MD".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(condition(&r, "scope_exclusions"), ConditionStatus::Differs);
    }

    #[test]
    fn missing_finite_evidence_does_not_hide_another_tested_difference() {
        let (p, mut s) = pair("waited");
        s.tokens[2].tag = "VB".into();
        s.tokens[7].tag = "VBP".into();
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Differs);
        assert_eq!(
            condition(&r, "predicate_operator_and_finite_evidence"),
            ConditionStatus::Differs
        );
    }

    #[test]
    fn literal_insertion_and_paragraph_gap_have_no_period_join_certificate() {
        let (p, s) = pair("waited");
        let mut no_period = p.clone();
        no_period.text = p.text.replace("forms.", "forms ");
        no_period.tokens[4].text = " ".into();
        no_period.tokens[4].lemma = " ".into();
        no_period.tokens[4].pos = "SPACE".into();
        no_period.tokens[4].is_punct = false;
        no_period.tokens[4].is_space = true;
        let edit = Edit {
            start_byte: p.tokens[4].start_byte,
            end_byte: p.tokens[4].end_byte,
            expected: " ".into(),
            replacement: ";".into(),
        };
        let mut target = s.clone();
        target.tokens[5].text = "The".into();
        target.text = p.text.replace("forms.", "forms;");
        let r = compare(&no_period, &target, &[edit]).unwrap();
        assert_eq!(r.outcome, Outcome::Unavailable);
        assert!(r.period_witnesses.is_empty());
        assert_never_licenses(&r);
        // Supply valid hand annotations with a longer between-sentence gap.
        let mut gap = p.clone();
        let index = p.tokens[5].start_byte;
        gap.text.insert(index, '\n');
        for t in &mut gap.tokens[5..] {
            t.start_byte += 1;
            t.end_byte += 1;
        }
        gap.sentences[1].start_byte += 1;
        gap.sentences[1].end_byte += 1;
        let mut semigap = s.clone();
        semigap.text.insert(index, '\n');
        for t in &mut semigap.tokens[5..] {
            t.start_byte += 1;
            t.end_byte += 1;
        }
        semigap.sentences[0].end_byte += 1;
        let broad = Edit {
            start_byte: 0,
            end_byte: gap.text.len(),
            expected: gap.text.clone(),
            replacement: semigap.text.clone(),
        };
        let r = compare(&gap, &semigap, &[broad]).unwrap();
        assert_eq!(r.outcome, Outcome::Unavailable);
        assert!(r.period_witnesses.is_empty());
    }

    #[test]
    fn tokenization_changes_do_not_acquire_lemma_alignment_and_invalid_inputs_error() {
        let (p, mut s) = pair("waited");
        // Split the same word into two valid source spans; no cross-word lookup.
        let old = s.tokens[3].clone();
        s.tokens[3].end_byte = old.start_byte + 1;
        s.tokens[3].text = "f".into();
        let mut second = old.clone();
        second.start_byte += 1;
        second.text = "orms".into();
        s.tokens.insert(4, second);
        for (i, t) in s.tokens.iter_mut().enumerate() {
            t.i = i;
            if t.head >= 4 {
                t.head += 1;
            }
        }
        s.sentences[0].root += 1;
        s.sentences[0].token_end += 1;
        let r = compare(&p, &s, &join(&p)).unwrap();
        assert_eq!(r.outcome, Outcome::Unavailable);
        assert_eq!(
            condition(&r, "byte_slot_correspondence"),
            ConditionStatus::Unavailable
        );
        let (p, mut s) = pair("waited");
        s.parser_identity.push('x');
        assert!(compare(&p, &s, &join(&p)).is_err());
        let (p, s) = pair("waited");
        let mut bad = join(&p);
        bad[0].expected = "bad".into();
        assert!(compare(&p, &s, &bad).is_err());
    }
}
