//! Evaluate all six frozen A runs on sealed Test, without fitting or reselection.
//! STAGE_ROOT NEW_OUTPUT. Uses raw-v1 and numeric-v1 in final-test-v1.
#[allow(dead_code, unused_imports)]
#[path = "evaluate_last_block.rs"]
mod prior;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use anyhow::{Context, Result, ensure};
use prior::evaluation;
use seal::{read, sha, text};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
type Pred = BTreeMap<String, Value>;
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn pin(sealed: &Value, p: &Path) -> Result<String> {
    let p = p.canonicalize()?;
    let h = sha(&p)?;
    ensure!(
        sealed["binding_files"]
            .as_array()
            .context("sealed files")?
            .iter()
            .any(|v| v["path"] == p.to_str().unwrap() && v["sha256"] == h),
        "file absent from frozen seal"
    );
    Ok(h)
}
fn load(
    data: &Path,
    run: &Path,
    dm: &Value,
    cal: &Value,
    part: &str,
) -> Result<(Vec<Value>, Pred, Value)> {
    ensure!(["test", "reference"].contains(&part), "Test route only");
    let rm = read(&run.join(format!("{part}/manifest.json")))?;
    let name = format!("{part}.jsonl");
    let pp = run.join(format!("{part}/predictions.jsonl"));
    ensure!(
        rm["schema"] == "slop_ninja_last_block_test_raw_predictions_v1"
            && rm["complete"] == true
            && rm["final_test_opened"] == true
            && rm["test_used_for_selection"] == false
            && rm["partition"] == part
            && rm["selection_receipt_sha256"] == dm["selection_receipt_sha256"]
            && rm["selection_sha256"] == dm["selection_sha256"]
            && rm["contract_sha256"] == dm["contract_sha256"]
            && rm["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?
            && rm["fitting_dataset_manifest_sha256"] == dm["fitting_dataset_manifest_sha256"]
            && rm["partition_sha256"] == dm["files_sha256"][&name]
            && rm["partition_sha256"] == sha(&data.join(&name))?
            && rm["predictions_sha256"] == sha(&pp)?
            && rm["origin_logits_semantics"]
                == "original_classifier_linear_divided_by_base_temperature; no_CAL_temperature",
        "raw Test predictions are not bound to frozen inputs"
    );
    for k in ["condition", "seed", "selected_checkpoint_sha256"] {
        ensure!(rm[k] == cal[k], "mixed checkpoints");
    }
    let truth = rows(&data.join(name))?;
    let mut pred = Pred::new();
    for r in rows(&pp)? {
        ensure!(
            pred.insert(text(&r, "id")?.into(), r).is_none(),
            "duplicate prediction"
        );
    }
    ensure!(
        rm["requested"] == truth.len()
            && rm["supported"] == pred.len()
            && rm["unsupported"] == 0
            && pred.keys().cloned().collect::<BTreeSet<_>>()
                == truth
                    .iter()
                    .map(|r| Ok(text(r, "id")?.to_owned()))
                    .collect::<Result<BTreeSet<_>>>()?,
        "incomplete/extra prediction population"
    );
    for r in &truth {
        ensure!(
            r["frozen_split"] == "test"
                && r["purpose"] == if part == "test" { "query" } else { "reference" }
                && pred[text(r, "id")?]["prefix_sha256"] == r["prefix"]["sha256"],
            "wrong Test row/prefix"
        );
    }
    Ok((truth, pred, rm))
}
fn compare(data: &[Value], predictions: &BTreeMap<String, Vec<Pred>>) -> Result<Value> {
    let balanced_counts = |mut summary: Value| {
        summary["family_origin_cells"] = summary["documents"].clone();
        summary["documents"] = json!(data.len());
        summary
    };
    let mut per_condition = BTreeMap::new();
    let mut errors = BTreeMap::<String, Vec<(String, f64)>>::new();
    let mut balanced = BTreeMap::<String, Vec<(String, f64)>>::new();
    for (condition, runs) in predictions {
        ensure!(runs.len() == 3, "all three fixed seeds required");
        let mut loss = Vec::new();
        let mut cells = BTreeMap::<(String, u64), (f64, usize)>::new();
        for r in data {
            let y = r["targets"]["origin"].as_u64().context("origin label")?;
            let id = text(r, "id")?;
            let g = text(r, "source_group")?;
            let mut sum = 0.;
            for p in runs {
                let score = p[id]["ai_confidence_percent"].as_f64().context("score")? / 100.;
                ensure!(
                    score.is_finite() && (0.0..=1.).contains(&score),
                    "invalid confidence"
                );
                sum += (score - f64::from(y != 0)).powi(2);
            }
            let v = sum / 3.;
            loss.push((g.to_owned(), v));
            let cell = cells.entry((g.to_owned(), y)).or_default();
            cell.0 += v;
            cell.1 += 1;
        }
        let b = cells
            .into_iter()
            .map(|((g, _), (s, n))| (g, s / n as f64))
            .collect::<Vec<_>>();
        per_condition.insert(
            condition,
            json!({"family":evaluation::summary(&loss),
                "family_origin_balanced":balanced_counts(evaluation::summary(&b))}),
        );
        errors.insert(condition.clone(), loss);
        balanced.insert(condition.clone(), b);
    }
    let paired = |m: &BTreeMap<String, Vec<(String, f64)>>| -> Result<Value> {
        let a = &m["adapt_last_block"];
        let b = &m["frozen_last_block"];
        ensure!(
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0),
            "unpaired source population"
        );
        Ok(evaluation::summary(
            &a.iter()
                .zip(b)
                .map(|(x, y)| (x.0.clone(), x.1 - y.1))
                .collect(),
        ))
    };
    Ok(
        json!({"three_seed_mean_loss":per_condition,"adapt_minus_frozen":{
        "family":paired(&errors)?,"family_origin_balanced":balanced_counts(paired(&balanced)?)},
        "method":"mean each document loss across all three fixed seeds, then paired source-family bootstrap; not best seed or an ensemble",
        "test_reselection":false}),
    )
}
fn run(stage: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new evaluation directory required");
    let root = stage.join("final-test-v1");
    let seal_path = root.join("seal.json");
    let sealed = seal::validate(&seal_path)?;
    let seal_hash = sha(&seal_path)?;
    let data = root.join("numeric-v1");
    let dm = read(&data.join("manifest.json"))?;
    let c = read(&data.join("contract.json"))?;
    let fitting = stage.join("fit-bundle-v1");
    let selection = stage.join("calibration-run-v1/selection.json");
    ensure!(
        dm["schema"] == "slop_ninja_last_block_test_dataset_v1"
            && dm["complete"] == true
            && dm["final_test_opened"] == true
            && dm["normalization_refit"] == false
            && dm["profile_enrollment_changed"] == false
            && dm["test_used_for_selection"] == false
            && dm["selection_receipt_sha256"] == seal_hash
            && dm["contract_sha256"] == sha(&data.join("contract.json"))?
            && dm["contract_sha256"] == pin(&sealed, &fitting.join("contract.json"))?
            && dm["fitting_dataset_manifest_sha256"]
                == pin(&sealed, &fitting.join("manifest.json"))?
            && dm["selection_sha256"] == pin(&sealed, &selection)?,
        "Test dataset lineage changed"
    );
    fs::create_dir(out)?;
    let mut comparisons = BTreeMap::<String, Vec<Pred>>::new();
    let mut reports = Vec::new();
    let mut truth = None;
    for condition in ["adapt_last_block", "frozen_last_block"] {
        for seed in [17, 29, 41] {
            let name = format!("{condition}-seed-{seed}");
            let cal_root = stage.join("calibration-run-v1/runs").join(&name);
            let cal_path = cal_root.join("export/calibration.json");
            let cal_hash = pin(&sealed, &cal_path)?;
            let cal = read(&cal_path)?;
            ensure!(
                cal["fit_split"] == "calibration"
                    && cal["final_test_opened"] == false
                    && cal["condition"] == condition
                    && cal["seed"] == seed,
                "frozen calibration scope"
            );
            let raw = root.join("raw-v1").join(&name);
            let (queries, qraw, qm) = load(&data, &raw, &dm, &cal, "test")?;
            let (test_refs, test_ref_raw, trm) = load(&data, &raw, &dm, &cal, "reference")?;
            let (mut refs, mut ref_raw, rm) =
                prior::load_part(&fitting, &cal_root, &selection, &cal, "reference")?;
            refs.extend(test_refs);
            for (k, v) in test_ref_raw {
                ensure!(ref_raw.insert(k, v).is_none(), "reference ID overlap");
            }
            let mut pred = Pred::new();
            for (id, p) in qraw {
                pred.insert(id, prior::prediction(&p, &c, &cal)?);
            }
            let (gallery, scope) = prior::gallery(&refs, &ref_raw, &queries, "test")?;
            let report = json!({"partition":"test","condition":condition,"seed":seed,
            "requested_queries":dm["upstream_requested_queries"],"supported_queries":queries.len(),
            "requested_references":dm["upstream_requested_references"],"numeric_coverage":dm,
            "metrics":prior::metrics(&queries,&pred,&c,&cal)?,
            "reference_retrieval":evaluation::retrieval_metrics(&queries,&pred,&gallery,&c)?,"reference_scope":scope,
            "calibration_sha256":cal_hash,"raw_predictions_manifest":qm,"raw_reference_manifest":trm,
            "prior_reference_manifest":rm,"calibration_fitted_on_this_population":false,"model_reselected":false});
            let dir = out.join(&name);
            fs::create_dir(&dir)?;
            put(&dir.join("test-predictions.json"), &json!(pred))?;
            put(&dir.join("test.json"), &report)?;
            reports.push(json!({"condition":condition,"seed":seed,"report_sha256":sha(&dir.join("test.json"))?,
            "predictions_sha256":sha(&dir.join("test-predictions.json"))?}));
            if let Some(previous) = &truth {
                ensure!(previous == &queries, "seed/condition truth changed");
            } else {
                truth = Some(queries);
            }
            comparisons.entry(condition.into()).or_default().push(pred);
        }
    }
    let comparison = compare(&truth.context("no Test truth")?, &comparisons)?;
    put(&out.join("comparison.json"), &comparison)?;
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_test_evaluation_v1","complete":true,
        "selection_receipt_sha256":seal_hash,"dataset_manifest_sha256":sha(&data.join("manifest.json"))?,
        "selected_condition":sealed["selected_condition"],"test_used_for_selection":false,"fitting_performed":false,
        "reports":reports,"comparison_sha256":sha(&out.join("comparison.json"))?,
        "implementation_sha256":hex::encode(sha2::Sha256::digest(include_bytes!("evaluate_last_block_test.rs"))),
        "metrics_source_sha256":hex::encode(sha2::Sha256::digest(include_bytes!("evaluate_supervised.rs")))}),
    )?;
    println!(
        "{}",
        json!({"complete":true,"runs":6,"comparison":comparison})
    );
    Ok(())
}
use sha2::Digest;
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 2, "STAGE NEW_OUTPUT");
    run(Path::new(&a[0]), Path::new(&a[1]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_seed_comparison_averages_losses_and_pairs_families() {
        let data=vec![json!({"id":"human","source_group":"work","targets":{"origin":0}}),
            json!({"id":"ai","source_group":"work","targets":{"origin":1}})];
        let make=|p:f64|BTreeMap::from([("human".into(),json!({"ai_confidence_percent":100.*p})),
            ("ai".into(),json!({"ai_confidence_percent":100.}))]);
        let mut runs=BTreeMap::from([("adapt_last_block".into(),vec![make(0.2),make(0.4),make(0.6)]),
            ("frozen_last_block".into(),vec![make(0.5);3])]);
        let r=compare(&data,&runs).unwrap();
        let estimate=r["adapt_minus_frozen"]["family"]["estimate"].as_f64().unwrap();
        assert!((estimate-((0.04+0.16+0.36)/3.-0.25)/2.).abs()<1e-12);
        runs.get_mut("adapt_last_block").unwrap().pop();
        assert!(compare(&data,&runs).is_err());
    }
}
