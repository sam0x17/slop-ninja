//! Versioned evidence overlay. Baseline rules, annotations and reports stay intact.
use crate::{
    dative_alternation::{self as alternation, ArgumentPolicy, Proposal},
    morphology_evidence, orthographic_evidence, sentence_membership_evidence,
    verbnet_resource::Resource,
};
use anyhow::{Result, ensure};
use grammar_core::syntax::Document;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-structural-evidence-v1";
fn preserved(b: &alternation::AlignmentReport, condition: &str) -> bool {
    b.findings
        .iter()
        .any(|f| f.condition == condition && f.outcome == alternation::Outcome::ObservedPreserved)
}
fn outcome(findings: &[Value]) -> &'static str {
    if findings.iter().any(|f| f["outcome"] == "changed") {
        "changed"
    } else if findings.iter().any(|f| f["outcome"] == "unresolved") {
        "unresolved"
    } else {
        "observed_preserved"
    }
}
pub fn compare(
    source: &Document,
    candidate: &Document,
    proposal: &Proposal,
    resource: &Resource,
) -> Result<Value> {
    let baseline = alternation::compare(source, candidate, proposal, resource)?;
    let map = baseline
        .token_alignment
        .iter()
        .map(|a| (a.source_index, a.candidate_index))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        map.len() == baseline.token_alignment.len(),
        "Repeated alignment source index"
    );
    let paired_roles = preserved(&baseline, "same_class_opposite_frame_roles");
    let mut findings = baseline
        .findings
        .iter()
        .filter(|f| {
            !matches!(
                f.condition.as_str(),
                "token_lexical_morphology" | "sentence_partition"
            )
        })
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut morphology = Vec::new();
    for (&s, &t) in &map {
        let recipient = proposal.argument_policy == ArgumentPolicy::RoleAwareV1
            && paired_roles
            && proposal.roles.recipient.head_index == s
            && proposal.roles.recipient.token_indices == vec![s]
            && baseline.candidate_roles.as_ref().is_some_and(|r| {
                r.recipient.head_index == t && r.recipient.token_indices == vec![t]
            })
            && baseline.findings.iter().any(|f| {
                f.condition == "dependency_attachment"
                    && f.outcome == alternation::Outcome::ObservedPreserved
                    && f.evidence["source"] == s
                    && f.evidence["candidate"] == t
                    && f.evidence["licensed_recipient_change"] == true
            });
        let evidence =
            morphology_evidence::compare(&source.tokens[s], &candidate.tokens[t], recipient)?;
        findings.push(json!({"condition":"lexical_morphology_evidence","outcome":evidence["outcome"],"evidence":evidence}));
        morphology.push(json!({"source":s,"candidate":t,"outcome":findings.last().unwrap()["outcome"],"supported_recipient":recipient}));
    }
    let omissions = if paired_roles {
        proposal
            .source_preposition
            .into_iter()
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let insertions = baseline
        .findings
        .iter()
        .filter(|f| {
            f.condition == "inserted_carrier_role"
                && f.outcome == alternation::Outcome::ObservedPreserved
        })
        .filter_map(|f| f.evidence["inserted"].as_u64())
        .map(|n| n as usize)
        .collect::<BTreeSet<_>>();
    let sentence =
        sentence_membership_evidence::compare(source, candidate, &map, &omissions, &insertions)?;
    let punctuation = orthographic_evidence::compare(source, candidate, &map)?;
    for (condition, evidence) in [
        ("sentence_membership", &sentence),
        ("terminal_punctuation", &punctuation),
    ] {
        ensure!(
            matches!(
                evidence["outcome"].as_str(),
                Some("observed_preserved" | "changed" | "unresolved")
            ),
            "Unknown evidence outcome"
        );
        findings
            .push(json!({"condition":condition,"outcome":evidence["outcome"],"evidence":evidence}));
    }
    let final_outcome = outcome(&findings);
    Ok(
        json!({"schema":SCHEMA,"status":"compared","outcome":final_outcome,
        "assessment_complete":!findings.iter().any(|f|f["outcome"]=="unresolved"),
        "baseline":baseline,"findings":findings,"morphology_alignment":morphology,
        "sentence_membership":sentence,"terminal_punctuation":punctuation,
        "annotations_modified":false,"legacy_policy_modified":false,
        "automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbnet_resource::XmlSource;
    use grammar_core::{
        edits,
        syntax::{Sentence, Token},
    };
    fn resource() -> Resource {
        let frame = |s: &str| {
            format!(
                "<FRAME><DESCRIPTION primary='synthetic'/><EXAMPLES/><SYNTAX>{s}</SYNTAX><SEMANTICS/></FRAME>"
            )
        };
        let xml = format!(
            "<VNCLASS ID='give-13.1'><MEMBERS><MEMBER name='lend'/></MEMBERS><THEMROLES><THEMROLE type='Agent'/><THEMROLE type='Theme'/><THEMROLE type='Recipient'/></THEMROLES><FRAMES>{}{}</FRAMES></VNCLASS>",
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
        // Matcher-unit provenance stub only; production resource bytes remain pinned.
        for class in r.classes.values_mut() {
            for f in &mut class.effective_frames {
                f.origin.source_sha256 =
                    "bbddd2c3cbe0a8a6862eee7a140827938921df2eaf7fc2852f3f4d133800fbee".into();
            }
        }
        r
    }
    fn doc(items: &[(&str, &str, &str, usize)]) -> Document {
        let mut text = String::new();
        let mut tokens = Vec::new();
        for (i, &(word, pos, dep, head)) in items.iter().enumerate() {
            if i > 0 {
                text.push(' ');
            }
            let start = text.len();
            text.push_str(word);
            tokens.push(Token {
                i,
                start_byte: start,
                end_byte: text.len(),
                text: word.into(),
                lemma: if word == "lent" {
                    "lend".into()
                } else {
                    word.to_lowercase()
                },
                pos: pos.into(),
                tag: match pos {
                    "PRON" => "PRP",
                    "VERB" => "VBD",
                    _ => pos,
                }
                .into(),
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
        Document {
            text: text.clone(),
            parser_identity: "synthetic-v1".into(),
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: text.len(),
                root: 1,
                token_start: 0,
                token_end: tokens.len(),
            }],
            tokens,
        }
    }
    fn pair(recipient: &str, pos: &str) -> (Document, Document) {
        (
            doc(&[
                ("Nia", "PROPN", "nsubj", 1),
                ("lent", "VERB", "ROOT", 1),
                ("the", "DET", "det", 3),
                ("atlas", "NOUN", "dobj", 1),
                ("to", "ADP", "dative", 1),
                (recipient, pos, "pobj", 4),
            ]),
            doc(&[
                ("Nia", "PROPN", "nsubj", 1),
                ("lent", "VERB", "ROOT", 1),
                (recipient, pos, "dative", 1),
                ("the", "DET", "det", 4),
                ("atlas", "NOUN", "dobj", 1),
            ]),
        )
    }
    fn run(s: &Document, t: &Document, r: &Resource) -> Value {
        let p = alternation::propose_with_policy(s, r, ArgumentPolicy::RoleAwareV1).unwrap();
        let proposal = p
            .proposals
            .iter()
            .find(|p| p.candidate.text == t.text)
            .expect("Expected source proposal");
        compare(s, t, proposal, r).unwrap()
    }
    #[test]
    fn bound_recipient_missing_case_is_compatible_without_modifying_baseline_or_tokens() {
        let r = resource();
        let (mut s, t) = pair("him", "PRON");
        s.tokens[5].morph.insert("Case".into(), vec!["Acc".into()]);
        for (a, b) in [(&s, &t), (&t, &s)] {
            let out = run(a, b, &r);
            assert_eq!(out["baseline"]["outcome"], "changed");
            assert_eq!(out["outcome"], "observed_preserved", "{out}");
            assert_eq!(out["annotations_modified"], false);
        }
        assert!(t.tokens[2].morph.is_empty());
    }
    #[test]
    fn missing_agent_case_and_changed_recipient_attachment_do_not_get_the_exception() {
        let r = resource();
        let (mut s, mut t) = pair("him", "PRON");
        s.tokens[0].morph.insert("Case".into(), vec!["Nom".into()]);
        assert_eq!(run(&s, &t, &r)["outcome"], "unresolved");
        s.tokens[0].morph.clear();
        s.tokens[5].morph.insert("Case".into(), vec!["Acc".into()]);
        t.tokens[2].dep = "dobj".into();
        assert_ne!(run(&s, &t, &r)["outcome"], "observed_preserved");
    }
    #[test]
    fn accepted_legacy_fused_terminal_movement_is_rejected_in_both_directions() {
        let r = resource();
        let (s, t) = pair("I.", "PROPN");
        for (a, b) in [(&s, &t), (&t, &s)] {
            let out = run(a, b, &r);
            assert_eq!(out["baseline"]["outcome"], "observed_preserved");
            assert_eq!(out["terminal_punctuation"]["outcome"], "changed");
            assert_eq!(out["outcome"], "changed");
        }
    }
}
