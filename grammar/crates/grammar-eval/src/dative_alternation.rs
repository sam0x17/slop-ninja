//! Exact participant movement for one pinned VerbNet alternation.
//! Neither a proposal nor a checked structural alignment certifies meaning.
use crate::lexical_context::{lexical, normalized_lemma};
use crate::lexical_frame_observation::{self as observation, FrameObservation, Mapping};
use crate::predicate_operators;
use crate::verbnet_resource::Resource;
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Candidate, Edit, RuleIdentity},
    syntax::{self, Document},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[path = "dative_argument_policy.rs"]
pub mod argument_policy;
pub use argument_policy::ArgumentPolicy;

pub const SCHEMA: &str = "slopninja-dative-alternation-v1";
const CLASS: &str = "give-13.1";
const PREPOSITIONAL: &str = "[\"give-13.1\",0]";
const DOUBLE_OBJECT: &str = "[\"give-13.1\",1]";
const XML_SHA: &str = "bbddd2c3cbe0a8a6862eee7a140827938921df2eaf7fc2852f3f4d133800fbee";
// A declared conservative surface exclusion, not a general quantifier parser.
const QUANTIFIERS: &[&str] = &[
    "all",
    "any",
    "both",
    "each",
    "either",
    "enough",
    "every",
    "few",
    "fewer",
    "fewest",
    "half",
    "less",
    "little",
    "many",
    "more",
    "most",
    "much",
    "neither",
    "no",
    "none",
    "several",
    "some",
    "such",
    "various",
    "numerous",
    "countless",
    "multiple",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "dozen",
    "hundred",
    "thousand",
    "million",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ToDoubleObject,
    ToPrepositional,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NounPhrase {
    pub head_index: usize,
    pub span: [usize; 2],
    pub token_indices: Vec<usize>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RolePhrases {
    pub agent: NounPhrase,
    pub theme: NounPhrase,
    pub recipient: NounPhrase,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ByteSegment {
    pub source: [usize; 2],
    pub candidate: [usize; 2],
    pub purpose: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Proposal {
    #[serde(skip_serializing_if = "ArgumentPolicy::is_legacy")]
    pub argument_policy: ArgumentPolicy,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub argument_assessments: Vec<Value>,
    pub id: String,
    pub direction: Direction,
    pub predicate_index: usize,
    pub member_class_id: String,
    pub member_ordinal: usize,
    pub source_frame_id: String,
    pub target_frame_id: String,
    pub roles: RolePhrases,
    pub source_preposition: Option<usize>,
    pub candidate: Candidate,
    pub inverse_edits: Vec<Edit>,
    pub byte_segments: Vec<ByteSegment>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Site {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub argument_assessments: Vec<Value>,
    pub predicate_index: usize,
    pub member_class_id: Option<String>,
    pub member_ordinal: Option<usize>,
    pub source_frame_id: Option<String>,
    pub proposal_id: Option<String>,
    pub rejection: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProposalReport {
    #[serde(skip_serializing_if = "ArgumentPolicy::is_legacy")]
    pub argument_policy: ArgumentPolicy,
    pub schema: String,
    pub source_sha256: String,
    pub parser_identity: String,
    pub proposals: Vec<Proposal>,
    pub sites: Vec<Site>,
    pub coverage_limits: Vec<String>,
    pub automatic_edit_license: bool,
    pub semantic_equivalence_certified: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    ObservedPreserved,
    Changed,
    Unresolved,
}
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub condition: String,
    pub outcome: Outcome,
    pub evidence: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct TokenAlignment {
    pub source_index: usize,
    pub candidate_index: usize,
    pub source_span: [usize; 2],
    pub candidate_span: [usize; 2],
}
#[derive(Clone, Debug, Serialize)]
pub struct AlignmentReport {
    #[serde(skip_serializing_if = "ArgumentPolicy::is_legacy")]
    pub argument_policy: ArgumentPolicy,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_argument_assessments: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidate_argument_assessments: Vec<Value>,
    pub schema: String,
    pub proposal_id: String,
    pub source_sha256: String,
    pub candidate_sha256: String,
    pub parser_identity: String,
    pub outcome: Outcome,
    pub findings: Vec<Finding>,
    pub token_alignment: Vec<TokenAlignment>,
    pub source_roles: RolePhrases,
    pub candidate_roles: Option<RolePhrases>,
    pub automatic_edit_license: bool,
    pub semantic_equivalence_certified: bool,
}

fn registry(resource: &Resource) -> Result<()> {
    let class = resource
        .classes
        .get(CLASS)
        .context("Missing pinned give-13.1 class")?;
    for id in [PREPOSITIONAL, DOUBLE_OBJECT] {
        let frames = class
            .effective_frames
            .iter()
            .filter(|f| f.id == id)
            .collect::<Vec<_>>();
        ensure!(
            frames.len() == 1
                && frames[0].origin.declaring_class_id == CLASS
                && frames[0].origin.source_sha256 == XML_SHA,
            "Pinned alternation frame provenance differs"
        );
    }
    Ok(())
}
fn registered_class(resource: &Resource, id: &str) -> bool {
    resource.classes.get(id).is_some_and(|class| {
        [PREPOSITIONAL, DOUBLE_OBJECT].iter().all(|id| {
            let frames = class
                .effective_frames
                .iter()
                .filter(|f| f.id == *id)
                .collect::<Vec<_>>();
            frames.len() == 1
                && frames[0].origin.declaring_class_id == CLASS
                && frames[0].origin.source_sha256 == XML_SHA
        })
    })
}
fn children(doc: &Document) -> Vec<Vec<usize>> {
    let mut result = vec![Vec::new(); doc.tokens.len()];
    for t in &doc.tokens {
        if t.head != t.i {
            result[t.head].push(t.i);
        }
    }
    result
}
fn phrase(doc: &Document, kids: &[Vec<usize>], head: usize) -> Result<NounPhrase> {
    ensure!(
        matches!(doc.tokens[head].pos.as_str(), "NOUN" | "PROPN"),
        "NP head is not an explicit noun or proper noun"
    );
    let mut tokens = vec![head];
    let mut at = 0;
    while at < tokens.len() {
        tokens.extend(&kids[tokens[at]]);
        at += 1;
    }
    tokens.sort_unstable();
    ensure!(
        tokens.last().unwrap() - tokens[0] + 1 == tokens.len(),
        "NP subtree is discontinuous"
    );
    for &i in &tokens {
        let t = &doc.tokens[i];
        let lemma = normalized_lemma(t).context("Missing NP lemma")?;
        ensure!(
            lexical(t)
                && !QUANTIFIERS.contains(&lemma.as_str())
                && !t.morph.contains_key("NumType")
                && !t.morph.contains_key("Poss"),
            "NP quantifier or nonlexical/possessive evidence"
        );
        if i != head {
            ensure!(
                match (t.pos.as_str(), t.dep.as_str()) {
                    ("DET", "det") => matches!(lemma.as_str(), "a" | "an" | "the"),
                    ("ADJ", "amod") => true,
                    ("NOUN" | "PROPN", "compound" | "flat" | "flat:name") => true,
                    _ => false,
                },
                "Unsupported NP modifier, pronoun, possessive, clause or coordination"
            );
        }
    }
    for pair in tokens.windows(2) {
        ensure!(
            &doc.text[doc.tokens[pair[0]].end_byte..doc.tokens[pair[1]].start_byte] == " ",
            "NP spacing is not one ordinary space"
        );
    }
    Ok(NounPhrase {
        head_index: head,
        span: [
            doc.tokens[tokens[0]].start_byte,
            doc.tokens[*tokens.last().unwrap()].end_byte,
        ],
        token_indices: tokens,
    })
}
fn role_head(frame: &FrameObservation, name: &str) -> Result<usize> {
    let bindings = frame
        .bindings
        .iter()
        .filter(|b| b.role_name.as_deref() == Some(name))
        .collect::<Vec<_>>();
    ensure!(
        bindings.len() == 1 && bindings[0].unique && bindings[0].alternatives.len() == 1,
        "Role is not uniquely bound"
    );
    ensure!(
        bindings[0].alternatives[0].proposition_head_index.is_none(),
        "Clausal role unsupported"
    );
    Ok(bindings[0].alternatives[0].anchor.token.i)
}
fn roles(doc: &Document, kids: &[Vec<usize>], frame: &FrameObservation) -> Result<RolePhrases> {
    let result = RolePhrases {
        agent: phrase(doc, kids, role_head(frame, "Agent")?)?,
        theme: phrase(doc, kids, role_head(frame, "Theme")?)?,
        recipient: phrase(doc, kids, role_head(frame, "Recipient")?)?,
    };
    let mut used = BTreeSet::new();
    for p in [&result.agent, &result.theme, &result.recipient] {
        for &i in &p.token_indices {
            ensure!(used.insert(i), "Role NPs overlap");
        }
    }
    Ok(result)
}

fn assess_roles(
    doc: &Document,
    frame: &FrameObservation,
) -> Result<Vec<argument_policy::ArgumentAssessment>> {
    use argument_policy::{Role, assess, describe};
    [
        (Role::Agent, "Agent"),
        (Role::Theme, "Theme"),
        (Role::Recipient, "Recipient"),
    ]
    .into_iter()
    .map(|(role, name)| assess(doc, role, describe(doc, role_head(frame, name)?)?))
    .collect()
}
fn assessment_values(doc: &Document, frame: &FrameObservation) -> Result<Vec<Value>> {
    match assess_roles(doc, frame) {
        Ok(values) => values
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(Into::into),
        Err(error) => Ok(vec![
            json!({"decision":"unresolved","role_binding_assessment_error":error.to_string(),"frame_id":frame.frame.id}),
        ]),
    }
}
fn roles_with_policy(
    doc: &Document,
    kids: &[Vec<usize>],
    frame: &FrameObservation,
    policy: ArgumentPolicy,
) -> Result<(RolePhrases, Vec<Value>)> {
    if policy.is_legacy() {
        return Ok((roles(doc, kids, frame)?, Vec::new()));
    }
    let assessments = assess_roles(doc, frame)?;
    for a in &assessments {
        ensure!(
            a.decision == argument_policy::Decision::SupportedForStructuralTest,
            "RoleAwareV1 {:?}: {:?} ({})",
            a.role,
            a.decision,
            a.findings
                .iter()
                .filter(|f| f.decision != argument_policy::Decision::SupportedForStructuralTest)
                .map(|f| f.condition.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let phrases = assessments
        .iter()
        .map(|a| NounPhrase {
            head_index: a.phrase.head_index,
            span: a.phrase.bounding_span,
            token_indices: a.phrase.token_indices.clone(),
        })
        .collect::<Vec<_>>();
    let mut used = BTreeSet::new();
    for p in &phrases {
        for &i in &p.token_indices {
            ensure!(used.insert(i), "Role NPs overlap");
        }
    }
    Ok((
        RolePhrases {
            agent: phrases[0].clone(),
            theme: phrases[1].clone(),
            recipient: phrases[2].clone(),
        },
        assessments
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?,
    ))
}
fn make(
    doc: &Document,
    kids: &[Vec<usize>],
    head: usize,
    frame: &FrameObservation,
    policy: ArgumentPolicy,
) -> Result<Proposal> {
    ensure!(
        frame.syntax_binding_complete,
        "Source pinned frame is not complete"
    );
    let (roles, argument_assessments) = roles_with_policy(doc, kids, frame, policy)?;
    ensure!(
        roles.agent.span[1] <= doc.tokens[head].start_byte,
        "Subject does not precede predicate"
    );
    let theme = &doc.text[roles.theme.span[0]..roles.theme.span[1]];
    let recipient = &doc.text[roles.recipient.span[0]..roles.recipient.span[1]];
    let (
        direction,
        start,
        end,
        replacement,
        source_preposition,
        theme_start,
        recipient_start,
        target,
    ) = if frame.frame.id == PREPOSITIONAL {
        let b = frame
            .bindings
            .iter()
            .find(|b| b.role_name.as_deref() == Some("Recipient"))
            .unwrap();
        let a = &b.alternatives[0];
        ensure!(
            a.supporting_anchors.len() == 1,
            "Prepositional role has ambiguous carrier"
        );
        let p = &doc.tokens[a.supporting_anchors[0].token.i];
        ensure!(
            p.text == "to"
                && p.pos == "ADP"
                && p.head == head
                && matches!(p.dep.as_str(), "prep" | "dative"),
            "Carrier is not a direct literal to"
        );
        ensure!(
            roles.theme.span[1] < p.start_byte
                && p.end_byte < roles.recipient.span[0]
                && &doc.text[roles.theme.span[1]..p.start_byte] == " "
                && &doc.text[p.end_byte..roles.recipient.span[0]] == " ",
            "Object layout is not Theme to Recipient"
        );
        let start = roles.theme.span[0];
        (
            Direction::ToDoubleObject,
            start,
            roles.recipient.span[1],
            format!("{recipient} {theme}"),
            Some(p.i),
            start + recipient.len() + 1,
            start,
            DOUBLE_OBJECT,
        )
    } else {
        ensure!(
            roles.recipient.span[1] < roles.theme.span[0]
                && &doc.text[roles.recipient.span[1]..roles.theme.span[0]] == " ",
            "Object layout is not Recipient Theme"
        );
        let start = roles.recipient.span[0];
        (
            Direction::ToPrepositional,
            start,
            roles.theme.span[1],
            format!("{theme} to {recipient}"),
            None,
            start,
            start + theme.len() + 4,
            PREPOSITIONAL,
        )
    };
    ensure!(
        doc.tokens[head].end_byte <= start,
        "Objects do not follow predicate"
    );
    let candidate = edits::candidate(
        &doc.text,
        RuleIdentity {
            rule: "dative_alternation",
            rule_version: policy.schema(),
            catalog_version: policy.schema(),
            parser_identity: &doc.parser_identity,
        },
        vec![Edit {
            start_byte: start,
            end_byte: end,
            expected: doc.text[start..end].into(),
            replacement: replacement.clone(),
        }],
        if policy.is_legacy() {
            &[
                "complete pinned same-class frames",
                "simple disjoint contiguous nominal phrases",
            ]
        } else {
            &[
                "complete pinned same-class frames",
                "role-aware unchanged Agent and supported moved arguments; reference unresolved",
            ]
        },
        &[
            "VerbNet sense and selectional restrictions remain unresolved",
            "Dative constructions can differ in discourse and affectedness; review meaning and tone",
        ],
    )?;
    let inverse_edits = vec![Edit {
        start_byte: start,
        end_byte: start + replacement.len(),
        expected: replacement,
        replacement: doc.text[start..end].into(),
    }];
    ensure!(
        edits::apply(&candidate.text, &inverse_edits)? == doc.text,
        "Inverse patches do not restore source"
    );
    let segments = vec![
        ByteSegment {
            source: [0, start],
            candidate: [0, start],
            purpose: "untouched_prefix".into(),
        },
        ByteSegment {
            source: roles.theme.span,
            candidate: [theme_start, theme_start + theme.len()],
            purpose: "theme".into(),
        },
        ByteSegment {
            source: roles.recipient.span,
            candidate: [recipient_start, recipient_start + recipient.len()],
            purpose: "recipient".into(),
        },
        ByteSegment {
            source: [end, doc.text.len()],
            candidate: [inverse_edits[0].end_byte, candidate.text.len()],
            purpose: "untouched_suffix".into(),
        },
    ];
    let legacy_id = serde_json::to_string(&(
        head,
        &frame.member_class_id,
        frame.member_ordinal,
        &frame.frame.id,
        &candidate.edits,
    ))?;
    let id = if policy.is_legacy() {
        edits::digest(&legacy_id)
    } else {
        edits::digest(&serde_json::to_string(&(policy, &legacy_id))?)
    };
    Ok(Proposal {
        argument_policy: policy,
        argument_assessments,
        id,
        direction,
        predicate_index: head,
        member_class_id: frame.member_class_id.clone(),
        member_ordinal: frame.member_ordinal,
        source_frame_id: frame.frame.id.clone(),
        target_frame_id: target.into(),
        roles,
        source_preposition,
        candidate,
        inverse_edits,
        byte_segments: segments,
    })
}

pub fn propose(source: &Document, resource: &Resource) -> Result<ProposalReport> {
    propose_with_policy(source, resource, ArgumentPolicy::LegacyV1)
}

pub fn propose_with_policy(
    source: &Document,
    resource: &Resource,
    policy: ArgumentPolicy,
) -> Result<ProposalReport> {
    syntax::validate(source)?;
    registry(resource)?;
    let observed = observation::observe_with_mapping(
        source,
        resource,
        &BTreeSet::new(),
        Mapping::DativeAlternationV1,
    )?;
    let kids = children(source);
    let mut proposals = Vec::new();
    let mut sites = Vec::new();
    for p in &observed.predicates {
        let mut considered = false;
        for f in p.frame_possibilities.iter().filter(|f| {
            registered_class(resource, &f.member_class_id)
                && [PREPOSITIONAL, DOUBLE_OBJECT].contains(&f.frame.id.as_str())
        }) {
            considered = true;
            let made = make(source, &kids, p.head.token.i, f, policy);
            let (proposal_id, rejection, argument_assessments) = match made {
                Ok(value) => {
                    let id = value.id.clone();
                    let assessments = value.argument_assessments.clone();
                    proposals.push(value);
                    (Some(id), None, assessments)
                }
                Err(error) => (
                    None,
                    Some(error.to_string()),
                    if !policy.is_legacy() && f.syntax_binding_complete {
                        assessment_values(source, f)?
                    } else {
                        Vec::new()
                    },
                ),
            };
            sites.push(Site {
                argument_assessments,
                predicate_index: p.head.token.i,
                member_class_id: Some(f.member_class_id.clone()),
                member_ordinal: Some(f.member_ordinal),
                source_frame_id: Some(f.frame.id.clone()),
                proposal_id,
                rejection,
            });
        }
        if !considered {
            sites.push(Site {
                argument_assessments: Vec::new(),
                predicate_index: p.head.token.i,
                member_class_id: None,
                member_ordinal: None,
                source_frame_id: None,
                proposal_id: None,
                rejection: Some("No registered same-class frame alternative".into()),
            });
        }
    }
    let mut report=ProposalReport {argument_policy:policy,schema:policy.schema().into(),source_sha256:edits::digest(&source.text),parser_identity:source.parser_identity.clone(),proposals,sites,coverage_limits:vec!["Only give-13.1 frame0/frame1 in an effective member class retaining both pinned frames; same membership across the edit, no sense selection".into(),"No candidate cap or text deduplication; each frame membership is retained".into(),"Single-space object layout and simple NP subtrees only".into(),format!("Quantifier exclusion uses NUM/unsupported POS and morphology plus fixed normalized-lemma list: {}",QUANTIFIERS.join(", ")),"Unchanged source must be retained separately by the caller".into()],automatic_edit_license:false,semantic_equivalence_certified:false};
    if !policy.is_legacy() {
        report.coverage_limits[2]="Single-space object layout; unchanged Agent subtree, simple moved Theme nominals and nominal or singleton oblique Recipient; my/our/your modifiers supported without resolving reference".into();
        report.coverage_limits[3] = format!(
            "Moved quantifiers, reflexives, anaphoric possessives and unsupported modifiers remain excluded; unchanged Agent modifiers are preserved, not subjected to moved-NP gate. Quantifier list: {}",
            QUANTIFIERS.join(", ")
        );
    }
    Ok(report)
}

fn finding(report: &mut AlignmentReport, condition: &str, outcome: Outcome, evidence: Value) {
    report.findings.push(Finding {
        condition: condition.into(),
        outcome,
        evidence,
    });
}
fn mapped_span(proposal: &Proposal, span: [usize; 2]) -> Option<[usize; 2]> {
    proposal
        .byte_segments
        .iter()
        .find(|s| s.source[0] <= span[0] && span[1] <= s.source[1])
        .map(|s| {
            [
                s.candidate[0] + span[0] - s.source[0],
                s.candidate[0] + span[1] - s.source[0],
            ]
        })
}
fn mapped_phrase(
    source: &NounPhrase,
    map: &BTreeMap<usize, usize>,
    candidate: &NounPhrase,
    proposal: &Proposal,
) -> bool {
    map.get(&source.head_index) == Some(&candidate.head_index)
        && mapped_span(proposal, source.span) == Some(candidate.span)
        && source
            .token_indices
            .iter()
            .map(|i| map.get(i).copied())
            .collect::<Option<Vec<_>>>()
            == Some(candidate.token_indices.clone())
}

/// Compare the actual reparsed bytes to a regenerated authorized proposal.
/// `observed_preserved` is restricted to these structural checks, not semantics.
pub fn compare(
    source: &Document,
    candidate: &Document,
    proposal: &Proposal,
    resource: &Resource,
) -> Result<AlignmentReport> {
    syntax::validate(source)?;
    syntax::validate(candidate)?;
    ensure!(
        source.parser_identity == candidate.parser_identity,
        "Parser identities differ"
    );
    ensure!(
        propose_with_policy(source, resource, proposal.argument_policy)?
            .proposals
            .iter()
            .any(|p| p == proposal),
        "Proposal is not an exact generated source proposal"
    );
    ensure!(
        candidate.text == proposal.candidate.text
            && edits::apply(&source.text, &proposal.candidate.edits)? == candidate.text
            && edits::apply(&candidate.text, &proposal.inverse_edits)? == source.text,
        "Candidate/patch/inverse bytes differ"
    );
    let mut report = AlignmentReport {
        argument_policy: proposal.argument_policy,
        source_argument_assessments: proposal.argument_assessments.clone(),
        candidate_argument_assessments: Vec::new(),
        schema: proposal.argument_policy.schema().into(),
        proposal_id: proposal.id.clone(),
        source_sha256: edits::digest(&source.text),
        candidate_sha256: edits::digest(&candidate.text),
        parser_identity: source.parser_identity.clone(),
        outcome: Outcome::ObservedPreserved,
        findings: Vec::new(),
        token_alignment: Vec::new(),
        source_roles: proposal.roles.clone(),
        candidate_roles: None,
        automatic_edit_license: false,
        semantic_equivalence_certified: false,
    };
    let mut map = BTreeMap::new();
    let mut used = BTreeSet::new();
    for t in &source.tokens {
        if proposal.source_preposition == Some(t.i) {
            continue;
        }
        let span = mapped_span(proposal, [t.start_byte, t.end_byte]);
        let matched = candidate
            .tokens
            .iter()
            .find(|c| Some([c.start_byte, c.end_byte]) == span && c.text == t.text);
        if let Some(c) = matched {
            ensure!(used.insert(c.i), "Token correspondence is not injective");
            map.insert(t.i, c.i);
            report.token_alignment.push(TokenAlignment {
                source_index: t.i,
                candidate_index: c.i,
                source_span: [t.start_byte, t.end_byte],
                candidate_span: [c.start_byte, c.end_byte],
            });
            let known_difference = t.pos != c.pos
                || t.tag != c.tag
                || t.morph != c.morph
                || t.is_punct != c.is_punct
                || t.is_space != c.is_space
                || matches!((normalized_lemma(t),normalized_lemma(c)),(Some(a),Some(b)) if a!=b);
            let outcome = if known_difference {
                Outcome::Changed
            } else if lexical(t) && (normalized_lemma(t).is_none() || normalized_lemma(c).is_none())
            {
                Outcome::Unresolved
            } else {
                Outcome::ObservedPreserved
            };
            finding(
                &mut report,
                "token_lexical_morphology",
                outcome,
                json!({"source":t.i,"candidate":c.i}),
            );
        } else {
            finding(
                &mut report,
                "unmapped_source_token",
                Outcome::Unresolved,
                json!({"source_token":t,"expected_span":span}),
            );
        }
    }
    let additions = candidate
        .tokens
        .iter()
        .filter(|c| !used.contains(&c.i))
        .collect::<Vec<_>>();
    let inserted = if proposal.direction == Direction::ToPrepositional
        && additions.len() == 1
        && additions[0].text == "to"
        && additions[0].pos == "ADP"
    {
        Some(additions[0].i)
    } else {
        None
    };
    if !(additions.is_empty() && proposal.direction == Direction::ToDoubleObject)
        && inserted.is_none()
    {
        finding(
            &mut report,
            "unmapped_candidate_tokens",
            Outcome::Unresolved,
            json!({"tokens":additions}),
        );
    }
    let candidate_observation = observation::observe_with_mapping(
        candidate,
        resource,
        &BTreeSet::new(),
        Mapping::DativeAlternationV1,
    )?;
    let mut candidate_role_sets = Vec::new();
    for p in &candidate_observation.predicates {
        if map.get(&proposal.predicate_index) != Some(&p.head.token.i) {
            continue;
        }
        for f in p.frame_possibilities.iter().filter(|f| {
            f.member_class_id == proposal.member_class_id
                && f.member_ordinal == proposal.member_ordinal
                && f.frame.id == proposal.target_frame_id
                && f.syntax_binding_complete
        }) {
            if !proposal.argument_policy.is_legacy() {
                report
                    .candidate_argument_assessments
                    .extend(assessment_values(candidate, f)?);
            }
            if let Ok((r, assessments)) =
                roles_with_policy(candidate, &children(candidate), f, proposal.argument_policy)
            {
                candidate_role_sets.push((r, f, assessments));
            }
        }
    }
    if candidate_role_sets.len() == 1 {
        let (r, f, assessments) = &candidate_role_sets[0];
        if report.candidate_argument_assessments.is_empty() {
            report.candidate_argument_assessments = assessments.clone();
        }
        let preserved = mapped_phrase(&proposal.roles.agent, &map, &r.agent, proposal)
            && mapped_phrase(&proposal.roles.theme, &map, &r.theme, proposal)
            && mapped_phrase(&proposal.roles.recipient, &map, &r.recipient, proposal);
        finding(
            &mut report,
            "same_class_opposite_frame_roles",
            if preserved {
                Outcome::ObservedPreserved
            } else {
                Outcome::Changed
            },
            json!({"source_frame":proposal.source_frame_id,"candidate_frame":f.frame.id,"candidate_roles":r}),
        );
        if let Some(inserted) = inserted {
            let carriers = f
                .bindings
                .iter()
                .filter(|b| b.role_name.as_deref() == Some("Recipient"))
                .flat_map(|b| &b.alternatives)
                .flat_map(|a| &a.supporting_anchors)
                .map(|a| a.token.i)
                .collect::<Vec<_>>();
            finding(
                &mut report,
                "inserted_carrier_role",
                if carriers == vec![inserted] {
                    Outcome::ObservedPreserved
                } else {
                    Outcome::Changed
                },
                json!({"inserted":inserted,"role_carriers":carriers}),
            );
        }
        report.candidate_roles = Some(r.clone());
    } else {
        finding(
            &mut report,
            "same_class_opposite_frame_roles",
            Outcome::Unresolved,
            json!({"complete_simple_matches":candidate_role_sets.len()}),
        );
    }
    for (&s, &c) in &map {
        let a = &source.tokens[s];
        let b = &candidate.tokens[c];
        let recipient = s == proposal.roles.recipient.head_index;
        let allowed = if recipient {
            match proposal.direction {
                Direction::ToDoubleObject => {
                    a.dep == "pobj"
                        && Some(a.head) == proposal.source_preposition
                        && matches!(b.dep.as_str(), "dative" | "iobj")
                        && map.get(&proposal.predicate_index) == Some(&b.head)
                }
                Direction::ToPrepositional => {
                    matches!(a.dep.as_str(), "dative" | "iobj")
                        && a.head == proposal.predicate_index
                        && b.dep == "pobj"
                        && Some(b.head) == inserted
                }
            }
        } else {
            a.dep == b.dep && map.get(&a.head) == Some(&b.head)
        };
        finding(
            &mut report,
            "dependency_attachment",
            if allowed {
                Outcome::ObservedPreserved
            } else {
                Outcome::Changed
            },
            json!({"source":s,"candidate":c,"source_parent":a.head,"candidate_parent":b.head,"source_dep":a.dep,"candidate_dep":b.dep,"licensed_recipient_change":recipient}),
        );
        if a.sentence != b.sentence {
            finding(
                &mut report,
                "sentence_partition",
                Outcome::Changed,
                json!({"source":s,"candidate":c}),
            );
        }
    }
    let source_events = predicate_operators::document_observations(source)?;
    let target_events = predicate_operators::document_observations(candidate)?;
    let target_by_head = target_events
        .iter()
        .map(|p| (p.head_index, &p.event))
        .collect::<BTreeMap<_, _>>();
    let mut matched_heads = BTreeSet::new();
    for p in source_events {
        if let Some(&target_head) = map.get(&p.head_index) {
            if let Some(event) = target_by_head.get(&target_head) {
                matched_heads.insert(target_head);
                finding(
                    &mut report,
                    "predicate_operator_evidence",
                    if p.event == **event {
                        Outcome::ObservedPreserved
                    } else {
                        Outcome::Changed
                    },
                    json!({"source_head":p.head_index,"candidate_head":target_head,"source_event":p.event,"candidate_event":event}),
                );
            } else {
                finding(
                    &mut report,
                    "unmapped_predicate",
                    Outcome::Unresolved,
                    json!({"source_head":p.head_index,"candidate_head":target_head}),
                );
            }
        } else {
            finding(
                &mut report,
                "unmapped_predicate",
                Outcome::Unresolved,
                json!({"source_head":p.head_index}),
            );
        }
    }
    if target_by_head
        .keys()
        .any(|head| !matched_heads.contains(head))
    {
        finding(
            &mut report,
            "additional_predicates",
            Outcome::Unresolved,
            json!({"unmatched_heads":target_by_head.keys().filter(|h|!matched_heads.contains(h)).collect::<Vec<_>>()}),
        );
    }
    report.outcome = if report
        .findings
        .iter()
        .any(|f| f.outcome == Outcome::Changed)
    {
        Outcome::Changed
    } else if report
        .findings
        .iter()
        .any(|f| f.outcome == Outcome::Unresolved)
    {
        Outcome::Unresolved
    } else {
        Outcome::ObservedPreserved
    };
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbnet_resource::XmlSource;
    use grammar_core::syntax::{Sentence, Token};

    fn resource() -> Resource {
        let frame = |syntax: &str| {
            format!(
                "<FRAME><DESCRIPTION primary='synthetic'/><EXAMPLES/><SYNTAX>{syntax}</SYNTAX><SEMANTICS/></FRAME>"
            )
        };
        let xml = format!(
            "<VNCLASS ID='give-13.1'><MEMBERS><MEMBER name='lend'/><MEMBER name='loan'/></MEMBERS><THEMROLES><THEMROLE type='Agent'/><THEMROLE type='Theme'/><THEMROLE type='Recipient'/></THEMROLES><FRAMES>{}{}</FRAMES><SUBCLASSES><VNSUBCLASS ID='give-13.1-1'><MEMBERS><MEMBER name='give'/></MEMBERS></VNSUBCLASS></SUBCLASSES></VNCLASS>",
            frame(
                "<NP value='Agent'/><VERB/><NP value='Theme'/><PREP value='to'/><NP value='Recipient'/>"
            ),
            frame("<NP value='Agent'/><VERB/><NP value='Recipient'/><NP value='Theme'/>")
        );
        let mut r = Resource::from_sources(vec![XmlSource {
            path: "synthetic.xml".into(),
            sha256: edits::digest(&xml),
            xml,
        }])
        .unwrap();
        // Explicit in-memory provenance stub for matcher units only. This is
        // not a valid resource export and does not relax production XML hashes.
        for class in r.classes.values_mut() {
            for frame in &mut class.effective_frames {
                frame.origin.source_sha256 = XML_SHA.into();
            }
        }
        r
    }
    fn document(items: &[(&str, &str, &str, &str, usize)]) -> Document {
        let mut text = String::new();
        let mut tokens = Vec::new();
        for (i, &(surface, lemma, pos, dep, head)) in items.iter().enumerate() {
            if i > 0 {
                text.push(' ');
            }
            let start = text.len();
            text.push_str(surface);
            tokens.push(Token {
                i,
                start_byte: start,
                end_byte: text.len(),
                text: surface.into(),
                lemma: lemma.into(),
                pos: pos.into(),
                tag: if pos == "VERB" { "VBD" } else { pos }.into(),
                dep: dep.into(),
                head,
                sentence: 0,
                morph: if pos == "VERB" {
                    BTreeMap::from([
                        ("VerbForm".into(), vec!["Fin".into()]),
                        ("Tense".into(), vec!["Past".into()]),
                    ])
                } else {
                    BTreeMap::new()
                },
                is_punct: pos == "PUNCT",
                is_space: false,
            });
        }
        let root = tokens.iter().find(|t| t.i == t.head).unwrap().i;
        let d = Document {
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: text.len(),
                root,
                token_start: 0,
                token_end: tokens.len(),
            }],
            text,
            parser_identity: "synthetic-v1".into(),
            tokens,
        };
        syntax::validate(&d).unwrap();
        d
    }
    fn pair() -> (Document, Document) {
        (
            document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                ("a", "a", "DET", "det", 3),
                ("book", "book", "NOUN", "dobj", 1),
                ("to", "to", "ADP", "dative", 1),
                ("Lee", "lee", "PROPN", "pobj", 4),
                (".", ".", "PUNCT", "punct", 1),
            ]),
            document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                ("Lee", "lee", "PROPN", "dative", 1),
                ("a", "a", "DET", "det", 4),
                ("book", "book", "NOUN", "dobj", 1),
                (".", ".", "PUNCT", "punct", 1),
            ]),
        )
    }
    fn one(doc: &Document, r: &Resource) -> Proposal {
        let p = propose(doc, r).unwrap();
        assert_eq!(p.proposals.len(), 1, "{:?}", p.sites);
        p.proposals[0].clone()
    }

    #[test]
    fn both_directions_move_exact_bytes_and_retain_full_inverse() {
        let r = resource();
        let (to, bare) = pair();
        for (source, candidate) in [(&to, &bare), (&bare, &to)] {
            let p = one(source, &r);
            assert_eq!(p.candidate.text, candidate.text);
            assert_eq!(
                edits::apply(&candidate.text, &p.inverse_edits).unwrap(),
                source.text
            );
            assert_eq!(p.candidate.edits.len(), 1);
            let check = compare(source, candidate, &p, &r).unwrap();
            assert_eq!(
                check.outcome,
                Outcome::ObservedPreserved,
                "{:?}",
                check.findings
            );
            assert!(!check.automatic_edit_license && !check.semantic_equivalence_certified);
            assert_eq!(
                check.token_alignment.len(),
                source.tokens.len() - usize::from(p.source_preposition.is_some())
            );
        }
    }
    #[test]
    fn articles_adjectives_compounds_and_multibyte_names_are_moved_as_subtrees() {
        let r = resource();
        let source = document(&[
            ("Zoë", "zoë", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("the", "the", "DET", "det", 5),
            ("red", "red", "ADJ", "amod", 5),
            ("office", "office", "NOUN", "compound", 5),
            ("book", "book", "NOUN", "dobj", 1),
            ("to", "to", "ADP", "prep", 1),
            ("María", "maría", "PROPN", "compound", 8),
            ("Jones", "jones", "PROPN", "pobj", 6),
        ]);
        let p = one(&source, &r);
        assert_eq!(p.candidate.text, "Zoë lent María Jones the red office book");
        assert_eq!(
            &source.text[p.roles.theme.span[0]..p.roles.theme.span[1]],
            "the red office book"
        );
        let candidate = document(&[
            ("Zoë", "zoë", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("María", "maría", "PROPN", "compound", 3),
            ("Jones", "jones", "PROPN", "iobj", 1),
            ("the", "the", "DET", "det", 7),
            ("red", "red", "ADJ", "amod", 7),
            ("office", "office", "NOUN", "compound", 7),
            ("book", "book", "NOUN", "dobj", 1),
        ]);
        assert_eq!(
            compare(&source, &candidate, &p, &r).unwrap().outcome,
            Outcome::ObservedPreserved
        );
    }
    #[test]
    fn repeated_names_use_occurrence_identity_and_reject_a_role_swap() {
        let r = resource();
        let source = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Kim", "kim", "PROPN", "dobj", 1),
            ("to", "to", "ADP", "prep", 1),
            ("Kim", "kim", "PROPN", "pobj", 3),
        ]);
        let mut candidate = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Kim", "kim", "PROPN", "dative", 1),
            ("Kim", "kim", "PROPN", "dobj", 1),
        ]);
        let p = one(&source, &r);
        let report = compare(&source, &candidate, &p, &r).unwrap();
        assert_eq!(report.outcome, Outcome::ObservedPreserved);
        assert!(
            report
                .token_alignment
                .iter()
                .any(|a| a.source_index == 2 && a.candidate_index == 3)
        );
        assert!(
            report
                .token_alignment
                .iter()
                .any(|a| a.source_index == 4 && a.candidate_index == 2)
        );
        candidate.tokens[2].dep = "dobj".into();
        candidate.tokens[3].dep = "dative".into();
        assert_eq!(
            compare(&source, &candidate, &p, &r).unwrap().outcome,
            Outcome::Changed
        );
    }
    #[test]
    fn pronoun_quantifier_possessive_coordination_and_clause_nps_abstain() {
        let r = resource();
        let (source, _) = pair();
        for mutation in 0..5 {
            let mut d = source.clone();
            match mutation {
                0 => d.tokens[5].pos = "PRON".into(),
                1 => d.tokens[2].lemma = "many".into(),
                2 => d.tokens[2].dep = "poss".into(),
                3 => d.tokens[2].dep = "conj".into(),
                _ => {
                    d.tokens[2].pos = "VERB".into();
                    d.tokens[2].dep = "relcl".into();
                }
            }
            let result = propose(&d, &r).unwrap();
            assert!(result.proposals.is_empty(), "mutation {mutation}");
            assert!(!result.sites.is_empty());
        }
    }
    #[test]
    fn discontinuous_subtree_and_extra_object_region_tokens_abstain() {
        let r = resource();
        let discontinuous = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("a", "a", "DET", "det", 4),
            ("quickly", "quickly", "ADV", "advmod", 1),
            ("book", "book", "NOUN", "dobj", 1),
            ("to", "to", "ADP", "dative", 1),
            ("Lee", "lee", "PROPN", "pobj", 5),
        ]);
        let intervening = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("books", "book", "NOUN", "dobj", 1),
            ("yesterday", "yesterday", "ADV", "advmod", 1),
            ("to", "to", "ADP", "dative", 1),
            ("Lee", "lee", "PROPN", "pobj", 4),
        ]);
        for d in [discontinuous, intervening] {
            assert!(propose(&d, &r).unwrap().proposals.is_empty());
        }
    }
    #[test]
    fn reparsed_morphology_surrounding_attachments_and_unknowns_are_not_preserved() {
        let r = resource();
        let (source, candidate) = pair();
        let p = one(&source, &r);
        let mut changed = candidate.clone();
        changed.tokens[1]
            .morph
            .insert("Tense".into(), vec!["Pres".into()]);
        assert_eq!(
            compare(&source, &changed, &p, &r).unwrap().outcome,
            Outcome::Changed
        );
        let mut changed = candidate.clone();
        changed.tokens[0].head = 4;
        assert_eq!(
            compare(&source, &changed, &p, &r).unwrap().outcome,
            Outcome::Changed
        );
        let mut unknown = candidate;
        unknown.tokens[4].lemma.clear();
        assert_eq!(
            compare(&source, &unknown, &p, &r).unwrap().outcome,
            Outcome::Unresolved
        );
    }
    #[test]
    fn stale_patches_parser_identity_and_registry_pin_fail_closed() {
        let r = resource();
        let (source, mut candidate) = pair();
        let mut p = one(&source, &r);
        candidate.parser_identity = "different".into();
        assert!(compare(&source, &candidate, &p, &r).is_err());
        candidate.parser_identity = source.parser_identity.clone();
        p.inverse_edits[0].replacement.push('x');
        assert!(compare(&source, &candidate, &p, &r).is_err());
        let mut wrong = r;
        wrong.classes.get_mut(CLASS).unwrap().effective_frames[0]
            .origin
            .source_sha256 = "wrong".into();
        assert!(propose(&source, &wrong).is_err());
    }
    #[test]
    fn inherited_frames_keep_the_exact_effective_membership() {
        let r = resource();
        let (mut source, mut candidate) = pair();
        source.tokens[1].lemma = "give".into();
        candidate.tokens[1].lemma = "give".into();
        let p = one(&source, &r);
        assert_eq!(p.member_class_id, "give-13.1-1");
        assert_eq!(
            compare(&source, &candidate, &p, &r).unwrap().outcome,
            Outcome::ObservedPreserved
        );
    }

    #[test]
    fn auxiliary_finiteness_and_negative_attachment_are_preserved_separately() {
        let r = resource();
        let mut source = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 3),
            ("did", "do", "AUX", "aux", 3),
            ("not", "not", "PART", "neg", 3),
            ("lend", "lend", "VERB", "ROOT", 3),
            ("a", "a", "DET", "det", 5),
            ("book", "book", "NOUN", "dobj", 3),
            ("to", "to", "ADP", "prep", 3),
            ("Lee", "lee", "PROPN", "pobj", 6),
        ]);
        let mut candidate = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 3),
            ("did", "do", "AUX", "aux", 3),
            ("not", "not", "PART", "neg", 3),
            ("lend", "lend", "VERB", "ROOT", 3),
            ("Lee", "lee", "PROPN", "iobj", 3),
            ("a", "a", "DET", "det", 6),
            ("book", "book", "NOUN", "dobj", 3),
        ]);
        for d in [&mut source, &mut candidate] {
            d.tokens[1].tag = "VBD".into();
            d.tokens[1].morph = BTreeMap::from([
                ("VerbForm".into(), vec!["Fin".into()]),
                ("Tense".into(), vec!["Past".into()]),
            ]);
            d.tokens[3].tag = "VB".into();
            d.tokens[3].morph = BTreeMap::from([("VerbForm".into(), vec!["Inf".into()])]);
        }
        let p = one(&source, &r);
        assert_eq!(
            compare(&source, &candidate, &p, &r).unwrap().outcome,
            Outcome::ObservedPreserved
        );
        candidate.tokens[2].head = 1;
        let changed = compare(&source, &candidate, &p, &r).unwrap();
        assert_eq!(changed.outcome, Outcome::Changed);
        assert!(
            changed
                .findings
                .iter()
                .any(|f| f.condition == "predicate_operator_evidence"
                    && f.outcome == Outcome::Changed)
        );
    }

    #[test]
    fn multiple_identical_occurrences_remain_distinct_proposals() {
        let r = resource();
        let (mut source, second) = pair();
        // Two copies of the prepositional sentence, with independently anchored
        // parser sentences. No global text deduplication is permitted.
        let (copy, _) = pair();
        let shift = source.tokens.len();
        let offset = source.text.len() + 1;
        source.text.push(' ');
        source.text.push_str(&copy.text);
        for mut t in copy.tokens {
            t.i += shift;
            t.head += shift;
            t.sentence += 1;
            t.start_byte += offset;
            t.end_byte += offset;
            source.tokens.push(t);
        }
        for mut s in copy.sentences {
            s.root += shift;
            s.token_start += shift;
            s.token_end += shift;
            s.start_byte += offset;
            s.end_byte += offset;
            source.sentences.push(s);
        }
        let report = propose(&source, &r).unwrap();
        assert_eq!(report.proposals.len(), 2);
        assert_ne!(report.proposals[0].id, report.proposals[1].id);
        assert_eq!(
            report.proposals[0].candidate.text,
            format!("{} {}", second.text, &source.text[offset..])
        );
        assert_eq!(
            report.proposals[1].candidate.text,
            format!("{} {}", &source.text[..offset - 1], second.text)
        );
    }

    fn role_pair(subject: &str, recipient: &str, determiner: &str) -> (Document, Document) {
        let mut a = document(&[
            (subject, subject, "PRON", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            (determiner, determiner, "PRON", "poss", 3),
            ("book", "book", "NOUN", "dobj", 1),
            ("to", "to", "ADP", "dative", 1),
            (recipient, recipient, "PRON", "pobj", 4),
        ]);
        let mut b = document(&[
            (subject, subject, "PRON", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            (recipient, recipient, "PRON", "dative", 1),
            (determiner, determiner, "PRON", "poss", 4),
            ("book", "book", "NOUN", "dobj", 1),
        ]);
        for d in [&mut a, &mut b] {
            for t in &mut d.tokens {
                if t.pos == "PRON" {
                    t.tag = if t.dep == "poss" { "PRP$" } else { "PRP" }.into();
                    if t.dep == "poss" {
                        t.morph.insert("Poss".into(), vec!["Yes".into()]);
                    }
                }
            }
        }
        (a, b)
    }
    #[test]
    fn role_aware_both_directions_preserve_pronouns_deictic_modifiers_and_policy_identity() {
        let r = resource();
        for (subject, recipient, det) in [
            ("I", "him", "my"),
            ("everyone", "us", "our"),
            ("someone", "them", "your"),
            ("whoever", "you", "my"),
        ] {
            let (a, b) = role_pair(subject, recipient, det);
            assert!(propose(&a, &r).unwrap().proposals.is_empty());
            for (source, candidate) in [(&a, &b), (&b, &a)] {
                let p = propose_with_policy(source, &r, ArgumentPolicy::RoleAwareV1)
                    .unwrap()
                    .proposals
                    .remove(0);
                assert_eq!(p.candidate.text, candidate.text);
                assert_eq!(p.argument_assessments.len(), 3);
                let compared = compare(source, candidate, &p, &r).unwrap();
                assert_eq!(compared.outcome, Outcome::ObservedPreserved);
                assert_eq!(compared.candidate_argument_assessments.len(), 3);
                assert_eq!(
                    edits::apply(&candidate.text, &p.inverse_edits).unwrap(),
                    source.text
                );
                let mut wrong = p;
                wrong.argument_policy = ArgumentPolicy::LegacyV1;
                assert!(compare(source, candidate, &wrong, &r).is_err());
            }
        }
    }
    #[test]
    fn role_aware_preserves_agent_and_reference_evidence_instead_of_bypassing_comparison() {
        let r = resource();
        let (a, mut b) = role_pair("everyone", "her", "my");
        let p = propose_with_policy(&a, &r, ArgumentPolicy::RoleAwareV1)
            .unwrap()
            .proposals
            .remove(0);
        b.tokens[0].head = 4;
        assert_eq!(compare(&a, &b, &p, &r).unwrap().outcome, Outcome::Changed);
        let (_, mut b) = role_pair("everyone", "her", "my");
        b.tokens[3].head = 2;
        let changed = compare(&a, &b, &p, &r).unwrap();
        assert_ne!(changed.outcome, Outcome::ObservedPreserved);
        assert!(!changed.automatic_edit_license);
    }
    #[test]
    fn role_aware_rejections_retain_assessments_and_legacy_omits_new_fields() {
        let r = resource();
        let (a, _) = role_pair("I", "myself", "his");
        let report = propose_with_policy(&a, &r, ArgumentPolicy::RoleAwareV1).unwrap();
        assert!(report.proposals.is_empty());
        assert!(
            report
                .sites
                .iter()
                .any(|s| !s.argument_assessments.is_empty())
        );
        let (a, b) = pair();
        let legacy = propose(&a, &r).unwrap();
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json.get("argument_policy").is_none());
        assert!(json["proposals"][0].get("argument_assessments").is_none());
        let aligned =
            serde_json::to_value(compare(&a, &b, &legacy.proposals[0], &r).unwrap()).unwrap();
        assert!(aligned.get("source_argument_assessments").is_none());
        let new = propose_with_policy(&a, &r, ArgumentPolicy::RoleAwareV1).unwrap();
        assert_ne!(legacy.proposals[0].id, new.proposals[0].id);
        assert_eq!(
            legacy.proposals[0].candidate.text,
            new.proposals[0].candidate.text
        );
    }
    #[test]
    fn role_aware_description_separates_unchanged_scope_from_moved_grammar() {
        use argument_policy::{Decision, Role, assess, describe};
        let d = document(&[
            ("Every", "every", "DET", "det", 1),
            ("editor", "editor", "NOUN", "ROOT", 1),
            ("with", "with", "ADP", "prep", 1),
            ("notes", "note", "NOUN", "pobj", 2),
        ]);
        let e = describe(&d, 1).unwrap();
        assert_eq!(
            assess(&d, Role::Agent, e.clone()).unwrap().decision,
            Decision::SupportedForStructuralTest
        );
        assert_eq!(
            assess(&d, Role::Theme, e).unwrap().decision,
            Decision::Unsupported
        );
        let (mut d, _) = role_pair("I", "her", "my");
        let e = describe(&d, 3).unwrap();
        for role in [Role::Theme, Role::Recipient] {
            assert_eq!(
                assess(&d, role, e.clone()).unwrap().decision,
                Decision::SupportedForStructuralTest
            );
        }
        d.tokens[2].morph.insert("Poss".into(), vec!["No".into()]);
        assert_eq!(
            assess(&d, Role::Theme, describe(&d, 3).unwrap())
                .unwrap()
                .decision,
            Decision::Unsupported
        );
    }
    #[test]
    fn role_aware_case_reflexive_quantifier_and_anaphoric_possessive_controls() {
        use argument_policy::{Decision, Role, assess, describe};
        for recipient in ["me", "us", "you", "him", "her", "it", "them"] {
            let (mut d, _) = role_pair("someone", recipient, "my");
            assert_eq!(
                assess(&d, Role::Recipient, describe(&d, 5).unwrap())
                    .unwrap()
                    .decision,
                Decision::SupportedForStructuralTest
            );
            assert_eq!(
                assess(&d, Role::Theme, describe(&d, 5).unwrap())
                    .unwrap()
                    .decision,
                Decision::Unsupported
            );
            d.tokens[5].morph.insert("Case".into(), vec!["Nom".into()]);
            assert_eq!(
                assess(&d, Role::Recipient, describe(&d, 5).unwrap())
                    .unwrap()
                    .decision,
                Decision::Unsupported
            );
        }
        for (recipient, det) in [
            ("I", "my"),
            ("they", "my"),
            ("myself", "my"),
            ("themselves", "my"),
            ("her", "his"),
            ("her", "their"),
            ("her", "every"),
        ] {
            let (d, _) = role_pair("everyone", recipient, det);
            assert!(
                propose_with_policy(&d, &resource(), ArgumentPolicy::RoleAwareV1)
                    .unwrap()
                    .proposals
                    .is_empty(),
                "{recipient}/{det}"
            );
        }
        let (mut d, _) = role_pair("I", "her", "my");
        d.tokens[3].lemma.clear();
        assert_eq!(
            assess(&d, Role::Theme, describe(&d, 3).unwrap())
                .unwrap()
                .decision,
            Decision::Unresolved
        );
        d.tokens[3]
            .morph
            .insert("Reflex".into(), vec!["Yes".into()]);
        assert_eq!(
            assess(&d, Role::Theme, describe(&d, 3).unwrap())
                .unwrap()
                .decision,
            Decision::Unsupported
        );
        let mut evidence = describe(&d, 3).unwrap();
        evidence.head_form = "wrong".into();
        assert!(assess(&d, Role::Theme, evidence).is_err());
    }
    #[test]
    fn role_aware_repeated_occurrences_are_not_text_deduplicated() {
        let (mut d, _) = role_pair("everyone", "her", "my");
        let copy = d.clone();
        let byte_offset = d.text.len() + 1;
        let token_offset = d.tokens.len();
        d.text.push('\n');
        d.text.push_str(&copy.text);
        for mut t in copy.tokens {
            t.i += token_offset;
            t.head += token_offset;
            t.start_byte += byte_offset;
            t.end_byte += byte_offset;
            t.sentence += 1;
            d.tokens.push(t);
        }
        for mut s in copy.sentences {
            s.root += token_offset;
            s.token_start += token_offset;
            s.token_end += token_offset;
            s.start_byte += byte_offset;
            s.end_byte += byte_offset;
            d.sentences.push(s);
        }
        let out = propose_with_policy(&d, &resource(), ArgumentPolicy::RoleAwareV1).unwrap();
        assert_eq!(out.proposals.len(), 2);
        assert_ne!(out.proposals[0].id, out.proposals[1].id);
        assert_ne!(
            out.proposals[0].candidate.text,
            out.proposals[1].candidate.text
        );
    }
}
