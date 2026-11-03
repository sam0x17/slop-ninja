//! Corpus count tiers over the unchanged pinned dative observer and proposer.
//! Counts describe parser evidence; no sense choice or edit license is inferred.
use crate::dative_alternation::{self, Proposal};
use crate::lexical_context::{lexical, normalized_lemma};
use crate::lexical_frame_observation::{self, Mapping, PredicateLedger};
use crate::predicate_operators;
use crate::verbnet_resource::{MemberRef, Resource};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits,
    syntax::{self, Document, Token},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-dative-corpus-observation-v1";
const CLASS: &str = "give-13.1";
const PREPOSITIONAL: &str = "[\"give-13.1\",0]";
const DOUBLE_OBJECT: &str = "[\"give-13.1\",1]";
const XML_SHA: &str = "bbddd2c3cbe0a8a6862eee7a140827938921df2eaf7fc2852f3f4d133800fbee";

#[derive(Debug, Serialize)]
pub struct CorpusObservation {
    pub report: Value,
    /// Exact, uncapped generator results for later candidate parsing/alignment.
    pub proposals: Vec<Proposal>,
}

struct Catalog {
    class_ids: BTreeSet<String>,
    lemmas: BTreeMap<String, Vec<MemberRef>>,
    report: Value,
}

fn catalog(resource: &Resource) -> Result<Catalog> {
    ensure!(
        resource.schema == crate::verbnet_resource::SCHEMA,
        "Unexpected VerbNet resource schema"
    );
    let registered = |id: &str| {
        resource.classes.get(id).is_some_and(|class| {
            [PREPOSITIONAL, DOUBLE_OBJECT].iter().all(|id| {
                let frames: Vec<_> = class
                    .effective_frames
                    .iter()
                    .filter(|f| f.id == *id)
                    .collect();
                frames.len() == 1
                    && frames[0].origin.declaring_class_id == CLASS
                    && frames[0].origin.source_sha256 == XML_SHA
            })
        })
    };
    ensure!(
        registered(CLASS),
        "Pinned alternation frame provenance differs"
    );
    let class_ids: BTreeSet<_> = resource
        .classes
        .keys()
        .filter(|id| registered(id))
        .cloned()
        .collect();
    let mut lemmas = BTreeMap::new();
    for (lemma, members) in &resource.lemma_index {
        let matching: Vec<_> = members
            .iter()
            .filter(|m| class_ids.contains(&m.class_id))
            .cloned()
            .collect();
        for member in &matching {
            let declaration = resource.classes[&member.class_id]
                .own_members
                .get(member.member_ordinal)
                .context("Registered member ordinal is outside its class")?;
            ensure!(
                declaration.normalized_lemma.as_ref() == Some(lemma),
                "Registered lemma index differs from member declaration"
            );
        }
        if !matching.is_empty() {
            lemmas.insert(lemma.clone(), matching);
        }
    }
    // This catalog follows the generator's exact effective-class/provenance test.
    // It does not restrict membership to the declaring root or select a sense.
    let classes: Vec<_> = class_ids.iter().map(|id| {
        let c = &resource.classes[id];
        json!({"class_id":id,"parent_id":c.parent_id,"ancestor_ids":c.ancestor_ids,
            "origin":c.origin,"own_members":c.own_members,
            "pinned_effective_frames":c.effective_frames.iter().filter(|f| [PREPOSITIONAL, DOUBLE_OBJECT].contains(&f.id.as_str())).collect::<Vec<_>>()})
    }).collect();
    let report = json!({"schema":"slopninja-dative-corpus-registry-v1",
        "resource_schema":resource.schema,"declaring_class":CLASS,
        "frame_ids":[PREPOSITIONAL,DOUBLE_OBJECT],"source_xml_sha256":XML_SHA,
        "classes":classes,"lemmas":lemmas,"class_count":class_ids.len(),"lemma_count":lemmas.len(),
        "sense_selected":false,"semantic_equivalence_certified":false,"automatic_edit_license":false});
    Ok(Catalog {
        class_ids,
        lemmas,
        report,
    })
}

pub fn registry(resource: &Resource) -> Result<Value> {
    Ok(catalog(resource)?.report)
}

fn event_key(token: &Token) -> Value {
    json!([token.i, token.start_byte, token.end_byte])
}

/// Descriptive subtree evidence only; this does not duplicate the NP eligibility gate.
fn shape(doc: &Document, children: &[Vec<usize>], head: usize) -> Value {
    let mut indices = vec![head];
    let mut at = 0;
    while at < indices.len() {
        indices.extend(&children[indices[at]]);
        at += 1;
    }
    indices.sort_unstable();
    let first = indices[0];
    let last = *indices.last().unwrap();
    json!({"head_index":head,"head_span":[doc.tokens[head].start_byte,doc.tokens[head].end_byte],
        "head_pos":doc.tokens[head].pos,"head_dependency":doc.tokens[head].dep,
        "token_indices":indices,"token_count":indices.len(),
        "lexical_token_count":indices.iter().filter(|&&i| lexical(&doc.tokens[i])).count(),
        "bounding_span":[doc.tokens[first].start_byte,doc.tokens[last].end_byte],
        "contiguous_tokens":last-first+1==indices.len(),
        "pos_sequence":indices.iter().map(|&i| &doc.tokens[i].pos).collect::<Vec<_>>(),
        "dependencies":indices.iter().map(|&i| &doc.tokens[i].dep).collect::<Vec<_>>(),
        "eligibility_claimed":false})
}

fn event_summary(
    doc: &Document,
    p: &PredicateLedger,
    classes: &BTreeSet<String>,
    children: &[Vec<usize>],
    proposal_report: &dative_alternation::ProposalReport,
) -> Value {
    let mut prepositional = false;
    let mut double_object = false;
    let mut hypotheses = Vec::new();
    for (frame_index, f) in p.frame_possibilities.iter().enumerate() {
        if !classes.contains(&f.member_class_id)
            || ![PREPOSITIONAL, DOUBLE_OBJECT].contains(&f.frame.id.as_str())
        {
            continue;
        }
        if f.syntax_binding_complete {
            prepositional |= f.frame.id == PREPOSITIONAL;
            double_object |= f.frame.id == DOUBLE_OBJECT;
        }
        let participants: Vec<_> =
            f.bindings
                .iter()
                .enumerate()
                .flat_map(|(binding_index, b)| {
                    b.alternatives.iter().enumerate().map(move |(alternative_index,a)| {
                json!({"binding_index":binding_index,"alternative_index":alternative_index,
                    "role_name":b.role_name,"unique_binding":b.unique,"kind":a.kind,
                    "shape":shape(doc,children,a.anchor.token.i)})
            })
                })
                .collect();
        hypotheses.push(json!({"frame_index":frame_index,"member_class_id":f.member_class_id,
            "member_ordinal":f.member_ordinal,"frame_id":f.frame.id,
            "syntax_binding_complete":f.syntax_binding_complete,"syntactic_binding_status":f.syntactic_binding_status,
            "full_compatibility_status":f.full_compatibility_status,"participant_shapes":participants}));
    }
    let form = match (prepositional, double_object) {
        (true, false) => "prepositional",
        (false, true) => "double_object",
        (true, true) => "ambiguous",
        (false, false) => "neither",
    };
    let token = &p.head.token;
    let proposals: Vec<_> = proposal_report
        .proposals
        .iter()
        .filter(|v| v.predicate_index == token.i)
        .collect();
    let sites: Vec<_> = proposal_report
        .sites
        .iter()
        .filter(|v| v.predicate_index == token.i)
        .collect();
    let complete_count = hypotheses
        .iter()
        .filter(|v| v["syntax_binding_complete"] == true)
        .count();
    json!({"event_key":event_key(token),"normalized_lemma":p.head.normalized_lemma,
        "predicate_index":token.i,"predicate_span":[token.start_byte,token.end_byte],
        "sentence_index":token.sentence,
        "sentence_token_count":doc.sentences[token.sentence].token_end-doc.sentences[token.sentence].token_start,
        "frame_form":form,"registered_frame_hypothesis_count":hypotheses.len(),
        "complete_frame_hypothesis_count":complete_count,"frame_hypotheses":hypotheses,
        "proposal_count":proposals.len(),"source_proposal_eligible":!proposals.is_empty(),
        "proposal_ids":proposals.iter().map(|v| &v.id).collect::<Vec<_>>(),
        "proposal_directions":proposals.iter().map(|v| v.direction).collect::<Vec<_>>(),
        "proposal_role_shapes":proposals.iter().map(|v| json!({"proposal_id":v.id,
            "agent":shape(doc,children,v.roles.agent.head_index),
            "theme":shape(doc,children,v.roles.theme.head_index),
            "recipient":shape(doc,children,v.roles.recipient.head_index)})).collect::<Vec<_>>(),
        "site_count":sites.len(),"rejection_count":sites.iter().filter(|v| v.rejection.is_some()).count(),
        "sites":sites,"opposite_frame_structurally_checked":false,
        "semantic_equivalence_certified":false,"automatic_edit_license":false})
}

pub fn observe(doc: &Document, resource: &Resource) -> Result<CorpusObservation> {
    observe_with_policy(doc, resource, dative_alternation::ArgumentPolicy::LegacyV1)
}

pub fn observe_with_policy(
    doc: &Document,
    resource: &Resource,
    policy: dative_alternation::ArgumentPolicy,
) -> Result<CorpusObservation> {
    syntax::validate(doc)?;
    let catalog = catalog(resource)?;
    let mut exposures = Vec::new();
    let mut lexical_count = 0;
    let mut missing_count = 0;
    let mut exposed_indices = BTreeSet::new();
    for token in doc.tokens.iter().filter(|t| lexical(t)) {
        lexical_count += 1;
        let Some(lemma) = normalized_lemma(token) else {
            missing_count += 1;
            continue;
        };
        if let Some(members) = catalog.lemmas.get(&lemma) {
            exposures.push(json!({"event_key":event_key(token),"token":token,
                "normalized_lemma":lemma,"registered_members":members,"all_lookup":resource.lookup(&token.lemma)}));
            exposed_indices.insert(token.i);
        }
    }
    let mut relevant = Vec::new();
    let mut events = Vec::new();
    let mut proposals = Vec::new();
    let mut proposal_json = Value::Null;
    let skipped = exposures.is_empty();
    let total_predicates;
    if skipped {
        // The frozen predicate observer requires lexical heads and exact lemma
        // lookup, so absence here proves the proposer has no registered head.
        total_predicates = predicate_operators::document_observations(doc)?.len();
    } else {
        let observed = lexical_frame_observation::observe_with_mapping(
            doc,
            resource,
            &BTreeSet::new(),
            Mapping::DativeAlternationV1,
        )?;
        total_predicates = observed.predicates.len();
        let proposal_report = dative_alternation::propose_with_policy(doc, resource, policy)?;
        ensure!(
            proposal_report
                .proposals
                .iter()
                .all(|p| exposed_indices.contains(&p.predicate_index)),
            "Proposal head missing from exact lemma exposures"
        );
        let mut children = vec![Vec::new(); doc.tokens.len()];
        for t in &doc.tokens {
            if t.head != t.i {
                children[t.head].push(t.i);
            }
        }
        for p in observed
            .predicates
            .into_iter()
            .filter(|p| exposed_indices.contains(&p.head.token.i))
        {
            events.push(event_summary(
                doc,
                &p,
                &catalog.class_ids,
                &children,
                &proposal_report,
            ));
            relevant.push(p);
        }
        proposal_json = serde_json::to_value(&proposal_report)?;
        proposals = proposal_report.proposals;
    }
    let relevant_indices: BTreeSet<_> = relevant.iter().map(|p| p.head.token.i).collect();
    let unobserved: Vec<_> = exposures
        .iter()
        .filter(|v| !relevant_indices.contains(&(v["token"]["i"].as_u64().unwrap() as usize)))
        .collect();
    let forms: BTreeMap<_, _> = ["prepositional", "double_object", "ambiguous", "neither"]
        .into_iter()
        .map(|form| {
            (
                form,
                events.iter().filter(|e| e["frame_form"] == form).count(),
            )
        })
        .collect();
    let registered_hypotheses: usize = events
        .iter()
        .map(|v| v["registered_frame_hypothesis_count"].as_u64().unwrap() as usize)
        .sum();
    let complete_hypotheses: usize = events
        .iter()
        .map(|v| v["complete_frame_hypothesis_count"].as_u64().unwrap() as usize)
        .sum();
    let mut report = json!({"schema":SCHEMA,"mapping_identity":Mapping::DativeAlternationV1.identity(),
        "source_sha256":edits::digest(&doc.text),"parser_identity":doc.parser_identity,
        "document_value_sha256":edits::digest(&serde_json::to_string(doc)?),
        "registry_value_sha256":edits::digest(&serde_json::to_string(&catalog.report)?),
        "source_byte_count":doc.text.len(),"token_count":doc.tokens.len(),"sentence_count":doc.sentences.len(),
        "lexical_token_count":lexical_count,"missing_lexical_lemma_count":missing_count,
        "registered_lemma_token_count":exposures.len(),"lemma_exposures":exposures,
        "unobserved_exposure_events":unobserved,"total_predicate_count":total_predicates,
        "unrelated_predicate_count":total_predicates-relevant.len(),
        "relevant_predicate_count":relevant.len(),"relevant_predicates":relevant,
        "event_summaries":events,"frame_form_counts":forms,
        "registered_frame_hypothesis_count":registered_hypotheses,
        "complete_frame_hypothesis_count":complete_hypotheses,
        "complete_frame_event_count":events.iter().filter(|v| v["frame_form"]!="neither").count(),
        "source_proposal_event_count":events.iter().filter(|v| v["source_proposal_eligible"]==true).count(),
        "proposal_count":proposals.len(),"proposal_report":proposal_json,
        "full_observer_and_proposer_skipped":skipped,
        "skip_reason":if skipped {Some("No lexical token has an exact registered supplied lemma")} else {None},
        "opposite_frame_structurally_checked_count":0,
        "coverage_limits":["Exposure scans every lexical token, without POS or finite-head filtering; missing/wrong supplied lemmas remain a limitation",
            "Every frame alternative of a relevant head is retained; no mask, sense choice or candidate cap",
            "Event counts use predicate occurrence, not frame-membership multiplicity",
            "Frame form requires complete registered pinned syntax, not a surface preposition",
            "Participant subtree shapes are descriptive, not an additional eligibility rule",
            "No candidate parsing or opposite-frame comparison occurs in this module"],
        "semantic_equivalence_certified":false,"automatic_edit_license":false});
    if !policy.is_legacy() {
        report["schema"] = json!("slopninja-dative-corpus-observation-role-aware-v1");
        report["argument_policy"] = serde_json::to_value(policy)?;
    }
    Ok(CorpusObservation { report, proposals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbnet_resource::XmlSource;
    use grammar_core::syntax::Sentence;

    fn resource() -> Resource {
        let frame = |syntax: &str| {
            format!(
                "<FRAME><DESCRIPTION primary='synthetic'/><EXAMPLES/><SYNTAX>{syntax}</SYNTAX><SEMANTICS/></FRAME>"
            )
        };
        let give = format!(
            "<VNCLASS ID='give-13.1'><MEMBERS><MEMBER name='lend'/><MEMBER name='lend'/><MEMBER name='loan'/></MEMBERS><THEMROLES><THEMROLE type='Agent'/><THEMROLE type='Theme'/><THEMROLE type='Recipient'/></THEMROLES><FRAMES>{}{}</FRAMES><SUBCLASSES><VNSUBCLASS ID='give-13.1-1'><MEMBERS><MEMBER name='give'/></MEMBERS></VNSUBCLASS></SUBCLASSES></VNCLASS>",
            frame(
                "<NP value='Agent'/><VERB/><NP value='Theme'/><PREP value='to'/><NP value='Recipient'/>"
            ),
            frame("<NP value='Agent'/><VERB/><NP value='Recipient'/><NP value='Theme'/>")
        );
        let other = format!(
            "<VNCLASS ID='other-1'><MEMBERS><MEMBER name='lend'/></MEMBERS><THEMROLES><THEMROLE type='Agent'/></THEMROLES><FRAMES>{}</FRAMES></VNCLASS>",
            frame("<NP value='Agent'/><VERB/>")
        );
        let mut r = Resource::from_sources(
            vec![give, other]
                .into_iter()
                .enumerate()
                .map(|(i, xml)| XmlSource {
                    path: format!("synthetic-{i}.xml"),
                    sha256: edits::digest(&xml),
                    xml,
                })
                .collect(),
        )
        .unwrap();
        // In-memory matcher provenance stub only, not a hash-valid resource
        // export. Production resources remain bound by the caller's manifest.
        for class in r.classes.values_mut() {
            for f in &mut class.effective_frames {
                if f.origin.declaring_class_id == CLASS {
                    f.origin.source_sha256 = XML_SHA.into();
                }
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
                token_start: 0,
                token_end: tokens.len(),
                root,
            }],
            text,
            tokens,
            parser_identity: "synthetic-v1".into(),
        };
        syntax::validate(&d).unwrap();
        d
    }
    fn pair() -> [Document; 2] {
        [
            document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                ("a", "a", "DET", "det", 3),
                ("book", "book", "NOUN", "dobj", 1),
                ("to", "to", "ADP", "dative", 1),
                ("María", "maría", "PROPN", "pobj", 4),
                (".", ".", "PUNCT", "punct", 1),
            ]),
            document(&[
                ("Kim", "kim", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                ("María", "maría", "PROPN", "dative", 1),
                ("a", "a", "DET", "det", 4),
                ("book", "book", "NOUN", "dobj", 1),
                (".", ".", "PUNCT", "punct", 1),
            ]),
        ]
    }

    #[test]
    fn registry_preserves_inheritance_and_every_member_without_selecting_a_sense() {
        let r = resource();
        let catalog = registry(&r).unwrap();
        assert_eq!(catalog["class_count"], 2);
        assert_eq!(catalog["lemma_count"], 3);
        assert_eq!(catalog["lemmas"]["lend"].as_array().unwrap().len(), 2);
        assert_eq!(catalog["lemmas"]["give"][0]["class_id"], "give-13.1-1");
        assert!(
            catalog["classes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["class_id"] != "other-1")
        );
        let mut changed = r.clone();
        changed.classes.get_mut(CLASS).unwrap().effective_frames[0]
            .origin
            .source_sha256 = "changed".into();
        assert!(registry(&changed).is_err());
        let mut broken = r;
        broken.lemma_index.get_mut("lend").unwrap()[0].member_ordinal = 999;
        assert!(registry(&broken).is_err());
    }

    #[test]
    fn noun_and_nonhead_lemma_exposures_remain_visible_without_surface_fallback() {
        let r = resource();
        let d = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("reads", "read", "VERB", "ROOT", 1),
            ("loan", "loan", "NOUN", "dobj", 1),
        ]);
        let found = observe(&d, &r).unwrap().report;
        assert_eq!(found["registered_lemma_token_count"], 1);
        assert_eq!(found["relevant_predicate_count"], 0);
        assert_eq!(found["unrelated_predicate_count"], 1);
        assert_eq!(
            found["unobserved_exposure_events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(found["full_observer_and_proposer_skipped"], false);
        let noun = document(&[("LOAN", "LOAN", "NOUN", "ROOT", 0)]);
        let noun_report = observe(&noun, &r).unwrap().report;
        assert_eq!(noun_report["relevant_predicate_count"], 1);
        assert_eq!(noun_report["event_summaries"][0]["frame_form"], "neither");
        let missing = document(&[("lend", "", "VERB", "ROOT", 0)]);
        let absent = observe(&missing, &r).unwrap().report;
        assert_eq!(absent["missing_lexical_lemma_count"], 1);
        assert_eq!(absent["registered_lemma_token_count"], 0);
        assert_eq!(absent["full_observer_and_proposer_skipped"], true);
    }

    #[test]
    fn zero_exposure_shortcut_retains_denominators_and_validates_inputs_first() {
        let d = document(&[
            ("Kim", "kim", "PROPN", "nsubj", 1),
            ("walked", "walk", "VERB", "ROOT", 1),
        ]);
        let r = resource();
        let out = observe(&d, &r).unwrap();
        assert!(out.proposals.is_empty());
        assert_eq!(out.report["lexical_token_count"], 2);
        assert_eq!(out.report["total_predicate_count"], 1);
        assert!(out.report["proposal_report"].is_null());
        assert_eq!(out.report["frame_form_counts"]["neither"], 0);
        let mut invalid = d.clone();
        invalid.tokens[0].text = "wrong".into();
        assert!(observe(&invalid, &r).is_err());
        let mut invalid_resource = r;
        invalid_resource.classes.remove(CLASS);
        assert!(observe(&d, &invalid_resource).is_err());
    }

    #[test]
    fn both_forms_retain_exact_generator_and_all_unregistered_frame_alternatives() {
        let r = resource();
        for (doc, form) in pair().into_iter().zip(["prepositional", "double_object"]) {
            let out = observe(&doc, &r).unwrap();
            let generated = dative_alternation::propose(&doc, &r).unwrap();
            assert_eq!(
                out.report["proposal_report"],
                serde_json::to_value(&generated).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&out.proposals).unwrap(),
                serde_json::to_value(&generated.proposals).unwrap()
            );
            assert_eq!(out.proposals.len(), 2); // Duplicate membership; one event.
            assert_eq!(out.report["source_proposal_event_count"], 1);
            assert_eq!(out.report["complete_frame_event_count"], 1);
            assert_eq!(out.report["complete_frame_hypothesis_count"], 2);
            assert_eq!(out.report["frame_form_counts"][form], 1);
            let p = &out.report["relevant_predicates"][0];
            assert_eq!(p["lookup"]["members"].as_array().unwrap().len(), 3);
            assert!(
                p["frame_possibilities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v["member_class_id"] == "other-1")
            );
            assert_eq!(
                out.report["event_summaries"][0]["predicate_span"],
                json!([doc.tokens[1].start_byte, doc.tokens[1].end_byte])
            );
            assert_eq!(out.report["semantic_equivalence_certified"], false);
        }
    }

    #[test]
    fn complete_frame_is_not_equivalent_to_simple_np_proposal_eligibility() {
        let r = resource();
        let mut d = pair()[0].clone();
        d.tokens[0].pos = "PRON".into();
        d.tokens[0].tag = "PRP".into();
        let out = observe(&d, &r).unwrap();
        assert_eq!(out.report["frame_form_counts"]["prepositional"], 1);
        assert_eq!(out.report["source_proposal_event_count"], 0);
        assert!(out.proposals.is_empty());
        assert!(
            out.report["event_summaries"][0]["rejection_count"]
                .as_u64()
                .unwrap()
                > 0
        );
        let shapes = out.report["event_summaries"][0]["frame_hypotheses"]
            .as_array()
            .unwrap();
        assert!(
            shapes
                .iter()
                .flat_map(|v| v["participant_shapes"].as_array().unwrap())
                .any(|v| v["role_name"] == "Agent" && v["shape"]["head_pos"] == "PRON")
        );
    }

    #[test]
    fn repeated_occurrences_have_distinct_byte_keys_not_membership_counts() {
        let r = resource();
        let mut d = pair()[0].clone();
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
        let out = observe(&d, &r).unwrap();
        assert_eq!(out.report["registered_lemma_token_count"], 2);
        assert_eq!(out.report["source_proposal_event_count"], 2);
        assert_eq!(out.report["proposal_count"], 4);
        assert_ne!(
            out.report["event_summaries"][0]["event_key"],
            out.report["event_summaries"][1]["event_key"]
        );
        assert_eq!(out.report["source_sha256"], edits::digest(&d.text));
    }

    #[test]
    fn form_aggregation_retains_ambiguous_hypotheses_without_membership_voting() {
        let r = resource();
        let d = pair()[0].clone();
        let mut observed = lexical_frame_observation::observe_with_mapping(
            &d,
            &r,
            &BTreeSet::new(),
            Mapping::DativeAlternationV1,
        )
        .unwrap();
        // A bookkeeping fixture, not a claim these two forms can both bind this
        // sentence: conflicting complete hypotheses must remain ambiguous.
        let p = &mut observed.predicates[0];
        p.frame_possibilities
            .iter_mut()
            .find(|f| f.frame.id == DOUBLE_OBJECT)
            .unwrap()
            .syntax_binding_complete = true;
        let mut children = vec![Vec::new(); d.tokens.len()];
        for t in &d.tokens {
            if t.i != t.head {
                children[t.head].push(t.i);
            }
        }
        let summary = event_summary(
            &d,
            p,
            &catalog(&r).unwrap().class_ids,
            &children,
            &dative_alternation::propose(&d, &r).unwrap(),
        );
        assert_eq!(summary["frame_form"], "ambiguous");
        assert_eq!(summary["complete_frame_hypothesis_count"], 3);
        assert_eq!(summary["proposal_count"], 2);
    }
    #[test]
    fn opt_in_argument_policy_keeps_same_frame_events_and_separate_proposal_tier() {
        let r = resource();
        let mut d = pair()[0].clone();
        d.tokens[0].pos = "PRON".into();
        d.tokens[0].tag = "PRP".into();
        let legacy = observe(&d, &r).unwrap();
        let new =
            observe_with_policy(&d, &r, dative_alternation::ArgumentPolicy::RoleAwareV1).unwrap();
        assert_eq!(
            legacy.report["complete_frame_event_count"],
            new.report["complete_frame_event_count"]
        );
        assert_eq!(legacy.proposals.len(), 0);
        assert_eq!(new.proposals.len(), 2);
        assert_eq!(new.report["source_proposal_event_count"], 1);
        assert!(legacy.report.get("argument_policy").is_none());
        assert_eq!(new.report["argument_policy"], "role_aware_v1");
    }
}
