//! Offline sealed-Test comparison with explicit incomplete-provider coverage.
//! STAGE FIXTURES_TEST_DIRECTORY NEW_OUTPUT
//! Never fits a mapping, makes provider calls, or changes model selection.
#[allow(dead_code, unused_imports)]
#[path = "compare_last_block_pangram.rs"]
mod common;
#[allow(dead_code, unused_imports)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use aggregate::fusion::{balance_origins, number, read, sha, text};
use anyhow::{Context, Result, ensure};
use common::{aggregate, evaluation, pangram};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
type Pred = BTreeMap<String, Value>;
type Scores = BTreeMap<String, f64>;
const HISTORIES: [&str; 3] = ["human_only", "model_only", "mixed"];

fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn history(r: &Value) -> Result<usize> {
    let n = r["targets"]["origin"].as_u64().context("origin")? as usize;
    ensure!(
        n < 3 && r["targets"]["ai_involvement"] == (n != 0),
        "production-history labels disagree"
    );
    Ok(n)
}
fn support(data: &[Value], pred: &Pred) -> Result<()> {
    let ids = data
        .iter()
        .map(|r| Ok(text(r, "id")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        ids.len() == data.len() && ids == pred.keys().cloned().collect(),
        "exact local prediction support required"
    );
    Ok(())
}
fn scores(pred: &Pred) -> Result<Scores> {
    pred.iter()
        .map(|(id, p)| Ok((id.clone(), number(&p["ai_confidence_percent"])? / 100.)))
        .collect()
}
fn missing_status(folder: &Path) -> &'static str {
    if folder.join("archive-blocked.json").exists() {
        "archived_submission_requires_reconciliation"
    } else if folder.join("submission.json").exists() {
        "submitted_without_usable_result"
    } else if folder.join("intent.json").exists() {
        "uncertain_submission_do_not_repeat"
    } else {
        "not_submitted"
    }
}
fn validate_observation(
    v: &Value,
    input: &str,
    manifest: &str,
    versions: &BTreeSet<String>,
) -> Result<()> {
    ensure!(
        v["split"] == "test"
            && v["model"] == "pangram-4"
            && v["input_sha256"] == input
            && v["manifest_sha256"] == manifest
            && versions.contains(text(&v["response"], "version")?),
        "provider binding/version differs from sealed mapping"
    );
    Ok(())
}
fn paired(data: &[Value], predictions: &[Pred], mapped: &Scores) -> Result<Value> {
    if data.is_empty() {
        return Ok(Value::Null);
    }
    let pg = common::samples(data, mapped)?;
    let mut differences = Vec::new();
    for pred in predictions {
        let s = scores(pred)?
            .into_iter()
            .filter(|(id, _)| mapped.contains_key(id))
            .collect();
        let errors = common::samples(data, &s)?;
        differences.push(
            errors
                .iter()
                .zip(&pg)
                .map(|((g, a), (_, b))| (g.clone(), a - b))
                .collect::<Vec<_>>(),
        );
    }
    let summarize = |values: &[Vec<(String, f64)>]| -> Result<Value> {
        if values.len() == 1 {
            Ok(aggregate::fusion::family_summary(&values[0]))
        } else {
            aggregate::fixed_seed_summary(values)
        }
    };
    let balanced = differences
        .iter()
        .map(|s| balance_origins(data, s))
        .collect::<Result<Vec<_>>>()?;
    let mut balanced_report = summarize(&balanced)?;
    let cells = balanced_report
        .as_object_mut()
        .unwrap()
        .remove("documents")
        .context("cell count")?;
    balanced_report["family_origin_cells"] = cells;
    balanced_report["documents"] = json!(data.len());
    Ok(
        json!({"family":summarize(&differences)?, "family_origin_balanced":balanced_report,
        "negative_favors_candidate":true,"fixed_seed_runs":predictions.len(),"observed_subset_only":true}),
    )
}

fn run(stage: &Path, fixtures: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let root = stage.join("final-test-v1");
    let sp = root.join("seal.json");
    let sealed = seal::validate(&sp)?;
    let seal_hash = sha(&sp)?;
    let source = root.join("dataset-v1");
    let numeric = root.join("numeric-v1");
    let sm = read(&source.join("manifest.json"))?;
    let dm = read(&numeric.join("manifest.json"))?;
    let input_hash = sha(&source.join("records.jsonl"))?;
    let source_manifest_hash = sha(&source.join("manifest.json"))?;
    ensure!(
        sm["schema"] == "slop_ninja_a_supervised_dataset_v1"
            && sm["complete"] == true
            && sm["records_sha256"] == input_hash
            && sm["selection_receipt_sha256"] == seal_hash
            && dm["schema"] == "slop_ninja_last_block_test_dataset_v1"
            && dm["complete"] == true
            && dm["selection_receipt_sha256"] == seal_hash
            && dm["test_used_for_selection"] == false
            && dm["source_dataset"]["sha256"] == source_manifest_hash
            && dm["files_sha256"]["test.jsonl"] == sha(&numeric.join("test.jsonl"))?,
        "frozen Test inputs differ"
    );
    let data = rows(&numeric.join("test.jsonl"))?;
    let original = rows(&source.join("records.jsonl"))?;
    let hashes = original
        .iter()
        .map(|r| {
            Ok((
                text(r, "id")?.to_owned(),
                text(r, "text_sha256")?.to_owned(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    ensure!(
        hashes.len() == original.len() && hashes.len() == data.len(),
        "source support differs"
    );
    for r in &data {
        ensure!(
            r["purpose"] == "query"
                && r["frozen_split"] == "test"
                && hashes
                    .get(text(r, "id")?)
                    .is_some_and(|h| r["text_sha256"] == *h),
            "query scope/hash differs"
        );
        history(r)?;
    }
    let mapping_path = stage.join("pangram-evaluation-v1/mapping.json");
    let mapping_hash = sha(&mapping_path)?;
    ensure!(
        sealed["pangram_mapping"]["sha256"] == mapping_hash,
        "mapping was not frozen before Test"
    );
    let m = read(&mapping_path)?;
    ensure!(
        m["schema"] == "slop_ninja_last_block_pangram_mapping_v1"
            && m["complete"] == true
            && m["fitting_split"] == "calibration"
            && m["development_used_for_fit"] == false
            && m["final_test_opened"] == false
            && m["selector"] == "pangram-4"
            && m["mapping"]["weighting"] == "family_origin_balanced",
        "mapping scope differs"
    );
    let mapping = pangram::Isotonic::from_frozen_description(&m["mapping"])?;
    let versions = m["versions"]
        .as_array()
        .context("versions")?
        .iter()
        .map(|v| Ok(v.as_str().context("version")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        versions.len() == 1,
        "single frozen provider version required"
    );
    let mut raw = Scores::new();
    let mut bindings = Vec::new();
    let mut counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for r in &data {
        let id = text(r, "id")?;
        ensure!(
            Path::new(id).components().count() == 1
                && matches!(
                    Path::new(id).components().next(),
                    Some(std::path::Component::Normal(_))
                ),
            "unsafe fixture ID"
        );
        let folder = fixtures.join(id);
        let status;
        let mut binding =
            json!({"id":id,"text_sha256":r["text_sha256"],"history":HISTORIES[history(r)?]});
        if folder.join("result.json").exists() {
            let o = pangram::observation(fixtures, r, &hashes)?;
            validate_observation(&o, &input_hash, &source_manifest_hash, &versions)?;
            ensure!(
                raw.insert(id.to_owned(), pangram::score(&o)).is_none(),
                "duplicate observation"
            );
            binding["result_sha256"] = json!(sha(&folder.join("result.json"))?);
            binding["version"] = o["response"]["version"].clone();
            status = "complete";
        } else {
            status = missing_status(&folder);
            for name in [
                "intent.json",
                "submission.json",
                "failure.json",
                "archive-blocked.json",
            ] {
                if folder.join(name).exists() {
                    binding[name] = json!(sha(&folder.join(name))?);
                }
            }
        }
        *counts
            .entry(HISTORIES[history(r)?].into())
            .or_default()
            .entry(status.into())
            .or_default() += 1;
        binding["status"] = json!(status);
        bindings.push(binding);
    }
    let observed = data
        .iter()
        .filter(|r| raw.contains_key(r["id"].as_str().unwrap()))
        .cloned()
        .collect::<Vec<_>>();
    let mapped = raw
        .iter()
        .map(|(id, p)| (id.clone(), mapping.predict(*p)))
        .collect::<Scores>();

    let evaluated = root.join("evaluation-v2");
    let em = read(&evaluated.join("manifest.json"))?;
    let prepared = root.join("fixed-comparators-v1");
    let pm = read(&prepared.join("manifest.json"))?;
    let fixed = root.join("fixed-comparator-evaluation-v1");
    let fm = read(&fixed.join("manifest.json"))?;
    ensure!(
        em["schema"] == "slop_ninja_last_block_test_evaluation_v1"
            && em["complete"] == true
            && em["dataset_manifest_sha256"] == sha(&numeric.join("manifest.json"))?
            && em["selection_receipt_sha256"] == seal_hash
            && pm["schema"] == "slop_ninja_last_block_test_fixed_comparators_v1"
            && pm["complete"] == true
            && pm["dataset_manifest_sha256"] == sha(&numeric.join("manifest.json"))?
            && pm["selection_receipt_sha256"] == seal_hash
            && fm["schema"] == "slop_ninja_fixed_last_block_test_comparator_evaluation_v1"
            && fm["complete"] == true
            && fm["prepared_manifest_sha256"] == sha(&prepared.join("manifest.json"))?
            && fm["selection_receipt_sha256"] == seal_hash,
        "local evaluation binding differs"
    );
    let mut models = BTreeMap::<String, Vec<(Option<u64>, Pred)>>::new();
    let mut prediction_bindings = Vec::new();
    for condition in ["adapt_last_block", "frozen_last_block", "prior_a"] {
        for seed in [17u64, 29, 41] {
            let (path, expected) = if condition == "prior_a" {
                let name = format!("prior-{seed}-test.json");
                (prepared.join(&name), pm["files_sha256"][&name].clone())
            } else {
                let matches = em["reports"]
                    .as_array()
                    .context("report bindings")?
                    .iter()
                    .filter(|r| r["condition"] == condition && r["seed"] == seed)
                    .collect::<Vec<_>>();
                ensure!(matches.len() == 1, "missing/duplicate fixed run");
                ensure!(
                    fm["current_evaluations"][format!("{condition}-{seed}")]
                        == sha(&evaluated.join("manifest.json"))?,
                    "fixed/current comparison differs"
                );
                (
                    evaluated.join(format!("{condition}-seed-{seed}/test-predictions.json")),
                    matches[0]["predictions_sha256"].clone(),
                )
            };
            ensure!(expected == sha(&path)?, "local predictions changed");
            let pred: Pred = serde_json::from_slice(&fs::read(&path)?)?;
            support(&data, &pred)?;
            models
                .entry(condition.into())
                .or_default()
                .push((Some(seed), pred));
            prediction_bindings.push(json!({"condition":condition,"seed":seed,"sha256":expected}));
        }
    }
    let v6 = root.join("fixed-v6-v1");
    let vm = read(&v6.join("manifest.json"))?;
    ensure!(
        fm["v6_manifest_sha256"] == sha(&v6.join("manifest.json"))?
            && vm["schema"] == "slop_ninja_frozen_v6_test_head_predictions_v1"
            && vm["complete"] == true
            && vm["prepared_manifest_sha256"] == sha(&prepared.join("manifest.json"))?
            && vm["selection_receipt_sha256"] == seal_hash
            && vm["predictions_sha256"] == sha(&v6.join("predictions.json"))?,
        "V6 predictions changed"
    );
    let vp = read(&v6.join("predictions.json"))?;
    let mut pred = Pred::new();
    for r in vp
        .as_array()
        .context("V6 prediction array")?
        .iter()
        .filter(|r| r["part"] == "test")
    {
        let id = text(r, "id")?;
        ensure!(
            hashes.get(id).is_some_and(|h| r["text_sha256"] == *h)
                && pred.insert(id.into(), r["prediction"].clone()).is_none(),
            "V6 ID/hash differs"
        );
    }
    support(&data, &pred)?;
    models.insert("v6".into(), vec![(None, pred)]);
    prediction_bindings
        .push(json!({"condition":"v6","seed":null,"sha256":vm["predictions_sha256"]}));
    let mut reports = BTreeMap::new();
    for (condition, runs) in models {
        let mut truth = data.clone();
        if condition == "v6" {
            for r in &mut truth {
                r["targets"]["share"] = Value::Null;
            }
        }
        let mut local = Vec::new();
        for (seed, pred) in &runs {
            let mut by_history = BTreeMap::new();
            for (i, name) in HISTORIES.iter().enumerate() {
                let subset = truth
                    .iter()
                    .filter(|r| r["targets"]["origin"] == i)
                    .cloned()
                    .collect::<Vec<_>>();
                by_history.insert(
                    *name,
                    evaluation::origin_share_metrics(&subset, pred, None)?,
                );
            }
            let all_scores = scores(pred)?;
            let partial_scores = all_scores
                .iter()
                .filter(|(id, _)| mapped.contains_key(*id))
                .map(|(id, p)| (id.clone(), *p))
                .collect();
            local.push(json!({"seed":seed,"requested":truth.len(),"supported":pred.len(),"unsupported":0,
                "full_population_involvement":common::binary_metrics(&truth,&all_scores)?,
                "by_production_history":by_history,"share_supported":condition!="v6",
                "observed_pangram_subset":if observed.is_empty(){Value::Null}else{common::binary_metrics(&observed,&partial_scores)?}}));
        }
        let preds = runs.into_iter().map(|(_, p)| p).collect::<Vec<_>>();
        reports.insert(condition, json!({"runs":local,"candidate_minus_mapped_pangram_brier":paired(&observed,&preds,&mapped)?}));
    }
    let quota = fixtures.join("quota-stop.json");
    let quota_hash = if quota.exists() {
        Some(sha(&quota)?)
    } else {
        None
    };
    let report = json!({"schema":"slop_ninja_last_block_test_pangram_comparison_v1","partition":"test",
        "requested":data.len(),"pangram_supported":observed.len(),"pangram_missing":data.len()-observed.len(),
        "pangram_population_complete":observed.len()==data.len(),"coverage_by_production_history":counts,
        "upstream_requested_queries":dm["upstream_requested_queries"],"upstream_exclusions":sm["excluded"].as_array().context("exclusions")?.len(),
        "mapped_pangram_observed_subset":if observed.is_empty(){Value::Null}else{common::binary_metrics(&observed,&mapped)?},
        "comparators":reports,"mapping_sha256":mapping_hash,"submission_quota_stop_sha256":quota_hash,
        "partial_comparison_supports_overall_parity_conclusion":false,
        "sampling":"provider results are availability-selected, not a random Test sample; missing responses remain in coverage",
        "Pangram_fraction_is_not_native_involvement_confidence_or_share_ground_truth":true,
        "production_history_and_share_truth":"retained history and replacement-span final-word counts, never detector outputs",
        "fitting_or_calibration_performed":false,"model_reselected":false,"final_test_opened":true});
    fs::create_dir(out)?;
    put(&out.join("test.json"), &report)?;
    put(&out.join("fixture-bindings.json"), &json!(bindings))?;
    put(
        &out.join("scores.json"),
        &json!({"fraction":raw,"CAL_mapped_involvement":mapped}),
    )?;
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_test_pangram_evaluation_v1",
        "complete":true,"meaning":"offline report complete; provider population coverage is separate",
        "pangram_population_complete":observed.len()==data.len(),"selection_receipt_sha256":seal_hash,
        "dataset_manifest_sha256":sha(&numeric.join("manifest.json"))?,"source_manifest_sha256":source_manifest_hash,
        "current_evaluation_manifest_sha256":sha(&evaluated.join("manifest.json"))?,
        "fixed_comparator_manifest_sha256":sha(&fixed.join("manifest.json"))?,"mapping_sha256":mapping_hash,
        "report_sha256":sha(&out.join("test.json"))?,"fixture_bindings_sha256":sha(&out.join("fixture-bindings.json"))?,
        "scores_sha256":sha(&out.join("scores.json"))?,"predictions":prediction_bindings,"network_calls":0,
        "fitting_or_calibration_performed":false,"model_reselected":false,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("compare_last_block_test_pangram.rs"))),
        "shared_metrics_sha256":hex::encode(Sha256::digest(include_bytes!("compare_last_block_pangram.rs"))),
        "mapping_loader_sha256":hex::encode(Sha256::digest(include_bytes!("compare_supervised_pangram.rs")))}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "STAGE FIXTURES_TEST_DIRECTORY NEW_OUTPUT");
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_provider_results_preserve_submission_uncertainty() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(missing_status(t.path()), "not_submitted");
        fs::write(t.path().join("intent.json"), b"{}").unwrap();
        assert_eq!(
            missing_status(t.path()),
            "uncertain_submission_do_not_repeat"
        );
        fs::write(t.path().join("submission.json"), b"{}").unwrap();
        assert_eq!(missing_status(t.path()), "submitted_without_usable_result");
        assert!(paired(&[], &[], &Scores::new()).unwrap().is_null());
    }
    #[test]
    fn changed_provider_version_or_request_scope_is_rejected() {
        let versions = BTreeSet::from(["frozen".to_owned()]);
        let good = json!({"split":"test","model":"pangram-4","input_sha256":"input",
            "manifest_sha256":"manifest","response":{"version":"frozen"}});
        validate_observation(&good, "input", "manifest", &versions).unwrap();
        for (pointer, value) in [
            ("/split", "calibration"),
            ("/response/version", "different"),
            ("/input_sha256", "different"),
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = json!(value);
            assert!(validate_observation(&bad, "input", "manifest", &versions).is_err());
        }
    }
}
