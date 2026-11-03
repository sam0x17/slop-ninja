//! Fresh structural fixtures and exact-baseline replay of saved sentence views.
use crate::{
    dative_alternation::{self as alternation, ArgumentPolicy, Proposal, ProposalReport},
    experiment_io::{self as io, Parsed, bound, bytes, output, rows, text},
    structural_evidence,
    verbnet_resource::Resource,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    edits::{self, Edit},
    syntax::Document,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

const PREVIOUS: &str = "data/author-corpora/blog-authorship-2004/sentence-context-v1";
const FIXTURE: &str = "grammar/fixtures/evidence-contracts-v1/manifest.json";
const SCHEMA: &str = "slopninja-evidence-contract-study-protocol-v1";
const VARIANTS: [&str; 2] = ["incumbent_sm", "isolated_trf"];

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Freeze {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Run {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        protocol: PathBuf,
        #[arg(long)]
        expected_protocol_sha256: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Input {
    sha256: String,
    text: String,
    origins: Vec<Value>,
}

fn fixture_inputs(fixture: &Value) -> Result<Vec<Input>> {
    let mut ids = BTreeSet::new();
    let mut inputs = BTreeMap::<String, Input>::new();
    ensure!(
        rows(fixture, "cases")?.len() == 40,
        "Expected 40 fixture rows"
    );
    for c in rows(fixture, "cases")? {
        let id = text(c, "id")?;
        io::safe(id)?;
        ensure!(
            !id.contains('/') && ids.insert(id),
            "Repeated/invalid fixture ID"
        );
        ensure!(
            matches!(
                text(c, "expectation")?,
                "positive_structural_pair" | "reject_terminal_movement"
            ),
            "Unknown fixture expectation"
        );
        text(c, "category")?;
        let source = text(c, "source")?;
        let target = text(c, "target")?;
        ensure!(source != target, "Fixture does not change text");
        for (key, from, to) in [("edits", source, target), ("inverse_edits", target, source)] {
            let patches: Vec<Edit> = serde_json::from_value(c[key].clone())?;
            ensure!(
                !patches.is_empty() && edits::apply(from, &patches)? == to,
                "Fixture patch bytes differ"
            );
        }
        for side in ["source", "target"] {
            let s = text(c, side)?;
            let sha = edits::digest(s);
            let declared = &c[format!("{side}_sha256")];
            if !declared.is_null() {
                ensure!(*declared == sha, "Fixture text hash differs");
            }
            let input = inputs.entry(sha.clone()).or_insert_with(|| Input {
                sha256: sha,
                text: s.into(),
                origins: vec![],
            });
            ensure!(input.text == s, "Exact-byte digest collision");
            input.origins.push(json!({"case_id":id,"side":side}));
        }
    }
    Ok(inputs.into_values().collect())
}
fn parser_catalog(p: &Value) -> Result<&Vec<Value>> {
    let parsers = rows(p, "parsers")?;
    ensure!(
        parsers.len() == 2 && parsers.iter().zip(VARIANTS).all(|(p, id)| p["id"] == id),
        "Parser catalog differs"
    );
    Ok(parsers)
}
fn saved_catalog(repo: &Path, report: &Value) -> Result<Vec<Value>> {
    let index = io::index(report)?;
    let mut cases = BTreeMap::<String, BTreeMap<String, Value>>::new();
    for variant in VARIANTS {
        let entry = rows(report, "reports")?
            .iter()
            .find(|v| v["variant"] == variant)
            .context("Missing previous parser report")?;
        let vb = io::rebase(&format!("{PREVIOUS}/run-v1"), &entry["report"])?;
        let v = bound(repo, &vb)?;
        ensure!(
            v["variant"] == variant && rows(&v, "cases")?.len() == 60,
            "Previous case scope differs"
        );
        for row in rows(&v, "cases")? {
            let id = text(row, "case_id")?;
            ensure!(
                index.get(text(&row["report"], "path")?) == Some(&row["report"]),
                "Case not bound by previous main report"
            );
            let b = io::rebase(&format!("{PREVIOUS}/run-v1"), &row["report"])?;
            let raw = bound(repo, &b)?;
            ensure!(
                raw["case_id"] == id && raw["variant"] == variant,
                "Case identity differs"
            );
            ensure!(
                cases
                    .entry(id.into())
                    .or_default()
                    .insert(variant.into(), b)
                    .is_none(),
                "Repeated saved case"
            );
        }
    }
    ensure!(
        cases.len() == 60 && cases.values().all(|v| v.len() == 2),
        "Expected 60 paired saved cases"
    );
    let mut result = Vec::new();
    for (id, bindings) in cases {
        let a = bound(repo, &bindings["incumbent_sm"])?;
        let b = bound(repo, &bindings["isolated_trf"])?;
        ensure!(
            a["preparation"] == b["preparation"]
                && a["original_alignment"] == b["original_alignment"],
            "Saved views do not share primary preparation"
        );
        bytes(repo, &b["original_alignment"])?;
        result.push(
            json!({"case_id":id,"bindings":bindings,"original_alignment":b["original_alignment"]}),
        );
    }
    Ok(result)
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let predecessor = io::binding(repo, &format!("{PREVIOUS}/final-checks-v1.json"))?;
    let completed = bound(repo, &predecessor)?;
    ensure!(
        completed["stage_completed"] == true,
        "Predecessor incomplete"
    );
    let old_binding = io::rebase(PREVIOUS, &completed["protocol"])?;
    let old = bound(repo, &old_binding)?;
    let report_binding = io::rebase(PREVIOUS, &completed["report"])?;
    let report = bound(repo, &report_binding)?;
    let cases = saved_catalog(repo, &report)?;
    let fixture_binding = io::binding(repo, FIXTURE)?;
    let fixture = bound(repo, &fixture_binding)?;
    let inputs = fixture_inputs(&fixture)?;
    let parsers = parser_catalog(&old)?;
    for parser in parsers {
        io::verify_parser(repo, parser)?;
    }
    io::resource(repo, &old["resource_manifest"])?;
    let mut names = BTreeSet::new();
    for b in rows(&old, "source_bindings")? {
        bytes(repo, b)?;
        names.insert(text(b, "path")?.to_owned());
    }
    ensure!(names.len() == 45, "Predecessor source scope differs");
    for name in [
        "evidence_contract_study.rs",
        "experiment_io.rs",
        "morphology_evidence.rs",
        "orthographic_evidence.rs",
        "sentence_membership_evidence.rs",
        "structural_evidence.rs",
        "bin/slop_ninja-evidence-contracts.rs",
    ] {
        names.insert(format!("grammar/crates/grammar-eval/src/{name}"));
    }
    let p = json!({"schema":SCHEMA,"predecessor":predecessor,"previous_protocol":old_binding,"previous_report":report_binding,
        "source_bindings":names.iter().map(|n|io::binding(repo,n)).collect::<Result<Vec<_>>>()?,
        "executable_sha256":io::hash(&fs::read(std::env::current_exe()?)?),
        "resource_manifest":old["resource_manifest"],"parsers":parsers,"fixture":fixture_binding,"fixture_inputs":inputs,
        "saved_cases":cases,"saved_view_count":300,"fresh_fixture_rows_per_parser":40,"fresh_annotation_requests_per_parser":inputs.len(),"total_annotation_requests":inputs.len()*2,
        "saved_views":["whole_primary","projected_incumbent_sm","isolated_incumbent_sm","projected_isolated_trf","isolated_isolated_trf"],
        "annotation_schedule":"One unchanged parse_batch call per parser, SHA256-sorted shared unique fixture source/target strings; no additional candidate parses",
        "saved_replay":"No NLP; regenerate each exact saved executed Proposal and require wrapper.baseline JSON equality to its prior AlignmentReport before using new findings",
        "positive_criterion":"Exactly one matching source proposal and exactly one same-membership/mapped-predicate restoring reverse, both new outcomes observed_preserved; ambiguity retained",
        "negative_criterion":"An actual exact-target forward has new terminal_punctuation or sentence_membership outcome changed. No proposal or missing parse is unavailable for this specific test; inverse availability is separate",
        "new_saved_pass_criterion":"Exactly one original source match and one original reverse match; all executed baselines verified and both new outcomes observed_preserved",
        "interpretation":"Structural engineering fixtures and descriptive saved-view overlays; no semantic correctness, preference fitting, edit adoption or whole-post eligibility override",
        "runtime_changes":false,"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,
        "automatic_edit_license":false,"semantic_equivalence_certified":false,"whole_post_eligibility_overridden":false});
    fs::create_dir(out)?;
    println!("{}", output(out, "protocol.json", &p)?);
    Ok(())
}

fn generated(source: &Document, resource: &Resource) -> Result<ProposalReport> {
    alternation::propose_with_policy(source, resource, ArgumentPolicy::RoleAwareV1)
}
fn exact_saved<'a>(current: &'a ProposalReport, saved: &Value) -> Result<&'a Proposal> {
    let matches = current
        .proposals
        .iter()
        .filter_map(|p| match serde_json::to_value(p) {
            Ok(v) if v == *saved => Some(Ok(p)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        matches.len() == 1,
        "Saved executed Proposal does not regenerate exactly once"
    );
    Ok(matches[0])
}
fn saved_by_id<'a>(report: &'a Value, id: &str) -> Result<&'a Value> {
    let found = rows(report, "proposals")?
        .iter()
        .filter(|p| p["id"] == id)
        .collect::<Vec<_>>();
    ensure!(found.len() == 1, "Saved proposal ID is absent or ambiguous");
    Ok(found[0])
}
fn compared(
    source: &Document,
    candidate: &Document,
    p: &Proposal,
    resource: &Resource,
    saved: Option<&Value>,
) -> Result<Value> {
    match structural_evidence::compare(source, candidate, p, resource) {
        Ok(mut v) => {
            if let Some(old) = saved {
                ensure!(
                    v["baseline"] == *old,
                    "Executed baseline AlignmentReport changed"
                );
            }
            v["saved_baseline_verified"] = json!(saved.map(|_| true));
            Ok(v)
        }
        Err(e) => Ok(
            json!({"status":"execution_error","error":format!("{e:#}"),"outcome":null,"assessment_complete":false,"saved_baseline_verified":saved.map(|_|false),"automatic_edit_license":false,"semantic_equivalence_certified":false}),
        ),
    }
}
fn mapped_head(forward: &Value, p: &Proposal) -> Option<usize> {
    forward["baseline"]["token_alignment"]
        .as_array()?
        .iter()
        .find(|a| a["source_index"] == p.predicate_index)?["candidate_index"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
}
fn reverse_matches(
    p: &Proposal,
    reverse: &Proposal,
    source: &Document,
    mapped: Option<usize>,
) -> Result<bool> {
    let yes = reverse.candidate.text == source.text
        && reverse.member_class_id == p.member_class_id
        && reverse.member_ordinal == p.member_ordinal
        && reverse.source_frame_id == p.target_frame_id
        && reverse.target_frame_id == p.source_frame_id
        && reverse.direction != p.direction
        && Some(reverse.predicate_index) == mapped;
    if yes {
        ensure!(
            edits::apply(&p.candidate.text, &reverse.candidate.edits)? == source.text,
            "Inverse bytes differ"
        );
    }
    Ok(yes)
}
fn all_complete(forward: &Value, reverse: &[Value]) -> bool {
    forward["assessment_complete"] == true
        && reverse
            .iter()
            .all(|r| r["evidence"]["assessment_complete"] == true)
}
fn reason_changes(evidence: &Value) -> Value {
    json!({"old_failed_conditions":evidence["baseline"]["findings"].as_array().map(|f|f.iter().filter(|f|f["outcome"]!="observed_preserved").map(|f|json!({"condition":f["condition"],"outcome":f["outcome"]})).collect::<Vec<_>>()),
        "new_failed_conditions":evidence["findings"].as_array().map(|f|f.iter().filter(|f|f["outcome"]!="observed_preserved").map(|f|json!({"condition":f["condition"],"outcome":f["outcome"]})).collect::<Vec<_>>()),
        "terminal_punctuation":evidence["terminal_punctuation"]["outcome"],"sentence_membership":evidence["sentence_membership"]["outcome"]})
}
fn replay(
    source: &Document,
    candidate: &Document,
    original: &Value,
    old: &Value,
    whole: bool,
    resource: &Resource,
) -> Result<Value> {
    let mut result = json!({"status":"unavailable","reason":old["reason"],"old_status":old["status"],"old_preserved":old["reciprocally_observed_preserved"],
        "new_preserved":false,"assessment_complete":false,"source_match_count":if whole{json!(1)}else{old["source_match_count"].clone()},
        "reverse_match_count":if whole{json!(rows(old,"exact_same_membership_reverse_matches")?.len())}else{old["reverse_match_count"].clone()},
        "baseline_checks_requested":0,"baseline_checks_verified":0,"forward":null,"reverse":[],
        "automatic_edit_license":false,"semantic_equivalence_certified":false,"whole_post_eligibility_overridden":false});
    let old_forward = &old["forward"];
    if old_forward.is_null() {
        result["reason"] = json!(if old["source_match_count"] == 0 {
            "source_match_missing"
        } else {
            "no_saved_executed_forward"
        });
        result["assessment_complete"] = old["assessment_complete"].clone();
        return Ok(result);
    }
    let source_report = generated(source, resource)?;
    let saved_source = if whole {
        &original["proposal"]
    } else {
        ensure!(
            old["source_match_count"] == 1,
            "Executed local forward had ambiguous source matches"
        );
        let ids = rows(old, "source_matching_proposal_ids")?;
        ensure!(ids.len() == 1, "Expected one saved local source ID");
        saved_by_id(
            &old["source_proposal_report"],
            ids[0].as_str().context("Invalid saved source ID")?,
        )?
    };
    let p = exact_saved(&source_report, saved_source)?;
    ensure!(
        p.candidate.text == candidate.text,
        "Saved forward candidate differs"
    );
    let forward = compared(source, candidate, p, resource, Some(old_forward))?;
    let old_reverse = rows(
        old,
        if whole {
            "exact_same_membership_reverse_matches"
        } else {
            "reverse"
        },
    )?;
    let reverse_report = generated(candidate, resource)?;
    let mut reversed = Vec::new();
    let mut checks = 1usize;
    let mut verified = usize::from(forward["saved_baseline_verified"] == true);
    // Use the saved forward mapping for relation validation even if the new
    // overlay cannot execute. No new evidence is accepted in that error case.
    let old_mapping = json!({"baseline":old_forward});
    let mapped = mapped_head(&old_mapping, p);
    for row in old_reverse {
        let prior = &row["alignment"];
        if prior.is_null() {
            continue;
        }
        let saved = saved_by_id(&old["reverse_proposal_report"], text(row, "proposal_id")?)?;
        let reverse = exact_saved(&reverse_report, saved)?;
        ensure!(
            reverse_matches(p, reverse, source, mapped)?,
            "Saved inverse membership/predicate relation differs"
        );
        let evidence = compared(candidate, source, reverse, resource, Some(prior))?;
        checks += 1;
        verified += usize::from(evidence["saved_baseline_verified"] == true);
        reversed.push(json!({"proposal_id":reverse.id,"proposal":reverse,"reason_changes":reason_changes(&evidence),"evidence":evidence}));
    }
    result["baseline_checks_requested"] = json!(checks);
    result["baseline_checks_verified"] = json!(verified);
    result["forward_reason_changes"] = reason_changes(&forward);
    result["forward"] = forward.clone();
    result["reverse"] = json!(reversed);
    result["source_proposal"] = json!(p);
    let unique = result["source_match_count"] == 1
        && result["reverse_match_count"] == 1
        && reversed.len() == 1;
    result["status"] = json!(if checks != verified {
        "execution_error"
    } else if unique {
        "compared"
    } else {
        "unavailable"
    });
    result["reason"] = json!(if checks != verified {
        Some("overlay_execution_error")
    } else if !unique {
        Some("reverse_match_missing_or_ambiguous")
    } else {
        None
    });
    result["assessment_complete"] =
        json!(unique && checks == verified && all_complete(&forward, &reversed));
    result["new_preserved"] = json!(
        unique
            && checks == verified
            && forward["outcome"] == "observed_preserved"
            && reversed[0]["evidence"]["outcome"] == "observed_preserved"
    );
    Ok(result)
}

fn projection(v: &Value, identity: &str, expected: &str) -> Result<Parsed> {
    match text(v, "status")? {
        "available" => Ok(Ok(io::validate_doc(
            &v["document"],
            identity,
            &edits::digest(expected),
        )?)),
        "unavailable" => Ok(Err(v["reason"]
            .as_str()
            .unwrap_or("projection_unavailable")
            .into())),
        _ => anyhow::bail!("Unknown projection status"),
    }
}
fn view_docs(repo: &Path, case: &Value, variant: &Value, view: &str) -> Result<(Parsed, Parsed)> {
    let identity = text(variant, "parser_identity")?;
    let prepare = &case["preparation"];
    let mut docs = Vec::new();
    for side in ["source", "candidate"] {
        let parsed = match view {
            "whole" => io::document(
                repo,
                &case["full_annotations"][side],
                identity,
                text(prepare, &format!("{side}_full_sha256"))?,
            )?,
            "projected" => projection(
                &case["full_sentence_projections"][side],
                identity,
                text(prepare, &format!("{side}_text"))?,
            )?,
            "isolated" => io::document(
                repo,
                &io::rebase(
                    &format!("{PREVIOUS}/run-v1"),
                    &case["isolated_annotations"][side],
                )?,
                identity,
                &edits::digest(text(prepare, &format!("{side}_text"))?),
            )?,
            _ => anyhow::bail!("Unknown saved view"),
        };
        docs.push(parsed);
    }
    let candidate = docs.pop().unwrap();
    Ok((docs.pop().unwrap(), candidate))
}
fn replay_saved(
    repo: &Path,
    out: &Path,
    protocol: &Value,
    resource: &Resource,
    artifacts: &mut Vec<Value>,
) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for c in rows(protocol, "saved_cases")? {
        let id = text(c, "case_id")?;
        let original = bound(repo, &c["original_alignment"])?;
        for parser in parser_catalog(protocol)? {
            let variant = text(parser, "id")?;
            let case = bound(repo, &c["bindings"][variant])?;
            ensure!(
                case["case_id"] == id && case["original_alignment"] == c["original_alignment"],
                "Saved case binding differs"
            );
            let views = if variant == "isolated_trf" {
                vec!["whole", "projected", "isolated"]
            } else {
                vec!["projected", "isolated"]
            };
            for view in views {
                let old = if view == "whole" {
                    &original["result"]
                } else {
                    &case[format!("{view}_evaluation")]
                };
                let (s, t) = view_docs(repo, &case, parser, view)?;
                let value = match (&s, &t) {
                    (Ok(s), Ok(t)) => replay(s, t, &original, old, view == "whole", resource)?,
                    _ => {
                        json!({"status":"unavailable","reason":"annotation_or_projection_unavailable","source_error":s.as_ref().err(),"candidate_error":t.as_ref().err(),"old_preserved":old["reciprocally_observed_preserved"],"new_preserved":null,"assessment_complete":false,"baseline_checks_requested":0,"baseline_checks_verified":0})
                    }
                };
                let view_id = if view == "whole" {
                    "whole_primary".to_owned()
                } else {
                    format!("{view}_{variant}")
                };
                let row = json!({"case_id":id,"view":view_id,"variant":variant,"case_binding":c["bindings"][variant],"original_alignment":c["original_alignment"],"result":value,"automatic_edit_license":false,"semantic_equivalence_certified":false,"whole_post_eligibility_overridden":false});
                let b = output(out, &format!("saved/{view_id}/{id}.json"), &row)?;
                artifacts.push(b.clone());
                results.push(json!({"case_id":id,"view":view_id,"variant":variant,"status":value["status"],"reason":value["reason"],"old_preserved":value["old_preserved"],"new_preserved":value["new_preserved"],"assessment_complete":value["assessment_complete"],"baseline_checks_requested":value["baseline_checks_requested"],"baseline_checks_verified":value["baseline_checks_verified"],"report":b}));
            }
        }
    }
    ensure!(results.len() == 300, "Saved view denominator differs");
    Ok(results)
}

fn fresh_pair(source: &Document, target: &Document, resource: &Resource) -> Result<Value> {
    let source_report = generated(source, resource)?;
    let reverse_report = generated(target, resource)?;
    let matched = source_report
        .proposals
        .iter()
        .filter(|p| p.candidate.text == target.text)
        .collect::<Vec<_>>();
    let mut forwards = Vec::new();
    for p in &matched {
        ensure!(
            edits::apply(&source.text, &p.candidate.edits)? == target.text,
            "Fresh proposal patch differs"
        );
        let forward = compared(source, target, p, resource, None)?;
        let mapped = mapped_head(&forward, p);
        let mut reverses = Vec::new();
        for r in &reverse_report.proposals {
            if reverse_matches(p, r, source, mapped)? {
                let reverse = compared(target, source, r, resource, None)?;
                reverses.push(json!({"proposal_id":r.id,"proposal":r,"evidence":reverse}));
            }
        }
        let unique_reverse = reverses.len() == 1;
        let new_pass = unique_reverse
            && forward["outcome"] == "observed_preserved"
            && reverses[0]["evidence"]["outcome"] == "observed_preserved";
        let old_pass = unique_reverse
            && forward["baseline"]["outcome"] == "observed_preserved"
            && reverses[0]["evidence"]["baseline"]["outcome"] == "observed_preserved";
        forwards.push(json!({"proposal_id":p.id,"proposal":p,"forward":forward,"reverse_match_count":reverses.len(),"reverse":reverses,
            "old_preserved":old_pass,"new_preserved":new_pass}));
    }
    let unique = forwards.len() == 1;
    Ok(
        json!({"status":if matched.is_empty(){"unavailable"}else{"tested"},"reason":if matched.is_empty(){Some("no_exact_target_proposal")}else{None},
        "source_match_count":matched.len(),"source_proposal_report":source_report,"reverse_proposal_report":reverse_report,"matches":forwards,
        "unique_old_preserved":unique && forwards[0]["old_preserved"]==true,
        "unique_new_preserved":unique && forwards[0]["new_preserved"]==true,
        "specific_directional_rejection":forwards.iter().any(|f|f["forward"]["terminal_punctuation"]["outcome"]=="changed" || f["forward"]["sentence_membership"]["outcome"]=="changed"),
        "automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}
fn expectation(case: &Value, result: &Value) -> Result<Value> {
    let expected = text(case, "expectation")?;
    let attempted = result["matches"]
        .as_array()
        .is_some_and(|m| m.iter().any(|m| m["forward"]["status"] == "compared"));
    let status = if !attempted {
        "unavailable"
    } else if expected == "positive_structural_pair" {
        if result["unique_new_preserved"] == true {
            "met"
        } else {
            "mismatch"
        }
    } else if result["specific_directional_rejection"] == true {
        "met"
    } else {
        "mismatch"
    };
    Ok(
        json!({"expectation":expected,"status":status,"tested_forward":attempted,"structural_only":true,
        "specific_terminal_or_membership_rejection":result["specific_directional_rejection"],"emission_abstention":result["source_match_count"]==0}),
    )
}
fn fresh_fixtures(
    repo: &Path,
    out: &Path,
    p: &Value,
    resource: &Resource,
    artifacts: &mut Vec<Value>,
) -> Result<Vec<Value>> {
    let fixture = bound(repo, &p["fixture"])?;
    let inputs: Vec<Input> = serde_json::from_value(p["fixture_inputs"].clone())?;
    ensure!(
        fixture_inputs(&fixture)? == inputs,
        "Fixture input schedule differs"
    );
    let mut reports = Vec::new();
    for parser in parser_catalog(p)? {
        let variant = text(parser, "id")?;
        io::verify_parser(repo, parser)?;
        println!(
            "{variant}: {} fresh fixture texts in one interpreter",
            inputs.len()
        );
        let requested = inputs.iter().map(|i| i.text.as_str()).collect::<Vec<_>>();
        let started = Instant::now();
        let parsed = grammar_spacy::parse_batch(
            &requested,
            &repo
                .join(text(&parser["parser_python"], "path")?)
                .to_string_lossy(),
        );
        let parse_seconds = started.elapsed().as_secs_f64();
        let (parsed, process_error) = match parsed {
            Ok(v) => (v, None),
            Err(e) => {
                let error = format!("{e:#}");
                (
                    (0..inputs.len()).map(|_| Err(error.clone())).collect(),
                    Some(error),
                )
            }
        };
        ensure!(parsed.len() == inputs.len(), "Annotation count differs");
        io::verify_parser(repo, parser)?;
        let mut docs = BTreeMap::new();
        let mut refs = BTreeMap::new();
        let mut errors = 0;
        for (input, doc) in inputs.iter().zip(parsed) {
            let value = match &doc {
                Ok(d) => {
                    io::validate_doc(&json!(d), text(parser, "parser_identity")?, &input.sha256)?;
                    json!({"status":"ok","document":d,"origins":input.origins})
                }
                Err(e) => {
                    errors += 1;
                    json!({"status":if process_error.is_some(){"process_error"}else{"error"},"text":input.text,"error":e,"origins":input.origins})
                }
            };
            let b = output(
                out,
                &format!("fresh/{variant}/annotations/{}.json", input.sha256),
                &value,
            )?;
            artifacts.push(b.clone());
            refs.insert(input.sha256.clone(), b);
            docs.insert(input.sha256.clone(), doc);
        }
        let mut results = Vec::new();
        for case in rows(&fixture, "cases")? {
            let source_hash = edits::digest(text(case, "source")?);
            let target_hash = edits::digest(text(case, "target")?);
            let s = &docs[&source_hash];
            let t = &docs[&target_hash];
            let result = match (s, t) {
                (Ok(s), Ok(t)) => match fresh_pair(s, t, resource) {
                    Ok(v) => v,
                    Err(e) => {
                        json!({"status":"execution_error","error":format!("{e:#}"),"unique_new_preserved":null,"source_match_count":null})
                    }
                },
                _ => {
                    json!({"status":"unavailable","reason":"annotation_unavailable","source_error":s.as_ref().err(),"target_error":t.as_ref().err(),"source_match_count":null,"unique_new_preserved":null})
                }
            };
            let exp = expectation(case, &result)?;
            let row = json!({"case_id":case["id"],"category":case["category"],"pair_id":case["pair_id"],"inverse_case_id":case["inverse_case_id"],"variant":variant,"source_annotation":refs[&source_hash],"target_annotation":refs[&target_hash],"result":result,"expectation":exp,"automatic_edit_license":false,"semantic_equivalence_certified":false});
            let b = output(
                out,
                &format!("fresh/{variant}/cases/{}.json", text(case, "id")?),
                &row,
            )?;
            artifacts.push(b.clone());
            results.push(json!({"case_id":case["id"],"category":case["category"],"pair_id":case["pair_id"],"status":result["status"],"source_match_count":result["source_match_count"],"unique_old_preserved":result["unique_old_preserved"],"unique_new_preserved":result["unique_new_preserved"],"specific_directional_rejection":result["specific_directional_rejection"],"expectation":exp,"report":b}));
        }
        ensure!(results.len() == 40, "Fresh row denominator differs");
        reports.push(json!({"variant":variant,"annotation_requests":inputs.len(),"annotation_errors":errors,"process_error":process_error,"parse_seconds":parse_seconds,"cases":results}));
    }
    Ok(reports)
}
fn saved_summary(rows: &[Value]) -> Value {
    let mut groups = BTreeMap::<String, Vec<&Value>>::new();
    for r in rows {
        groups
            .entry(r["view"].as_str().unwrap().into())
            .or_default()
            .push(r);
    }
    json!(groups.into_iter().map(|(view,rows)|{
        let mut transitions=BTreeMap::<String,usize>::new();
        for r in &rows {let key=format!("{} -> {}",r["old_preserved"],r["new_preserved"]);*transitions.entry(key).or_default()+=1;}
        (view,json!({"requested":rows.len(),"old_preserved":rows.iter().filter(|r|r["old_preserved"]==true).count(),"new_preserved":rows.iter().filter(|r|r["new_preserved"]==true).count(),"unavailable_or_error":rows.iter().filter(|r|r["status"]!="compared").count(),"assessment_complete":rows.iter().filter(|r|r["assessment_complete"]==true).count(),"transitions":transitions,"baseline_checks_requested":rows.iter().map(|r|r["baseline_checks_requested"].as_u64().unwrap_or(0)).sum::<u64>(),"baseline_checks_verified":rows.iter().map(|r|r["baseline_checks_verified"].as_u64().unwrap_or(0)).sum::<u64>()}))
    }).collect::<BTreeMap<_,_>>())
}
fn run(repo: &Path, file: &Path, expected: &str, out: &Path) -> Result<()> {
    let p: Value = serde_json::from_slice(&io::checked(file, expected)?)?;
    ensure!(
        p["schema"] == SCHEMA
            && p["saved_view_count"] == 300
            && p["fresh_fixture_rows_per_parser"] == 40
            && p["runtime_changes"] == false
            && p["fit_models"] == false
            && p["later_posts"] == 0,
        "Study protocol differs"
    );
    ensure!(
        p["executable_sha256"] == io::hash(&fs::read(std::env::current_exe()?)?),
        "Executable differs"
    );
    ensure!(
        bound(repo, &p["predecessor"])?["stage_completed"] == true,
        "Predecessor incomplete"
    );
    for b in rows(&p, "source_bindings")? {
        io::safe(text(b, "path")?)?;
        bytes(repo, b)?;
    }
    let old_report = bound(repo, &p["previous_report"])?;
    ensure!(
        saved_catalog(repo, &old_report)? == *rows(&p, "saved_cases")?,
        "Saved case allocation differs"
    );
    let old = bound(repo, &p["previous_protocol"])?;
    ensure!(
        old["parsers"] == p["parsers"] && old["resource_manifest"] == p["resource_manifest"],
        "Pinned parser/resource catalog differs"
    );
    let resource = io::resource(repo, &p["resource_manifest"])?;
    fs::create_dir(out)?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![output(out, "protocol.json", &p)?];
        for b in rows(&p, "source_bindings")? {
            let name = format!("executed-source/{}", text(b, "path")?);
            let target = out.join(&name);
            fs::create_dir_all(target.parent().unwrap())?;
            let source = bytes(repo, b)?;
            fs::write(target, &source)?;
            artifacts.push(json!({"path":name,"sha256":io::hash(&source)}));
        }
        // Detect any changed predecessor baseline before fresh fixture NLP.
        let saved = replay_saved(repo, out, &p, &resource, &mut artifacts)?;
        let saved_summary = saved_summary(&saved);
        artifacts.push(output(
            out,
            "saved-report.json",
            &json!({"requested":300,"summary":saved_summary,"cases":saved}),
        )?);
        let fresh = fresh_fixtures(repo, out, &p, &resource, &mut artifacts)?;
        let report = json!({"schema":"slopninja-evidence-contract-study-report-v1","protocol_sha256":expected,"saved_requested":300,"saved_summary":saved_summary,"saved_cases":saved,"fresh_reports":fresh,"total_annotation_requests":p["total_annotation_requests"],"artifacts":artifacts,"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false,"whole_post_eligibility_overridden":false});
        let b = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"schema":"slopninja-evidence-contract-study-receipt-v1","status":"completed","protocol_sha256":expected,"report":b}),
        )?;
        Ok(())
    })();
    if let Err(e) = &result {
        output(
            out,
            "failure.json",
            &json!({"status":"failed","protocol_sha256":expected,"error":format!("{e:#}")}),
        )?;
    }
    result
}
pub fn main() -> Result<()> {
    match Args::parse().command {
        Command::Freeze { repo, out } => freeze(&repo, &out),
        Command::Run {
            repo,
            out,
            protocol,
            expected_protocol_sha256,
        } => run(&repo, &protocol, &expected_protocol_sha256, &out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positive_ambiguity_and_negative_unavailability_are_not_successes() {
        let positive = json!({"expectation":"positive_structural_pair"});
        let tested = json!({"matches":[{"forward":{"status":"compared"}}],"source_match_count":2,"unique_new_preserved":false});
        assert_eq!(
            expectation(&positive, &tested).unwrap()["status"],
            "mismatch"
        );
        let negative = json!({"expectation":"reject_terminal_movement"});
        assert_eq!(
            expectation(&negative, &json!({"source_match_count":0})).unwrap()["status"],
            "unavailable"
        );
        let unrelated = json!({"matches":[{"forward":{"status":"compared","outcome":"changed"}}],"specific_directional_rejection":false});
        assert_eq!(
            expectation(&negative, &unrelated).unwrap()["status"],
            "mismatch"
        );
        let specific = json!({"matches":[{"forward":{"status":"compared"}}],"specific_directional_rejection":true});
        assert_eq!(expectation(&negative, &specific).unwrap()["status"], "met");
    }
    #[test]
    fn missing_saved_views_remain_in_denominator() {
        let rows = vec![
            json!({"view":"whole_primary","old_preserved":false,"new_preserved":null,"status":"unavailable","baseline_checks_requested":0,"baseline_checks_verified":0}),
            json!({"view":"whole_primary","old_preserved":false,"new_preserved":true,"status":"compared","assessment_complete":true,"baseline_checks_requested":2,"baseline_checks_verified":2}),
        ];
        let s = saved_summary(&rows);
        assert_eq!(s["whole_primary"]["requested"], 2);
        assert_eq!(s["whole_primary"]["unavailable_or_error"], 1);
        assert_eq!(s["whole_primary"]["new_preserved"], 1);
        assert_eq!(s["whole_primary"]["baseline_checks_verified"], 2);
    }
}
