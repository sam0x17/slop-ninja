//! Source-aligned observations of predicate, operator and content preservation.
//!
//! `ObservedPreserved` means that these parser observations matched through
//! explicit patch correspondence. It never certifies semantic equivalence.
//! Unrecognized broad replacements do not acquire alignment from lemma counts.

use crate::{
    lexical_context::{lexical, normalized_lemma},
    predicate_operators::{self, OperatorRole, OperatorSequence, PredicateEvent},
};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Edit},
    rules,
    syntax::{self, Document, Token},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-source-claim-guard-v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    ObservedPreserved,
    Changed,
    Unresolved,
}

fn combine(outcomes: impl IntoIterator<Item = Outcome>) -> Outcome {
    let mut result = Outcome::ObservedPreserved;
    for value in outcomes {
        if value == Outcome::Changed {
            return value;
        }
        if value == Outcome::Unresolved {
            result = value;
        }
    }
    result
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentKind {
    UnchangedBytes,
    SingleTokenSubstitution,
    LicensedNegativeContraction,
    LicensedBoundary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Anchor {
    pub token_index: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
    pub lemma: Option<String>,
    pub pos: String,
    pub dependency: String,
    pub head_index: usize,
}

impl From<&Token> for Anchor {
    fn from(token: &Token) -> Self {
        Self {
            token_index: token.i,
            start_byte: token.start_byte,
            end_byte: token.end_byte,
            text: token.text.clone(),
            lemma: normalized_lemma(token),
            pos: token.pos.clone(),
            dependency: token.dep.clone(),
            head_index: token.head,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenAlignment {
    pub source: Anchor,
    pub candidate: Anchor,
    pub kind: AlignmentKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    MissingLemma,
    LemmaChanged,
    PartOfSpeechChanged,
    MorphologyChanged,
    TagChanged,
    DependencyChanged,
    AttachmentChanged,
    AttachmentUnmapped,
    OperatorSequenceChanged,
    OperatorCorrespondenceChanged,
    FiniteEvidenceChanged,
    PredicateDeleted,
    PredicateAdded,
    PredicateAlignmentUnresolved,
    OperatorDeleted,
    OperatorAdded,
    OperatorAlignmentUnresolved,
    ContentDeleted,
    ContentAdded,
    ContentAlignmentUnresolved,
    PunctuationChangeUnresolved,
    NoPredicateObservations,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Finding {
    pub outcome: Outcome,
    pub reason: Reason,
    pub source: Option<Anchor>,
    pub candidate: Option<Anchor>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Claim {
    pub head: Anchor,
    pub event: PredicateEvent,
    /// Same order as event.operators; spans are outside the feature keys.
    pub operator_anchors: Vec<Anchor>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PredicateCheck {
    pub source: Anchor,
    pub candidate: Anchor,
    pub alignment: AlignmentKind,
    pub local_operator_outcome: Outcome,
    pub content_scope_outcome: Outcome,
    pub findings: Vec<Finding>,
    pub source_claim: Claim,
    pub candidate_claim: Claim,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuardReport {
    pub schema: String,
    pub parser_identity: String,
    pub source_sha256: String,
    pub candidate_sha256: String,
    pub outcome: Outcome,
    pub local_operator_outcome: Outcome,
    pub content_scope_outcome: Outcome,
    pub semantic_equivalence_certified: bool,
    pub licensed_boundary_rule: Option<String>,
    pub predicates: Vec<PredicateCheck>,
    pub content_checks: Vec<Finding>,
    pub added_predicates: Vec<Finding>,
    pub deleted_predicates: Vec<Finding>,
    pub unresolved_predicates: Vec<Finding>,
    pub added_operators: Vec<Finding>,
    pub deleted_operators: Vec<Finding>,
    pub unresolved_operators: Vec<Finding>,
    pub alignment: Vec<TokenAlignment>,
}

#[derive(Clone)]
struct Patch {
    edit: Edit,
    candidate_start: usize,
    candidate_end: usize,
}

struct Correspondence {
    forward: BTreeMap<usize, (usize, AlignmentKind)>,
    reverse: BTreeMap<usize, usize>,
    patches: Vec<Patch>,
    boundary: Option<String>,
}

fn sorted_edits(edits: &[Edit]) -> Vec<Edit> {
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| (edit.start_byte, edit.end_byte));
    sorted
}

fn matching_rule(source: &Document, edits: &[Edit], names: &[&str]) -> Result<Option<String>> {
    let names = names.iter().map(|name| (*name).into()).collect::<Vec<_>>();
    Ok(
        rules::generate_selected(source, rules::MAX_CANDIDATES, &names)?
            .into_iter()
            .find(|candidate| candidate.edits == edits)
            .map(|candidate| candidate.rule),
    )
}

fn contained_tokens(doc: &Document, start: usize, end: usize) -> Vec<&Token> {
    doc.tokens
        .iter()
        .filter(|token| token.start_byte >= start && token.end_byte <= end)
        .collect()
}

fn insert_mapping(
    map: &mut Correspondence,
    source: usize,
    candidate: usize,
    kind: AlignmentKind,
) -> Result<()> {
    ensure!(
        !map.forward.contains_key(&source) && !map.reverse.contains_key(&candidate),
        "Ambiguous token correspondence"
    );
    map.forward.insert(source, (candidate, kind));
    map.reverse.insert(candidate, source);
    Ok(())
}

fn correspondence(
    source: &Document,
    candidate: &Document,
    edits: &[Edit],
) -> Result<Correspondence> {
    let edits = sorted_edits(edits);
    let boundary = matching_rule(
        source,
        &edits,
        &[
            "split_independent_coordination",
            "split_independent_semicolon",
            "join_independent_sentences",
        ],
    )?;
    let mut map = Correspondence {
        forward: BTreeMap::new(),
        reverse: BTreeMap::new(),
        patches: Vec::new(),
        boundary,
    };
    let mut source_cursor = 0;
    let mut candidate_cursor = 0;
    let mut runs = Vec::new();
    for edit in edits {
        let length = edit.start_byte - source_cursor;
        runs.push((source_cursor, edit.start_byte, candidate_cursor));
        candidate_cursor += length;
        let candidate_start = candidate_cursor;
        candidate_cursor += edit.replacement.len();
        source_cursor = edit.end_byte;
        map.patches.push(Patch {
            edit,
            candidate_start,
            candidate_end: candidate_cursor,
        });
    }
    runs.push((source_cursor, source.text.len(), candidate_cursor));
    let by_span = candidate
        .tokens
        .iter()
        .map(|token| ((token.start_byte, token.end_byte), token.i))
        .collect::<BTreeMap<_, _>>();
    for token in &source.tokens {
        if let Some(&(start, _, destination)) = runs
            .iter()
            .find(|&&(start, end, _)| token.start_byte >= start && token.end_byte <= end)
        {
            let span = (
                destination + token.start_byte - start,
                destination + token.end_byte - start,
            );
            if let Some(&target) = by_span.get(&span) {
                insert_mapping(&mut map, token.i, target, AlignmentKind::UnchangedBytes)?;
            }
        }
    }
    for patch in map.patches.clone() {
        let old = contained_tokens(source, patch.edit.start_byte, patch.edit.end_byte);
        let new = contained_tokens(candidate, patch.candidate_start, patch.candidate_end);
        let old_lexical = old
            .iter()
            .copied()
            .filter(|token| lexical(token))
            .collect::<Vec<_>>();
        let new_lexical = new
            .iter()
            .copied()
            .filter(|token| lexical(token))
            .collect::<Vec<_>>();
        let negative = matching_rule(
            source,
            std::slice::from_ref(&patch.edit),
            &["contract_negative_auxiliary", "expand_negative_auxiliary"],
        )?
        .is_some();
        if negative
            && old.len() == 2
            && new.len() == 2
            && old[0].pos == "AUX"
            && new[0].pos == "AUX"
            && old[1].dep == "neg"
            && new[1].dep == "neg"
            && old.iter().zip(&new).all(|(a, b)| {
                normalized_lemma(a).is_some() && normalized_lemma(a) == normalized_lemma(b)
            })
        {
            for (a, b) in old.iter().zip(new) {
                insert_mapping(
                    &mut map,
                    a.i,
                    b.i,
                    AlignmentKind::LicensedNegativeContraction,
                )?;
            }
        } else if map.boundary.is_some()
            && old_lexical.len() == 1
            && new_lexical.len() == 1
            && old_lexical[0].text.to_lowercase() == new_lexical[0].text.to_lowercase()
            && old_lexical[0].pos == new_lexical[0].pos
        {
            insert_mapping(
                &mut map,
                old_lexical[0].i,
                new_lexical[0].i,
                AlignmentKind::LicensedBoundary,
            )?;
        } else if old.len() == 1
            && new.len() == 1
            && lexical(old[0])
            && lexical(new[0])
            && patch.edit.expected.trim() == old[0].text
            && patch.edit.replacement.trim() == new[0].text
        {
            insert_mapping(
                &mut map,
                old[0].i,
                new[0].i,
                AlignmentKind::SingleTokenSubstitution,
            )?;
        }
    }
    Ok(map)
}

fn operator_role(token: &Token) -> Option<OperatorRole> {
    match token.dep.as_str() {
        "aux" => Some(OperatorRole::Aux),
        "auxpass" | "aux:pass" => Some(OperatorRole::PassiveAux),
        "cop" => Some(OperatorRole::Cop),
        "neg" => Some(OperatorRole::Neg),
        _ => None,
    }
}

/// Reconstruct only the source indices omitted from the immutable feature key,
/// then cross-check their roles and lemmas against the immutable observations.
fn claims(doc: &Document) -> Result<BTreeMap<usize, Claim>> {
    let mut children = vec![Vec::new(); doc.tokens.len()];
    for token in doc.tokens.iter().filter(|token| lexical(token)) {
        if token.head != token.i {
            children[token.head].push(token.i);
        }
    }
    predicate_operators::document_observations(doc)?
        .into_iter()
        .map(|observation| {
            let head = &doc.tokens[observation.head_index];
            let mut indices = BTreeMap::new();
            if head.pos == "AUX" {
                indices.insert(head.i, OperatorRole::HeadAux);
            }
            let mut pending = vec![head.i];
            while let Some(parent) = pending.pop() {
                for &child in &children[parent] {
                    if let Some(role) = operator_role(&doc.tokens[child]) {
                        indices.insert(child, role);
                        if role != OperatorRole::Neg {
                            pending.push(child);
                        }
                    }
                }
            }
            let items = match &observation.event.operators {
                OperatorSequence::None => &[][..],
                OperatorSequence::Present { items } => items,
            };
            ensure!(
                indices.len() == items.len(),
                "Operator trace length differs from extractor"
            );
            for ((&index, &role), item) in indices.iter().zip(items) {
                ensure!(
                    role == item.role && normalized_lemma(&doc.tokens[index]) == item.lemma,
                    "Operator trace differs from frozen extractor"
                );
            }
            Ok((
                head.i,
                Claim {
                    head: head.into(),
                    event: observation.event,
                    operator_anchors: indices.keys().map(|&i| (&doc.tokens[i]).into()).collect(),
                },
            ))
        })
        .collect()
}

fn finding(
    outcome: Outcome,
    reason: Reason,
    source: Option<&Token>,
    candidate: Option<&Token>,
) -> Finding {
    Finding {
        outcome,
        reason,
        source: source.map(Into::into),
        candidate: candidate.map(Into::into),
    }
}

fn canonical_dep(token: &Token) -> &str {
    if token.i == token.head {
        "ROOT"
    } else {
        match token.dep.as_str() {
            "auxpass" | "aux:pass" => "aux:pass",
            "nsubjpass" | "nsubj:pass" => "nsubj:pass",
            other => other,
        }
    }
}

fn boundary_attachment(
    source: &Document,
    candidate: &Document,
    a: &Token,
    b: &Token,
    map: &Correspondence,
) -> bool {
    if map.boundary.is_none() {
        return false;
    }
    let x = canonical_dep(a);
    let y = canonical_dep(b);
    if (x == "ROOT" && matches!(y, "conj" | "parataxis"))
        || (y == "ROOT" && matches!(x, "conj" | "parataxis"))
    {
        let (nonroot_doc, nonroot, root_doc, root, lookup_forward) = if x == "ROOT" {
            (candidate, b, source, a, false)
        } else {
            (source, a, candidate, b, true)
        };
        let parent = &nonroot_doc.tokens[nonroot.head];
        let mapped_parent = if lookup_forward {
            map.forward.get(&parent.i).map(|&(i, _)| i)
        } else {
            map.reverse.get(&parent.i).copied()
        };
        return parent.head == parent.i
            && mapped_parent.is_some_and(|i| {
                root_doc.tokens[i].head == i && root_doc.tokens[i].sentence != root.sentence
            })
            && matches!(a.pos.as_str(), "VERB" | "AUX")
            && a.pos == b.pos;
    }
    // The retained And/But may attach to either independent clause root.
    if x == "cc" && y == "cc" && matches!(normalized_lemma(a).as_deref(), Some("and" | "but")) {
        let pa = &source.tokens[a.head];
        let pb = &candidate.tokens[b.head];
        return matches!(canonical_dep(pa), "ROOT" | "conj" | "parataxis")
            && matches!(canonical_dep(pb), "ROOT" | "conj" | "parataxis")
            && map.forward.contains_key(&pa.i)
            && map.reverse.contains_key(&pb.i);
    }
    false
}

fn token_findings(
    source: &Document,
    candidate: &Document,
    a: &Token,
    b: &Token,
    map: &Correspondence,
) -> Vec<Finding> {
    let mut result = Vec::new();
    let mut add = |outcome, reason| result.push(finding(outcome, reason, Some(a), Some(b)));
    match (normalized_lemma(a), normalized_lemma(b)) {
        (Some(x), Some(y)) if x != y => add(Outcome::Changed, Reason::LemmaChanged),
        (None, _) | (_, None) => add(Outcome::Unresolved, Reason::MissingLemma),
        _ => (),
    }
    if a.pos != b.pos {
        add(Outcome::Changed, Reason::PartOfSpeechChanged);
    }
    let sorted_morph = |token: &Token| {
        token
            .morph
            .iter()
            .map(|(k, v)| {
                let mut v = v.clone();
                v.sort();
                (k.clone(), v)
            })
            .collect::<BTreeMap<_, _>>()
    };
    if sorted_morph(a) != sorted_morph(b) {
        add(Outcome::Changed, Reason::MorphologyChanged);
    }
    if a.tag != b.tag {
        add(Outcome::Changed, Reason::TagChanged);
    }
    let boundary = boundary_attachment(source, candidate, a, b, map);
    if canonical_dep(a) != canonical_dep(b) && !boundary {
        add(Outcome::Changed, Reason::DependencyChanged);
    }
    match map.forward.get(&a.head) {
        Some(&(head, _)) if head != b.head && !boundary => {
            add(Outcome::Changed, Reason::AttachmentChanged)
        }
        None if !boundary => add(Outcome::Unresolved, Reason::AttachmentUnmapped),
        _ => (),
    }
    result
}

/// Unmapped spans in a replacement containing lexical text on both sides remain
/// ambiguous. A one-sided lexical deletion/insertion is an observed change.
fn unmapped_outcome(
    token: &Token,
    old_side: bool,
    source: &Document,
    candidate: &Document,
    map: &Correspondence,
) -> Outcome {
    for patch in &map.patches {
        let (start, end, other_doc, other_start, other_end) = if old_side {
            (
                patch.edit.start_byte,
                patch.edit.end_byte,
                candidate,
                patch.candidate_start,
                patch.candidate_end,
            )
        } else {
            (
                patch.candidate_start,
                patch.candidate_end,
                source,
                patch.edit.start_byte,
                patch.edit.end_byte,
            )
        };
        if token.start_byte >= start && token.end_byte <= end {
            return if contained_tokens(other_doc, other_start, other_end)
                .iter()
                .any(|token| lexical(token))
            {
                Outcome::Unresolved
            } else {
                Outcome::Changed
            };
        }
    }
    Outcome::Unresolved
}

fn owning_claim(
    doc: &Document,
    mut index: usize,
    claims: &BTreeMap<usize, Claim>,
) -> Option<usize> {
    loop {
        if claims.contains_key(&index) {
            return Some(index);
        }
        let parent = doc.tokens[index].head;
        if parent == index {
            return None;
        }
        index = parent;
    }
}

/// Compare validated annotations of the exact declared edit application.
/// No parsing, corpus access, model fitting or semantic inference occurs here.
pub fn compare(source: &Document, candidate: &Document, edits: &[Edit]) -> Result<GuardReport> {
    syntax::validate(source).context("Invalid source document")?;
    syntax::validate(candidate).context("Invalid candidate document")?;
    ensure!(
        source.parser_identity == candidate.parser_identity,
        "Source/candidate parser identities differ"
    );
    ensure!(
        edits::apply(&source.text, edits)? == candidate.text,
        "Candidate text differs from exact edit application"
    );
    let map = correspondence(source, candidate, edits)?;
    let old_claims = claims(source)?;
    let new_claims = claims(candidate)?;
    let mut report = GuardReport {
        schema: SCHEMA.into(),
        parser_identity: source.parser_identity.clone(),
        source_sha256: edits::digest(&source.text),
        candidate_sha256: edits::digest(&candidate.text),
        outcome: Outcome::ObservedPreserved,
        local_operator_outcome: Outcome::ObservedPreserved,
        content_scope_outcome: Outcome::ObservedPreserved,
        semantic_equivalence_certified: false,
        licensed_boundary_rule: map.boundary.clone(),
        predicates: vec![],
        content_checks: vec![],
        added_predicates: vec![],
        deleted_predicates: vec![],
        unresolved_predicates: vec![],
        added_operators: vec![],
        deleted_operators: vec![],
        unresolved_operators: vec![],
        alignment: vec![],
    };
    for (&i, &(j, kind)) in &map.forward {
        report.alignment.push(TokenAlignment {
            source: (&source.tokens[i]).into(),
            candidate: (&candidate.tokens[j]).into(),
            kind,
        });
    }
    let old_ops = old_claims
        .values()
        .flat_map(|claim| claim.operator_anchors.iter().map(|a| a.token_index))
        .collect::<BTreeSet<_>>();
    let new_ops = new_claims
        .values()
        .flat_map(|claim| claim.operator_anchors.iter().map(|a| a.token_index))
        .collect::<BTreeSet<_>>();
    for (old_side, doc, other_doc, operators, other_operators) in [
        (true, source, candidate, &old_ops, &new_ops),
        (false, candidate, source, &new_ops, &old_ops),
    ] {
        for token in doc.tokens.iter().filter(|token| !token.is_space) {
            let matched = if old_side {
                map.forward.get(&token.i).map(|&(i, _)| i)
            } else {
                map.reverse.get(&token.i).copied()
            };
            if let Some(other) = matched {
                if old_side && lexical(token) {
                    report.content_checks.extend(token_findings(
                        source,
                        candidate,
                        token,
                        &other_doc.tokens[other],
                        &map,
                    ));
                }
                if operators.contains(&token.i) && !other_operators.contains(&other) {
                    let f = if old_side {
                        finding(
                            Outcome::Changed,
                            Reason::OperatorDeleted,
                            Some(token),
                            Some(&other_doc.tokens[other]),
                        )
                    } else {
                        finding(
                            Outcome::Changed,
                            Reason::OperatorAdded,
                            Some(&other_doc.tokens[other]),
                            Some(token),
                        )
                    };
                    if old_side {
                        report.deleted_operators.push(f);
                    } else {
                        report.added_operators.push(f);
                    }
                }
                continue;
            }
            if !lexical(token) {
                if map.boundary.is_none() {
                    report.content_checks.push(finding(
                        Outcome::Unresolved,
                        Reason::PunctuationChangeUnresolved,
                        old_side.then_some(token),
                        (!old_side).then_some(token),
                    ));
                }
                continue;
            }
            let outcome = unmapped_outcome(token, old_side, source, candidate, &map);
            let reason = match (old_side, outcome) {
                (_, Outcome::Unresolved) => Reason::ContentAlignmentUnresolved,
                (true, _) => Reason::ContentDeleted,
                (false, _) => Reason::ContentAdded,
            };
            report.content_checks.push(finding(
                outcome,
                reason,
                old_side.then_some(token),
                (!old_side).then_some(token),
            ));
            if operators.contains(&token.i) {
                let reason = match (old_side, outcome) {
                    (_, Outcome::Unresolved) => Reason::OperatorAlignmentUnresolved,
                    (true, _) => Reason::OperatorDeleted,
                    (false, _) => Reason::OperatorAdded,
                };
                let f = finding(
                    outcome,
                    reason,
                    old_side.then_some(token),
                    (!old_side).then_some(token),
                );
                if outcome == Outcome::Unresolved {
                    report.unresolved_operators.push(f);
                } else if old_side {
                    report.deleted_operators.push(f);
                } else {
                    report.added_operators.push(f);
                }
            }
        }
    }
    for (old_side, doc, other_doc, these, others) in [
        (true, source, candidate, &old_claims, &new_claims),
        (false, candidate, source, &new_claims, &old_claims),
    ] {
        for (&head, claim) in these {
            let matched = if old_side {
                map.forward.get(&head).map(|&(i, _)| i)
            } else {
                map.reverse.get(&head).copied()
            };
            if let Some(other_head) = matched.filter(|i| others.contains_key(i)) {
                if !old_side {
                    continue;
                }
                let other = &others[&other_head];
                let mut local = Vec::new();
                let a = &source.tokens[head];
                let b = &candidate.tokens[other_head];
                if claim.event.operators != other.event.operators {
                    local.push(finding(
                        Outcome::Changed,
                        Reason::OperatorSequenceChanged,
                        Some(a),
                        Some(b),
                    ));
                }
                if claim.event.finite != other.event.finite {
                    local.push(finding(
                        Outcome::Changed,
                        Reason::FiniteEvidenceChanged,
                        Some(a),
                        Some(b),
                    ));
                }
                let correspondence_matches = claim.operator_anchors.len()
                    == other.operator_anchors.len()
                    && claim
                        .operator_anchors
                        .iter()
                        .zip(&other.operator_anchors)
                        .all(|(a, b)| {
                            map.forward
                                .get(&a.token_index)
                                .is_some_and(|&(j, _)| j == b.token_index)
                        });
                if !correspondence_matches {
                    let known_changed = claim
                        .operator_anchors
                        .iter()
                        .any(|a| map.forward.contains_key(&a.token_index))
                        || claim.operator_anchors.is_empty()
                        || other.operator_anchors.is_empty();
                    local.push(finding(
                        if known_changed {
                            Outcome::Changed
                        } else {
                            Outcome::Unresolved
                        },
                        Reason::OperatorCorrespondenceChanged,
                        Some(a),
                        Some(b),
                    ));
                }
                if claim
                    .operator_anchors
                    .iter()
                    .chain(&other.operator_anchors)
                    .any(|a| a.lemma.is_none())
                {
                    local.push(finding(
                        Outcome::Unresolved,
                        Reason::MissingLemma,
                        Some(a),
                        Some(b),
                    ));
                }
                let mut content = token_findings(source, candidate, a, b, &map);
                for finding in &report.content_checks {
                    let belongs = finding.source.as_ref().is_some_and(|anchor| {
                        owning_claim(source, anchor.token_index, &old_claims) == Some(head)
                    }) || finding.candidate.as_ref().is_some_and(|anchor| {
                        owning_claim(candidate, anchor.token_index, &new_claims) == Some(other_head)
                    });
                    if belongs && !content.contains(finding) {
                        content.push(finding.clone());
                    }
                }
                let local_operator_outcome = combine(local.iter().map(|f| f.outcome));
                let content_scope_outcome = combine(content.iter().map(|f| f.outcome));
                local.extend(content);
                report.predicates.push(PredicateCheck {
                    source: a.into(),
                    candidate: b.into(),
                    alignment: map.forward[&head].1,
                    local_operator_outcome,
                    content_scope_outcome,
                    findings: local,
                    source_claim: claim.clone(),
                    candidate_claim: other.clone(),
                });
            } else {
                let token = &doc.tokens[head];
                let outcome = if matched.is_some() {
                    Outcome::Changed
                } else {
                    unmapped_outcome(token, old_side, source, candidate, &map)
                };
                let reason = match (old_side, outcome) {
                    (_, Outcome::Unresolved) => Reason::PredicateAlignmentUnresolved,
                    (true, _) => Reason::PredicateDeleted,
                    (false, _) => Reason::PredicateAdded,
                };
                let f = if old_side {
                    finding(
                        outcome,
                        reason,
                        Some(token),
                        matched.map(|i| &other_doc.tokens[i]),
                    )
                } else {
                    finding(
                        outcome,
                        reason,
                        matched.map(|i| &other_doc.tokens[i]),
                        Some(token),
                    )
                };
                if outcome == Outcome::Unresolved {
                    report.unresolved_predicates.push(f);
                } else if old_side {
                    report.deleted_predicates.push(f);
                } else {
                    report.added_predicates.push(f);
                }
            }
        }
    }
    if old_claims.is_empty() || new_claims.is_empty() {
        report.unresolved_predicates.push(finding(
            Outcome::Unresolved,
            Reason::NoPredicateObservations,
            None,
            None,
        ));
    }
    report.local_operator_outcome = combine(
        report
            .predicates
            .iter()
            .map(|p| p.local_operator_outcome)
            .chain(
                report
                    .added_operators
                    .iter()
                    .chain(&report.deleted_operators)
                    .chain(&report.unresolved_operators)
                    .map(|f| f.outcome),
            ),
    );
    report.content_scope_outcome = combine(
        report
            .content_checks
            .iter()
            .map(|f| f.outcome)
            .chain(report.predicates.iter().map(|p| p.content_scope_outcome))
            .chain(
                report
                    .added_predicates
                    .iter()
                    .chain(&report.deleted_predicates)
                    .chain(&report.unresolved_predicates)
                    .map(|f| f.outcome),
            ),
    );
    report.outcome = combine([report.local_operator_outcome, report.content_scope_outcome]);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    type Spec<'a> = (&'a str, &'a str, &'a str, usize, &'a str);

    fn doc(text: &str, specs: &[Spec<'_>], sentence_ends: &[usize]) -> Document {
        let mut cursor = 0;
        let mut tokens = Vec::new();
        for (i, &(word, pos, dep, head, tag)) in specs.iter().enumerate() {
            let start = cursor + text[cursor..].find(word).unwrap();
            cursor = start + word.len();
            tokens.push(Token {
                i,
                start_byte: start,
                end_byte: cursor,
                text: word.into(),
                lemma: word.to_lowercase(),
                pos: pos.into(),
                dep: dep.into(),
                head,
                tag: tag.into(),
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: pos == "PUNCT",
                is_space: false,
            });
        }
        let mut start = 0;
        let mut sentences = Vec::new();
        for &end in sentence_ends {
            for token in &mut tokens[start..end] {
                token.sentence = sentences.len();
            }
            sentences.push(Sentence {
                start_byte: tokens[start].start_byte,
                end_byte: tokens[end - 1].end_byte,
                root: tokens[start..end].iter().find(|t| t.head == t.i).unwrap().i,
                token_start: start,
                token_end: end,
            });
            start = end;
        }
        let result = Document {
            text: text.into(),
            parser_identity: "claim-guard-synthetic-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&result).unwrap();
        result
    }

    fn patch(text: &str, expected: &str, replacement: &str) -> Edit {
        let start = text.find(expected).unwrap();
        Edit {
            start_byte: start,
            end_byte: start + expected.len(),
            expected: expected.into(),
            replacement: replacement.into(),
        }
    }

    fn modal(name: &str, modal: &str) -> Document {
        doc(
            &format!("{name} {modal} leave."),
            &[
                (name, "PROPN", "nsubj", 2, "NNP"),
                (modal, "AUX", "aux", 2, "MD"),
                ("leave", "VERB", "ROOT", 2, "VB"),
                (".", "PUNCT", "punct", 2, "."),
            ],
            &[4],
        )
    }

    #[test]
    fn exact_no_change_and_input_validation_are_explicit() {
        let source = modal("Mira", "may");
        let result = compare(&source, &source, &[]).unwrap();
        assert_eq!(result.outcome, Outcome::ObservedPreserved);
        assert!(!result.semantic_equivalence_certified);
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(
            result.predicates[0].source_claim.operator_anchors[0].text,
            "may"
        );
        let mut bad = source.clone();
        bad.parser_identity.push('x');
        assert!(compare(&source, &bad, &[]).is_err());
        assert!(compare(&source, &modal("Mira", "must"), &[]).is_err());
        let mut stale = patch(&source.text, "may", "must");
        stale.expected = "can".into();
        assert!(compare(&source, &modal("Mira", "must"), &[stale]).is_err());
        let mut bad = source.clone();
        bad.tokens[1].end_byte -= 1;
        assert!(compare(&source, &bad, &[]).is_err());
    }

    #[test]
    fn utf8_modal_substitution_tracks_source_heads_and_detects_change() {
        let source = modal("Zoë", "may");
        let candidate = modal("Zoë", "must");
        let report = compare(&source, &candidate, &[patch(&source.text, "may", "must")]).unwrap();
        assert_eq!(report.outcome, Outcome::Changed);
        assert_eq!(report.local_operator_outcome, Outcome::Changed);
        assert_eq!(
            report.predicates[0].alignment,
            AlignmentKind::UnchangedBytes
        );
        assert_eq!(
            report.predicates[0].source.start_byte + 1,
            report.predicates[0].candidate.start_byte
        );
        assert!(
            report.predicates[0]
                .findings
                .iter()
                .any(|f| f.reason == Reason::OperatorSequenceChanged)
        );
    }

    #[test]
    fn subject_substitution_cannot_pass_on_preserved_operators() {
        let source = modal("Mira", "may");
        let candidate = modal("Nora", "may");
        let report = compare(&source, &candidate, &[patch(&source.text, "Mira", "Nora")]).unwrap();
        assert_eq!(report.local_operator_outcome, Outcome::ObservedPreserved);
        assert_eq!(report.content_scope_outcome, Outcome::Changed);
        assert_eq!(report.predicates[0].content_scope_outcome, Outcome::Changed);
        assert_eq!(report.outcome, Outcome::Changed);
    }

    #[test]
    fn same_lemma_finite_head_substitution_still_checks_tense() {
        let source = doc(
            "Mira leaves.",
            &[
                ("Mira", "PROPN", "nsubj", 1, "NNP"),
                ("leaves", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[3],
        );
        let mut candidate = doc(
            "Mira left.",
            &[
                ("Mira", "PROPN", "nsubj", 1, "NNP"),
                ("left", "VERB", "ROOT", 1, "VBD"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[3],
        );
        let mut source = source;
        source.tokens[1].lemma = "leave".into();
        candidate.tokens[1].lemma = "leave".into();
        let report = compare(
            &source,
            &candidate,
            &[patch(&source.text, "leaves", "left")],
        )
        .unwrap();
        assert_eq!(
            report.predicates[0].alignment,
            AlignmentKind::SingleTokenSubstitution
        );
        assert_eq!(report.outcome, Outcome::Changed);
        assert!(
            report.predicates[0]
                .findings
                .iter()
                .any(|f| f.reason == Reason::FiniteEvidenceChanged)
        );
    }

    fn negative(contracted: bool, auxiliary_head: bool) -> Document {
        let (text, aux, neg, lemma, pos, tag) = if auxiliary_head {
            (
                if contracted {
                    "She isn't ready."
                } else {
                    "She is not ready."
                },
                "is",
                if contracted { "n't" } else { "not" },
                "be",
                "ADJ",
                "JJ",
            )
        } else {
            (
                if contracted {
                    "We don't leave."
                } else {
                    "We do not leave."
                },
                "do",
                if contracted { "n't" } else { "not" },
                "do",
                "VERB",
                "VB",
            )
        };
        let head = if auxiliary_head { 1 } else { 3 };
        let mut result = doc(
            text,
            &[
                (
                    if auxiliary_head { "She" } else { "We" },
                    "PRON",
                    "nsubj",
                    head,
                    "PRP",
                ),
                (
                    aux,
                    "AUX",
                    if auxiliary_head { "ROOT" } else { "aux" },
                    head,
                    if auxiliary_head { "VBZ" } else { "VBP" },
                ),
                (neg, "PART", "neg", head, "RB"),
                (
                    if auxiliary_head { "ready" } else { "leave" },
                    pos,
                    if auxiliary_head { "acomp" } else { "ROOT" },
                    head,
                    tag,
                ),
                (".", "PUNCT", "punct", head, "."),
            ],
            &[5],
        );
        result.tokens[1].lemma = lemma.into();
        result.tokens[2].lemma = "not".into();
        result
    }

    #[test]
    fn licensed_contraction_and_expansion_align_auxiliary_heads_and_operators() {
        for auxiliary_head in [false, true] {
            let source = negative(false, auxiliary_head);
            let candidate = negative(true, auxiliary_head);
            let (full, short) = if auxiliary_head {
                ("is not", "isn't")
            } else {
                ("do not", "don't")
            };
            for (a, b, expected, replacement) in [
                (&source, &candidate, full, short),
                (&candidate, &source, short, full),
            ] {
                let report = compare(a, b, &[patch(&a.text, expected, replacement)]).unwrap();
                assert_eq!(report.outcome, Outcome::ObservedPreserved, "{report:#?}");
                assert!(
                    report
                        .alignment
                        .iter()
                        .any(|a| a.kind == AlignmentKind::LicensedNegativeContraction)
                );
                assert!(report.added_operators.is_empty() && report.deleted_operators.is_empty());
            }
        }
    }

    #[test]
    fn broad_replacement_does_not_align_repeated_lemmas_by_counts() {
        let source = modal("Mira", "may");
        let candidate = modal("Nora", "may");
        let report = compare(
            &source,
            &candidate,
            &[patch(&source.text, &source.text, &candidate.text)],
        )
        .unwrap();
        assert_eq!(report.outcome, Outcome::Unresolved);
        assert!(report.predicates.is_empty());
        assert_eq!(report.unresolved_predicates.len(), 2);
        assert_eq!(report.unresolved_operators.len(), 2);
    }

    #[test]
    fn negation_movement_between_predicates_is_not_an_aggregate_match() {
        let source = doc(
            "We do not leave. They do stay.",
            &[
                ("We", "PRON", "nsubj", 3, "PRP"),
                ("do", "AUX", "aux", 3, "VBP"),
                ("not", "PART", "neg", 3, "RB"),
                ("leave", "VERB", "ROOT", 3, "VB"),
                (".", "PUNCT", "punct", 3, "."),
                ("They", "PRON", "nsubj", 7, "PRP"),
                ("do", "AUX", "aux", 7, "VBP"),
                ("stay", "VERB", "ROOT", 7, "VB"),
                (".", "PUNCT", "punct", 7, "."),
            ],
            &[5, 9],
        );
        let candidate = doc(
            "We do leave. They do not stay.",
            &[
                ("We", "PRON", "nsubj", 2, "PRP"),
                ("do", "AUX", "aux", 2, "VBP"),
                ("leave", "VERB", "ROOT", 2, "VB"),
                (".", "PUNCT", "punct", 2, "."),
                ("They", "PRON", "nsubj", 7, "PRP"),
                ("do", "AUX", "aux", 7, "VBP"),
                ("not", "PART", "neg", 7, "RB"),
                ("stay", "VERB", "ROOT", 7, "VB"),
                (".", "PUNCT", "punct", 7, "."),
            ],
            &[4, 9],
        );
        assert_eq!(
            predicate_operators::document_family(&source).unwrap(),
            predicate_operators::document_family(&candidate).unwrap()
        );
        let report = compare(
            &source,
            &candidate,
            &[
                patch(&source.text, "not ", ""),
                Edit {
                    start_byte: source.text.find("stay").unwrap(),
                    end_byte: source.text.find("stay").unwrap(),
                    expected: String::new(),
                    replacement: "not ".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(report.outcome, Outcome::Changed);
        assert_eq!(report.deleted_operators.len(), 1);
        assert_eq!(report.added_operators.len(), 1);
        assert!(
            report
                .predicates
                .iter()
                .all(|p| p.local_operator_outcome == Outcome::Changed)
        );
    }

    #[test]
    fn attachment_to_a_different_auxiliary_is_detected_even_with_the_same_role_path() {
        let source = doc(
            "We may have not left.",
            &[
                ("We", "PRON", "nsubj", 4, "PRP"),
                ("may", "AUX", "aux", 4, "MD"),
                ("have", "AUX", "aux", 4, "VB"),
                ("not", "PART", "neg", 1, "RB"),
                ("left", "VERB", "ROOT", 4, "VBN"),
                (".", "PUNCT", "punct", 4, "."),
            ],
            &[6],
        );
        let mut candidate = source.clone();
        candidate.tokens[3].head = 2;
        let report = compare(&source, &candidate, &[]).unwrap();
        assert_eq!(report.local_operator_outcome, Outcome::Changed);
        assert!(
            report
                .content_checks
                .iter()
                .any(|f| f.reason == Reason::AttachmentChanged)
        );
    }

    #[test]
    fn boundary_exception_requires_the_exact_frozen_rule_candidate() {
        let source = doc(
            "We close, and they open.",
            &[
                ("We", "PRON", "nsubj", 1, "PRP"),
                ("close", "VERB", "ROOT", 1, "VBP"),
                (",", "PUNCT", "punct", 1, ","),
                ("and", "CCONJ", "cc", 1, "CC"),
                ("they", "PRON", "nsubj", 5, "PRP"),
                ("open", "VERB", "conj", 1, "VBP"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[7],
        );
        let candidate = doc(
            "We close. And they open.",
            &[
                ("We", "PRON", "nsubj", 1, "PRP"),
                ("close", "VERB", "ROOT", 1, "VBP"),
                (".", "PUNCT", "punct", 1, "."),
                ("And", "CCONJ", "cc", 5, "CC"),
                ("they", "PRON", "nsubj", 5, "PRP"),
                ("open", "VERB", "ROOT", 5, "VBP"),
                (".", "PUNCT", "punct", 5, "."),
            ],
            &[3, 7],
        );
        let report = compare(
            &source,
            &candidate,
            &[patch(&source.text, ", and", ". And")],
        )
        .unwrap();
        assert_eq!(
            report.licensed_boundary_rule.as_deref(),
            Some("split_independent_coordination")
        );
        assert_eq!(report.outcome, Outcome::ObservedPreserved, "{report:#?}");
        let report = compare(
            &source,
            &candidate,
            &[patch(&source.text, &source.text, &candidate.text)],
        )
        .unwrap();
        assert!(report.licensed_boundary_rule.is_none());
        assert_eq!(report.outcome, Outcome::Unresolved);
        let mut candidate = candidate;
        candidate.tokens[4].head = 3;
        let report = compare(
            &source,
            &candidate,
            &[patch(&source.text, ", and", ". And")],
        )
        .unwrap();
        assert_eq!(report.outcome, Outcome::Changed);
    }

    #[test]
    fn missing_lemma_and_unmapped_retokenization_cannot_pass() {
        let mut source = modal("Mira", "may");
        source.tokens[1].lemma = " ".into();
        assert_eq!(
            compare(&source, &source, &[]).unwrap().outcome,
            Outcome::Unresolved
        );
        let source = modal("Mira", "may");
        let candidate = doc(
            "Mira may leave.",
            &[
                ("Mi", "PROPN", "compound", 1, "NNP"),
                ("ra", "PROPN", "nsubj", 3, "NNP"),
                ("may", "AUX", "aux", 3, "MD"),
                ("leave", "VERB", "ROOT", 3, "VB"),
                (".", "PUNCT", "punct", 3, "."),
            ],
            &[5],
        );
        let report = compare(&source, &candidate, &[]).unwrap();
        assert_eq!(report.outcome, Outcome::Unresolved);
    }

    #[test]
    fn predicate_deletion_and_nonlexical_documents_remain_visible() {
        let source = doc(
            "We leave and they stay.",
            &[
                ("We", "PRON", "nsubj", 1, "PRP"),
                ("leave", "VERB", "ROOT", 1, "VBP"),
                ("and", "CCONJ", "cc", 1, "CC"),
                ("they", "PRON", "nsubj", 4, "PRP"),
                ("stay", "VERB", "conj", 1, "VBP"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[6],
        );
        let candidate = doc(
            "We leave.",
            &[
                ("We", "PRON", "nsubj", 1, "PRP"),
                ("leave", "VERB", "ROOT", 1, "VBP"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[3],
        );
        let report = compare(
            &source,
            &candidate,
            &[patch(&source.text, " and they stay", "")],
        )
        .unwrap();
        assert_eq!(report.deleted_predicates.len(), 1);
        assert_eq!(report.outcome, Outcome::Changed);
        let empty = Document {
            text: String::new(),
            parser_identity: "synthetic".into(),
            tokens: vec![],
            sentences: vec![],
        };
        assert_eq!(
            compare(&empty, &empty, &[]).unwrap().outcome,
            Outcome::Unresolved
        );
    }
}
