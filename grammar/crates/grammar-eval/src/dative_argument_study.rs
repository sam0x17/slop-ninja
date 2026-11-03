//! Fresh structural confirmation, followed by a descriptive TRAIN replay.
use crate::{
    dative_alternation::{self as alternation, ArgumentPolicy},
    dative_corpus_study as corpus, lexical_frame_study as study,
    verbnet_resource::Resource,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{edits, features, syntax::Document};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use study::{bound, bound_bytes, checked, hash, output, rows, safe_relative, text};

const BASE: &str = "data/author-corpora/blog-authorship-2004";
const SCHEMA: &str = "slopninja-dative-arguments-protocol-v1";
const FIXTURE: &str = "grammar/fixtures/dative-arguments-v1/manifest.json";
const POLICY: ArgumentPolicy = ArgumentPolicy::RoleAwareV1;

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
        protocol: PathBuf,
        #[arg(long)]
        expected_protocol_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
}
fn binding(repo: &Path, name: &str) -> Result<Value> {
    safe_relative(name)?;
    Ok(json!({"path":name,"sha256":hash(&fs::read(repo.join(name))?)}))
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let previous_protocol = binding(
        repo,
        &format!("{BASE}/dative-corpus-v1/protocol-v1/protocol.json"),
    )?;
    let previous = bound(repo, &previous_protocol)?;
    let previous_run = format!("{BASE}/dative-corpus-v1/run-v1");
    let receipt = bound(
        repo,
        &binding(repo, &format!("{previous_run}/receipt.json"))?,
    )?;
    ensure!(
        receipt["status"] == "completed",
        "Corpus predecessor incomplete"
    );
    let report_binding = binding(repo, &format!("{previous_run}/report.json"))?;
    ensure!(
        report_binding["sha256"] == receipt["report"]["sha256"],
        "Predecessor report changed"
    );
    let report = bound(repo, &report_binding)?;
    let mut names = BTreeSet::new();
    let mut changed = Vec::new();
    for old in rows(&previous, "source_bindings")? {
        let name = text(old, "path")?;
        checked(
            &repo.join(&previous_run).join("executed-source").join(name),
            text(old, "sha256")?,
        )?;
        let current = binding(repo, name)?;
        if current["sha256"] != old["sha256"] {
            ensure!(
                [
                    "dative_alternation.rs",
                    "dative_corpus_observation.rs",
                    "dative_corpus_study.rs"
                ]
                .iter()
                .any(|s| name == format!("grammar/crates/grammar-eval/src/{s}")),
                "Undeclared predecessor source change: {name}"
            );
            changed.push(json!({"previous":old,"current":current}));
        }
        names.insert(name.to_owned());
    }
    names.insert("grammar/crates/grammar-eval/src/dative_argument_study.rs".into());
    names.insert("grammar/crates/grammar-eval/src/bin/slop_ninja-dative-arguments.rs".into());
    if repo
        .join("grammar/crates/grammar-eval/src/dative_argument_policy.rs")
        .exists()
    {
        names.insert("grammar/crates/grammar-eval/src/dative_argument_policy.rs".into());
    }
    // Nested policy modules, if present, are executable source too.
    let nested = repo.join("grammar/crates/grammar-eval/src/dative_alternation");
    if nested.exists() {
        for entry in fs::read_dir(nested)? {
            let file = entry?.path();
            ensure!(
                file.is_file() && file.extension().is_some_and(|e| e == "rs"),
                "Unexpected nested policy source"
            );
            names.insert(file.strip_prefix(repo)?.to_string_lossy().into_owned());
        }
    }
    let sources = rows(&report, "artifacts")?
        .iter()
        .filter(|b| {
            b["path"]
                .as_str()
                .is_some_and(|s| s.starts_with("source-annotations/isolated_trf/"))
        })
        .map(|b| {
            json!({"path":format!("{previous_run}/{}", b["path"].as_str().unwrap()),
            "sha256":b["sha256"],"storage":"bound_prior_corpus"})
        })
        .collect::<Vec<_>>();
    ensure!(
        sources.len() == 3889,
        "Prior full-source annotation count differs"
    );
    let fixture_binding = binding(repo, FIXTURE)?;
    validate_fixture(&bound(repo, &fixture_binding)?)?;
    let p = json!({"schema":SCHEMA,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "source_bindings":names.iter().map(|n| binding(repo,n)).collect::<Result<Vec<_>>>()?,
        "changed_predecessor_sources":changed,"previous_protocol":previous_protocol,
        "previous_report":report_binding,"predecessor":binding(repo,&format!("{BASE}/dative-corpus-v1/final-checks-v1.json"))?,
        "fixture_manifest":fixture_binding,"fixture_builder_receipt":binding(repo,&format!("{BASE}/dative-arguments-v1/fixture-build-v1/receipt.json"))?,"fixture_cases":64,"positive_cases":48,"negative_cases":16,
        "argument_policy":POLICY,"parsers":previous["parsers"],"resource_manifest":previous["resource_manifest"],
        "prior_source_annotations":sources,"corpus_protocol":previous,
        "fixture_evaluation":"All frozen sources and every actual candidate under both parsers; exact byte, role, frame and reciprocal checks",
        "corpus_evaluation":"Descriptive replay of all 3889 reused TRAIN posts, including all zero and incomplete assessments; unchanged support policy",
        "no_result_conditioned_fixture_changes":true,"fit_models":false,"llm_calls":0,"detector_calls":0,
        "automatic_edit_license":false,"semantic_equivalence_certified":false});
    fs::create_dir(out)?;
    println!("{}", output(out, "protocol.json", &p)?);
    Ok(())
}

fn words(s: &str) -> Vec<String> {
    let mut w = features::words(s);
    w.sort();
    w
}
fn validate_fixture(f: &Value) -> Result<BTreeMap<String, String>> {
    ensure!(
        f["schema"] == "slopninja-dative-arguments-fixtures-v1",
        "Fixture schema differs"
    );
    let cases = rows(f, "cases")?;
    ensure!(cases.len() == 64, "Fixture count differs");
    let mut texts = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut positives = 0;
    for c in cases {
        let id = text(c, "id")?;
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                && ids.insert(id),
            "Unsafe/repeated fixture ID"
        );
        let source = text(c, "source_text")?;
        let sha = hash(source.as_bytes());
        ensure!(
            c["source_sha256"] == sha && texts.insert(sha, source.into()).is_none(),
            "Repeated/changed source"
        );
        ensure!(
            c["expected_proposal"].is_boolean(),
            "All fixture expectations must be fixed booleans"
        );
        for anchors in ["source_anchors", "candidate_anchors"] {
            let content = if anchors == "source_anchors" {
                source
            } else {
                c["candidate_text"].as_str().unwrap_or("")
            };
            if let Some(o) = c[anchors].as_object() {
                for a in o.values().filter(|a| !a.is_null()) {
                    let [start, end]: [usize; 2] = serde_json::from_value(a.clone())?;
                    ensure!(
                        start < end && content.get(start..end).is_some(),
                        "Invalid fixture anchor"
                    );
                }
            }
        }
        if c["expected_proposal"] == true {
            positives += 1;
            let candidate = text(c, "candidate_text")?;
            ensure!(
                c["candidate_sha256"] == hash(candidate.as_bytes()),
                "Counterpart hash differs"
            );
            let edits: Vec<edits::Edit> = serde_json::from_value(c["expected_edits"].clone())?;
            let inverse: Vec<edits::Edit> =
                serde_json::from_value(c["expected_inverse_edits"].clone())?;
            ensure!(
                edits.len() == 1
                    && inverse.len() == 1
                    && edits::apply(source, &edits)? == candidate
                    && edits::apply(candidate, &inverse)? == source,
                "Fixture patches differ"
            );
            let a = words(source);
            let b = words(candidate);
            let (mut longer, shorter) = if a.len() > b.len() { (a, b) } else { (b, a) };
            let index = longer
                .iter()
                .position(|w| w == "to")
                .context("Missing alternation to")?;
            longer.remove(index);
            ensure!(longer == shorter, "Changed words beyond one to");
            let reciprocal = cases
                .iter()
                .find(|r| r["id"] == c["reciprocal_case_id"])
                .context("Missing reciprocal fixture")?;
            ensure!(
                reciprocal["source_text"] == candidate
                    && reciprocal["candidate_text"] == source
                    && reciprocal["reciprocal_case_id"] == id,
                "Reciprocal pair differs"
            );
        }
    }
    ensure!(positives == 48, "Fixture allocation differs");
    Ok(texts)
}

fn expected(case: &Value, doc: &Document, p: &alternation::Proposal) -> bool {
    let head = &doc.tokens[p.predicate_index];
    let anchor = &case["source_anchors"];
    let roles = [
        ("agent", &p.roles.agent),
        ("theme", &p.roles.theme),
        ("recipient", &p.roles.recipient),
    ]
    .into_iter()
    .all(|(name, r)| {
        let h = &doc.tokens[r.head_index];
        anchor[format!("{name}_head_span")] == json!([h.start_byte, h.end_byte])
            && anchor[format!("{name}_np_span")] == json!(r.span)
    });
    anchor["predicate_span"] == json!([head.start_byte, head.end_byte])
        && roles
        && case["candidate_text"] == p.candidate.text
        && case["expected_edits"] == json!(p.candidate.edits)
        && case["expected_inverse_edits"] == json!(p.inverse_edits)
        && case["resource_expectation"]["member_class"] == p.member_class_id
        && case["resource_expectation"]["source_frame_id"] == p.source_frame_id
        && case["resource_expectation"]["target_frame_id"] == p.target_frame_id
}

fn annotations(
    repo: &Path,
    out: &Path,
    parser: &Value,
    texts: &BTreeMap<String, String>,
    docs: &mut BTreeMap<String, corpus::Parsed>,
    artifacts: &mut Vec<Value>,
) -> Result<()> {
    let missing = texts
        .iter()
        .filter(|(sha, _)| !docs.contains_key(*sha))
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect::<Vec<_>>();
    study::verify_parser(repo, parser)?;
    for batch in missing.chunks(64) {
        for ((sha, source), parsed) in batch.iter().zip(corpus::parse(repo, parser, batch)?) {
            let v = match &parsed {
                Ok(d) => json!({"status":"ok","document":d}),
                Err(e) => json!({"status":"error","source":source,"error":e}),
            };
            artifacts.push(output(
                out,
                &format!("fixtures/annotations/{}/{sha}.json", text(parser, "id")?),
                &v,
            )?);
            docs.insert(sha.clone(), parsed);
        }
    }
    study::verify_parser(repo, parser)?;
    Ok(())
}

fn fixtures(
    repo: &Path,
    out: &Path,
    p: &Value,
    resource: &Resource,
    artifacts: &mut Vec<Value>,
) -> Result<Value> {
    let manifest = bound(repo, &p["fixture_manifest"])?;
    let texts = validate_fixture(&manifest)?;
    let mut results = Vec::new();
    for parser in rows(p, "parsers")? {
        let variant = text(parser, "id")?;
        let mut docs = BTreeMap::new();
        annotations(repo, out, parser, &texts, &mut docs, artifacts)?;
        let mut pending = Vec::new();
        let mut candidates = BTreeMap::new();
        let mut cases = Vec::new();
        for case in rows(&manifest, "cases")? {
            match &docs[text(case,"source_sha256")?] {
                Ok(doc)=>{
                    let legacy=alternation::propose(doc,resource)?;
                    let current=alternation::propose_with_policy(doc,resource,POLICY)?;
                    for proposal in &current.proposals {
                        candidates.insert(hash(proposal.candidate.text.as_bytes()),proposal.candidate.text.clone());
                    }
                    pending.push((case.clone(),legacy,current));
                },
                Err(e)=>cases.push(json!({"case_id":case["id"],"category":case["category"],"expected_proposal":case["expected_proposal"],"status":"source_parse_error","error":e,"expectation_met":null})),
            }
        }
        annotations(repo, out, parser, &candidates, &mut docs, artifacts)?;
        for (case, legacy, current) in pending {
            let source = docs[text(&case, "source_sha256")?].as_ref().unwrap();
            let exact = current
                .proposals
                .iter()
                .filter(|s| expected(&case, source, s))
                .count();
            let met = if case["expected_proposal"] == true {
                exact > 0 && exact == current.proposals.len()
            } else {
                current.proposals.is_empty()
            };
            let mut comparisons = Vec::new();
            for proposal in &current.proposals {
                let candidate = &docs[&hash(proposal.candidate.text.as_bytes())];
                let result = match candidate {
                    Ok(d) => {
                        match corpus::match_pair_with_policy(source, d, proposal, resource, POLICY)
                        {
                            Ok(v) => v,
                            Err(e) => {
                                json!({"status":"comparison_error","error":format!("{e:#}"),"assessment_complete":false})
                            }
                        }
                    }
                    Err(e) => {
                        json!({"status":"candidate_parse_error","error":e,"assessment_complete":false})
                    }
                };
                comparisons.push(json!({"proposal_id":proposal.id,"matches_frozen_expectation":expected(&case,source,proposal),"result":result}));
            }
            let reciprocal = comparisons.iter().any(|c| {
                c["matches_frozen_expectation"] == true
                    && c["result"]["reciprocally_observed_preserved"] == true
            });
            let b = output(
                out,
                &format!("fixtures/cases/{variant}/{}.json", text(&case, "id")?),
                &json!({"case":case,"legacy":legacy,"current":current,"comparisons":comparisons,"expectation_met":met,"expected_reciprocal_observed_preserved":reciprocal}),
            )?;
            artifacts.push(b.clone());
            cases.push(json!({"case_id":case["id"],"category":case["category"],"expected_proposal":case["expected_proposal"],"status":"observed","legacy_proposals":legacy.proposals.len(),"current_proposals":current.proposals.len(),"expectation_met":met,"expected_reciprocal_observed_preserved":reciprocal,"report":b}));
        }
        let summary = json!({"variant":variant,"cases":cases,"annotations":docs.len(),"annotation_errors":docs.values().filter(|v| v.is_err()).count(),
            "positive_expectations_met":cases.iter().filter(|c|c["expected_proposal"]==true && c["expectation_met"]==true).count(),
            "negative_expectations_met":cases.iter().filter(|c|c["expected_proposal"]==false && c["expectation_met"]==true).count(),
            "positive_reciprocal_checks_passed":cases.iter().filter(|c|c["expected_proposal"]==true && c["expected_reciprocal_observed_preserved"]==true).count()});
        artifacts.push(output(
            out,
            &format!("fixtures/{variant}/report.json"),
            &summary,
        )?);
        println!(
            "{variant}: {} / 48 positive proposal expectations, {} / 48 reciprocal checks, {} / 16 abstentions",
            summary["positive_expectations_met"],
            summary["positive_reciprocal_checks_passed"],
            summary["negative_expectations_met"]
        );
        results.push(summary);
    }
    Ok(
        json!({"reports":results,"automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

fn run(repo: &Path, file: &Path, expected_sha: &str, out: &Path) -> Result<()> {
    let p: Value = serde_json::from_slice(&checked(file, expected_sha)?)?;
    ensure!(
        p["schema"] == SCHEMA
            && p["argument_policy"] == json!(POLICY)
            && p["executable_sha256"] == hash(&fs::read(std::env::current_exe()?)?),
        "Protocol/executable differs"
    );
    for b in rows(&p, "source_bindings")? {
        bound_bytes(repo, b)?;
    }
    ensure!(
        bound(repo, &p["predecessor"])?["stage_completed"] == true,
        "Predecessor stage incomplete"
    );
    fs::create_dir(out)?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![output(out, "protocol.json", &p)?];
        for b in rows(&p, "source_bindings")? {
            let name = format!("executed-source/{}", text(b, "path")?);
            let target = out.join(&name);
            fs::create_dir_all(target.parent().unwrap())?;
            let bytes = bound_bytes(repo, b)?;
            fs::write(target, &bytes)?;
            artifacts.push(json!({"path":name,"sha256":hash(&bytes)}));
        }
        let resource = study::resource(repo, &bound(repo, &p["resource_manifest"])?)?;
        let fixture_report = fixtures(repo, out, &p, &resource, &mut artifacts)?;
        artifacts.push(output(out, "fixtures/report.json", &fixture_report)?);
        let mut sources = BTreeMap::new();
        for b in rows(&p, "prior_source_annotations")? {
            let name = Path::new(text(b, "path")?)
                .file_stem()
                .context("Source annotation name")?
                .to_string_lossy()
                .into_owned();
            ensure!(
                sources.insert(name, b.clone()).is_none(),
                "Duplicate cached source"
            );
        }
        let corpus_out = out.join("corpus");
        fs::create_dir(&corpus_out)?;
        let mut corpus_artifacts = Vec::new();
        let mut corpus_report = corpus::execute_with_policy(
            repo,
            &corpus_out,
            &p["corpus_protocol"],
            &mut corpus_artifacts,
            POLICY,
            Some(&sources),
        )?;
        corpus_report["argument_policy"] = json!(POLICY);
        corpus_report["artifacts"] = json!(corpus_artifacts);
        let cb = output(&corpus_out, "report.json", &corpus_report)?;
        artifacts
            .push(json!({"path":format!("corpus/{}",text(&cb,"path")?),"sha256":cb["sha256"]}));
        let report = json!({"schema":"slopninja-dative-arguments-report-v1","protocol_sha256":expected_sha,"argument_policy":POLICY,"fixtures":fixture_report,"corpus_report":artifacts.last(),"artifacts":artifacts,
            "fit_models":false,"llm_calls":0,"detector_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false});
        let b = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"schema":"slopninja-dative-arguments-receipt-v1","status":"completed","protocol_sha256":expected_sha,"report":b}),
        )?;
        Ok(())
    })();
    if let Err(e) = &result {
        output(
            out,
            "failure.json",
            &json!({"status":"failed","error":format!("{e:#}")}),
        )?;
    }
    result
}
pub fn main() -> Result<()> {
    match Args::parse().command {
        Command::Freeze { repo, out } => freeze(&repo, &out),
        Command::Run {
            repo,
            protocol,
            expected_protocol_sha256,
            out,
        } => run(&repo, &protocol, &expected_protocol_sha256, &out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../fixtures/dative-arguments-v1/manifest.json"
        ))
        .unwrap()
    }
    #[test]
    fn validates_complete_frozen_fixture() {
        assert_eq!(validate_fixture(&fixture()).unwrap().len(), 64);
    }
    #[test]
    fn rejects_changed_inverse_and_missing_reciprocal() {
        let mut f = fixture();
        f["cases"][0]["expected_inverse_edits"][0]["replacement"] = json!("different participants");
        assert!(validate_fixture(&f).is_err());
        let mut f = fixture();
        f["cases"][0]["reciprocal_case_id"] = json!("absent");
        assert!(validate_fixture(&f).is_err());
    }
    #[test]
    fn rejects_duplicate_case_and_invalid_anchor() {
        let mut f = fixture();
        f["cases"][1]["id"] = f["cases"][0]["id"].clone();
        assert!(validate_fixture(&f).is_err());
        let mut f = fixture();
        f["cases"][0]["source_anchors"]["predicate_span"] = json!([0, 99999]);
        assert!(validate_fixture(&f).is_err());
    }
}
