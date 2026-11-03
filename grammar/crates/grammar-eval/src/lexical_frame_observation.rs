//! Anchored observations and conservative VerbNet syntax bindings.
//!
//! This is an open-world ledger. A checked syntactic assignment neither selects
//! a verb sense nor certifies the semantic restrictions or licenses an edit.

use crate::lexical_context::{lexical, normalized_lemma};
use crate::predicate_operators::{self, FiniteSignature, PredicateEvent};
use crate::verbnet_resource::{Class, Frame, Resource, Role, XmlElement, XmlNode};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits,
    syntax::{self, Document, Token},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-lexical-frame-observation-v1";
pub const EXTENDED_SCHEMA: &str = "slopninja-lexical-frame-observation-v2";
pub const ALTERNATION_SCHEMA: &str = "slopninja-lexical-frame-observation-v3";
const GIVE_SOURCE_SHA256: &str = "bbddd2c3cbe0a8a6862eee7a140827938921df2eaf7fc2852f3f4d133800fbee";

/// Explicit parser-label mapping; neither mode changes the source annotations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mapping {
    BaselineV1,
    PrepositionalDativeV1,
    DativeAlternationV1,
}

impl Mapping {
    pub fn identity(self) -> &'static str {
        match self {
            Self::BaselineV1 => "baseline_v1",
            Self::PrepositionalDativeV1 => "prepositional_dative_v1",
            Self::DativeAlternationV1 => "dative_alternation_v1",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Anchor {
    pub token: Token,
    pub normalized_lemma: Option<String>,
    pub lexical: bool,
}

impl From<&Token> for Anchor {
    fn from(token: &Token) -> Self {
        Self {
            token: token.clone(),
            normalized_lemma: normalized_lemma(token),
            lexical: lexical(token),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Dependent {
    pub anchor: Anchor,
    /// Exact token membership, including punctuation; never a bounding-span guess.
    pub subtree: Vec<Anchor>,
    pub proposition_head_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Checked,
    Contradicted,
    Unresolved,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub domain: String,
    pub constraint: String,
    pub syntax_index: Option<usize>,
    pub status: Status,
    pub evidence: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct BindingAlternative {
    pub kind: String,
    pub anchor: Anchor,
    pub supporting_anchors: Vec<Anchor>,
    pub proposition_head_index: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Binding {
    pub syntax_index: usize,
    pub role_name: Option<String>,
    /// Effective role declaration from this member class, with its XML provenance.
    pub role_declaration: Option<Role>,
    /// All nearest-class declarations, including duplicate origins or conflicts.
    pub role_declarations: Vec<Role>,
    pub alternatives: Vec<BindingAlternative>,
    pub unique: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameObservation {
    pub member_class_id: String,
    pub member_ordinal: usize,
    pub frame: Frame,
    pub bindings: Vec<Binding>,
    pub findings: Vec<Finding>,
    pub syntactic_binding_status: Status,
    pub syntax_binding_complete: bool,
    pub full_compatibility_status: Status,
    pub unbound_observed_arguments: Vec<Anchor>,
    pub sense_selected: bool,
    pub automatic_edit_license: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PredicateLedger {
    pub head: Anchor,
    pub raw_parent: Anchor,
    pub particle_anchors: Vec<Anchor>,
    pub original_operator_event: PredicateEvent,
    pub direct_dependents: Vec<Dependent>,
    pub head_support: Vec<Finding>,
    pub lookup: Value,
    pub frame_possibilities: Vec<FrameObservation>,
    pub open_world_unknown_use_possible: bool,
    pub automatic_edit_license: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObservationReport {
    pub schema: String,
    /// Absent in the unchanged v1 serialization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping_identity: Option<String>,
    pub source_sha256: String,
    pub parser_identity: String,
    pub resource_schema: String,
    pub resource_sources: Vec<Value>,
    pub deliberately_masked_lemmas: Vec<String>,
    pub predicate_count: usize,
    pub predicates: Vec<PredicateLedger>,
    pub semantic_equivalence_certified: bool,
    pub automatic_edit_license: bool,
}

fn finding(
    domain: &str,
    constraint: &str,
    index: Option<usize>,
    status: Status,
    evidence: Value,
) -> Finding {
    Finding {
        domain: domain.into(),
        constraint: constraint.into(),
        syntax_index: index,
        status,
        evidence,
    }
}

fn aggregate<'a>(items: impl Iterator<Item = &'a Finding>) -> Status {
    let statuses: Vec<_> = items.map(|item| item.status).collect();
    if statuses.contains(&Status::Contradicted) {
        Status::Contradicted
    } else if statuses.contains(&Status::Unresolved) {
        Status::Unresolved
    } else {
        Status::Checked
    }
}

fn finite(event: &PredicateEvent) -> bool {
    matches!(&event.finite, FiniteSignature::One {carrier} if carrier.other_verb_forms.is_empty())
}

fn nominal(token: &Token) -> bool {
    lexical(token) && matches!(token.pos.as_str(), "NOUN" | "PROPN" | "PRON")
}

fn descendants(head: usize, children: &[Vec<usize>]) -> Vec<usize> {
    let mut result = Vec::new();
    let mut pending = vec![head];
    while let Some(index) = pending.pop() {
        result.push(index);
        pending.extend(children[index].iter().copied());
    }
    result.sort_unstable();
    result
}

fn argument_relation(dep: &str) -> bool {
    matches!(
        dep,
        "nsubj"
            | "nsubjpass"
            | "nsubj:pass"
            | "csubj"
            | "csubjpass"
            | "csubj:pass"
            | "dobj"
            | "obj"
            | "iobj"
            | "dative"
            | "prep"
            | "obl"
            | "ccomp"
            | "xcomp"
    )
}

#[derive(Clone)]
struct Slot {
    kind: &'static str,
    root: usize,
    direct: usize,
    supporting: Vec<usize>,
    proposition: Option<usize>,
}

fn observed_slots(
    doc: &Document,
    head: usize,
    children: &[Vec<usize>],
    events: &BTreeMap<usize, PredicateEvent>,
    mapping: Mapping,
) -> Vec<Slot> {
    let mut slots = Vec::new();
    for &index in &children[head] {
        let token = &doc.tokens[index];
        let kind = match token.dep.as_str() {
            "nsubj" if nominal(token) && index < head => Some("active_subject"),
            "dobj" | "obj" if nominal(token) && index > head => Some("nominal_object"),
            "ccomp"
                if index > head
                    && matches!(token.pos.as_str(), "VERB" | "AUX")
                    && events.get(&index).is_some_and(finite)
                    && children[index].iter().any(|&i| {
                        doc.tokens[i].dep == "nsubj" && nominal(&doc.tokens[i]) && i < index
                    }) =>
            {
                Some("finite_complement")
            }
            _ => None,
        };
        if let Some(kind) = kind {
            let supporting = if kind == "finite_complement" {
                children[index]
                    .iter()
                    .copied()
                    .filter(|&i| {
                        lexical(&doc.tokens[i])
                            && matches!(doc.tokens[i].dep.as_str(), "mark" | "complm")
                            && normalized_lemma(&doc.tokens[i]).as_deref() == Some("that")
                    })
                    .collect()
            } else {
                Vec::new()
            };
            slots.push(Slot {
                kind,
                root: index,
                direct: index,
                supporting,
                proposition: (kind == "finite_complement").then_some(index),
            });
        }
        if (token.dep == "prep"
            || (matches!(
                mapping,
                Mapping::PrepositionalDativeV1 | Mapping::DativeAlternationV1
            ) && token.dep == "dative"
                && doc.tokens[head].pos == "VERB"))
            && token.pos == "ADP"
            && lexical(token)
            && index > head
        {
            for &object in &children[index] {
                if doc.tokens[object].dep == "pobj"
                    && nominal(&doc.tokens[object])
                    && object > index
                {
                    slots.push(Slot {
                        kind: "prepositional_object",
                        root: object,
                        direct: index,
                        supporting: vec![index],
                        proposition: None,
                    });
                }
            }
        }
        if token.dep == "obl" && nominal(token) && index > head {
            let markers: Vec<_> = children[index]
                .iter()
                .copied()
                .filter(|&i| {
                    let marker = &doc.tokens[i];
                    marker.dep == "case" && marker.pos == "ADP" && lexical(marker) && i < index
                })
                .collect();
            if markers.len() == 1 {
                slots.push(Slot {
                    kind: "prepositional_object",
                    root: index,
                    direct: index,
                    supporting: markers,
                    proposition: None,
                });
            }
        }
    }
    slots
}

fn alternative(doc: &Document, slot: &Slot) -> BindingAlternative {
    BindingAlternative {
        kind: slot.kind.into(),
        anchor: (&doc.tokens[slot.root]).into(),
        supporting_anchors: slot
            .supporting
            .iter()
            .map(|&i| (&doc.tokens[i]).into())
            .collect(),
        proposition_head_index: slot.proposition,
    }
}

fn pinned_double_object_frame(frame: &Frame, nodes: &[&XmlElement]) -> bool {
    frame.id == "[\"give-13.1\",1]"
        && frame.origin.declaring_class_id == "give-13.1"
        && frame.origin.source_sha256 == GIVE_SOURCE_SHA256
        && nodes.len() == 4
        && nodes[0].is("NP")
        && nodes[0].attr("value") == Some("Agent")
        && nodes[1].is("VERB")
        && nodes[2].is("NP")
        && nodes[2].attr("value") == Some("Recipient")
        && nodes[3].is("NP")
        && nodes[3].attr("value") == Some("Theme")
}

/// Bind the three participants together; a missing indirect object must never
/// cause the direct object to be assigned to the frame's Recipient slot.
fn double_object_pattern(ledger: &PredicateLedger) -> (Vec<Slot>, Value) {
    let head = &ledger.head.token;
    let direct = &ledger.direct_dependents;
    let subjects = direct
        .iter()
        .filter(|d| {
            d.anchor.token.dep == "nsubj" && nominal(&d.anchor.token) && d.anchor.token.i < head.i
        })
        .collect::<Vec<_>>();
    let indirect = direct
        .iter()
        .filter(|d| {
            matches!(d.anchor.token.dep.as_str(), "dative" | "iobj")
                && nominal(&d.anchor.token)
                && d.anchor.token.i > head.i
        })
        .collect::<Vec<_>>();
    let objects = direct
        .iter()
        .filter(|d| {
            matches!(d.anchor.token.dep.as_str(), "dobj" | "obj")
                && nominal(&d.anchor.token)
                && d.anchor.token.i > head.i
        })
        .collect::<Vec<_>>();
    let active = head.pos == "VERB"
        && ledger
            .head_support
            .iter()
            .all(|f| f.status == Status::Checked);
    let unique = subjects.len() == 1 && indirect.len() == 1 && objects.len() == 1;
    let order = unique
        && subjects[0].anchor.token.i < head.i
        && head.i < indirect[0].anchor.token.i
        && indirect[0].anchor.token.i < objects[0].anchor.token.i;
    let arguments = direct
        .iter()
        .filter(|d| argument_relation(&d.anchor.token.dep))
        .map(|d| d.anchor.token.i)
        .collect::<BTreeSet<_>>();
    let chosen = subjects
        .iter()
        .chain(&indirect)
        .chain(&objects)
        .map(|d| d.anchor.token.i)
        .collect::<BTreeSet<_>>();
    let exact_arguments = unique && chosen.len() == 3 && arguments == chosen;
    let participants = subjects
        .iter()
        .chain(&indirect)
        .chain(&objects)
        .flat_map(|d| d.subtree.iter())
        .collect::<Vec<_>>();
    let no_coordination = !matches!(head.dep.as_str(), "conj" | "cc")
        && !direct
            .iter()
            .any(|d| matches!(d.anchor.token.dep.as_str(), "conj" | "cc"))
        && !participants
            .iter()
            .any(|a| matches!(a.token.dep.as_str(), "conj" | "cc"));
    let no_case_markers = !indirect
        .iter()
        .chain(&objects)
        .flat_map(|d| &d.subtree)
        .any(|a| a.token.dep == "case");
    let no_clausal_participants = !participants.iter().any(|a| {
        matches!(a.token.pos.as_str(), "VERB" | "AUX")
            || matches!(
                a.token.dep.as_str(),
                "acl"
                    | "acl:relcl"
                    | "relcl"
                    | "ccomp"
                    | "xcomp"
                    | "csubj"
                    | "csubjpass"
                    | "csubj:pass"
            )
    });
    let supported = active
        && unique
        && order
        && exact_arguments
        && no_coordination
        && no_case_markers
        && no_clausal_participants;
    let evidence = json!({"active_supported_head":active,"subject_indices":subjects.iter().map(|d|d.anchor.token.i).collect::<Vec<_>>(),
        "indirect_object_indices":indirect.iter().map(|d|d.anchor.token.i).collect::<Vec<_>>(),"direct_object_indices":objects.iter().map(|d|d.anchor.token.i).collect::<Vec<_>>(),
        "unique_participants":unique,"ordered_participants":order,"all_direct_arguments_accounted_for":exact_arguments,
        "no_coordination":no_coordination,"no_case_markers":no_case_markers,"no_clausal_participants":no_clausal_participants,"supported":supported});
    let slots = if supported {
        [
            ("active_subject", subjects[0]),
            ("indirect_object", indirect[0]),
            ("nominal_object", objects[0]),
        ]
        .into_iter()
        .map(|(kind, d)| Slot {
            kind,
            root: d.anchor.token.i,
            direct: d.anchor.token.i,
            supporting: Vec::new(),
            proposition: None,
        })
        .collect()
    } else {
        Vec::new()
    };
    (slots, evidence)
}

fn has_restriction(element: &XmlElement, kind: &str, value: &str) -> bool {
    element.child("SYNRESTRS").is_some_and(|container| {
        container.elements().any(|r| {
            r.is("SYNRESTR") && r.attr("type") == Some(kind) && r.attr("Value") == Some(value)
        })
    })
}

fn literal_preposition(element: &XmlElement) -> Option<&str> {
    element
        .attr("value")
        .filter(|value| !value.is_empty() && value.chars().all(char::is_alphabetic))
}

fn constraint_text(element: &XmlElement) -> bool {
    element
        .children
        .iter()
        .any(|node| matches!(node,XmlNode::Text {value,..} if !value.trim().is_empty()))
}

fn restrictions(
    element: &XmlElement,
    syntax_index: usize,
    kind: &str,
    bound: bool,
    marker: bool,
) -> Vec<Finding> {
    let mut result = Vec::new();
    if constraint_text(element) {
        result.push(finding(
            "syntax",
            "unsupported_syntax_text",
            Some(syntax_index),
            Status::Unresolved,
            json!({"xml":element}),
        ));
    }
    let unsupported_attrs = element
        .attributes
        .iter()
        .filter(|a| a.namespace.is_some() || a.name != "value" || element.is("VERB"))
        .collect::<Vec<_>>();
    if !unsupported_attrs.is_empty() {
        result.push(finding(
            "syntax",
            "unsupported_syntax_attributes",
            Some(syntax_index),
            Status::Unresolved,
            json!({"attributes":unsupported_attrs}),
        ));
    }
    for (name, domain) in [("SYNRESTRS", "syntax"), ("SELRESTRS", "semantic")] {
        let count = element.elements().filter(|e| e.is(name)).count();
        if count > 1 {
            result.push(finding(
                domain,
                "duplicate_restriction_container",
                Some(syntax_index),
                Status::Unresolved,
                json!({"name":name,"count":count}),
            ));
        }
    }
    for child in element.elements() {
        if child.is("SYNRESTRS") {
            let direct: Vec<_> = child.elements().collect();
            if constraint_text(child)
                || child
                    .attributes
                    .iter()
                    .any(|a| a.namespace.is_some() || a.name != "logic")
            {
                result.push(finding(
                    "syntax",
                    "unsupported_restriction_container_attributes",
                    Some(syntax_index),
                    Status::Unresolved,
                    json!({"xml":child}),
                ));
            }
            if child.attr("logic").is_some_and(|logic| logic != "and") {
                result.push(finding(
                    "syntax",
                    "unsupported_restriction_logic",
                    Some(syntax_index),
                    Status::Unresolved,
                    json!({"xml":child}),
                ));
            }
            for restriction in direct {
                let supported = bound
                    && restriction.is("SYNRESTR")
                    && !constraint_text(restriction)
                    && restriction.elements().next().is_none()
                    && restriction.attributes.iter().all(|a| {
                        a.namespace.is_none() && matches!(a.name.as_str(), "Value" | "type")
                    })
                    && ((restriction.attr("type") == Some("that_comp")
                        && restriction.attr("Value") == Some("+")
                        && kind == "finite_complement"
                        && marker)
                        || (restriction.attr("type") == Some("sentential")
                            && restriction.attr("Value") == Some("-")
                            && matches!(
                                kind,
                                "active_subject" | "nominal_object" | "prepositional_object"
                            )));
                result.push(finding("syntax","syntax_restriction",Some(syntax_index),
                    if supported {Status::Checked} else {Status::Unresolved},json!({"xml":restriction,"observed_kind":kind,"unique_observed_binding":bound,"attached_that_marker":marker})));
            }
        } else if child.is("SELRESTRS") {
            if constraint_text(child)
                || child.elements().next().is_some()
                || child
                    .attributes
                    .iter()
                    .any(|a| a.namespace.is_some() || a.name != "logic")
                || child
                    .attr("logic")
                    .is_some_and(|logic| !matches!(logic, "and" | "or"))
            {
                result.push(finding(
                    "semantic",
                    "selectional_restrictions_unassessed",
                    Some(syntax_index),
                    Status::Unresolved,
                    json!({"xml":child}),
                ));
            }
        } else {
            result.push(finding(
                "syntax",
                "unsupported_syntax_child",
                Some(syntax_index),
                Status::Unresolved,
                json!({"xml":child}),
            ));
        }
    }
    result
}

fn bind_frame(
    doc: &Document,
    ledger: &PredicateLedger,
    slots: &[Slot],
    frame: &Frame,
    class: &Class,
    member_ordinal: usize,
    mapping: Mapping,
) -> FrameObservation {
    let head = &ledger.head.token;
    let mut findings = ledger.head_support.clone();
    let mut bindings = Vec::new();
    let mut consumed = BTreeSet::new();
    let syntax_sections = frame
        .xml
        .elements()
        .filter(|e| e.is("SYNTAX"))
        .collect::<Vec<_>>();
    if syntax_sections.len() != 1 {
        findings.push(finding(
            "syntax",
            "syntax_section_count",
            None,
            Status::Unresolved,
            json!({"count":syntax_sections.len()}),
        ));
    }
    for syntax in &syntax_sections {
        if !syntax.attributes.is_empty() || constraint_text(syntax) {
            findings.push(finding(
                "syntax",
                "unsupported_syntax_container_attributes",
                None,
                Status::Unresolved,
                json!({"xml":syntax}),
            ));
        }
    }
    if !frame.xml.attributes.is_empty() {
        findings.push(finding(
            "semantic",
            "unassessed_frame_attributes",
            None,
            Status::Unresolved,
            json!({"attributes":frame.xml.attributes}),
        ));
    }
    let nodes = frame
        .xml
        .child("SYNTAX")
        .map(|s| s.elements().collect::<Vec<_>>());
    if let Some(nodes) = nodes {
        let double_object =
            mapping == Mapping::DativeAlternationV1 && pinned_double_object_frame(frame, &nodes);
        let pattern_slots = if double_object {
            let (slots, evidence) = double_object_pattern(ledger);
            findings.push(finding(
                "syntax",
                "pinned_double_object_pattern",
                None,
                if slots.is_empty() {
                    Status::Unresolved
                } else {
                    Status::Checked
                },
                json!({"frame_id":frame.id,"origin":frame.origin,"pattern":evidence}),
            ));
            slots
        } else {
            Vec::new()
        };
        let verbs: Vec<_> = nodes
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is("VERB"))
            .map(|(i, _)| i)
            .collect();
        let shape = verbs.len() == 1
            && verbs[0] == 1
            && nodes.first().is_some_and(|e| e.is("NP"))
            && nodes
                .iter()
                .all(|e| e.is("NP") || e.is("VERB") || e.is("PREP"));
        if !shape {
            findings.push(finding(
                "syntax",
                "unsupported_frame_layout",
                None,
                Status::Unresolved,
                json!({"syntax":frame.xml.child("SYNTAX")}),
            ));
        }
        let mut nominal_objects = 0;
        for (index, node) in nodes.iter().enumerate() {
            if node.is("VERB") {
                bindings.push(Binding {
                    syntax_index: index,
                    role_name: None,
                    role_declaration: None,
                    role_declarations: Vec::new(),
                    alternatives: vec![BindingAlternative {
                        kind: "predicate".into(),
                        anchor: (&ledger.head.token).into(),
                        supporting_anchors: Vec::new(),
                        proposition_head_index: Some(head.i),
                    }],
                    unique: true,
                });
                findings.push(finding(
                    "syntax",
                    "predicate_anchor",
                    Some(index),
                    Status::Checked,
                    json!({"head_index":head.i}),
                ));
                findings.extend(restrictions(node, index, "predicate", true, false));
                continue;
            }
            if node.is("PREP") {
                let specified = literal_preposition(node);
                let candidates: Vec<_> = slots
                    .iter()
                    .filter(|s| {
                        s.kind == "prepositional_object"
                            && specified.is_some_and(|value| {
                                normalized_lemma(&doc.tokens[s.supporting[0]]).as_deref()
                                    == Some(value)
                            })
                    })
                    .collect();
                let alternatives = candidates
                    .iter()
                    .map(|slot| BindingAlternative {
                        kind: "preposition".into(),
                        anchor: (&doc.tokens[slot.supporting[0]]).into(),
                        supporting_anchors: Vec::new(),
                        proposition_head_index: None,
                    })
                    .collect();
                bindings.push(Binding {
                    syntax_index: index,
                    role_name: None,
                    role_declaration: None,
                    role_declarations: Vec::new(),
                    alternatives,
                    unique: candidates.len() == 1,
                });
                let status = if specified.is_none() {
                    Status::Unresolved
                } else if candidates.len() == 1 {
                    Status::Checked
                } else if candidates.is_empty()
                    && slots.iter().any(|s| s.kind == "prepositional_object")
                {
                    Status::Contradicted
                } else {
                    Status::Unresolved
                };
                findings.push(finding("syntax","specified_preposition",Some(index),status,json!({"raw_value":node.attr("value"),"supported_literal":specified,"candidate_count":candidates.len()})));
                if !nodes.get(index + 1).is_some_and(|e| e.is("NP")) {
                    findings.push(finding(
                        "syntax",
                        "preposition_requires_following_np",
                        Some(index),
                        Status::Unresolved,
                        json!({}),
                    ));
                }
                findings.extend(restrictions(
                    node,
                    index,
                    "preposition",
                    candidates.len() == 1,
                    false,
                ));
                continue;
            }
            if !node.is("NP") {
                findings.push(finding(
                    "syntax",
                    "unsupported_syntax_element",
                    Some(index),
                    Status::Unresolved,
                    json!({"xml":node}),
                ));
                continue;
            }
            let prior_prep = index
                .checked_sub(1)
                .and_then(|i| nodes.get(i))
                .filter(|e| e.is("PREP"));
            let kind = if index == 0 {
                "active_subject"
            } else if double_object && index == 2 {
                "indirect_object"
            } else if prior_prep.is_some() {
                "prepositional_object"
            } else if has_restriction(node, "that_comp", "+") {
                "finite_complement"
            } else {
                "nominal_object"
            };
            if kind == "nominal_object" {
                nominal_objects += 1;
            }
            let candidates: Vec<_> = if double_object { &pattern_slots } else { slots }
                .iter()
                .filter(|slot| slot.kind == kind && !consumed.contains(&slot.direct))
                .filter(|slot| {
                    prior_prep.is_none_or(|prep| {
                        literal_preposition(prep).is_some_and(|value| {
                            normalized_lemma(&doc.tokens[slot.supporting[0]]).as_deref()
                                == Some(value)
                        })
                    })
                })
                .collect();
            let unique = candidates.len() == 1;
            if unique {
                consumed.insert(candidates[0].direct);
            }
            let role_name = node.attr("value").map(str::to_owned);
            let role_declaration = role_name
                .as_ref()
                .and_then(|name| class.effective_roles.get(name))
                .cloned();
            let role_declarations = role_name
                .as_ref()
                .and_then(|name| class.effective_role_declarations.get(name))
                .cloned()
                .unwrap_or_default();
            if role_name.is_some() && role_declaration.is_none() {
                findings.push(finding(
                    "semantic",
                    if role_declarations.is_empty() {
                        "missing_role_declaration"
                    } else {
                        "ambiguous_role_declaration"
                    },
                    Some(index),
                    Status::Unresolved,
                    json!({"role":role_name,"declarations":role_declarations}),
                ));
            }
            if let Some(role) = &role_declaration
                && role.xml.elements().any(|r| {
                    !r.is("SELRESTRS")
                        || constraint_text(r)
                        || r.elements().next().is_some()
                        || r.attributes
                            .iter()
                            .any(|a| a.namespace.is_some() || a.name != "logic")
                        || r.attr("logic")
                            .is_some_and(|logic| !matches!(logic, "and" | "or"))
                })
            {
                findings.push(finding(
                    "semantic",
                    "role_selectional_restrictions_unassessed",
                    Some(index),
                    Status::Unresolved,
                    json!({"role":role}),
                ));
            }
            bindings.push(Binding {
                syntax_index: index,
                role_name,
                role_declaration,
                role_declarations,
                alternatives: candidates.iter().map(|s| alternative(doc, s)).collect(),
                unique,
            });
            findings.push(finding("syntax","argument_binding",Some(index),if unique {Status::Checked} else {Status::Unresolved},
                json!({"kind":kind,"candidate_count":candidates.len(),"missing_is_not_licensed_omission":candidates.is_empty()})));
            findings.extend(restrictions(
                node,
                index,
                kind,
                unique,
                kind == "finite_complement" && unique && !candidates[0].supporting.is_empty(),
            ));
        }
        if nominal_objects > 1 {
            findings.push(finding(
                "syntax",
                "multiple_unmarked_postverbal_np_slots",
                None,
                Status::Unresolved,
                json!({"count":nominal_objects}),
            ));
        }
    } else {
        findings.push(finding(
            "syntax",
            "missing_syntax",
            None,
            Status::Unresolved,
            json!({}),
        ));
    }
    if bindings.iter().all(|b| b.unique) {
        let indices = bindings
            .iter()
            .map(|b| b.alternatives[0].anchor.token.i)
            .collect::<Vec<_>>();
        findings.push(finding(
            "syntax",
            "ordered_syntax_bindings",
            None,
            if indices.windows(2).all(|w| w[0] < w[1]) {
                Status::Checked
            } else {
                Status::Contradicted
            },
            json!({"indices":indices}),
        ));
    }
    let unbound_observed_arguments = ledger
        .direct_dependents
        .iter()
        .filter(|d| argument_relation(&d.anchor.token.dep) && !consumed.contains(&d.anchor.token.i))
        .map(|d| d.anchor.clone())
        .collect::<Vec<_>>();
    if !unbound_observed_arguments.is_empty() {
        findings.push(finding("syntax","unbound_observed_arguments",None,Status::Unresolved,
            json!({"token_indices":unbound_observed_arguments.iter().map(|a|a.token.i).collect::<Vec<_>>()})));
    }
    for element in frame.xml.elements() {
        if element.is("SEMANTICS") && element.elements().next().is_some() {
            findings.push(finding(
                "semantic",
                "lexical_semantic_predicates_unassessed",
                None,
                Status::Unresolved,
                json!({"xml":element}),
            ));
        } else if !element.is("SEMANTICS")
            && !element.is("SYNTAX")
            && !element.is("DESCRIPTION")
            && !element.is("EXAMPLES")
        {
            findings.push(finding(
                "semantic",
                "unsupported_frame_section",
                None,
                Status::Unresolved,
                json!({"xml":element}),
            ));
        }
    }
    let syntactic_binding_status = aggregate(findings.iter().filter(|f| f.domain == "syntax"));
    let full_compatibility_status = aggregate(findings.iter());
    FrameObservation {
        member_class_id: class.id.clone(),
        member_ordinal,
        frame: frame.clone(),
        bindings,
        findings,
        syntactic_binding_status,
        syntax_binding_complete: syntactic_binding_status == Status::Checked,
        full_compatibility_status,
        unbound_observed_arguments,
        sense_selected: false,
        automatic_edit_license: false,
    }
}

/// Construct observations from validated source annotations only. The resource
/// contributes possibilities, never semantic labels for the source participants.
pub fn observe(doc: &Document, resource: &Resource) -> Result<ObservationReport> {
    observe_with_mask(doc, resource, &BTreeSet::new())
}

/// Diagnostic masking removes every member/frame possibility for exactly these
/// normalized lemmas. It does not alter tokens or the bound source resource.
pub fn observe_with_mask(
    doc: &Document,
    resource: &Resource,
    masked: &BTreeSet<String>,
) -> Result<ObservationReport> {
    observe_with_mapping(doc, resource, masked, Mapping::BaselineV1)
}

/// The extended mapping accepts an observed ADP `dative` only through the same
/// direct, forward, lexical ADP-to-nominal-`pobj` path as the baseline `prep`,
/// with an explicitly VERB governing head for the new `dative` branch.
/// A frame must still specify the matching literal PREP; bare nominal datives
/// and prepositions attached to other heads remain unsupported.
pub fn observe_with_mapping(
    doc: &Document,
    resource: &Resource,
    masked: &BTreeSet<String>,
    mapping: Mapping,
) -> Result<ObservationReport> {
    syntax::validate(doc)?;
    ensure!(
        masked
            .iter()
            .all(|lemma| crate::verbnet_resource::normalize_lemma(lemma).as_ref() == Some(lemma)),
        "Mask lemmas must already use the exact normalized supplied-lemma identity"
    );
    let observed = predicate_operators::document_observations(doc)?;
    let events = observed
        .iter()
        .map(|o| (o.head_index, o.event.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut children = vec![Vec::new(); doc.tokens.len()];
    for token in &doc.tokens {
        if token.i != token.head {
            children[token.head].push(token.i);
        }
    }
    let mut predicates = Vec::new();
    for observation in observed {
        let head = &doc.tokens[observation.head_index];
        let direct_dependents = children[head.i]
            .iter()
            .map(|&index| {
                let subtree = descendants(index, &children);
                Dependent {
                    anchor: (&doc.tokens[index]).into(),
                    proposition_head_indices: subtree
                        .iter()
                        .copied()
                        .filter(|i| events.contains_key(i))
                        .collect(),
                    subtree: subtree.iter().map(|&i| (&doc.tokens[i]).into()).collect(),
                }
            })
            .collect();
        let particle_anchors = children[head.i]
            .iter()
            .copied()
            .filter(|&i| matches!(doc.tokens[i].dep.as_str(), "prt" | "compound:prt"))
            .map(|i| (&doc.tokens[i]).into())
            .collect::<Vec<Anchor>>();
        let mut head_support = Vec::new();
        let passive = head
            .morph
            .get("Voice")
            .is_some_and(|v| v.iter().any(|x| x == "Pass"))
            || children[head.i].iter().any(|&i| {
                matches!(
                    doc.tokens[i].dep.as_str(),
                    "nsubjpass" | "nsubj:pass" | "auxpass" | "aux:pass"
                )
            });
        head_support.push(finding(
            "syntax",
            "supported_active_finite_verb",
            None,
            if head.pos == "VERB" && !passive && finite(&observation.event) {
                Status::Checked
            } else {
                Status::Unresolved
            },
            json!({"pos":head.pos,"passive_evidence":passive,"finite":observation.event.finite}),
        ));
        if normalized_lemma(head).is_none() {
            head_support.push(finding(
                "syntax",
                "missing_predicate_lemma",
                None,
                Status::Unresolved,
                json!({"no_surface_fallback":true}),
            ));
        }
        if !particle_anchors.is_empty() {
            head_support.push(finding("syntax","particle_use_not_lexically_resolved",None,Status::Unresolved,
                json!({"policy":"Lookup the exact normalized head lemma; retain particles separately without inventing a compound member key."})));
        }
        let lookup = resource.lookup(&head.lemma);
        let is_masked = lookup
            .normalized_lemma
            .as_ref()
            .is_some_and(|lemma| masked.contains(lemma));
        let lookup_value = if is_masked {
            json!({"normalized_lemma":lookup.normalized_lemma,"status":"deliberately_masked",
            "members":[],"resource_lookup_status":lookup.status,"masked_member_count":lookup.members.len(),
            "open_world_unknown_use_possible":true,"sense_selected":false})
        } else {
            serde_json::to_value(&lookup)?
        };
        let mut ledger = PredicateLedger {
            head: head.into(),
            raw_parent: (&doc.tokens[head.head]).into(),
            particle_anchors,
            original_operator_event: observation.event,
            direct_dependents,
            head_support,
            lookup: lookup_value,
            frame_possibilities: Vec::new(),
            open_world_unknown_use_possible: true,
            automatic_edit_license: false,
        };
        let slots = observed_slots(doc, head.i, &children, &events, mapping);
        for member in lookup.members.iter().filter(|_| !is_masked) {
            let class = resource
                .classes
                .get(&member.class_id)
                .context("Lookup references missing VerbNet class")?;
            for frame in &class.effective_frames {
                ledger.frame_possibilities.push(bind_frame(
                    doc,
                    &ledger,
                    &slots,
                    frame,
                    class,
                    member.member_ordinal,
                    mapping,
                ));
            }
        }
        predicates.push(ledger);
    }
    Ok(ObservationReport {
        schema: match mapping {
            Mapping::BaselineV1 => SCHEMA,
            Mapping::PrepositionalDativeV1 => EXTENDED_SCHEMA,
            Mapping::DativeAlternationV1 => ALTERNATION_SCHEMA,
        }
        .into(),
        mapping_identity: (mapping != Mapping::BaselineV1).then(|| mapping.identity().into()),
        source_sha256: edits::digest(&doc.text),
        parser_identity: doc.parser_identity.clone(),
        resource_schema: resource.schema.clone(),
        resource_sources: resource
            .sources
            .iter()
            .map(|s| json!({"path":s.path,"sha256":s.sha256}))
            .collect(),
        deliberately_masked_lemmas: masked.iter().cloned().collect(),
        predicate_count: predicates.len(),
        predicates,
        semantic_equivalence_certified: false,
        automatic_edit_license: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbnet_resource::XmlSource;
    use grammar_core::syntax::Sentence;

    fn document(items: &[(&str, &str, &str, &str, usize)]) -> Document {
        let text = items.iter().map(|i| i.0).collect::<Vec<_>>().join(" ");
        let mut cursor = 0;
        let tokens = items
            .iter()
            .enumerate()
            .map(|(i, (word, lemma, pos, dep, head))| {
                let start_byte = cursor;
                cursor += word.len() + 1;
                let is_verb = matches!(*pos, "VERB" | "AUX");
                Token {
                    i,
                    text: (*word).into(),
                    start_byte,
                    end_byte: start_byte + word.len(),
                    lemma: (*lemma).into(),
                    pos: (*pos).into(),
                    tag: if is_verb { "VBD".into() } else { (*pos).into() },
                    dep: (*dep).into(),
                    head: *head,
                    sentence: 0,
                    morph: if is_verb {
                        BTreeMap::from([("VerbForm".into(), vec!["Fin".into()])])
                    } else {
                        BTreeMap::new()
                    },
                    is_punct: *pos == "PUNCT",
                    is_space: *pos == "SPACE",
                }
            })
            .collect::<Vec<_>>();
        let root = tokens.iter().find(|t| t.i == t.head).unwrap().i;
        let doc = Document {
            parser_identity: "hand-annotated-lexical-frame-test-v1".into(),
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: text.len(),
                root,
                token_start: 0,
                token_end: tokens.len(),
            }],
            text,
            tokens,
        };
        syntax::validate(&doc).unwrap();
        doc
    }

    fn resource(lemma: &str, frames: &str) -> Resource {
        let xml = format!(
            "<VNCLASS ID='test'><MEMBERS><MEMBER name='{lemma}'/></MEMBERS><THEMROLES><THEMROLE type='Agent'><SELRESTRS><SELRESTR Value='+' type='animate'/></SELRESTRS></THEMROLE><THEMROLE type='Material'/><THEMROLE type='Theme'/><THEMROLE type='Topic'/><THEMROLE type='Destination'/></THEMROLES><FRAMES>{frames}</FRAMES></VNCLASS>"
        );
        Resource::from_sources(vec![XmlSource {
            path: "synthetic.xml".into(),
            sha256: edits::digest(&xml),
            xml,
        }])
        .unwrap()
    }

    fn frame(syntax: &str) -> String {
        format!(
            "<FRAME><SYNTAX>{syntax}</SYNTAX><SEMANTICS><PRED value='synthetic_event'><ARGS><ARG type='ThemRole' value='Theme'/></ARGS></PRED></SEMANTICS></FRAME>"
        )
    }

    fn transitive() -> Document {
        document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("built", "build", "VERB", "ROOT", 1),
            ("boats", "boat", "NOUN", "dobj", 1),
        ])
    }

    #[test]
    fn identical_syntax_keeps_distinct_frame_roles_and_unresolved_semantics() {
        let r = resource(
            "build",
            &(frame("<NP value='Agent'/><VERB/><NP value='Theme'/>")
                + &frame("<NP value='Material'/><VERB/><NP value='Theme'/>")),
        );
        let report = observe(&transitive(), &r).unwrap();
        let head = &report.predicates[0];
        assert_eq!(head.frame_possibilities.len(), 2);
        let roles = head
            .frame_possibilities
            .iter()
            .map(|f| f.bindings[0].role_name.as_deref().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(roles, vec!["Agent", "Material"]);
        for possibility in &head.frame_possibilities {
            assert!(possibility.syntax_binding_complete);
            assert_eq!(possibility.bindings[0].alternatives[0].anchor.token.i, 0);
            assert_eq!(possibility.full_compatibility_status, Status::Unresolved);
            assert!(!possibility.sense_selected);
            assert!(!possibility.automatic_edit_license);
        }
        assert!(head.open_world_unknown_use_possible);
        assert_eq!(
            head.original_operator_event,
            predicate_operators::document_observations(&transitive()).unwrap()[0].event
        );
    }

    #[test]
    fn finite_complement_keeps_proposition_and_that_marker_anchors() {
        let r = resource(
            "say",
            &frame(
                "<NP value='Agent'/><VERB/><NP value='Topic'><SYNRESTRS><SYNRESTR Value='+' type='that_comp'/></SYNRESTRS></NP>",
            ),
        );
        let doc = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("said", "say", "VERB", "ROOT", 1),
            ("that", "that", "SCONJ", "mark", 4),
            ("Lee", "lee", "PROPN", "nsubj", 4),
            ("left", "leave", "VERB", "ccomp", 1),
        ]);
        let report = observe(&doc, &r).unwrap();
        assert_eq!(report.predicates.len(), 2);
        let predicate = &report.predicates[0];
        let matched = &predicate.frame_possibilities[0];
        assert!(matched.syntax_binding_complete);
        let bound = &matched.bindings[2].alternatives[0];
        assert_eq!(bound.proposition_head_index, Some(4));
        assert_eq!(bound.supporting_anchors[0].token.i, 2);
        assert_eq!(
            predicate
                .direct_dependents
                .iter()
                .find(|d| d.anchor.token.i == 4)
                .unwrap()
                .proposition_head_indices,
            vec![4]
        );
        assert_eq!(
            predicate
                .direct_dependents
                .iter()
                .find(|d| d.anchor.token.i == 4)
                .unwrap()
                .subtree
                .iter()
                .map(|t| t.token.i)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        let mut bare = doc.clone();
        bare.tokens[2].lemma = "whether".into();
        let bare_report = observe(&bare, &r).unwrap();
        assert!(!bare_report.predicates[0].frame_possibilities[0].syntax_binding_complete);
        assert!(
            bare_report.predicates[0].frame_possibilities[0]
                .findings
                .iter()
                .any(|f| f.constraint == "syntax_restriction" && f.status == Status::Unresolved)
        );
    }

    #[test]
    fn nominal_object_cannot_silently_stand_for_a_proposition() {
        let r = resource(
            "say",
            &frame(
                "<NP value='Agent'/><VERB/><NP value='Topic'><SYNRESTRS><SYNRESTR Value='+' type='that_comp'/></SYNRESTRS></NP>",
            ),
        );
        let doc = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("said", "say", "VERB", "ROOT", 1),
            ("words", "word", "NOUN", "dobj", 1),
        ]);
        let report = observe(&doc, &r).unwrap();
        let matched = &report.predicates[0].frame_possibilities[0];
        assert!(!matched.syntax_binding_complete);
        assert!(matched.bindings[2].alternatives.is_empty());
        assert_eq!(matched.unbound_observed_arguments[0].token.i, 2);
    }

    #[test]
    fn explicit_prepositions_preserve_parser_aliases_and_reject_known_mismatch() {
        let r = resource(
            "send",
            &frame("<NP value='Agent'/><VERB/><PREP value='to'/><NP value='Destination'/>"),
        );
        let doc = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("sent", "send", "VERB", "ROOT", 1),
            ("to", "to", "ADP", "prep", 1),
            ("Lee", "lee", "PROPN", "pobj", 2),
        ]);
        let report = observe(&doc, &r).unwrap();
        assert!(report.predicates[0].frame_possibilities[0].syntax_binding_complete);
        let mut ud = doc.clone();
        ud.tokens[2].dep = "case".into();
        ud.tokens[2].head = 3;
        ud.tokens[3].dep = "obl".into();
        ud.tokens[3].head = 1;
        assert!(
            observe(&ud, &r).unwrap().predicates[0].frame_possibilities[0].syntax_binding_complete
        );
        let mismatch = resource(
            "send",
            &frame("<NP value='Agent'/><VERB/><PREP value='from'/><NP value='Destination'/>"),
        );
        assert_eq!(
            observe(&doc, &mismatch).unwrap().predicates[0].frame_possibilities[0]
                .syntactic_binding_status,
            Status::Contradicted
        );
        let phrase = resource(
            "send",
            &frame("<NP value='Agent'/><VERB/><PREP value='to | as if'/><NP value='Destination'/>"),
        );
        let result = observe(&doc, &phrase).unwrap();
        assert_eq!(
            result.predicates[0].frame_possibilities[0].syntactic_binding_status,
            Status::Unresolved
        );
        assert!(
            result.predicates[0].frame_possibilities[0].bindings[2]
                .alternatives
                .is_empty()
        );
    }

    #[test]
    fn unknown_restrictions_and_attributes_are_not_false_constraints() {
        let r = resource(
            "build",
            &frame(
                "<NP value='Agent'/><VERB/><NP value='Theme' future='yes'><SYNRESTRS logic='or'><SYNRESTR Value='-' type='unknown_future_symbol'/><SYNRESTR Value='-' type='sentential'/></SYNRESTRS></NP>",
            ),
        );
        let report = observe(&transitive(), &r).unwrap();
        let matched = &report.predicates[0].frame_possibilities[0];
        assert_eq!(matched.syntactic_binding_status, Status::Unresolved);
        assert!(
            matched
                .findings
                .iter()
                .all(|f| f.status != Status::Contradicted)
        );
        assert!(
            matched
                .findings
                .iter()
                .any(|f| f.constraint == "unsupported_syntax_attributes")
        );
        assert!(
            matched
                .findings
                .iter()
                .any(|f| f.constraint == "unsupported_restriction_logic")
        );
    }

    #[test]
    fn missing_arguments_and_unsupported_heads_remain_ledger_rows() {
        let r = resource(
            "build",
            &frame("<NP value='Agent'/><VERB/><NP value='Theme'/>"),
        );
        let doc = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("built", "build", "VERB", "ROOT", 1),
        ]);
        let report = observe(&doc, &r).unwrap();
        assert!(!report.predicates[0].frame_possibilities[0].syntax_binding_complete);
        assert!(
            report.predicates[0].frame_possibilities[0].bindings[2]
                .alternatives
                .is_empty()
        );
        for unsupported in [
            document(&[
                ("Boats", "boat", "NOUN", "nsubjpass", 1),
                ("built", "build", "VERB", "ROOT", 1),
            ]),
            document(&[("Quiet", "quiet", "ADJ", "ROOT", 0)]),
            document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("built", "build", "VERB", "ROOT", 1),
                ("up", "up", "ADP", "prt", 1),
            ]),
        ] {
            let report = observe(&unsupported, &r).unwrap();
            assert_eq!(report.predicate_count, 1);
            assert!(
                report.predicates[0]
                    .head_support
                    .iter()
                    .any(|f| f.status == Status::Unresolved)
            );
            assert!(report.predicates[0].open_world_unknown_use_possible);
        }
    }

    #[test]
    fn diagnostic_mask_is_distinct_from_natural_unknown_and_preserves_source() {
        let r = resource(
            "build",
            &frame("<NP value='Agent'/><VERB/><NP value='Theme'/>"),
        );
        let doc = transitive();
        let masked = observe_with_mask(&doc, &r, &BTreeSet::from(["build".into()])).unwrap();
        assert_eq!(masked.predicates[0].lookup["status"], "deliberately_masked");
        assert_eq!(masked.predicates[0].lookup["masked_member_count"], 1);
        assert!(masked.predicates[0].frame_possibilities.is_empty());
        assert_eq!(masked.predicates[0].head.token, doc.tokens[1]);
        assert_eq!(
            observe(&doc, &r).unwrap().predicates[0].lookup["status"],
            "found"
        );
        let mut unknown = doc.clone();
        unknown.tokens[1].lemma = "mizzle".into();
        assert_eq!(
            observe(&unknown, &r).unwrap().predicates[0].lookup["status"],
            "unknown"
        );
        unknown.tokens[1].lemma = "  ".into();
        assert_eq!(
            observe(&unknown, &r).unwrap().predicates[0].lookup["status"],
            "missing_lemma"
        );
        assert!(observe_with_mask(&doc, &r, &BTreeSet::from(["BUILD".into()])).is_err());
    }

    #[test]
    fn slot_order_and_malformed_document_cannot_pass_as_complete() {
        let r = resource(
            "build",
            &frame(
                "<NP value='Agent'/><VERB/><NP value='Theme'/><PREP value='with'/><NP value='Material'/>",
            ),
        );
        let doc = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("built", "build", "VERB", "ROOT", 1),
            ("with", "with", "ADP", "prep", 1),
            ("wood", "wood", "NOUN", "pobj", 2),
            ("boats", "boat", "NOUN", "dobj", 1),
        ]);
        assert_eq!(
            observe(&doc, &r).unwrap().predicates[0].frame_possibilities[0]
                .syntactic_binding_status,
            Status::Contradicted
        );
        let mut malformed = doc.clone();
        malformed.tokens[0].head = 999;
        assert!(observe(&malformed, &r).is_err());
    }

    #[test]
    fn participant_change_changes_anchors_without_changing_role_alternatives() {
        let r = resource(
            "build",
            &frame("<NP value='Agent'/><VERB/><NP value='Theme'/>"),
        );
        let original = observe(&transitive(), &r).unwrap();
        let changed = observe(
            &document(&[
                ("Lee", "lee", "PROPN", "nsubj", 1),
                ("built", "build", "VERB", "ROOT", 1),
                ("boats", "boat", "NOUN", "dobj", 1),
            ]),
            &r,
        )
        .unwrap();
        assert_ne!(original.source_sha256, changed.source_sha256);
        let a = &original.predicates[0].frame_possibilities[0].bindings[0];
        let b = &changed.predicates[0].frame_possibilities[0].bindings[0];
        assert_eq!(a.role_name, b.role_name);
        assert_ne!(
            a.alternatives[0].anchor.normalized_lemma,
            b.alternatives[0].anchor.normalized_lemma
        );
    }

    #[test]
    fn container_constraints_and_duplicate_syntax_cannot_be_silently_checked() {
        let syntax = "<NP value='Agent'/><VERB/><NP value='Theme'/>";
        for xml in [
            format!("<FRAME><SYNTAX unsupported='yes'>{syntax}</SYNTAX></FRAME>"),
            format!("<FRAME><SYNTAX>{syntax}</SYNTAX><SYNTAX>{syntax}</SYNTAX></FRAME>"),
            format!("<FRAME><SYNTAX>opaque constraint{syntax}</SYNTAX></FRAME>"),
            frame(
                "<NP value='Agent'/><VERB/><NP value='Theme'><SYNRESTRS unsupported='yes'><SYNRESTR Value='-' type='sentential'/></SYNRESTRS></NP>",
            ),
            frame(
                "<NP value='Agent'/><VERB/><NP value='Theme'><SYNRESTRS>opaque constraint</SYNRESTRS></NP>",
            ),
        ] {
            let result = observe(&transitive(), &resource("build", &xml)).unwrap();
            assert_eq!(
                result.predicates[0].frame_possibilities[0].syntactic_binding_status,
                Status::Unresolved
            );
        }
        let xml = frame(
            "<NP value='Agent'/><VERB/><NP value='Theme'><SELRESTRS unsupported='yes'/></NP>",
        );
        let result = observe(&transitive(), &resource("build", &xml)).unwrap();
        let possible = &result.predicates[0].frame_possibilities[0];
        assert!(possible.syntax_binding_complete);
        assert_eq!(possible.full_compatibility_status, Status::Unresolved);
        assert!(
            possible
                .findings
                .iter()
                .any(|f| f.constraint == "selectional_restrictions_unassessed")
        );
    }

    #[test]
    fn conflicting_role_declarations_retain_every_origin_and_no_representative() {
        let xml = format!(
            "<VNCLASS ID='duplicate'><MEMBERS><MEMBER name='build'/></MEMBERS><THEMROLES><THEMROLE type='Agent'><SELRESTRS><SELRESTR Value='+' type='animate'/></SELRESTRS></THEMROLE><THEMROLE type='Agent'><SELRESTRS><SELRESTR Value='-' type='animate'/></SELRESTRS></THEMROLE><THEMROLE type='Theme'/></THEMROLES><FRAMES>{}</FRAMES></VNCLASS>",
            frame("<NP value='Agent'/><VERB/><NP value='Theme'/>")
        );
        let r = Resource::from_sources(vec![XmlSource {
            path: "duplicate.xml".into(),
            sha256: edits::digest(&xml),
            xml,
        }])
        .unwrap();
        let result = observe(&transitive(), &r).unwrap();
        let possible = &result.predicates[0].frame_possibilities[0];
        assert!(possible.syntax_binding_complete);
        assert_eq!(possible.bindings[0].role_declarations.len(), 2);
        assert!(possible.bindings[0].role_declaration.is_none());
        assert!(possible.findings.iter().any(
            |f| f.constraint == "ambiguous_role_declaration" && f.status == Status::Unresolved
        ));
    }

    fn dative_document() -> Document {
        document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("sent", "send", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("to", "to", "ADP", "dative", 1),
            ("Lee", "lee", "PROPN", "pobj", 3),
        ])
    }

    fn dative_resource(preposition: &str) -> Resource {
        resource(
            "send",
            &frame(&format!(
                "<NP value='Agent'/><VERB/><NP value='Theme'/><PREP value='{preposition}'/><NP value='Destination'/>"
            )),
        )
    }

    fn extended(doc: &Document, resource: &Resource) -> ObservationReport {
        observe_with_mapping(
            doc,
            resource,
            &BTreeSet::new(),
            Mapping::PrepositionalDativeV1,
        )
        .unwrap()
    }

    #[test]
    fn explicit_dative_mapping_preserves_raw_anchors_and_baseline_serialization() {
        let doc = dative_document();
        let unchanged = doc.clone();
        let r = dative_resource("to");
        let baseline = observe(&doc, &r).unwrap();
        let baseline_json = serde_json::to_value(&baseline).unwrap();
        assert_eq!(baseline_json["schema"], SCHEMA);
        assert!(baseline_json.get("mapping_identity").is_none());
        assert_eq!(
            serde_json::to_vec(&baseline).unwrap(),
            serde_json::to_vec(
                &observe_with_mapping(&doc, &r, &BTreeSet::new(), Mapping::BaselineV1).unwrap()
            )
            .unwrap()
        );
        assert!(!baseline.predicates[0].frame_possibilities[0].syntax_binding_complete);

        let result = extended(&doc, &r);
        assert_eq!(result.schema, EXTENDED_SCHEMA);
        assert_eq!(
            result.mapping_identity.as_deref(),
            Some(Mapping::PrepositionalDativeV1.identity())
        );
        let matched = &result.predicates[0].frame_possibilities[0];
        assert!(matched.syntax_binding_complete);
        assert_eq!(matched.full_compatibility_status, Status::Unresolved);
        assert_eq!(
            matched.bindings[3].alternatives[0].anchor.token,
            doc.tokens[3]
        );
        assert_eq!(
            matched.bindings[3].alternatives[0].anchor.token.dep,
            "dative"
        );
        assert_eq!(
            matched.bindings[4].alternatives[0].anchor.token,
            doc.tokens[4]
        );
        assert_eq!(
            matched.bindings[4].alternatives[0].supporting_anchors[0].token,
            doc.tokens[3]
        );
        assert!(!result.automatic_edit_license);
        assert!(!result.semantic_equivalence_certified);
        assert_eq!(doc, unchanged);
    }

    #[test]
    fn dative_requires_a_lexical_adp_verb_head_and_own_nominal_object() {
        let r = dative_resource("to");
        let mut variants = Vec::new();
        let mut absent = dative_document();
        absent.tokens[4].dep = "dep".into();
        variants.push(absent);
        let mut nonnominal = dative_document();
        nonnominal.tokens[4].pos = "ADJ".into();
        variants.push(nonnominal);
        let mut nonadp = dative_document();
        nonadp.tokens[3].pos = "PART".into();
        variants.push(nonadp);
        let mut nonlexical = dative_document();
        nonlexical.tokens[3].is_punct = true;
        variants.push(nonlexical);
        let mut nonverb = dative_document();
        nonverb.tokens[1].pos = "NOUN".into();
        variants.push(nonverb);
        variants.push(document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("sent", "send", "VERB", "ROOT", 1),
            ("Lee", "lee", "PROPN", "dative", 1),
            ("books", "book", "NOUN", "dobj", 1),
        ]));
        for doc in variants {
            let result = extended(&doc, &r);
            let matched = &result.predicates[0].frame_possibilities[0];
            assert!(!matched.syntax_binding_complete);
            assert!(matched.bindings[4].alternatives.is_empty());
        }
    }

    #[test]
    fn dative_still_requires_the_explicit_literal_resource_preposition() {
        let doc = dative_document();
        let mismatch = extended(&doc, &dative_resource("from"));
        assert_eq!(
            mismatch.predicates[0].frame_possibilities[0].syntactic_binding_status,
            Status::Contradicted
        );
        for prep in ["", "to | as if"] {
            let result = extended(&doc, &dative_resource(prep));
            let matched = &result.predicates[0].frame_possibilities[0];
            assert!(!matched.syntax_binding_complete);
            assert!(matched.bindings[3].alternatives.is_empty());
            assert!(matched.bindings[4].alternatives.is_empty());
        }
    }

    #[test]
    fn dative_does_not_cross_noun_or_relative_heads_or_reverse_surface_order() {
        let r = dative_resource("to");
        let mut attached_to_noun = dative_document();
        attached_to_noun.tokens[3].head = 2;
        let relative = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("sent", "send", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("that", "that", "PRON", "nsubj", 4),
            ("belong", "belong", "VERB", "relcl", 2),
            ("to", "to", "ADP", "dative", 4),
            ("Lee", "lee", "PROPN", "pobj", 5),
        ]);
        let preceding = document(&[
            ("to", "to", "ADP", "dative", 3),
            ("Lee", "lee", "PROPN", "pobj", 0),
            ("Kim", "kim", "PROPN", "nsubj", 3),
            ("sent", "send", "VERB", "ROOT", 3),
            ("books", "book", "NOUN", "dobj", 3),
        ]);
        let object_before_prep = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("sent", "send", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("Lee", "lee", "PROPN", "pobj", 4),
            ("to", "to", "ADP", "dative", 1),
        ]);
        for doc in [attached_to_noun, relative, preceding, object_before_prep] {
            let result = extended(&doc, &r);
            let matched = &result
                .predicates
                .iter()
                .find(|p| p.head.normalized_lemma.as_deref() == Some("send"))
                .unwrap()
                .frame_possibilities[0];
            assert!(!matched.syntax_binding_complete);
            assert!(matched.bindings[4].alternatives.is_empty());
        }
    }

    #[test]
    fn dative_role_anchors_distinguish_swapped_and_repeated_occurrences() {
        let r = dative_resource("to");
        for (theme, recipient) in [("Kim", "Lee"), ("Lee", "Kim")] {
            let doc = document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("sent", "send", "VERB", "ROOT", 1),
                (theme, theme, "PROPN", "dobj", 1),
                ("to", "to", "ADP", "dative", 1),
                (recipient, recipient, "PROPN", "pobj", 3),
            ]);
            let result = extended(&doc, &r);
            let matched = &result.predicates[0].frame_possibilities[0];
            assert!(matched.syntax_binding_complete);
            for (binding, token) in [(0, 0), (2, 2), (4, 4)] {
                assert_eq!(matched.bindings[binding].alternatives.len(), 1);
                assert_eq!(
                    matched.bindings[binding].alternatives[0].anchor.token,
                    doc.tokens[token]
                );
            }
            assert_eq!(matched.bindings[2].alternatives[0].anchor.token.text, theme);
            assert_eq!(
                matched.bindings[4].alternatives[0].anchor.token.text,
                recipient
            );
            assert_ne!(
                matched.bindings[0].alternatives[0].anchor.token.start_byte,
                matched.bindings[4].alternatives[0].anchor.token.start_byte
            );
        }
    }

    #[test]
    fn nondative_evidence_and_masking_match_baseline_except_version_metadata() {
        let r = dative_resource("to");
        let mut prep = dative_document();
        prep.tokens[3].dep = "prep".into();
        let mut ud = prep.clone();
        ud.tokens[3].dep = "case".into();
        ud.tokens[3].head = 4;
        ud.tokens[4].dep = "obl".into();
        ud.tokens[4].head = 1;
        for doc in [prep, ud] {
            for masked in [BTreeSet::new(), BTreeSet::from(["send".into()])] {
                let baseline = observe_with_mask(&doc, &r, &masked).unwrap();
                let extended =
                    observe_with_mapping(&doc, &r, &masked, Mapping::PrepositionalDativeV1)
                        .unwrap();
                let mut v2 = serde_json::to_value(extended).unwrap();
                assert_eq!(v2["schema"], EXTENDED_SCHEMA);
                v2["schema"] = json!(SCHEMA);
                v2.as_object_mut().unwrap().remove("mapping_identity");
                assert_eq!(v2, serde_json::to_value(baseline).unwrap());
            }
        }
    }

    fn double_object_resource() -> Resource {
        let frames = frame(
            "<NP value='Agent'/><VERB/><NP value='Theme'/><PREP value='to'/><NP value='Recipient'/>",
        ) + &frame(
            "<NP value='Agent'/><VERB/><NP value='Recipient'/><NP value='Theme'/>",
        );
        let xml = format!(
            "<VNCLASS ID='give-13.1'><MEMBERS><MEMBER name='lend'/></MEMBERS><THEMROLES><THEMROLE type='Agent'/><THEMROLE type='Recipient'/><THEMROLE type='Theme'/></THEMROLES><FRAMES>{frames}</FRAMES></VNCLASS>"
        );
        let mut resource = Resource::from_sources(vec![XmlSource {
            path: "synthetic-double-object.xml".into(),
            sha256: edits::digest(&xml),
            xml,
        }])
        .unwrap();
        // Stub the verified source identity only inside this in-memory unit
        // fixture. Production Resource::from_sources verifies actual XML hashes;
        // these tests neither claim a valid export nor depend on ignored assets.
        resource
            .classes
            .get_mut("give-13.1")
            .unwrap()
            .effective_frames[1]
            .origin
            .source_sha256 = GIVE_SOURCE_SHA256.into();
        resource
    }

    fn double_object_document(indirect: &str, direct: &str) -> Document {
        document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Lee", "lee", "PROPN", indirect, 1),
            ("books", "book", "NOUN", direct, 1),
        ])
    }

    fn alternation(doc: &Document, resource: &Resource) -> ObservationReport {
        observe_with_mapping(
            doc,
            resource,
            &BTreeSet::new(),
            Mapping::DativeAlternationV1,
        )
        .unwrap()
    }

    fn double_frame(report: &ObservationReport) -> &FrameObservation {
        &report
            .predicates
            .iter()
            .find(|p| p.head.normalized_lemma.as_deref() == Some("lend"))
            .unwrap()
            .frame_possibilities[1]
    }

    #[test]
    fn pinned_double_object_pattern_binds_both_dependency_aliases_atomically() {
        let r = double_object_resource();
        for (indirect, direct) in [("dative", "dobj"), ("iobj", "obj")] {
            let doc = double_object_document(indirect, direct);
            let unchanged = doc.clone();
            let result = alternation(&doc, &r);
            assert_eq!(result.schema, ALTERNATION_SCHEMA);
            assert_eq!(
                result.mapping_identity.as_deref(),
                Some("dative_alternation_v1")
            );
            let f = double_frame(&result);
            assert!(f.syntax_binding_complete);
            assert_eq!(f.full_compatibility_status, Status::Unresolved);
            assert!(f.unbound_observed_arguments.is_empty());
            for (index, role, kind) in [
                (0, "Agent", "active_subject"),
                (2, "Recipient", "indirect_object"),
                (3, "Theme", "nominal_object"),
            ] {
                assert_eq!(f.bindings[index].role_name.as_deref(), Some(role));
                assert!(f.bindings[index].unique);
                let a = &f.bindings[index].alternatives[0];
                assert_eq!(a.kind, kind);
                assert_eq!(a.anchor.token, doc.tokens[index]);
                assert!(a.supporting_anchors.is_empty());
                assert_eq!(a.proposition_head_index, None);
            }
            assert_eq!(doc, unchanged);
            for mapping in [Mapping::BaselineV1, Mapping::PrepositionalDativeV1] {
                let baseline = observe_with_mapping(&doc, &r, &BTreeSet::new(), mapping).unwrap();
                assert!(!double_frame(&baseline).syntax_binding_complete);
            }
        }
    }

    #[test]
    fn missing_or_competing_double_objects_never_consume_theme_as_recipient() {
        let r = double_object_resource();
        let missing = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
        ]);
        let competing = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Lee", "lee", "PROPN", "dative", 1),
            ("Pat", "pat", "PROPN", "iobj", 1),
            ("books", "book", "NOUN", "dobj", 1),
        ]);
        let mut only_direct = double_object_document("dobj", "obj");
        only_direct.tokens[2].dep = "obj".into();
        let mut missing_direct = double_object_document("dative", "dobj");
        missing_direct.tokens[3].dep = "dep".into();
        for doc in [missing, competing, only_direct, missing_direct] {
            let result = alternation(&doc, &r);
            let f = double_frame(&result);
            assert!(!f.syntax_binding_complete);
            assert!(f.bindings[2].alternatives.is_empty());
            assert!(f.bindings[3].alternatives.is_empty());
            assert!(
                f.findings
                    .iter()
                    .any(|f| f.constraint == "pinned_double_object_pattern"
                        && f.status == Status::Unresolved)
            );
        }
    }

    #[test]
    fn double_objects_reject_passive_coordination_case_clauses_and_reversed_order() {
        let r = double_object_resource();
        let mut passive = double_object_document("dative", "dobj");
        passive.tokens[1]
            .morph
            .insert("Voice".into(), vec!["Pass".into()]);
        let mut coordinated = double_object_document("dative", "dobj");
        coordinated.tokens[1].dep = "conj".into();
        let marked = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("to", "to", "ADP", "case", 3),
            ("Lee", "lee", "PROPN", "iobj", 1),
            ("books", "book", "NOUN", "obj", 1),
        ]);
        let relative = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Lee", "lee", "PROPN", "dative", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("that", "that", "PRON", "nsubj", 5),
            ("help", "help", "VERB", "relcl", 3),
        ]);
        let reversed = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("Lee", "lee", "PROPN", "dative", 1),
        ]);
        let extra_argument = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Lee", "lee", "PROPN", "dative", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("Pat", "pat", "PROPN", "obl", 1),
        ]);
        for doc in [
            passive,
            coordinated,
            marked,
            relative,
            reversed,
            extra_argument,
        ] {
            let result = alternation(&doc, &r);
            assert!(!double_frame(&result).syntax_binding_complete);
            assert!(double_frame(&result).bindings[2].alternatives.is_empty());
        }
    }

    #[test]
    fn double_object_roles_preserve_swapped_repeated_occurrences() {
        let r = double_object_resource();
        for (recipient, theme) in [("Kim", "Lee"), ("Lee", "Kim")] {
            let doc = document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                (recipient, recipient, "PROPN", "dative", 1),
                (theme, theme, "PROPN", "dobj", 1),
            ]);
            let result = alternation(&doc, &r);
            let f = double_frame(&result);
            assert!(f.syntax_binding_complete);
            assert_eq!(f.bindings[0].alternatives[0].anchor.token, doc.tokens[0]);
            assert_eq!(f.bindings[2].alternatives[0].anchor.token, doc.tokens[2]);
            assert_eq!(f.bindings[3].alternatives[0].anchor.token, doc.tokens[3]);
            assert_eq!(f.bindings[2].alternatives[0].anchor.token.text, recipient);
            assert_eq!(f.bindings[3].alternatives[0].anchor.token.text, theme);
        }
    }

    #[test]
    fn double_object_support_requires_exact_pinned_frame_origin_and_layout() {
        let doc = double_object_document("dative", "dobj");
        for mutation in 0..4 {
            let mut r = double_object_resource();
            let f = &mut r.classes.get_mut("give-13.1").unwrap().effective_frames[1];
            match mutation {
                0 => f.id = "[\"give-13.1\",2]".into(),
                1 => f.origin.declaring_class_id = "other".into(),
                2 => f.origin.source_sha256 = "0".repeat(64),
                _ => {
                    let syntax = f
                        .xml
                        .children
                        .iter_mut()
                        .find_map(|n| match n {
                            XmlNode::Element(e) if e.is("SYNTAX") => Some(e),
                            _ => None,
                        })
                        .unwrap();
                    let recipient = syntax
                        .children
                        .iter_mut()
                        .filter_map(|n| match n {
                            XmlNode::Element(e) => Some(e),
                            _ => None,
                        })
                        .nth(2)
                        .unwrap();
                    recipient
                        .attributes
                        .iter_mut()
                        .find(|a| a.name == "value")
                        .unwrap()
                        .value = "Theme".into();
                }
            }
            let result = alternation(&doc, &r);
            assert!(!double_frame(&result).syntax_binding_complete);
            assert!(
                !double_frame(&result)
                    .findings
                    .iter()
                    .any(|f| f.constraint == "pinned_double_object_pattern")
            );
        }
    }

    #[test]
    fn alternation_preserves_prepositional_mapping_and_unrelated_frame_bodies() {
        let r = double_object_resource();
        let doc = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("to", "to", "ADP", "dative", 1),
            ("Lee", "lee", "PROPN", "pobj", 3),
        ]);
        let v2 = observe_with_mapping(&doc, &r, &BTreeSet::new(), Mapping::PrepositionalDativeV1)
            .unwrap();
        let v3 = alternation(&doc, &r);
        assert!(v3.predicates[0].frame_possibilities[0].syntax_binding_complete);
        assert_eq!(
            serde_json::to_value(&v2.predicates[0].frame_possibilities[0]).unwrap(),
            serde_json::to_value(&v3.predicates[0].frame_possibilities[0]).unwrap()
        );
        assert_eq!(
            v2.predicates[0].frame_possibilities.len(),
            v3.predicates[0].frame_possibilities.len()
        );
        let unrelated = resource(
            "build",
            &frame("<NP value='Agent'/><VERB/><NP value='Theme'/>"),
        );
        let a = observe_with_mapping(
            &transitive(),
            &unrelated,
            &BTreeSet::new(),
            Mapping::PrepositionalDativeV1,
        )
        .unwrap();
        let b = alternation(&transitive(), &unrelated);
        assert_eq!(
            serde_json::to_value(a.predicates).unwrap(),
            serde_json::to_value(b.predicates).unwrap()
        );
    }
}
