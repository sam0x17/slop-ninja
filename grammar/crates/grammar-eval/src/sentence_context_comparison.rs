//! Regenerated dative checks within exact, prepared sentence slices.
//! These diagnostics cannot replace the original whole-post preservation gate.
use crate::dative_alternation::{self as alternation, ArgumentPolicy, Direction, Proposal};
use crate::verbnet_resource::Resource;
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Edit},
    syntax::{self, Document},
};
use serde_json::{Value, json};

const SCHEMA: &str = "slopninja-sentence-context-comparison-v1";

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}

fn span(value: &Value, key: &str, source: &str) -> Result<[usize; 2]> {
    let result: [usize; 2] =
        serde_json::from_value(value[key].clone()).with_context(|| format!("Invalid {key}"))?;
    ensure!(
        result[0] < result[1] && source.get(result[0]..result[1]).is_some(),
        "Invalid UTF-8 predicate span {key}"
    );
    Ok(result)
}

fn opposite(direction: Direction) -> Direction {
    match direction {
        Direction::ToDoubleObject => Direction::ToPrepositional,
        Direction::ToPrepositional => Direction::ToDoubleObject,
    }
}

struct Expected<'a> {
    predicate_span: [usize; 2],
    predicate_text: &'a str,
    member_class: &'a str,
    member_ordinal: usize,
    direction: Direction,
    source_frame: &'a str,
    target_frame: &'a str,
    candidate_text: &'a str,
    forward_edits: &'a [Edit],
    inverse_edits: &'a [Edit],
}

fn matches(doc: &Document, proposal: &Proposal, expected: &Expected<'_>) -> bool {
    let Some(head) = doc.tokens.get(proposal.predicate_index) else {
        return false;
    };
    proposal.argument_policy == ArgumentPolicy::RoleAwareV1
        && [head.start_byte, head.end_byte] == expected.predicate_span
        && head.text == expected.predicate_text
        && proposal.member_class_id == expected.member_class
        && proposal.member_ordinal == expected.member_ordinal
        && proposal.direction == expected.direction
        && proposal.source_frame_id == expected.source_frame
        && proposal.target_frame_id == expected.target_frame
        && proposal.candidate.text == expected.candidate_text
        && proposal.candidate.edits == expected.forward_edits
        && proposal.inverse_edits == expected.inverse_edits
}

fn complete(report: &alternation::AlignmentReport) -> bool {
    report.outcome != alternation::Outcome::Unresolved
        && report
            .findings
            .iter()
            .all(|finding| finding.outcome != alternation::Outcome::Unresolved)
}

fn execution_error(result: &mut Value, stage: &str, error: &anyhow::Error) {
    result["status"] = json!("unavailable");
    result["reason"] = json!("execution_error");
    result["assessment_complete"] = json!(false);
    result["execution_errors"]
        .as_array_mut()
        .expect("Initialized execution error array")
        .push(json!({"stage":stage,"error":format!("{error:#}")}));
}

/// Validate the supplied slice contract, then run unchanged proposal/comparison
/// code. A missing rule match is a completed observation; execution errors and
/// unresolved comparison findings remain explicitly incomplete. No parser runs.
pub fn evaluate(
    source: &Document,
    candidate: &Document,
    prepared: &Value,
    original_proposal: &Value,
    resource: &Resource,
) -> Result<Value> {
    syntax::validate(source).context("Invalid source slice annotation")?;
    syntax::validate(candidate).context("Invalid candidate slice annotation")?;
    ensure!(prepared["status"] == "prepared", "Slice was not prepared");
    ensure!(
        source.text == text(prepared, "source_text")?
            && candidate.text == text(prepared, "candidate_text")?
            && source.parser_identity == candidate.parser_identity,
        "Slice text or parser identity differs"
    );
    ensure!(
        original_proposal["argument_policy"] == "role_aware_v1",
        "Expected original RoleAwareV1 proposal"
    );
    let forward_edits: Vec<Edit> = serde_json::from_value(prepared["forward_edits"].clone())?;
    let inverse_edits: Vec<Edit> = serde_json::from_value(prepared["inverse_edits"].clone())?;
    ensure!(
        !forward_edits.is_empty()
            && !inverse_edits.is_empty()
            && source.text != candidate.text
            && edits::apply(&source.text, &forward_edits)? == candidate.text
            && edits::apply(&candidate.text, &inverse_edits)? == source.text,
        "Prepared forward/inverse patch bytes differ"
    );
    let source_span = span(prepared, "source_predicate_span", &source.text)?;
    let candidate_span = span(prepared, "candidate_predicate_span", &candidate.text)?;
    let source_head = &source.text[source_span[0]..source_span[1]];
    let candidate_head = &candidate.text[candidate_span[0]..candidate_span[1]];
    ensure!(
        source_head == candidate_head,
        "Prepared predicate bytes differ"
    );
    let direction = match text(original_proposal, "direction")? {
        "to_double_object" => Direction::ToDoubleObject,
        "to_prepositional" => Direction::ToPrepositional,
        _ => anyhow::bail!("Unknown original proposal direction"),
    };
    let member_ordinal = original_proposal["member_ordinal"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .context("Invalid original member ordinal")?;
    let expected = Expected {
        predicate_span: source_span,
        predicate_text: source_head,
        member_class: text(original_proposal, "member_class_id")?,
        member_ordinal,
        direction,
        source_frame: text(original_proposal, "source_frame_id")?,
        target_frame: text(original_proposal, "target_frame_id")?,
        candidate_text: &candidate.text,
        forward_edits: &forward_edits,
        inverse_edits: &inverse_edits,
    };
    let mut result = json!({
        "schema":SCHEMA,"status":"unavailable","reason":null,
        "source_sha256":edits::digest(&source.text),"candidate_sha256":edits::digest(&candidate.text),
        "parser_identity":source.parser_identity,"original_proposal_id":original_proposal["id"],
        "expected":{"source_predicate_span":source_span,"candidate_predicate_span":candidate_span,
            "predicate_text":source_head,"member_class_id":expected.member_class,"member_ordinal":member_ordinal,
            "direction":direction,"source_frame_id":expected.source_frame,"target_frame_id":expected.target_frame,
            "forward_edits":forward_edits,"inverse_edits":inverse_edits},
        "source_match_count":null,"reverse_match_count":null,
        "source_matching_proposal_ids":[],"reverse_matching_proposal_ids":[],
        "source_proposal_report":null,"reverse_proposal_report":null,
        "forward":null,"reverse":[],"candidate_predicate_mapping_established":null,
        "execution_errors":[],"reciprocally_observed_preserved":false,"assessment_complete":true,
        "automatic_edit_license":false,"semantic_equivalence_certified":false,
        "whole_post_gate_overridden":false
    });
    let source_report =
        match alternation::propose_with_policy(source, resource, ArgumentPolicy::RoleAwareV1) {
            Ok(report) => report,
            Err(error) => {
                execution_error(&mut result, "source_proposal_generation", &error);
                return Ok(result);
            }
        };
    let source_matches: Vec<_> = source_report
        .proposals
        .iter()
        .filter(|proposal| matches(source, proposal, &expected))
        .collect();
    result["source_proposal_report"] = serde_json::to_value(&source_report)?;
    result["source_match_count"] = json!(source_matches.len());
    result["source_matching_proposal_ids"] =
        json!(source_matches.iter().map(|p| &p.id).collect::<Vec<_>>());
    if source_matches.len() != 1 {
        result["reason"] = json!(if source_matches.is_empty() {
            "source_match_missing"
        } else {
            "source_match_ambiguous"
        });
        return Ok(result);
    }
    let proposal = source_matches[0];
    let forward = match alternation::compare(source, candidate, proposal, resource) {
        Ok(report) => report,
        Err(error) => {
            execution_error(&mut result, "forward_comparison", &error);
            return Ok(result);
        }
    };
    result["assessment_complete"] = json!(complete(&forward));
    result["forward"] = serde_json::to_value(&forward)?;
    let mapped = forward.token_alignment.iter().find(|a| {
        a.source_index == proposal.predicate_index
            && a.source_span == source_span
            && a.candidate_span == candidate_span
    });
    let mapped_index = mapped.map(|a| a.candidate_index);
    result["candidate_predicate_mapping_established"] = json!(mapped.is_some());
    let reverse_report =
        match alternation::propose_with_policy(candidate, resource, ArgumentPolicy::RoleAwareV1) {
            Ok(report) => report,
            Err(error) => {
                execution_error(&mut result, "reverse_proposal_generation", &error);
                return Ok(result);
            }
        };
    let reverse_expected = Expected {
        predicate_span: candidate_span,
        predicate_text: candidate_head,
        direction: opposite(direction),
        source_frame: expected.target_frame,
        target_frame: expected.source_frame,
        candidate_text: &source.text,
        forward_edits: &inverse_edits,
        inverse_edits: &forward_edits,
        ..expected
    };
    let reverse_matches: Vec<_> = reverse_report
        .proposals
        .iter()
        .filter(|p| {
            mapped_index == Some(p.predicate_index) && matches(candidate, p, &reverse_expected)
        })
        .collect();
    result["reverse_proposal_report"] = serde_json::to_value(&reverse_report)?;
    result["reverse_match_count"] = json!(reverse_matches.len());
    result["reverse_matching_proposal_ids"] =
        json!(reverse_matches.iter().map(|p| &p.id).collect::<Vec<_>>());
    let mut reverse_preserved = false;
    for reverse in &reverse_matches {
        let row = match alternation::compare(candidate, source, reverse, resource) {
            Ok(comparison) => {
                result["assessment_complete"] =
                    json!(result["assessment_complete"] == true && complete(&comparison));
                reverse_preserved |= comparison.outcome == alternation::Outcome::ObservedPreserved;
                json!({"proposal_id":reverse.id,"status":"compared","alignment":comparison})
            }
            Err(error) => {
                execution_error(&mut result, "reverse_comparison", &error);
                json!({"proposal_id":reverse.id,"status":"execution_error","error":format!("{error:#}")})
            }
        };
        result["reverse"].as_array_mut().unwrap().push(row);
    }
    if !result["execution_errors"].as_array().unwrap().is_empty() {
        return Ok(result);
    }
    if reverse_matches.len() != 1 {
        result["reason"] = json!(if mapped_index.is_none() {
            "candidate_predicate_mapping_unavailable"
        } else if reverse_matches.is_empty() {
            "reverse_match_missing"
        } else {
            "reverse_match_ambiguous"
        });
        return Ok(result);
    }
    result["status"] = json!("compared");
    result["reciprocally_observed_preserved"] =
        json!(forward.outcome == alternation::Outcome::ObservedPreserved && reverse_preserved);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbnet_resource::XmlSource;
    use grammar_core::syntax::{Sentence, Token};
    use std::collections::BTreeMap;

    fn resource() -> Resource {
        let frame = |syntax| {
            format!(
                "<FRAME><DESCRIPTION primary='synthetic'/><EXAMPLES/><SYNTAX>{syntax}</SYNTAX><SEMANTICS/></FRAME>"
            )
        };
        let xml = format!(
            "<VNCLASS ID='give-13.1'><MEMBERS><MEMBER name='lend'/></MEMBERS><THEMROLES><THEMROLE type='Agent'/><THEMROLE type='Theme'/><THEMROLE type='Recipient'/></THEMROLES><FRAMES>{}{}</FRAMES></VNCLASS>",
            frame(
                "<NP value='Agent'/><VERB/><NP value='Theme'/><PREP value='to'/><NP value='Recipient'/>"
            ),
            frame("<NP value='Agent'/><VERB/><NP value='Recipient'/><NP value='Theme'/>")
        );
        let mut resource = Resource::from_sources(vec![XmlSource {
            path: "synthetic.xml".into(),
            sha256: edits::digest(&xml),
            xml,
        }])
        .unwrap();
        // Test-only in-memory provenance stub, following existing dative tests.
        // Production Resource provenance remains checked by the unchanged API.
        for class in resource.classes.values_mut() {
            for frame in &mut class.effective_frames {
                frame.origin.source_sha256 =
                    "bbddd2c3cbe0a8a6862eee7a140827938921df2eaf7fc2852f3f4d133800fbee".into();
            }
        }
        resource
    }

    fn document(items: &[(&str, &str, &str, &str, usize)]) -> Document {
        let mut source = String::new();
        let mut tokens = Vec::new();
        for (i, &(word, lemma, pos, dep, head)) in items.iter().enumerate() {
            if i > 0 {
                source.push(' ');
            }
            let start_byte = source.len();
            source.push_str(word);
            tokens.push(Token {
                i,
                start_byte,
                end_byte: source.len(),
                text: word.into(),
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
        let result = Document {
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: source.len(),
                token_start: 0,
                token_end: tokens.len(),
                root: tokens.iter().find(|t| t.head == t.i).unwrap().i,
            }],
            text: source,
            parser_identity: "synthetic-sentence-context-v1".into(),
            tokens,
        };
        syntax::validate(&result).unwrap();
        result
    }

    fn pair() -> (Document, Document) {
        (
            document(&[
                ("Zoë", "zoë", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                ("a", "a", "DET", "det", 3),
                ("book", "book", "NOUN", "dobj", 1),
                ("to", "to", "ADP", "dative", 1),
                ("Lee", "lee", "PROPN", "pobj", 4),
                (".", ".", "PUNCT", "punct", 1),
            ]),
            document(&[
                ("Zoë", "zoë", "PROPN", "nsubj", 1),
                ("lent", "lend", "VERB", "ROOT", 1),
                ("Lee", "lee", "PROPN", "dative", 1),
                ("a", "a", "DET", "det", 4),
                ("book", "book", "NOUN", "dobj", 1),
                (".", ".", "PUNCT", "punct", 1),
            ]),
        )
    }

    fn prepared(source: &Document, candidate: &Document, p: &Proposal) -> Value {
        let s = &source.tokens[p.predicate_index];
        let c = candidate.tokens.iter().find(|t| t.text == s.text).unwrap();
        json!({"status":"prepared","source_text":source.text,"candidate_text":candidate.text,
            "source_predicate_span":[s.start_byte,s.end_byte],"candidate_predicate_span":[c.start_byte,c.end_byte],
            "forward_edits":p.candidate.edits,"inverse_edits":p.inverse_edits})
    }

    fn one(source: &Document, r: &Resource) -> Proposal {
        let report =
            alternation::propose_with_policy(source, r, ArgumentPolicy::RoleAwareV1).unwrap();
        assert_eq!(report.proposals.len(), 1, "{:?}", report.sites);
        report.proposals[0].clone()
    }

    #[test]
    fn both_directions_require_exact_regenerated_reciprocal_proposals() {
        let r = resource();
        let (a, b) = pair();
        for (source, candidate) in [(&a, &b), (&b, &a)] {
            let p = one(source, &r);
            let output = evaluate(
                source,
                candidate,
                &prepared(source, candidate, &p),
                &json!(p),
                &r,
            )
            .unwrap();
            assert_eq!(output["status"], "compared", "{output}");
            assert_eq!(output["source_match_count"], 1);
            assert_eq!(output["reverse_match_count"], 1);
            assert_eq!(output["reciprocally_observed_preserved"], true);
            assert_eq!(output["assessment_complete"], true);
            assert_eq!(output["automatic_edit_license"], false);
        }
    }

    #[test]
    fn wrong_membership_is_missing_not_an_execution_failure() {
        let r = resource();
        let (source, candidate) = pair();
        let p = one(&source, &r);
        let mut original = json!(p);
        original["member_ordinal"] = json!(1);
        let output = evaluate(
            &source,
            &candidate,
            &prepared(&source, &candidate, &p),
            &original,
            &r,
        )
        .unwrap();
        assert_eq!(output["reason"], "source_match_missing");
        assert_eq!(output["source_match_count"], 0);
        assert_eq!(output["reverse_match_count"], Value::Null);
        assert_eq!(output["assessment_complete"], true);
        assert_eq!(output["execution_errors"], json!([]));
        assert_eq!(
            output["source_proposal_report"]["proposals"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn corrupt_prepared_patches_or_identity_fail_contract_validation() {
        let r = resource();
        let (source, candidate) = pair();
        let p = one(&source, &r);
        let mut prep = prepared(&source, &candidate, &p);
        prep["forward_edits"][0]["expected"] = json!("wrong bytes");
        assert!(evaluate(&source, &candidate, &prep, &json!(p), &r).is_err());
        let prep = prepared(&source, &candidate, &p);
        let mut changed = candidate.clone();
        changed.parser_identity = "different-parser".into();
        assert!(evaluate(&source, &changed, &prep, &json!(p), &r).is_err());
    }

    #[test]
    fn same_result_from_a_whole_span_patch_does_not_match_the_generated_edit() {
        let r = resource();
        let (source, candidate) = pair();
        let p = one(&source, &r);
        let mut prep = prepared(&source, &candidate, &p);
        prep["forward_edits"] = json!([Edit {
            start_byte: 0,
            end_byte: source.text.len(),
            expected: source.text.clone(),
            replacement: candidate.text.clone()
        }]);
        prep["inverse_edits"] = json!([Edit {
            start_byte: 0,
            end_byte: candidate.text.len(),
            expected: candidate.text.clone(),
            replacement: source.text.clone()
        }]);
        let output = evaluate(&source, &candidate, &prep, &json!(p), &r).unwrap();
        assert_eq!(output["reason"], "source_match_missing");
    }

    #[test]
    fn missing_reverse_proposal_is_distinct_from_a_generation_error() {
        let r = resource();
        let (source, mut candidate) = pair();
        let p = one(&source, &r);
        // Valid syntax with a changed Recipient label prevents the reverse frame.
        candidate.tokens[2].dep = "advmod".into();
        let prep = prepared(&source, &candidate, &p);
        let output = evaluate(&source, &candidate, &prep, &json!(p), &r).unwrap();
        assert_eq!(output["reason"], "reverse_match_missing");
        assert_eq!(output["source_match_count"], 1);
        assert_eq!(output["reverse_match_count"], 0);
        assert_eq!(output["assessment_complete"], false);
        assert_eq!(output["execution_errors"], json!([]));
        let mut empty = resource();
        empty.classes.clear();
        let output = evaluate(&source, &candidate, &prep, &json!(p), &empty).unwrap();
        assert_eq!(output["reason"], "execution_error");
        assert_eq!(output["source_match_count"], Value::Null);
        assert_eq!(output["assessment_complete"], false);
    }

    #[test]
    fn primary_parser_metadata_does_not_replace_local_occurrence_indices() {
        let r = resource();
        let (source, candidate) = pair();
        let p = one(&source, &r);
        let mut original = json!(p);
        original["predicate_index"] = json!(500);
        original["candidate"]["parser_identity"] = json!("primary-full-post-parser");
        let output = evaluate(
            &source,
            &candidate,
            &prepared(&source, &candidate, &p),
            &original,
            &r,
        )
        .unwrap();
        assert_eq!(output["reciprocally_observed_preserved"], true, "{output}");
    }

    #[test]
    fn repeated_predicate_surface_does_not_substitute_for_the_anchored_occurrence() {
        let r = resource();
        let source = document(&[
            ("Mira", "mira", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("a", "a", "DET", "det", 3),
            ("book", "book", "NOUN", "dobj", 1),
            ("to", "to", "ADP", "dative", 1),
            ("Lee", "lee", "PROPN", "pobj", 4),
            (";", ";", "PUNCT", "punct", 1),
            ("Mira", "mira", "PROPN", "nsubj", 8),
            ("lent", "lend", "VERB", "conj", 1),
            ("a", "a", "DET", "det", 10),
            ("pen", "pen", "NOUN", "dobj", 8),
            ("to", "to", "ADP", "dative", 8),
            ("Lee", "lee", "PROPN", "pobj", 11),
            (".", ".", "PUNCT", "punct", 8),
        ]);
        let candidate = document(&[
            ("Mira", "mira", "PROPN", "nsubj", 1),
            ("lent", "lend", "VERB", "ROOT", 1),
            ("Lee", "lee", "PROPN", "dative", 1),
            ("a", "a", "DET", "det", 4),
            ("book", "book", "NOUN", "dobj", 1),
            (";", ";", "PUNCT", "punct", 1),
            ("Mira", "mira", "PROPN", "nsubj", 7),
            ("lent", "lend", "VERB", "conj", 1),
            ("a", "a", "DET", "det", 9),
            ("pen", "pen", "NOUN", "dobj", 7),
            ("to", "to", "ADP", "dative", 7),
            ("Lee", "lee", "PROPN", "pobj", 10),
            (".", ".", "PUNCT", "punct", 7),
        ]);
        let generated =
            alternation::propose_with_policy(&source, &r, ArgumentPolicy::RoleAwareV1).unwrap();
        assert_eq!(generated.proposals.len(), 2);
        let p = generated
            .proposals
            .iter()
            .find(|p| p.predicate_index == 1)
            .unwrap();
        let mut prep = prepared(&source, &candidate, p);
        let output = evaluate(&source, &candidate, &prep, &json!(p), &r).unwrap();
        assert_eq!(output["source_match_count"], 1);
        assert_eq!(output["source_matching_proposal_ids"], json!([p.id]));
        // The unchanged atomic double-object frame excludes this extra
        // coordinated predicate. Preserve that unresolved counterpart finding;
        // the test's positive assertion concerns source occurrence matching.
        assert_eq!(output["forward"]["outcome"], "unresolved");
        assert_eq!(output["reason"], "reverse_match_missing");
        assert_eq!(output["assessment_complete"], false);
        assert_eq!(output["reciprocally_observed_preserved"], false);
        prep["source_predicate_span"] =
            json!([source.tokens[8].start_byte, source.tokens[8].end_byte]);
        prep["candidate_predicate_span"] =
            json!([candidate.tokens[7].start_byte, candidate.tokens[7].end_byte]);
        let output = evaluate(&source, &candidate, &prep, &json!(p), &r).unwrap();
        assert_eq!(output["reason"], "source_match_missing");
        assert_eq!(output["source_match_count"], 0);
        assert_eq!(output["execution_errors"], json!([]));
    }
}
