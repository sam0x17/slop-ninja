//! Fixed synthetic parser comparison with unchanged rules and guard semantics.
use crate::{boundary_choice_observation as observation, claim_guard};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{edits, syntax::Document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

const VARIANTS: [&str; 4] = ["incumbent_sm", "isolated_sm", "isolated_md", "isolated_trf"];
type Parsed = std::result::Result<Document, String>;
type Documents = BTreeMap<String, Parsed>;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing {k}"))
}
fn rows<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array()
        .with_context(|| format!("Missing array {k}"))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid SHA256 binding"
    );
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(hash(&bytes) == expected, "Hash mismatch {}", path.display());
    Ok(bytes)
}
fn bound(repo: &Path, v: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(
        &repo.join(text(v, "path")?),
        text(v, "sha256")?,
    )?)?)
}
fn fresh(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(hash(&bytes))
}
fn output(out: &Path, name: &str, value: &Value) -> Result<Value> {
    let path = out.join(name);
    fs::create_dir_all(path.parent().context("Output parent")?)?;
    Ok(json!({"path":name,"sha256":fresh(&path,value)?}))
}
fn safe_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "Unsafe artifact ID"
    );
    Ok(())
}

fn validate_fixture(binding: &Value, manifest: &Value) -> Result<()> {
    let id = text(binding, "id")?;
    safe_id(id)?;
    ensure!(manifest["id"] == id, "Fixture ID differs");
    let cases = rows(manifest, "cases")?;
    ensure!(
        cases.len()
            == binding["expected_cases"]
                .as_u64()
                .context("Expected cases")? as usize,
        "Fixture case count differs"
    );
    let mut by_id = BTreeMap::new();
    let mut pairs = BTreeMap::<String, Vec<&Value>>::new();
    for case in cases {
        let case_id = text(case, "id")?;
        safe_id(case_id)?;
        safe_id(text(case, "pair_id")?)?;
        ensure!(
            by_id.insert(case_id, case).is_none(),
            "Repeated fixture case ID"
        );
        ensure!(
            ["period", "semicolon", "complement"].contains(&text(case, "observed_form")?),
            "Unsupported fixture form"
        );
        ensure!(
            case["expected_rule_candidate"].is_boolean()
                || case["expected_rule_candidate"].is_null(),
            "Rule expectation must be bool or null"
        );
        ensure!(
            case["guard_must_reject"].is_boolean() || case.get("guard_must_reject").is_none(),
            "Guard requirement must be bool when supplied"
        );
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            !patches.is_empty()
                && text(case, "source_text")? != text(case, "candidate_text")?
                && edits::apply(text(case, "source_text")?, &patches)? == case["candidate_text"],
            "Invalid declared fixture patches"
        );
        pairs
            .entry(text(case, "pair_id")?.into())
            .or_default()
            .push(case);
    }
    ensure!(
        pairs.len()
            == binding["expected_pairs"]
                .as_u64()
                .context("Expected pairs")? as usize
            && pairs.values().all(|p| p.len() == 2),
        "Fixture pair counts differ"
    );
    for case in cases {
        let inverse = by_id
            .get(text(case, "inverse_case_id")?)
            .context("Missing inverse case")?;
        ensure!(
            inverse["id"] != case["id"]
                && inverse["inverse_case_id"] == case["id"]
                && inverse["pair_id"] == case["pair_id"]
                && inverse["source_text"] == case["candidate_text"]
                && inverse["candidate_text"] == case["source_text"],
            "Fixture inverse identity differs"
        );
    }
    Ok(())
}

fn evaluate_case(case: &Value, source: &Parsed, candidate: &Parsed) -> Result<Value> {
    let observed = source.as_ref().ok().map(observation::observe).transpose()?;
    let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
    let mut matches = Vec::new();
    if let Some(observed) = &observed {
        for opportunity in &observed.opportunities {
            if opportunity.counterpart.text != case["candidate_text"] {
                continue;
            }
            let pair = match (source, candidate) {
                (Ok(a), Ok(b)) => Some(observation::evaluate_counterpart(a, opportunity, b)?),
                _ => None,
            };
            matches.push(json!({"opportunity_id":opportunity.id,"declared_patch_match":opportunity.counterpart.edits==patches,"pair":pair,"pair_unavailable":candidate.is_err()}));
        }
    }
    let emitted = observed.as_ref().map(|_| !matches.is_empty());
    let patch_emitted = observed
        .as_ref()
        .map(|_| matches.iter().any(|m| m["declared_patch_match"] == true));
    let guard = match (source, candidate) {
        (Ok(a), Ok(b)) => Some(claim_guard::compare(a, b, &patches)?),
        _ => None,
    };
    let rule_expectation = case["expected_rule_candidate"].as_bool();
    let must_reject = case["guard_must_reject"].as_bool().unwrap_or(false);
    let violation = if must_reject {
        guard
            .as_ref()
            .map(|g| g.outcome == claim_guard::Outcome::ObservedPreserved)
    } else {
        None
    };
    let paired_eligible = matches.iter().any(|m| m["pair"]["eligible"] == true);
    let declared_patch_paired_eligible = matches
        .iter()
        .any(|m| m["declared_patch_match"] == true && m["pair"]["eligible"] == true);
    Ok(
        json!({"case":case,"status":if source.is_ok() && candidate.is_ok(){"evaluated"}else{"parse_error"},"source_parse_status":if source.is_ok(){"ok"}else{"error"},"candidate_parse_status":if candidate.is_ok(){"ok"}else{"error"},"source_error":source.as_ref().err(),"candidate_error":candidate.as_ref().err(),"observation":observed,"exact_text_candidate_emitted":emitted,"exact_patch_candidate_emitted":patch_emitted,"rule_expectation_met":rule_expectation.zip(emitted).map(|(expected,actual)|expected==actual),"text_matches":matches,"desired_pair_guard":guard,"guard_must_reject":must_reject,"guard_rejection_expectation_violated":violation,"paired_eligible":paired_eligible,"declared_patch_paired_eligible":declared_patch_paired_eligible,"semantic_equivalence_certified":false}),
    )
}

const COUNTS: &[&str] = &[
    "cases",
    "source_parse_errors",
    "candidate_parse_errors",
    "unavailable_directions",
    "evaluated_directions",
    "raw_sites",
    "all_rule_proposals",
    "cases_with_any_proposal",
    "cases_with_exact_text_candidate",
    "cases_with_exact_patch_candidate",
    "exact_text_matches",
    "exact_patch_matches",
    "parsed_exact_matches",
    "no_exact_inverse",
    "guard_rejected",
    "eligible_matches",
    "eligible_directions",
    "declared_patch_eligible_directions",
    "restoring_inverse_matches",
    "passed_inverse_matches",
    "inverse_proposals",
    "nonrestoring_inverse_proposals",
    "rule_expectations_true",
    "rule_expectations_false",
    "rule_expectations_null",
    "rule_expectations_unavailable",
    "rule_expectations_met",
    "rule_expectations_mismatched",
    "no_proposal_expectation_mismatches",
    "desired_guard_observed_preserved",
    "desired_guard_changed",
    "desired_guard_unresolved",
    "desired_guard_unavailable",
    "desired_boundary_rule_unrecognized",
    "forward_guard_observed_preserved",
    "forward_guard_changed",
    "forward_guard_unresolved",
    "forward_boundary_rule_unrecognized",
    "reverse_guard_observed_preserved",
    "reverse_guard_changed",
    "reverse_guard_unresolved",
    "reverse_boundary_rule_unrecognized",
    "guard_must_reject_assertions",
    "guard_must_reject_unavailable",
    "guard_must_reject_violations",
];
fn add(c: &mut BTreeMap<String, usize>, key: &str, n: usize) -> Result<()> {
    *c.get_mut(key)
        .with_context(|| format!("Unknown count {key}"))? += n;
    Ok(())
}
fn summarize(cases: &[&Value]) -> Result<Value> {
    let mut c = COUNTS
        .iter()
        .map(|k| ((*k).to_owned(), 0usize))
        .collect::<BTreeMap<_, _>>();
    for r in cases {
        add(&mut c, "cases", 1)?;
        add(
            &mut c,
            "source_parse_errors",
            usize::from(r["source_parse_status"] == "error"),
        )?;
        add(
            &mut c,
            "candidate_parse_errors",
            usize::from(r["candidate_parse_status"] == "error"),
        )?;
        add(
            &mut c,
            if r["status"] == "evaluated" {
                "evaluated_directions"
            } else {
                "unavailable_directions"
            },
            1,
        )?;
        if !r["observation"].is_null() {
            let obs = &r["observation"];
            add(&mut c, "raw_sites", rows(obs, "sites")?.len())?;
            let n = rows(obs, "opportunities")?.len();
            add(&mut c, "all_rule_proposals", n)?;
            add(&mut c, "cases_with_any_proposal", usize::from(n > 0))?;
        }
        for (field, key) in [
            (
                "exact_text_candidate_emitted",
                "cases_with_exact_text_candidate",
            ),
            (
                "exact_patch_candidate_emitted",
                "cases_with_exact_patch_candidate",
            ),
            ("paired_eligible", "eligible_directions"),
            (
                "declared_patch_paired_eligible",
                "declared_patch_eligible_directions",
            ),
        ] {
            add(&mut c, key, usize::from(r[field] == true))?;
        }
        let expected = &r["case"]["expected_rule_candidate"];
        add(
            &mut c,
            if expected == true {
                "rule_expectations_true"
            } else if expected == false {
                "rule_expectations_false"
            } else {
                "rule_expectations_null"
            },
            1,
        )?;
        if expected.is_boolean() {
            add(
                &mut c,
                if r["rule_expectation_met"] == true {
                    "rule_expectations_met"
                } else if r["rule_expectation_met"] == false {
                    "rule_expectations_mismatched"
                } else {
                    "rule_expectations_unavailable"
                },
                1,
            )?;
            add(
                &mut c,
                "no_proposal_expectation_mismatches",
                usize::from(expected == false && r["rule_expectation_met"] == false),
            )?;
        }
        if r["desired_pair_guard"].is_null() {
            add(&mut c, "desired_guard_unavailable", 1)?;
        } else {
            add(
                &mut c,
                &format!(
                    "desired_guard_{}",
                    text(&r["desired_pair_guard"], "outcome")?
                ),
                1,
            )?;
            add(
                &mut c,
                "desired_boundary_rule_unrecognized",
                usize::from(r["desired_pair_guard"]["licensed_boundary_rule"].is_null()),
            )?;
        }
        if r["guard_must_reject"] == true {
            add(&mut c, "guard_must_reject_assertions", 1)?;
            add(
                &mut c,
                "guard_must_reject_unavailable",
                usize::from(r["guard_rejection_expectation_violated"].is_null()),
            )?;
            add(
                &mut c,
                "guard_must_reject_violations",
                usize::from(r["guard_rejection_expectation_violated"] == true),
            )?;
        }
        for m in rows(r, "text_matches")? {
            add(&mut c, "exact_text_matches", 1)?;
            add(
                &mut c,
                "exact_patch_matches",
                usize::from(m["declared_patch_match"] == true),
            )?;
            let p = &m["pair"];
            if p.is_null() {
                continue;
            }
            add(&mut c, "parsed_exact_matches", 1)?;
            add(
                &mut c,
                if p["status"] == "eligible" {
                    "eligible_matches"
                } else {
                    text(p, "status")?
                },
                1,
            )?;
            add(
                &mut c,
                &format!("forward_guard_{}", text(&p["forward_guard"], "outcome")?),
                1,
            )?;
            add(
                &mut c,
                "forward_boundary_rule_unrecognized",
                usize::from(p["forward_guard"]["licensed_boundary_rule"].is_null()),
            )?;
            for (field, key) in [
                ("passed_inverse_matches", "passed_inverse_matches"),
                ("inverse_proposal_count", "inverse_proposals"),
                (
                    "inverse_nonrestoring_count",
                    "nonrestoring_inverse_proposals",
                ),
            ] {
                add(
                    &mut c,
                    key,
                    p[field].as_u64().context("Inverse count")? as usize,
                )?;
            }
            for inverse in rows(p, "inverse_matches")? {
                add(&mut c, "restoring_inverse_matches", 1)?;
                add(
                    &mut c,
                    &format!(
                        "reverse_guard_{}",
                        text(&inverse["reverse_guard"], "outcome")?
                    ),
                    1,
                )?;
                add(
                    &mut c,
                    "reverse_boundary_rule_unrecognized",
                    usize::from(inverse["reverse_guard"]["licensed_boundary_rule"].is_null()),
                )?;
            }
        }
    }
    Ok(json!(c))
}

fn pair_rows(cases: &[Value]) -> Result<Vec<Value>> {
    let mut pairs = BTreeMap::<String, Vec<&Value>>::new();
    for r in cases {
        pairs
            .entry(text(&r["case"], "pair_id")?.into())
            .or_default()
            .push(r);
    }
    pairs.into_iter().map(|(id,rows)|{
        ensure!(rows.len()==2,"Paired result coverage differs");
        let eligible=rows.iter().filter(|r|r["paired_eligible"]==true).count();
        let exact=rows.iter().filter(|r|r["declared_patch_paired_eligible"]==true).count();
        let unavailable=rows.iter().filter(|r|r["status"]!="evaluated").count();
        Ok(json!({"pair_id":id,"case_ids":rows.iter().map(|r|r["case"]["id"].clone()).collect::<Vec<_>>(),"eligible_directions":eligible,"declared_patch_eligible_directions":exact,"unavailable_directions":unavailable,"both_directions_eligible":eligible==2,"both_declared_patch_directions_eligible":exact==2,"eligibility_count_label":match eligible{2=>"both",1=>"one",_=>"neither"},"all_directions_available":unavailable==0}))
    }).collect()
}

fn legacy_projection(r: &Value) -> Value {
    if r["status"] == "evaluated" {
        json!({"case":r["case"],"status":"evaluated","observation":r["observation"],"exact_candidate_emitted":r["exact_text_candidate_emitted"],"rule_expectation_met":r["rule_expectation_met"],"exact_pairs":r["text_matches"].as_array().unwrap().iter().map(|m|m["pair"].clone()).collect::<Vec<_>>(),"desired_pair_guard":r["desired_pair_guard"]})
    } else {
        json!({"case":r["case"],"status":"parse_error","source_error":r["source_error"],"candidate_error":r["candidate_error"]})
    }
}

fn summarize_pairs(variant: &str, fixture: &str, pairs: &[Value]) -> Value {
    json!({"variant":variant,"fixture":fixture,"pairs":pairs.len(),"both_eligible":pairs.iter().filter(|r|r["eligible_directions"]==2).count(),"one_eligible":pairs.iter().filter(|r|r["eligible_directions"]==1).count(),"neither_eligible":pairs.iter().filter(|r|r["eligible_directions"]==0).count(),"neither_eligible_and_both_available":pairs.iter().filter(|r|r["eligible_directions"]==0 && r["unavailable_directions"]==0).count(),"with_unavailable_direction":pairs.iter().filter(|r|r["unavailable_directions"]!=0).count(),"both_declared_patch_eligible":pairs.iter().filter(|r|r["declared_patch_eligible_directions"]==2).count()})
}

fn baseline_check(
    repo: &Path,
    p: &Value,
    fixture: &Value,
    results: &[Value],
    docs: &Documents,
) -> Result<Value> {
    let b = &p["baseline"];
    let previous = bound(repo, &b["synthetic"])?;
    let previous = previous.as_array().context("Prior synthetic array")?;
    let mut previous_by_id = BTreeMap::new();
    for r in previous {
        ensure!(
            previous_by_id.insert(text(&r["case"], "id")?, r).is_none(),
            "Duplicate prior case"
        );
    }
    let mut differences = Vec::new();
    for r in results {
        let id = text(&r["case"], "id")?;
        let old = previous_by_id.get(id).context("Prior case missing")?;
        if legacy_projection(r) != **old {
            differences.push(json!({"kind":"synthetic_projection","case_id":id}));
        }
    }
    ensure!(
        results.len() == previous.len(),
        "Baseline prior case count differs"
    );
    let expected_texts = rows(fixture, "cases")?
        .iter()
        .flat_map(|c| {
            [
                c["source_text"].as_str().unwrap(),
                c["candidate_text"].as_str().unwrap(),
            ]
        })
        .map(|s| hash(s.as_bytes()))
        .collect::<BTreeSet<_>>();
    let mut prior_texts = BTreeSet::new();
    for binding in rows(b, "annotations")? {
        let old = bound(repo, binding)?;
        let (sha, parsed) = if old["status"] == "ok" {
            let doc: Document = serde_json::from_value(old["document"].clone())?;
            grammar_core::syntax::validate(&doc)?;
            (hash(doc.text.as_bytes()), Ok(doc))
        } else {
            ensure!(old["status"] == "error", "Unknown prior annotation status");
            (
                hash(text(&old, "text")?.as_bytes()),
                Err(text(&old, "error")?.to_owned()),
            )
        };
        ensure!(
            prior_texts.insert(sha.clone()),
            "Duplicate prior annotation text"
        );
        if docs
            .get(&sha)
            .context("Prior text absent from parser run")?
            != &parsed
        {
            differences.push(json!({"kind":"canonical_annotation","source_sha256":sha}));
        }
    }
    ensure!(
        expected_texts == prior_texts,
        "Baseline annotation scope differs"
    );
    Ok(
        json!({"status":if differences.is_empty(){"pass"}else{"mismatch"},"variant_id":b["variant_id"],"fixture_id":b["fixture_id"],"canonical_annotations_compared":prior_texts.len(),"historical_synthetic_rows_compared":results.len(),"differences":differences,"historical_emission_definition":"exact target text; patch equality additionally reported in the new probe"}),
    )
}

fn environment_comparison(incumbent: &Documents, isolated: &Documents) -> Result<Value> {
    ensure!(
        incumbent.keys().eq(isolated.keys()),
        "Small-parser text coverage differs"
    );
    let mut comparisons = Vec::new();
    for (sha, a) in incumbent {
        let b = &isolated[sha];
        let mut av = serde_json::to_value(a)?;
        let mut bv = serde_json::to_value(b)?;
        if let Some(d) = av.get_mut("Ok") {
            d.as_object_mut().unwrap().remove("parser_identity");
        }
        if let Some(d) = bv.get_mut("Ok") {
            d.as_object_mut().unwrap().remove("parser_identity");
        }
        comparisons.push(json!({"source_sha256":sha,"incumbent_ok":a.is_ok(),"isolated_ok":b.is_ok(),"canonical_annotation_including_identity_equal":a==b,"content_excluding_parser_identity_equal":av==bv}));
    }
    Ok(
        json!({"variants":["incumbent_sm","isolated_sm"],"texts":comparisons.len(),"both_parsed":comparisons.iter().filter(|r|r["incumbent_ok"]==true && r["isolated_ok"]==true).count(),"canonical_equal":comparisons.iter().filter(|r|r["canonical_annotation_including_identity_equal"]==true).count(),"content_equal_excluding_identity":comparisons.iter().filter(|r|r["content_excluding_parser_identity_equal"]==true).count(),"rows":comparisons,"interpretation":"Same named small model across the declared environments; differences are retained, not treated as accuracy judgments. Equality counts compare returned Results, including identical error strings; both_parsed and per-text statuses disclose successful annotation support."}),
    )
}

fn run(args: &Args, p: &Value) -> Result<()> {
    let mut fixtures = Vec::new();
    let mut requested = BTreeMap::new();
    for binding in rows(p, "fixture_manifests")? {
        let fixture = bound(&args.repo, binding)?;
        validate_fixture(binding, &fixture)?;
        for c in rows(&fixture, "cases")? {
            for key in ["source_text", "candidate_text"] {
                let source = text(c, key)?;
                let sha = hash(source.as_bytes());
                if let Some(old) = requested.insert(sha, source.to_owned()) {
                    ensure!(old == source, "Text hash collision");
                }
            }
        }
        ensure!(
            !fixtures.iter().any(|f: &Value| f["id"] == fixture["id"]),
            "Duplicate fixture ID"
        );
        fixtures.push(fixture);
    }
    ensure!(
        !fixtures.is_empty() && !requested.is_empty(),
        "Empty fixture scope"
    );
    let variants = rows(p, "variants")?;
    ensure!(
        variants
            .iter()
            .map(|v| text(v, "id"))
            .collect::<Result<Vec<_>>>()?
            == VARIANTS,
        "Parser order or variants differ"
    );
    ensure!(
        p["baseline"]["variant_id"] == "incumbent_sm",
        "Baseline variant differs"
    );
    for variant in variants {
        safe_id(text(variant, "id")?)?;
        let wrapper = &variant["parser_python"];
        checked(
            &args.repo.join(text(wrapper, "path")?),
            text(wrapper, "sha256")?,
        )?;
        bound(&args.repo, &variant["environment_manifest"])?;
        for asset in rows(variant, "asset_bindings")? {
            checked(
                &args.repo.join(text(asset, "path")?),
                text(asset, "sha256")?,
            )?;
        }
        ensure!(
            text(variant, "parser_identity")?.starts_with("grammar-spacy-annotation-v1;")
                && text(variant, "parser_identity")?.ends_with(";raw-text-v1"),
            "Malformed expected parser identity"
        );
    }
    let mut artifacts = Vec::new();
    let mut summaries = Vec::new();
    let mut pair_summaries = Vec::new();
    let mut all_docs = BTreeMap::new();
    let mut timing = Vec::new();
    for variant in variants {
        let id = text(variant, "id")?;
        let mut docs = Documents::new();
        let mut annotations = Vec::new();
        let mut batches = Vec::new();
        let start = Instant::now();
        let ordered = requested.iter().collect::<Vec<_>>();
        for (batch_index, batch) in ordered.chunks(64).enumerate() {
            let inputs = batch.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>();
            let began = Instant::now();
            let parsed = grammar_spacy::parse_batch(
                &inputs,
                args.repo
                    .join(text(&variant["parser_python"], "path")?)
                    .to_str()
                    .context("Parser path UTF8")?,
            )?;
            let seconds = began.elapsed().as_secs_f64();
            ensure!(parsed.len() == batch.len(), "Batch count differs");
            batches.push(json!({"batch_index":batch_index,"documents":batch.len(),"source_sha256":batch.iter().map(|(sha,_)|*sha).collect::<Vec<_>>(),"parse_seconds":seconds}));
            for ((sha, source), result) in batch.iter().zip(parsed) {
                if let Ok(doc) = &result {
                    ensure!(
                        &doc.text == *source && doc.parser_identity == variant["parser_identity"],
                        "Exact source or parser identity differs"
                    );
                }
                let record = match &result {
                    Ok(doc) => json!({"status":"ok","document":doc}),
                    Err(error) => json!({"status":"error","text":source,"error":error}),
                };
                annotations.push(output(
                    &args.out,
                    &format!("{id}/annotations/{sha}.json"),
                    &record,
                )?);
                ensure!(
                    docs.insert((*sha).clone(), result).is_none(),
                    "Repeated variant text"
                );
            }
        }
        let parse_seconds = start.elapsed().as_secs_f64();
        artifacts.push(output(&args.out,&format!("{id}/annotation-manifest.json"),&json!({"variant":variant,"text_order":"lexical SHA256","documents":annotations,"batches":batches,"parse_and_annotation_write_seconds":parse_seconds}))?);
        let evaluation_start = Instant::now();
        for fixture in &fixtures {
            let fixture_id = text(fixture, "id")?;
            let mut results = Vec::new();
            for case in rows(fixture, "cases")? {
                let a = &docs[&hash(text(case, "source_text")?.as_bytes())];
                let b = &docs[&hash(text(case, "candidate_text")?.as_bytes())];
                results.push(evaluate_case(case, a, b)?);
            }
            let pairs = pair_rows(&results)?;
            artifacts.push(output(
                &args.out,
                &format!("{id}/{fixture_id}-cases.json"),
                &json!(results),
            )?);
            artifacts.push(output(
                &args.out,
                &format!("{id}/{fixture_id}-pairs.json"),
                &json!(pairs),
            )?);
            let forms = rows(fixture, "cases")?
                .iter()
                .map(|c| text(c, "observed_form"))
                .collect::<Result<BTreeSet<_>>>()?;
            for form in forms {
                let subset = results
                    .iter()
                    .filter(|r| r["case"]["observed_form"] == form)
                    .collect::<Vec<_>>();
                summaries.push(json!({"variant":id,"fixture":fixture_id,"form":form,"counts":summarize(&subset)?}));
            }
            pair_summaries.push(summarize_pairs(id, fixture_id, &pairs));
            if id == "incumbent_sm" && p["baseline"]["fixture_id"] == fixture_id {
                let check = baseline_check(&args.repo, p, fixture, &results, &docs)?;
                artifacts.push(output(&args.out, "baseline-check.json", &check)?);
                ensure!(
                    check["status"] == "pass",
                    "Incumbent baseline differs; retained baseline-check.json"
                );
            }
        }
        timing.push(json!({"variant":id,"parse_and_annotation_write_seconds":parse_seconds,"evaluation_seconds":evaluation_start.elapsed().as_secs_f64()}));
        println!(
            "Completed parser variant {id}: {} exact fixture texts",
            docs.len()
        );
        all_docs.insert(id.to_owned(), docs);
    }
    ensure!(
        args.out.join("baseline-check.json").exists(),
        "Baseline fixture was not evaluated"
    );
    artifacts.push(output(
        &args.out,
        "small-model-environment-control.json",
        &environment_comparison(&all_docs["incumbent_sm"], &all_docs["isolated_sm"])?,
    )?);
    let report = json!({"schema":"slopninja-parser-variants-v1","status":"complete","protocol_sha256":args.expected_protocol_sha256,"unique_fixture_texts":requested.len(),"variant_order":VARIANTS,"summary_rows":summaries,"pair_summaries":pair_summaries,"timing":timing,"artifacts":artifacts,"interpretation":"Fixed synthetic parser sensitivity and compatibility probe. No accuracy, semantic-equivalence, readability, preference or detector certification.","training_corpus_reads":0,"model_fit_calls":0,"inference_logit_calls":0,"semantic_equivalence_certified":false});
    let report_sha = fresh(&args.out.join("report.json"), &report)?;
    fresh(
        &args.out.join("receipt.json"),
        &json!({"schema":"slopninja-parser-variants-receipt-v1","protocol_sha256":args.expected_protocol_sha256,"report_sha256":report_sha,"source_files":p["source_files"],"fixture_manifests":p["fixture_manifests"],"variants":p["variants"],"executable_sha256":hash(&fs::read(std::env::current_exe()?)?)}),
    )?;
    Ok(())
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "Use a fresh output directory; retain prior attempts"
    );
    let bytes = checked(&args.protocol, &args.expected_protocol_sha256)?;
    let p: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        p["schema"] == "slopninja-parser-variants-protocol-v1"
            && p["parser_batch_size"] == 64
            && p["training_corpus_reads"] == 0
            && p["model_fit_calls"] == 0
            && p["inference_logit_calls"] == 0,
        "Protocol design differs"
    );
    for (name, sha) in p["source_files"].as_object().context("Source bindings")? {
        checked(&args.repo.join(name), sha.as_str().context("Source hash")?)?;
    }
    fs::create_dir_all(&args.out)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.out.join("protocol.json"))?
        .write_all(&bytes)?;
    for name in p["source_files"].as_object().unwrap().keys() {
        let to = args.out.join("executed-source").join(name);
        fs::create_dir_all(to.parent().unwrap())?;
        fs::copy(args.repo.join(name), to)?;
    }
    let result = run(&args, &p);
    if let Err(error) = &result {
        fresh(
            &args.out.join("failure.json"),
            &json!({"status":"failed; retained attempt","error":format!("{error:#}")}),
        )?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::{Sentence, Token};

    fn doc(text: &str, items: &[(&str, &str, &str, usize, &str)], ends: &[usize]) -> Document {
        let mut tokens = Vec::new();
        let mut cursor = 0;
        for (i, &(word, pos, dep, head, tag)) in items.iter().enumerate() {
            let start_byte = cursor + text[cursor..].find(word).unwrap();
            let end_byte = start_byte + word.len();
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
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: pos == "PUNCT",
                is_space: false,
            });
            cursor = end_byte;
        }
        let mut start = 0;
        let mut sentences = Vec::new();
        for &end in ends {
            for t in &mut tokens[start..end] {
                t.sentence = sentences.len();
            }
            sentences.push(Sentence {
                start_byte: tokens[start].start_byte,
                end_byte: tokens[end - 1].end_byte,
                token_start: start,
                token_end: end,
                root: tokens[start..end].iter().find(|t| t.i == t.head).unwrap().i,
            });
            start = end;
        }
        let doc = Document {
            text: text.into(),
            parser_identity: "parser-variant-synthetic-v1".into(),
            tokens,
            sentences,
        };
        grammar_core::syntax::validate(&doc).unwrap();
        doc
    }
    fn period() -> Document {
        doc(
            "She reads. He writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
                ("He", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "ROOT", 4, "VBZ"),
                (".", "PUNCT", "punct", 4, "."),
            ],
            &[3, 6],
        )
    }
    fn semicolon() -> Document {
        doc(
            "She reads; he writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (";", "PUNCT", "punct", 1, ":"),
                ("he", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "conj", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[6],
        )
    }
    fn case(source: &Document, candidate: &Document, form: &str, id: &str, inverse: &str) -> Value {
        let emitted = observation::observe(source)
            .unwrap()
            .opportunities
            .into_iter()
            .find(|o| o.counterpart.text == candidate.text)
            .unwrap();
        json!({"id":id,"pair_id":"pair","inverse_case_id":inverse,"observed_form":form,"source_text":source.text,"candidate_text":candidate.text,"edits":emitted.counterpart.edits,"expected_rule_candidate":null})
    }
    fn reciprocal_fixture() -> (Value, Value) {
        let binding = json!({"id":"synthetic","expected_cases":2,"expected_pairs":1});
        let fixture = json!({"id":"synthetic","cases":[case(&period(),&semicolon(),"period","period","semicolon"),case(&semicolon(),&period(),"semicolon","semicolon","period")]});
        (binding, fixture)
    }

    #[test]
    fn fixture_validation_checks_full_bytes_reciprocals_and_nullable_expectations() {
        let (binding, fixture) = reciprocal_fixture();
        validate_fixture(&binding, &fixture).unwrap();
        let mut changed = fixture.clone();
        changed["cases"][0]["observed_form"] = json!("complement");
        changed["cases"][0]["guard_must_reject"] = json!(true);
        validate_fixture(&binding, &changed).unwrap();
        changed["cases"][0]["expected_rule_candidate"] = json!("false");
        assert!(validate_fixture(&binding, &changed).is_err());
        let mut changed = fixture.clone();
        changed["cases"][0]["inverse_case_id"] = json!("period");
        assert!(validate_fixture(&binding, &changed).is_err());
        let mut changed = fixture.clone();
        changed["cases"][0]["edits"][0]["expected"] = json!("wrong");
        assert!(validate_fixture(&binding, &changed).is_err());
        let mut changed = fixture;
        changed["cases"][0]["id"] = json!("../bad");
        assert!(validate_fixture(&binding, &changed).is_err());
    }

    #[test]
    fn exact_text_and_declared_patch_emission_are_separate() {
        let a = period();
        let b = semicolon();
        let mut c = case(&a, &b, "period", "period", "semicolon");
        c["edits"] = json!([{"start_byte":9,"end_byte":13,"expected":". He","replacement":"; he"}]);
        let row = evaluate_case(&c, &Ok(a), &Ok(b)).unwrap();
        assert_eq!(row["exact_text_candidate_emitted"], true);
        assert_eq!(row["exact_patch_candidate_emitted"], false);
        assert_eq!(row["paired_eligible"], true);
        assert_eq!(row["declared_patch_paired_eligible"], false);
        assert!(!row["desired_pair_guard"].is_null());
        assert!(row["rule_expectation_met"].is_null());
        assert!(row["guard_rejection_expectation_violated"].is_null());
        let counts = summarize(&[&row]).unwrap();
        assert_eq!(counts["exact_text_matches"], 1);
        assert_eq!(counts["exact_patch_matches"], 0);
        assert_eq!(counts["rule_expectations_null"], 1);
        assert_eq!(counts["rule_expectations_met"], 0);
    }

    #[test]
    fn partial_parse_errors_keep_source_emissions_but_not_guard_or_inverse_certainty() {
        let a = period();
        let b = semicolon();
        let mut c = case(&a, &b, "period", "period", "semicolon");
        c["expected_rule_candidate"] = json!(true);
        c["guard_must_reject"] = json!(true);
        let row = evaluate_case(&c, &Ok(a.clone()), &Err("synthetic failure".into())).unwrap();
        assert_eq!(row["status"], "parse_error");
        assert!(!row["observation"].is_null());
        assert_eq!(row["exact_text_candidate_emitted"], true);
        assert_eq!(row["rule_expectation_met"], true);
        assert!(row["text_matches"][0]["pair"].is_null());
        assert!(row["desired_pair_guard"].is_null());
        assert!(row["guard_rejection_expectation_violated"].is_null());
        assert_eq!(row["paired_eligible"], false);
        let counts = summarize(&[&row]).unwrap();
        assert_eq!(counts["unavailable_directions"], 1);
        assert_eq!(counts["guard_must_reject_unavailable"], 1);
        assert_eq!(counts["guard_must_reject_violations"], 0);
        assert_eq!(counts["parsed_exact_matches"], 0);
        let missing = evaluate_case(&c, &Err("source failed".into()), &Ok(b)).unwrap();
        assert!(missing["observation"].is_null());
        assert!(missing["exact_text_candidate_emitted"].is_null());
        assert!(missing["rule_expectation_met"].is_null());
    }

    #[test]
    fn direct_guard_is_retained_without_proposal_and_must_reject_is_one_sided() {
        let a = doc(
            "She said he writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("said", "VERB", "ROOT", 1, "VBD"),
                ("he", "PRON", "nsubj", 3, "PRP"),
                ("writes", "VERB", "ccomp", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[5],
        );
        let b = doc(
            "She said; he writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("said", "VERB", "ROOT", 1, "VBD"),
                (";", "PUNCT", "punct", 1, ":"),
                ("he", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "conj", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[6],
        );
        let c = json!({"id":"complement","pair_id":"attribution","observed_form":"complement","source_text":a.text,"candidate_text":b.text,"edits":[{"start_byte":8,"end_byte":8,"expected":"","replacement":";"}],"expected_rule_candidate":false,"guard_must_reject":true});
        let row = evaluate_case(&c, &Ok(a), &Ok(b)).unwrap();
        assert_eq!(row["exact_text_candidate_emitted"], false);
        assert_eq!(row["rule_expectation_met"], true);
        assert!(!row["desired_pair_guard"].is_null());
        assert_eq!(row["guard_rejection_expectation_violated"], false);
        let a = period();
        let b = semicolon();
        let mut c = case(&a, &b, "period", "period", "semicolon");
        c["guard_must_reject"] = json!(true);
        let row = evaluate_case(&c, &Ok(a), &Ok(b)).unwrap();
        assert_eq!(row["guard_rejection_expectation_violated"], true);
        assert_eq!(
            summarize(&[&row]).unwrap()["guard_must_reject_violations"],
            1
        );
    }

    #[test]
    fn paired_counts_keep_unavailable_directions_separate_from_eligible_count() {
        let a = period();
        let b = semicolon();
        let c = case(&a, &b, "period", "period", "semicolon");
        let inverse = case(&b, &a, "semicolon", "semicolon", "period");
        let forward = evaluate_case(&c, &Ok(a.clone()), &Ok(b.clone())).unwrap();
        let reverse = evaluate_case(&inverse, &Ok(b), &Err("unavailable".into())).unwrap();
        let pairs = pair_rows(&[forward, reverse]).unwrap();
        assert_eq!(pairs[0]["eligible_directions"], 1);
        assert_eq!(pairs[0]["unavailable_directions"], 1);
        assert_eq!(pairs[0]["eligibility_count_label"], "one");
        assert_eq!(pairs[0]["both_directions_eligible"], false);
        let unavailable = json!({"eligible_directions":0,"unavailable_directions":1,"declared_patch_eligible_directions":0});
        let rejected = json!({"eligible_directions":0,"unavailable_directions":0,"declared_patch_eligible_directions":0});
        let summary = summarize_pairs("synthetic", "fixture", &[unavailable, rejected]);
        assert_eq!(summary["neither_eligible"], 2);
        assert_eq!(summary["neither_eligible_and_both_available"], 1);
        assert_eq!(summary["with_unavailable_direction"], 1);
    }

    #[test]
    fn environment_control_separates_identity_from_annotation_content() {
        let a = period();
        let sha = hash(a.text.as_bytes());
        let mut b = a.clone();
        b.parser_identity = "other-version".into();
        let left = BTreeMap::from([(sha.clone(), Ok(a))]);
        let mut right = BTreeMap::from([(sha.clone(), Ok(b))]);
        let control = environment_comparison(&left, &right).unwrap();
        assert_eq!(control["canonical_equal"], 0);
        assert_eq!(control["content_equal_excluding_identity"], 1);
        right.get_mut(&sha).unwrap().as_mut().unwrap().tokens[1].lemma = "changed".into();
        assert_eq!(
            environment_comparison(&left, &right).unwrap()["content_equal_excluding_identity"],
            0
        );
        right.clear();
        assert!(environment_comparison(&left, &right).is_err());
    }
}
