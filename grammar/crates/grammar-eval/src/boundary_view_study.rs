//! Frozen synthetic boundary-view comparison, with unchanged guard evidence.
use crate::{boundary_choice_observation as observation, boundary_view, claim_guard};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{edits, syntax::Document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::Instant,
};

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
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}
fn rows<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value[key]
        .as_array()
        .with_context(|| format!("Missing array {key}"))
}
fn count(value: &Value, key: &str) -> Result<usize> {
    value[key]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .with_context(|| format!("Missing count {key}"))
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
fn bound(repo: &Path, binding: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(
        &repo.join(text(binding, "path")?),
        text(binding, "sha256")?,
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
        "Unsafe identifier"
    );
    Ok(())
}

fn validate_fixture(binding: &Value, fixture: &Value, labels: &Value) -> Result<()> {
    let id = text(binding, "id")?;
    safe_id(id)?;
    ensure!(fixture["id"] == id, "Fixture identity differs");
    ensure!(
        ["reuse", "parse_new"].contains(&text(binding, "annotation_mode")?),
        "Unknown annotation mode"
    );
    let cases = rows(fixture, "cases")?;
    ensure!(
        cases.len() == count(binding, "expected_cases")?,
        "Fixture case count differs"
    );
    let mut by_id = BTreeMap::new();
    let mut pairs = BTreeMap::<String, Vec<&Value>>::new();
    for case in cases {
        let case_id = text(case, "id")?;
        safe_id(case_id)?;
        safe_id(text(case, "pair_id")?)?;
        ensure!(by_id.insert(case_id, case).is_none(), "Repeated case ID");
        ensure!(
            !text(case, "observed_form")?.is_empty(),
            "Empty source form"
        );
        ensure!(
            case["expected_rule_candidate"].is_boolean()
                || case["expected_rule_candidate"].is_null(),
            "Rule expectation must be bool or null"
        );
        ensure!(
            case.get("guard_must_reject").is_none() || case["guard_must_reject"].is_boolean(),
            "Guard requirement must be bool when present"
        );
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            !patches.is_empty()
                && text(case, "source_text")? != text(case, "candidate_text")?
                && edits::apply(text(case, "source_text")?, &patches)? == case["candidate_text"],
            "Invalid declared patches"
        );
        let label = &labels[case_id];
        ensure!(
            !text(label, "category")?.is_empty()
                && label["attribution_sensitive"].is_boolean()
                && label["boundary_view_must_not_agree"].is_boolean(),
            "Missing frozen case labels"
        );
        for key in [
            "category",
            "attribution_sensitive",
            "boundary_view_must_not_agree",
        ] {
            if let Some(embedded) = case.get(key) {
                ensure!(
                    embedded == &label[key],
                    "Embedded fixture label differs from protocol"
                );
            }
        }
        pairs
            .entry(text(case, "pair_id")?.to_owned())
            .or_default()
            .push(case);
    }
    ensure!(
        labels
            .as_object()
            .context("Case-label map")?
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == by_id.keys().copied().collect(),
        "Case-label coverage differs"
    );
    ensure!(
        pairs.len() == count(binding, "expected_pairs")?
            && pairs.values().all(|pair| pair.len() == 2),
        "Pair counts differ"
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
            "Reciprocal fixture identity differs"
        );
        ensure!(
            labels[text(case, "id")?]["category"] == labels[text(inverse, "id")?]["category"]
                && labels[text(case, "id")?]["attribution_sensitive"]
                    == labels[text(inverse, "id")?]["attribution_sensitive"],
            "Pair category or attribution label differs by direction"
        );
    }
    Ok(())
}

fn insert_text(map: &mut BTreeMap<String, String>, source: &str) -> Result<()> {
    let sha = hash(source.as_bytes());
    if let Some(previous) = map.insert(sha, source.to_owned()) {
        ensure!(previous == source, "Exact-text SHA256 collision");
    }
    Ok(())
}

fn parse_saved(record: &Value, expected_text: &str, parser_identity: &str) -> Result<Parsed> {
    match text(record, "status")? {
        "ok" => {
            let doc: Document = serde_json::from_value(record["document"].clone())?;
            grammar_core::syntax::validate(&doc)?;
            ensure!(
                doc.text == expected_text && doc.parser_identity == parser_identity,
                "Saved annotation text or identity differs"
            );
            Ok(Ok(doc))
        }
        "error" => {
            ensure!(
                record["text"] == expected_text,
                "Saved parser error text differs"
            );
            Ok(Err(text(record, "error")?.to_owned()))
        }
        _ => anyhow::bail!("Unknown annotation record status"),
    }
}

fn load_saved(
    args: &Args,
    protocol: &Value,
    requested: &BTreeMap<String, String>,
    artifacts: &mut Vec<Value>,
) -> Result<Documents> {
    let binding = &protocol["saved_annotations"];
    let manifest = bound(&args.repo, &binding["annotation_manifest"])?;
    let old_receipt = bound(&args.repo, &binding["run_receipt"])?;
    ensure!(
        manifest["variant"]["id"] == "incumbent_sm"
            && manifest["variant"]["parser_identity"] == protocol["parser"]["parser_identity"],
        "Saved parser variant differs"
    );
    let records = rows(&manifest, "documents")?;
    ensure!(
        records.len() == count(binding, "expected_texts")? && records.len() == requested.len(),
        "Saved annotation scope differs"
    );
    let saved_root = args.repo.join(text(binding, "run_directory")?);
    let old_report: Value = serde_json::from_slice(&checked(
        &saved_root.join("report.json"),
        text(&old_receipt, "report_sha256")?,
    )?)?;
    ensure!(
        rows(&old_report, "artifacts")?.iter().any(|entry| {
            entry["sha256"] == binding["annotation_manifest"]["sha256"]
                && entry["path"] == "incumbent_sm/annotation-manifest.json"
        }),
        "Saved annotation manifest is not bound by the prior report"
    );
    let mut docs = Documents::new();
    let mut provenance = Vec::new();
    for entry in records {
        let name = text(entry, "path")?;
        let file = Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .context("Saved annotation filename")?;
        let source = requested
            .get(file)
            .context("Saved annotation outside fixed reused text set")?;
        let bytes = checked(&saved_root.join(name), text(entry, "sha256")?)?;
        let record: Value = serde_json::from_slice(&bytes)?;
        let parsed = parse_saved(
            &record,
            source,
            text(&protocol["parser"], "parser_identity")?,
        )?;
        ensure!(
            docs.insert(file.to_owned(), parsed).is_none(),
            "Repeated saved annotation"
        );
        let local_name = format!("annotations/{file}.json");
        let dest = args.out.join(&local_name);
        fs::create_dir_all(dest.parent().context("Annotation parent")?)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)?
            .write_all(&bytes)?;
        artifacts.push(json!({"path":local_name,"sha256":hash(&bytes)}));
        provenance.push(json!({"source_sha256":file,"mode":"reused_byte_for_byte","source_binding":{"path":saved_root.join(name),"sha256":entry["sha256"]},"local_path":local_name}));
    }
    ensure!(
        docs.keys().eq(requested.keys()),
        "Reused text coverage differs"
    );
    artifacts.push(output(&args.out,"reused-annotation-provenance.json",&json!({"records":provenance,"parser_calls":0,"manifest_binding":binding["annotation_manifest"],"prior_receipt":binding["run_receipt"]}))?);
    Ok(docs)
}

fn load_new(
    args: &Args,
    protocol: &Value,
    requested: &BTreeMap<String, String>,
    artifacts: &mut Vec<Value>,
) -> Result<(Documents, Value)> {
    let mut docs = Documents::new();
    let mut batches = Vec::new();
    let ordered = requested.iter().collect::<Vec<_>>();
    let began = Instant::now();
    for (batch_index, batch) in ordered.chunks(64).enumerate() {
        let input = batch
            .iter()
            .map(|(_, source)| source.as_str())
            .collect::<Vec<_>>();
        let start = Instant::now();
        let parsed = grammar_spacy::parse_batch(
            &input,
            args.repo
                .join(text(&protocol["parser"]["parser_python"], "path")?)
                .to_str()
                .context("Parser wrapper path UTF8")?,
        )?;
        let seconds = start.elapsed().as_secs_f64();
        ensure!(
            parsed.len() == batch.len(),
            "Returned parser batch count differs"
        );
        for ((sha, source), result) in batch.iter().zip(parsed) {
            if let Ok(doc) = &result {
                grammar_core::syntax::validate(doc)?;
                ensure!(
                    doc.text == **source
                        && doc.parser_identity == protocol["parser"]["parser_identity"],
                    "Returned parser text or identity differs"
                );
            }
            let record = match &result {
                Ok(doc) => json!({"status":"ok","document":doc}),
                Err(error) => json!({"status":"error","text":source,"error":error}),
            };
            artifacts.push(output(
                &args.out,
                &format!("annotations/{sha}.json"),
                &record,
            )?);
            ensure!(
                docs.insert((*sha).clone(), result).is_none(),
                "Repeated new text"
            );
        }
        batches.push(json!({"batch_index":batch_index,"source_sha256":batch.iter().map(|(sha,_)|*sha).collect::<Vec<_>>(),"documents":batch.len(),"parse_seconds":seconds}));
    }
    let timing = json!({"text_order":"lexical SHA256","texts":docs.len(),"parser_errors":docs.values().filter(|v|v.is_err()).count(),"batches":batches,"parse_and_annotation_write_seconds":began.elapsed().as_secs_f64()});
    artifacts.push(output(
        &args.out,
        "new-annotation-provenance.json",
        &timing,
    )?);
    Ok((docs, timing))
}

fn evaluate_case(
    case: &Value,
    label: &Value,
    source: &Parsed,
    candidate: &Parsed,
) -> Result<Value> {
    let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
    let observed = source.as_ref().ok().map(observation::observe).transpose()?;
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
    let both_parsed = source.is_ok() && candidate.is_ok();
    let (view, guard) = match (source, candidate) {
        (Ok(a), Ok(b)) => (
            Some(serde_json::to_value(boundary_view::compare(
                a, b, &patches,
            )?)?),
            Some(claim_guard::compare(a, b, &patches)?),
        ),
        _ => (None, None),
    };
    let agreement = view
        .as_ref()
        .and_then(|v| v["structural_agreement"].as_bool());
    if let Some(view) = &view {
        ensure!(
            view["automatic_edit_license"] == false
                && view["edit_licensed"] == false
                && view["semantic_equivalence_certified"] == false,
            "Boundary-view observation unexpectedly licensed an edit"
        );
        ensure!(
            matches!(
                (text(view, "outcome")?, agreement),
                ("local_structure_agrees", Some(true))
                    | ("differs", Some(false))
                    | ("unavailable", None)
            ),
            "Boundary-view outcome and agreement disagree"
        );
    }
    let exact_emitted = observed.as_ref().map(|_| !matches.is_empty());
    let patch_emitted = observed
        .as_ref()
        .map(|_| matches.iter().any(|m| m["declared_patch_match"] == true));
    let must_not_agree = label["boundary_view_must_not_agree"]
        .as_bool()
        .context("Boundary view requirement")?;
    let attribution_sensitive = label["attribution_sensitive"]
        .as_bool()
        .context("Attribution label")?;
    let guard_must_reject = case["guard_must_reject"].as_bool().unwrap_or(false);
    let original_eligible =
        both_parsed.then(|| matches.iter().any(|m| m["pair"]["eligible"] == true));
    let original_patch_eligible = both_parsed.then(|| {
        matches
            .iter()
            .any(|m| m["declared_patch_match"] == true && m["pair"]["eligible"] == true)
    });
    Ok(json!({
        "case":case,"labels":label,"status":if both_parsed{"evaluated"}else{"parse_error"},
        "source_parse_status":if source.is_ok(){"ok"}else{"error"},
        "candidate_parse_status":if candidate.is_ok(){"ok"}else{"error"},
        "source_error":source.as_ref().err(),"candidate_error":candidate.as_ref().err(),
        "boundary_view":view,"structural_agreement":agreement,
        "view_status":view.as_ref().map(|v|v["outcome"].clone()),
        "must_not_agree":must_not_agree,
        "must_not_agree_violated":if must_not_agree{agreement}else{None},
        "must_not_agree_met":if must_not_agree{agreement.map(|a|!a)}else{None},
        "attribution_sensitive_structural_collision":if attribution_sensitive{agreement}else{None},
        "original_observation":observed,"original_exact_text_emitted":exact_emitted,
        "original_exact_patch_emitted":patch_emitted,"original_text_matches":matches,
        "original_paired_eligible":original_eligible,
        "original_declared_patch_paired_eligible":original_patch_eligible,
        "original_rule_expectation_met":case["expected_rule_candidate"].as_bool().zip(exact_emitted).map(|(expected,actual)|expected==actual),
        "original_direct_guard":guard,
        "original_guard_must_reject":guard_must_reject,
        "original_guard_rejection_violated":if guard_must_reject{guard.as_ref().map(|g|g.outcome==claim_guard::Outcome::ObservedPreserved)}else{None},
        "automatic_edit_license":false,"semantic_equivalence_certified":false,
    }))
}

fn legacy_record(row: &Value) -> Value {
    // Reproduce the frozen predecessor schema, including its false eligibility
    // fallback on unavailable parses; the new row retains nullable eligibility.
    json!({"case":row["case"],"status":row["status"],
        "source_parse_status":row["source_parse_status"],"candidate_parse_status":row["candidate_parse_status"],
        "source_error":row["source_error"],"candidate_error":row["candidate_error"],
        "observation":row["original_observation"],"exact_text_candidate_emitted":row["original_exact_text_emitted"],
        "exact_patch_candidate_emitted":row["original_exact_patch_emitted"],
        "rule_expectation_met":row["original_rule_expectation_met"],"text_matches":row["original_text_matches"],
        "desired_pair_guard":row["original_direct_guard"],"guard_must_reject":row["original_guard_must_reject"],
        "guard_rejection_expectation_violated":row["original_guard_rejection_violated"],
        "paired_eligible":row["original_paired_eligible"].as_bool().unwrap_or(false),
        "declared_patch_paired_eligible":row["original_declared_patch_paired_eligible"].as_bool().unwrap_or(false),
        "semantic_equivalence_certified":false})
}

fn baseline_gate(
    args: &Args,
    protocol: &Value,
    fixtures: &[Value],
    docs: &Documents,
    artifacts: &mut Vec<Value>,
) -> Result<BTreeMap<String, Vec<Value>>> {
    let mut saved = BTreeMap::new();
    let mut differences = Vec::new();
    let mut compared = 0;
    for binding in rows(protocol, "saved_case_results")? {
        let id = text(binding, "fixture_id")?;
        let fixture = fixtures
            .iter()
            .find(|f| f["id"] == id)
            .context("Baseline fixture missing")?;
        let prior = bound(&args.repo, binding)?;
        let prior_rows = prior.as_array().context("Prior case array")?;
        let cases = rows(fixture, "cases")?;
        ensure!(
            cases.len() == prior_rows.len(),
            "Historical case count differs"
        );
        let mut recomputed = Vec::new();
        for (case, old) in cases.iter().zip(prior_rows) {
            let row = evaluate_case(
                case,
                &protocol["case_labels"][id][text(case, "id")?],
                &docs[&hash(text(case, "source_text")?.as_bytes())],
                &docs[&hash(text(case, "candidate_text")?.as_bytes())],
            )?;
            let projected = legacy_record(&row);
            if &projected != old {
                let keys = projected
                    .as_object()
                    .context("Projected record")?
                    .keys()
                    .chain(old.as_object().context("Prior record")?.keys())
                    .collect::<BTreeSet<_>>();
                let fields = keys
                    .into_iter()
                    .filter(|key| projected[*key] != old[*key])
                    .collect::<Vec<_>>();
                differences.push(json!({"fixture":id,"case_id":case["id"],"differing_fields":fields,"projected_record":projected}));
            }
            compared += 1;
            recomputed.push(row);
        }
        ensure!(
            saved.insert(id.to_owned(), recomputed).is_none(),
            "Repeated baseline fixture"
        );
    }
    artifacts.push(output(&args.out,"baseline-check.json",&json!({"status":if differences.is_empty(){"pass"}else{"mismatch"},"compared_cases":compared,
        "differences":differences,"prior_case_bindings":protocol["saved_case_results"],"new_parser_calls_before_check":0,
        "comparison":"Exact old case record after field-name projection; includes observations, direct guards, actual forward/inverse candidates, errors and expectations. New boundary-view fields are excluded."}))?);
    ensure!(
        compared == 48 && differences.is_empty(),
        "Historical baseline mismatch; retained baseline-check.json, no fresh text parsed"
    );
    Ok(saved)
}

const COUNTS: &[&str] = &[
    "directions",
    "both_texts_parsed",
    "source_parse_errors",
    "candidate_parse_errors",
    "structural_comparisons_available",
    "structural_comparisons_unavailable",
    "local_structure_agrees",
    "differs",
    "must_not_agree_assertions",
    "must_not_agree_tested",
    "must_not_agree_unavailable",
    "must_not_agree_violations",
    "attribution_sensitive_directions",
    "attribution_sensitive_tested",
    "attribution_sensitive_unavailable",
    "attribution_sensitive_structural_collisions",
    "non_attribution_sensitive_directions",
    "non_attribution_sensitive_tested",
    "non_attribution_sensitive_structure_agrees",
    "original_exact_text_emissions",
    "original_exact_patch_emissions",
    "original_paired_eligible_directions",
    "original_rule_expectations",
    "original_rule_expectations_unavailable",
    "original_rule_expectation_mismatches",
    "original_guard_observed_preserved",
    "original_guard_changed",
    "original_guard_unresolved",
    "original_guard_unavailable",
    "original_guard_rejection_assertions",
    "original_guard_rejection_unavailable",
    "original_guard_rejection_violations",
    "original_inverse_proposals",
    "original_restoring_inverse_matches",
    "original_reverse_guard_observed_preserved",
    "original_reverse_guard_changed",
    "original_reverse_guard_unresolved",
    "automatic_edit_licenses",
];
fn add(counts: &mut BTreeMap<&'static str, usize>, key: &'static str, n: usize) {
    *counts.get_mut(key).expect("Fixed count name") += n;
}
fn fraction(numerator: usize, denominator: usize) -> Value {
    json!({"numerator":numerator,"denominator":denominator,"fraction":if denominator==0{None}else{Some(numerator as f64/denominator as f64)}})
}
fn summarize(cases: &[&Value]) -> Result<Value> {
    let mut c = COUNTS
        .iter()
        .map(|key| (*key, 0usize))
        .collect::<BTreeMap<_, _>>();
    for row in cases {
        add(&mut c, "directions", 1);
        add(
            &mut c,
            "both_texts_parsed",
            usize::from(row["status"] == "evaluated"),
        );
        add(
            &mut c,
            "source_parse_errors",
            usize::from(row["source_parse_status"] == "error"),
        );
        add(
            &mut c,
            "candidate_parse_errors",
            usize::from(row["candidate_parse_status"] == "error"),
        );
        let agreement = row["structural_agreement"].as_bool();
        add(
            &mut c,
            if agreement.is_some() {
                "structural_comparisons_available"
            } else {
                "structural_comparisons_unavailable"
            },
            1,
        );
        if let Some(agree) = agreement {
            add(
                &mut c,
                if agree {
                    "local_structure_agrees"
                } else {
                    "differs"
                },
                1,
            );
        }
        if row["must_not_agree"] == true {
            add(&mut c, "must_not_agree_assertions", 1);
            add(
                &mut c,
                if agreement.is_some() {
                    "must_not_agree_tested"
                } else {
                    "must_not_agree_unavailable"
                },
                1,
            );
            add(
                &mut c,
                "must_not_agree_violations",
                usize::from(agreement == Some(true)),
            );
        }
        if row["labels"]["attribution_sensitive"] == true {
            add(&mut c, "attribution_sensitive_directions", 1);
            add(
                &mut c,
                if agreement.is_some() {
                    "attribution_sensitive_tested"
                } else {
                    "attribution_sensitive_unavailable"
                },
                1,
            );
            add(
                &mut c,
                "attribution_sensitive_structural_collisions",
                usize::from(agreement == Some(true)),
            );
        } else {
            add(&mut c, "non_attribution_sensitive_directions", 1);
            add(
                &mut c,
                "non_attribution_sensitive_tested",
                usize::from(agreement.is_some()),
            );
            add(
                &mut c,
                "non_attribution_sensitive_structure_agrees",
                usize::from(agreement == Some(true)),
            );
        }
        add(
            &mut c,
            "original_exact_text_emissions",
            usize::from(row["original_exact_text_emitted"] == true),
        );
        add(
            &mut c,
            "original_exact_patch_emissions",
            usize::from(row["original_exact_patch_emitted"] == true),
        );
        add(
            &mut c,
            "original_paired_eligible_directions",
            usize::from(row["original_paired_eligible"] == true),
        );
        if row["case"]["expected_rule_candidate"].is_boolean() {
            add(&mut c, "original_rule_expectations", 1);
            add(
                &mut c,
                "original_rule_expectations_unavailable",
                usize::from(row["original_rule_expectation_met"].is_null()),
            );
            add(
                &mut c,
                "original_rule_expectation_mismatches",
                usize::from(row["original_rule_expectation_met"] == false),
            );
        }
        let guard = &row["original_direct_guard"];
        add(
            &mut c,
            match guard["outcome"].as_str() {
                Some("observed_preserved") => "original_guard_observed_preserved",
                Some("changed") => "original_guard_changed",
                Some("unresolved") => "original_guard_unresolved",
                None => "original_guard_unavailable",
                _ => anyhow::bail!("Unknown original guard outcome"),
            },
            1,
        );
        if row["original_guard_must_reject"] == true {
            add(&mut c, "original_guard_rejection_assertions", 1);
            add(
                &mut c,
                "original_guard_rejection_unavailable",
                usize::from(row["original_guard_rejection_violated"].is_null()),
            );
            add(
                &mut c,
                "original_guard_rejection_violations",
                usize::from(row["original_guard_rejection_violated"] == true),
            );
        }
        for matching in rows(row, "original_text_matches")? {
            let pair = &matching["pair"];
            if pair.is_null() {
                continue;
            }
            add(
                &mut c,
                "original_inverse_proposals",
                count(pair, "inverse_proposal_count")?,
            );
            for inverse in rows(pair, "inverse_matches")? {
                add(&mut c, "original_restoring_inverse_matches", 1);
                add(
                    &mut c,
                    match text(&inverse["reverse_guard"], "outcome")? {
                        "observed_preserved" => "original_reverse_guard_observed_preserved",
                        "changed" => "original_reverse_guard_changed",
                        "unresolved" => "original_reverse_guard_unresolved",
                        _ => anyhow::bail!("Unknown reverse guard outcome"),
                    },
                    1,
                );
            }
        }
        ensure!(
            row["automatic_edit_license"] == false,
            "Study row unexpectedly licensed an edit"
        );
    }
    Ok(json!({"counts":c,"rates":{
        "agreement_among_tested":fraction(c["local_structure_agrees"],c["structural_comparisons_available"]),
        "must_not_agree_violation_among_tested":fraction(c["must_not_agree_violations"],c["must_not_agree_tested"]),
        "attribution_collision_among_tested":fraction(c["attribution_sensitive_structural_collisions"],c["attribution_sensitive_tested"]),
    }}))
}

fn pair_rows(cases: &[Value]) -> Result<Vec<Value>> {
    let mut grouped = BTreeMap::<String, Vec<&Value>>::new();
    for case in cases {
        grouped
            .entry(text(&case["case"], "pair_id")?.to_owned())
            .or_default()
            .push(case);
    }
    grouped.into_iter().map(|(id,pair)| {
        ensure!(pair.len()==2,"Pair direction coverage differs");
        let agrees = pair.iter().filter(|row|row["structural_agreement"]==true).count();
        let available = pair.iter().filter(|row|row["structural_agreement"].is_boolean()).count();
        let attr = pair[0]["labels"]["attribution_sensitive"]==true;
        Ok(json!({"pair_id":id,"case_ids":pair.iter().map(|row|&row["case"]["id"]).collect::<Vec<_>>(),
            "category":pair[0]["labels"]["category"],"attribution_sensitive":attr,
            "structural_agreement_directions":agrees,"structural_available_directions":available,
            "unavailable_directions":2-available,"agreement_label":match agrees{2=>"both",1=>"one",_=>"neither"},
            "both_directions_agree":agrees==2,"neither_agrees_and_both_available":agrees==0 && available==2,
            "attribution_sensitive_collision_directions":if attr{agrees}else{0},
            "automatic_edit_license":false}))
    }).collect()
}
fn summarize_pairs(pairs: &[&Value]) -> Value {
    json!({"pairs":pairs.len(),"both_agree":pairs.iter().filter(|p|p["agreement_label"]=="both").count(),
        "one_agrees":pairs.iter().filter(|p|p["agreement_label"]=="one").count(),
        "neither_agrees":pairs.iter().filter(|p|p["agreement_label"]=="neither").count(),
        "neither_agrees_and_both_available":pairs.iter().filter(|p|p["neither_agrees_and_both_available"]==true).count(),
        "with_unavailable_direction":pairs.iter().filter(|p|p["unavailable_directions"].as_u64().is_some_and(|n|n>0)).count(),
        "attribution_sensitive_pairs":pairs.iter().filter(|p|p["attribution_sensitive"]==true).count(),
        "attribution_sensitive_pairs_with_collision":pairs.iter().filter(|p|p["attribution_sensitive_collision_directions"].as_u64().is_some_and(|n|n>0)).count(),
        "automatic_edit_licenses":0})
}

fn validate_protocol(protocol: &Value) -> Result<()> {
    ensure!(
        protocol["schema"] == "slopninja-boundary-view-study-protocol-v1"
            && protocol["parser_batch_size"] == 64
            && protocol["automatic_edit_license"] == false,
        "Boundary-view protocol design differs"
    );
    for key in [
        "training_corpus_reads",
        "model_fit_calls",
        "llm_calls",
        "detector_calls",
    ] {
        ensure!(
            protocol[key] == 0,
            "Protocol authorizes a forbidden study activity"
        );
    }
    ensure!(
        protocol["parser"]["id"] == "incumbent_sm"
            && protocol["parser"]["parser_identity"]
                == "grammar-spacy-annotation-v1;spacy=3.8.16;model=en_core_web_sm@3.8.0;raw-text-v1",
        "Incumbent parser differs"
    );
    let fixtures = rows(protocol, "fixture_manifests")?;
    ensure!(fixtures.len() == 3, "Expected three fixed fixture panels");
    let expected = [
        ("boundary-choice-v1", 24, 12, "reuse"),
        ("parser-counterexamples-v1", 24, 12, "reuse"),
        ("boundary-view-challenges-v1", 48, 24, "parse_new"),
    ];
    for (binding, (id, cases, pairs, mode)) in fixtures.iter().zip(expected) {
        ensure!(
            binding["id"] == id
                && count(binding, "expected_cases")? == cases
                && count(binding, "expected_pairs")? == pairs
                && binding["annotation_mode"] == mode,
            "Fixed fixture order, scope or reuse policy differs"
        );
    }
    ensure!(
        protocol["case_labels"]
            .as_object()
            .context("Fixture-label map")?
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == expected.iter().map(|row| row.0).collect(),
        "Fixture-label identity coverage differs"
    );
    ensure!(
        rows(protocol, "saved_case_results")?
            .iter()
            .map(|row| text(row, "fixture_id"))
            .collect::<Result<Vec<_>>>()?
            == ["boundary-choice-v1", "parser-counterexamples-v1"],
        "Historical case-result scope differs"
    );
    let sources = protocol["source_files"]
        .as_object()
        .context("Source bindings")?;
    for mandatory in [
        "grammar/Cargo.lock",
        "grammar/crates/grammar-core/src/rules.rs",
        "grammar/crates/grammar-core/src/syntax.rs",
        "grammar/crates/grammar-core/src/edits.rs",
        "grammar/crates/grammar-eval/src/boundary_view.rs",
        "grammar/crates/grammar-eval/src/boundary_view_study.rs",
        "grammar/crates/grammar-eval/src/bin/slop_ninja-boundary-view.rs",
        "grammar/crates/grammar-eval/src/boundary_choice_observation.rs",
        "grammar/crates/grammar-eval/src/claim_guard.rs",
        "grammar/crates/grammar-eval/src/lexical_context.rs",
        "grammar/crates/grammar-eval/src/predicate_operators.rs",
        "grammar/crates/grammar-spacy/src/lib.rs",
        "grammar/crates/grammar-spacy/python/syntax_bridge.py",
    ] {
        ensure!(
            sources.contains_key(mandatory),
            "Missing required source binding {mandatory}"
        );
    }
    Ok(())
}

fn run(args: &Args, protocol: &Value) -> Result<()> {
    let executable = fs::read(std::env::current_exe()?)?;
    ensure!(
        hash(&executable) == text(protocol, "executable_sha256")?,
        "Running executable differs from frozen protocol"
    );
    for (name, sha) in protocol["source_files"]
        .as_object()
        .context("Source bindings")?
    {
        let path = Path::new(name);
        ensure!(
            !path.is_absolute() && path.components().all(|c| matches!(c, Component::Normal(_))),
            "Unsafe source snapshot path"
        );
        let bytes = checked(&args.repo.join(path), sha.as_str().context("Source hash")?)?;
        let target = args.out.join("executed-source").join(path);
        fs::create_dir_all(target.parent().context("Source snapshot parent")?)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?
            .write_all(&bytes)?;
    }
    let parser = &protocol["parser"];
    checked(
        &args.repo.join(text(&parser["parser_python"], "path")?),
        text(&parser["parser_python"], "sha256")?,
    )?;
    bound(&args.repo, &parser["environment_manifest"])?;
    ensure!(
        !rows(parser, "asset_bindings")?.is_empty(),
        "Missing parser asset bindings"
    );
    for asset in rows(parser, "asset_bindings")? {
        checked(
            &args.repo.join(text(asset, "path")?),
            text(asset, "sha256")?,
        )?;
    }
    let mut fixtures = Vec::new();
    let mut reused = BTreeMap::new();
    let mut new = BTreeMap::new();
    for binding in rows(protocol, "fixture_manifests")? {
        let fixture = bound(&args.repo, binding)?;
        let id = text(binding, "id")?;
        validate_fixture(binding, &fixture, &protocol["case_labels"][id])?;
        let requested = if binding["annotation_mode"] == "reuse" {
            &mut reused
        } else {
            &mut new
        };
        for case in rows(&fixture, "cases")? {
            insert_text(requested, text(case, "source_text")?)?;
            insert_text(requested, text(case, "candidate_text")?)?;
        }
        fixtures.push(fixture);
    }
    ensure!(
        reused.len() == 48 && new.len() == 48,
        "Expected 48 reused and 48 fresh exact texts"
    );
    ensure!(
        !reused.keys().any(|sha| new.contains_key(sha)),
        "Fresh challenge text overlaps reused fixture text"
    );
    let mut artifacts = Vec::new();
    let mut docs = load_saved(args, protocol, &reused, &mut artifacts)?;
    let baseline_start = Instant::now();
    let mut cached_cases = baseline_gate(args, protocol, &fixtures, &docs, &mut artifacts)?;
    let historical_replay_seconds = baseline_start.elapsed().as_secs_f64();
    let (new_docs, parsing) = load_new(args, protocol, &new, &mut artifacts)?;
    for (sha, doc) in new_docs {
        ensure!(
            docs.insert(sha, doc).is_none(),
            "Overlapping loaded annotation"
        );
    }
    let start = Instant::now();
    let mut summaries = Vec::new();
    let mut pair_summaries = Vec::new();
    let mut all_cases = Vec::new();
    let mut all_pairs = Vec::new();
    for fixture in &fixtures {
        let id = text(fixture, "id")?;
        let cases = if let Some(cached) = cached_cases.remove(id) {
            cached
        } else {
            let mut evaluated = Vec::new();
            for case in rows(fixture, "cases")? {
                evaluated.push(evaluate_case(
                    case,
                    &protocol["case_labels"][id][text(case, "id")?],
                    &docs[&hash(text(case, "source_text")?.as_bytes())],
                    &docs[&hash(text(case, "candidate_text")?.as_bytes())],
                )?);
            }
            evaluated
        };
        let pairs = pair_rows(&cases)?;
        artifacts.push(output(
            &args.out,
            &format!("{id}-cases.json"),
            &json!(cases),
        )?);
        artifacts.push(output(
            &args.out,
            &format!("{id}-pairs.json"),
            &json!(pairs),
        )?);
        summaries.push(json!({"fixture":id,"category":null,"form":null,"summary":summarize(&cases.iter().collect::<Vec<_>>())?}));
        pair_summaries.push(json!({"fixture":id,"category":null,"summary":summarize_pairs(&pairs.iter().collect::<Vec<_>>())}));
        let categories = cases
            .iter()
            .map(|row| text(&row["labels"], "category"))
            .collect::<Result<BTreeSet<_>>>()?;
        for category in categories {
            let subset = cases
                .iter()
                .filter(|row| row["labels"]["category"] == category)
                .collect::<Vec<_>>();
            summaries.push(
                json!({"fixture":id,"category":category,"form":null,"summary":summarize(&subset)?}),
            );
            pair_summaries.push(json!({"fixture":id,"category":category,"summary":summarize_pairs(&pairs.iter().filter(|p|p["category"]==category).collect::<Vec<_>>())}));
            let forms = subset
                .iter()
                .map(|row| text(&row["case"], "observed_form"))
                .collect::<Result<BTreeSet<_>>>()?;
            for form in forms {
                summaries.push(json!({"fixture":id,"category":category,"form":form,"summary":summarize(&subset.iter().copied().filter(|row|row["case"]["observed_form"]==form).collect::<Vec<_>>())?}));
            }
        }
        all_cases.extend(cases);
        all_pairs.extend(pairs);
        println!("Completed boundary view fixture {id}");
    }
    let report = json!({"schema":"slopninja-boundary-view-study-v1","status":"complete",
        "protocol_sha256":args.expected_protocol_sha256,"parser_identity":parser["parser_identity"],
        "reused_texts":reused.len(),"new_requested_texts":new.len(),"total_annotation_records":docs.len(),
        "historical_case_replay_passed":true,
        "successful_annotation_records":docs.values().filter(|doc|doc.is_ok()).count(),"parser_error_records":docs.values().filter(|doc|doc.is_err()).count(),
        "cases":all_cases.len(),"pairs":all_pairs.len(),"summary_rows":summaries,"pair_summaries":pair_summaries,
        "pooled":summarize(&all_cases.iter().collect::<Vec<_>>())?,"pooled_pairs":summarize_pairs(&all_pairs.iter().collect::<Vec<_>>()),
        "parsing":parsing,"historical_replay_seconds":historical_replay_seconds,
        "post_parse_evaluation_and_result_write_seconds":start.elapsed().as_secs_f64(),"artifacts":artifacts,
        "automatic_edit_license":false,"semantic_equivalence_certified":false,
        "training_corpus_reads":0,"model_fit_calls":0,"llm_calls":0,"detector_calls":0,
        "interpretation":"Source-aligned local structure only. Attribution-sensitive agreement is reported as a collision, regardless of unchanged guard rejection or the constant false edit license. No preference, semantic, tone, readability or detector certification."});
    let report_sha256 = fresh(&args.out.join("report.json"), &report)?;
    fresh(
        &args.out.join("receipt.json"),
        &json!({"schema":"slopninja-boundary-view-study-receipt-v1",
        "protocol_sha256":args.expected_protocol_sha256,"report_sha256":report_sha256,"executable_sha256":hash(&executable),
        "source_files":protocol["source_files"],"fixture_manifests":protocol["fixture_manifests"],
        "case_labels":protocol["case_labels"],"saved_annotations":protocol["saved_annotations"],"saved_case_results":protocol["saved_case_results"],"parser":parser}),
    )?;
    Ok(())
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "Use a fresh output directory; preserve previous attempts"
    );
    let bytes = checked(&args.protocol, &args.expected_protocol_sha256)?;
    let protocol: Value = serde_json::from_slice(&bytes)?;
    validate_protocol(&protocol)?;
    fs::create_dir_all(args.out.parent().context("Output parent")?)?;
    fs::create_dir(&args.out)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.out.join("protocol.json"))?
        .write_all(&bytes)?;
    let result = run(&args, &protocol);
    if let Err(error) = &result {
        fresh(
            &args.out.join("failure.json"),
            &json!({"schema":"slopninja-boundary-view-study-failure-v1","status":"failed_retained_attempt","error":format!("{error:#}"),"automatic_edit_license":false}),
        )?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Value, Value, Value) {
        let left = "A. B.";
        let right = "A; B.";
        let cases = json!([
            {"id":"forward","pair_id":"pair","inverse_case_id":"reverse","observed_form":"period","source_text":left,"candidate_text":right,"edits":[{"start_byte":1,"end_byte":2,"expected":".","replacement":";"}],"expected_rule_candidate":null},
            {"id":"reverse","pair_id":"pair","inverse_case_id":"forward","observed_form":"semicolon","source_text":right,"candidate_text":left,"edits":[{"start_byte":1,"end_byte":2,"expected":";","replacement":"."}],"expected_rule_candidate":false}
        ]);
        let label = json!({"category":"probe","attribution_sensitive":true,"boundary_view_must_not_agree":true});
        (
            json!({"id":"synthetic","annotation_mode":"reuse","expected_cases":2,"expected_pairs":1}),
            json!({"id":"synthetic","cases":cases}),
            json!({"forward":label,"reverse":label}),
        )
    }
    fn unavailable_rows() -> Vec<Value> {
        let (_, fixture, labels) = fixture();
        rows(&fixture, "cases")
            .unwrap()
            .iter()
            .map(|case| {
                evaluate_case(
                    case,
                    &labels[case["id"].as_str().unwrap()],
                    &Err("source parser failed".into()),
                    &Err("candidate parser failed".into()),
                )
                .unwrap()
            })
            .collect()
    }
    #[test]
    fn reciprocal_fixture_and_complete_frozen_labels_are_required() {
        let (binding, mut f, mut labels) = fixture();
        validate_fixture(&binding, &f, &labels).unwrap();
        labels.as_object_mut().unwrap().remove("reverse");
        assert!(validate_fixture(&binding, &f, &labels).is_err());
        labels = fixture().2;
        f["cases"][1]["inverse_case_id"] = json!("reverse");
        assert!(validate_fixture(&binding, &f, &labels).is_err());
    }
    #[test]
    fn changed_patch_or_embedded_label_fails_before_inference() {
        let (binding, mut f, labels) = fixture();
        f["cases"][0]["edits"][0]["expected"] = json!("!");
        assert!(validate_fixture(&binding, &f, &labels).is_err());
        f = fixture().1;
        f["cases"][0]["attribution_sensitive"] = json!(false);
        assert!(validate_fixture(&binding, &f, &labels).is_err());
    }
    #[test]
    fn no_parser_result_is_null_and_never_a_passed_exclusion() {
        let cases = unavailable_rows();
        assert!(cases[0]["structural_agreement"].is_null());
        assert!(cases[0]["must_not_agree_met"].is_null());
        assert!(cases[0]["must_not_agree_violated"].is_null());
        let summary = summarize(&cases.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!(summary["counts"]["must_not_agree_unavailable"], 2);
        assert_eq!(summary["counts"]["must_not_agree_tested"], 0);
        assert!(summary["rates"]["agreement_among_tested"]["fraction"].is_null());
    }
    #[test]
    fn collision_is_counted_despite_false_edit_license_and_guard_rejection() {
        let mut cases = unavailable_rows();
        cases[0]["status"] = json!("evaluated");
        cases[0]["structural_agreement"] = json!(true);
        cases[0]["original_direct_guard"] = json!({"outcome":"changed"});
        let summary = summarize(&[&cases[0]]).unwrap();
        assert_eq!(
            summary["counts"]["attribution_sensitive_structural_collisions"],
            1
        );
        assert_eq!(summary["counts"]["must_not_agree_violations"], 1);
        assert_eq!(summary["counts"]["original_guard_changed"], 1);
        assert_eq!(summary["counts"]["automatic_edit_licenses"], 0);
    }
    #[test]
    fn pair_counts_keep_unavailable_separate_from_observed_disagreement() {
        let mut cases = unavailable_rows();
        let unavailable = pair_rows(&cases).unwrap();
        assert_eq!(unavailable[0]["agreement_label"], "neither");
        assert_eq!(unavailable[0]["neither_agrees_and_both_available"], false);
        cases[0]["structural_agreement"] = json!(false);
        cases[1]["structural_agreement"] = json!(false);
        let available = pair_rows(&cases).unwrap();
        assert_eq!(available[0]["neither_agrees_and_both_available"], true);
        let summary = summarize_pairs(&[&unavailable[0], &available[0]]);
        assert_eq!(summary["neither_agrees"], 2);
        assert_eq!(summary["neither_agrees_and_both_available"], 1);
        assert_eq!(summary["with_unavailable_direction"], 1);
    }
    #[test]
    fn saved_error_retains_exact_text_and_rejects_mislabeled_source() {
        let record = json!({"status":"error","text":"A.","error":"parser failure"});
        assert_eq!(
            parse_saved(&record, "A.", "parser").unwrap(),
            Err("parser failure".into())
        );
        assert!(parse_saved(&record, "B.", "parser").is_err());
        assert!(parse_saved(&json!({"status":"ok","document":{}}), "A.", "parser").is_err());
    }
    #[test]
    fn exact_text_deduplication_keeps_distinct_whitespace() {
        let mut texts = BTreeMap::new();
        insert_text(&mut texts, "A. B.").unwrap();
        insert_text(&mut texts, "A. B.").unwrap();
        insert_text(&mut texts, "A.\nB.").unwrap();
        assert_eq!(texts.len(), 2);
    }
    #[test]
    fn artifacts_are_hash_checked_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");
        let sha = fresh(&path, &json!({"retained":true})).unwrap();
        checked(&path, &sha).unwrap();
        assert!(checked(&path, &"0".repeat(64)).is_err());
        assert!(fresh(&path, &json!({"retained":false})).is_err());
        checked(&path, &sha).unwrap();
    }

    #[test]
    fn historical_projection_excludes_new_view_but_preserves_guard_changes() {
        let mut row = unavailable_rows().remove(0);
        let before = legacy_record(&row);
        row["boundary_view"] = json!({"outcome":"local_structure_agrees"});
        row["structural_agreement"] = json!(true);
        assert_eq!(legacy_record(&row), before);
        row["original_direct_guard"] = json!({"outcome":"changed","findings":["retained"]});
        assert_ne!(legacy_record(&row), before);
        assert_eq!(
            legacy_record(&row)["desired_pair_guard"],
            row["original_direct_guard"]
        );
    }
}
