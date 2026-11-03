//! Freeze all completed joint candidates before opening the reserved final Test.
//! freeze_final_selection COHORT TRAIN_SCALE NEW_RECEIPT
#[path = "final_test_binding/mod.rs"]
mod binding;
use anyhow::{Context, Result, ensure};
use binding::{read, sha, text};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Bindings(BTreeMap<PathBuf, String>);
impl Bindings {
    fn add(&mut self, path: &Path) -> Result<String> {
        let path = fs::canonicalize(path)?;
        if let Some(hash) = self.0.get(&path) {
            return Ok(hash.clone());
        }
        let hash = sha(&path)?;
        self.0.insert(path, hash.clone());
        Ok(hash)
    }
    fn expected(&mut self, path: &Path, expected: &Value) -> Result<String> {
        let hash = self.add(path)?;
        ensure!(
            expected.as_str() == Some(hash.as_str()),
            "changed artifact: {}",
            path.display()
        );
        Ok(hash)
    }
    fn record(&mut self, path: &Path) -> Result<Value> {
        Ok(json!({"path":fs::canonicalize(path)?,"sha256":self.add(path)?}))
    }
}
fn metric(value: &Value) -> Result<f64> {
    let n = value.as_f64().context("missing metric")?;
    ensure!(
        n.is_finite() && (0.0..=1.).contains(&n),
        "invalid probability metric"
    );
    Ok(n)
}
fn rank(a: &Value, b: &Value) -> std::cmp::Ordering {
    a["mean_balanced_development_brier"]
        .as_f64()
        .unwrap()
        .total_cmp(&b["mean_balanced_development_brier"].as_f64().unwrap())
        .then_with(|| {
            a["mean_human_fpr_at_0_5"]
                .as_f64()
                .unwrap()
                .total_cmp(&b["mean_human_fpr_at_0_5"].as_f64().unwrap())
        })
        .then_with(|| {
            a["active_input_dimension"]
                .as_u64()
                .unwrap()
                .cmp(&b["active_input_dimension"].as_u64().unwrap())
        })
        .then_with(|| {
            a["candidate"]
                .as_str()
                .unwrap()
                .cmp(b["candidate"].as_str().unwrap())
        })
}
fn report_path(fit: &Path, part: &str) -> Result<PathBuf> {
    let inside = fit.join(format!("{part}-report.json"));
    let outside = fit.parent().context("fit parent")?.join(format!(
        "{}-{part}.json",
        fit.file_name().context("fit name")?.to_string_lossy()
    ));
    ensure!(
        inside.exists() ^ outside.exists(),
        "missing/ambiguous {part} report"
    );
    Ok(if inside.exists() { inside } else { outside })
}
fn replay_path(fit: &Path) -> Result<PathBuf> {
    let candidates = [
        fit.join("rust-development.json"),
        fit.join("rust-reference.json"),
        fit.parent().context("fit parent")?.join(format!(
            "{}-rust.json",
            fit.file_name().context("fit name")?.to_string_lossy()
        )),
    ];
    let present = candidates
        .into_iter()
        .filter(|p| p.exists())
        .collect::<Vec<_>>();
    ensure!(
        present.len() == 1,
        "missing/ambiguous Development Rust replay"
    );
    Ok(present[0].clone())
}
fn candidate(name: &str, numeric: &Path, fits: &Path, b: &mut Bindings) -> Result<Value> {
    let numeric = fs::canonicalize(numeric)?;
    let fits = fs::canonicalize(fits)?;
    let manifest = read(&numeric.join("manifest.json"))?;
    let contract = read(&numeric.join("contract.json"))?;
    ensure!(
        manifest["final_test_opened"] == false && manifest["excluded"] == 0,
        "Test opened or incomplete candidate inputs"
    );
    let mh = b.add(&numeric.join("manifest.json"))?;
    let ch = b.expected(
        &numeric.join("contract.json"),
        &manifest["contract_file_sha256"],
    )?;
    b.expected(
        &numeric.join("population.json"),
        &manifest["population_file_sha256"],
    )?;
    for split in manifest["splits"].as_array().context("numeric splits")? {
        let part = text(split, "split")?;
        ensure!(
            ["train", "development", "calibration", "reference"].contains(&part),
            "unexpected fitting split"
        );
        b.expected(&numeric.join(format!("{part}.jsonl")), &split["sha256"])?;
    }
    let dimension = contract["input_dimension"].as_u64().context("dimension")?;
    let mode = contract["ablation"].as_str().unwrap_or("");
    let active = if name.ends_with("text") || name == "text" {
        768
    } else {
        dimension
    };
    let mut runs = Vec::new();
    let mut mean_brier = 0.;
    let mut mean_fpr = 0.;
    for seed in [17, 29, 41] {
        let fit = fits.join(format!("joint-{seed}"));
        let f = read(&fit.join("fit.json"))?;
        let w = read(&fit.join("weights.json"))?;
        let warm = w["schema"] == "slop_ninja_v6_residual_a_weights_v1";
        ensure!(
            f["seed"] == seed
                && f["control"] == "joint"
                && f["query_weighting"] == "family_origin_balanced"
                && f["final_test_opened"] == false
                && f["data_manifest_sha256"] == mh,
            "fit protocol/input mismatch"
        );
        ensure!(
            f["training_status"]["origin"] == (if warm { "fine_tuned" } else { "trained" })
                && f["training_status"]["share_interpolation"] == "trained_fractional"
                && f["training_status"]["human"] == "trained"
                && f["training_status"]["ai"] == "trained",
            "candidate lacks required supervised heads"
        );
        ensure!(
            w["contract_sha256"] == manifest["contract_sha256"]
                && w["architecture"]["dimension"] == dimension,
            "model/contract mismatch"
        );
        if warm {
            ensure!(
                f["initial_equivalence"]["passed"] == true
                    && metric(&f["initial_equivalence"]["max_absolute_probability_error"])? <= 1e-6,
                "warm initializer equivalence failed"
            );
            let initial = fit.join("initial-equivalence.json");
            ensure!(
                read(&initial)? == f["initial_equivalence"],
                "initial receipt mismatch"
            );
            b.add(&initial)?;
        }
        if !f["source_size_sampling"].is_null() {
            b.expected(
                &fit.join("sampling-protocol.json"),
                &f["source_size_sampling"]["protocol_sha256"],
            )?;
            let sampling = fit.join("source-size-sampling.json");
            ensure!(
                read(&sampling)? == f["source_size_sampling"],
                "sampling receipt mismatch"
            );
            b.add(&sampling)?;
        }
        if !f["cohort_mixture"].is_null() {
            b.expected(
                &fit.join("mixture-protocol.json"),
                &f["cohort_mixture"]["protocol_sha256"],
            )?;
        }
        let weight_hash = b.expected(&fit.join("weights.json"), &f["weights_sha256"])?;
        let checkpoint_hash = b.expected(&fit.join("model.pt"), &f["checkpoint_sha256"])?;
        let fit_hash = b.add(&fit.join("fit.json"))?;
        b.add(&fit.join("reference-encodings.json"))?;
        // Preserve any qualified numeric binding, including extension/sampling provenance.
        if let Some(files) = f["input_binding"]["files_sha256"].as_object() {
            for (path, hash) in files {
                ensure!(
                    Path::new(path)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "unsafe numeric binding path"
                );
                b.expected(&numeric.join(path), hash)?;
            }
        }
        let dev = report_path(&fit, "development")?;
        let cal = report_path(&fit, "calibration")?;
        let dr = read(&dev)?;
        let cr = read(&cal)?;
        for (part, report) in [("development", &dr), ("calibration", &cr)] {
            ensure!(
                report["split"] == part
                    && report["test_used_for_selection"] == false
                    && report["fit"] == f,
                "evaluation binding mismatch"
            );
            b.expected(
                &fit.join(format!("{part}-predictions.json")),
                &report["prediction_file_sha256"],
            )?;
        }
        let rust = replay_path(&fit)?;
        let replay = read(&rust)?;
        ensure!(
            metric(&replay["max_absolute_delta_percentage_points"])? < 0.001,
            "Rust replay failed"
        );
        let score = metric(&dr["family_origin_balanced"]["involvement_brier"])?;
        let fpr = metric(&dr["model"]["human_fpr_at_0_5"])?;
        mean_brier += score / 3.;
        mean_fpr += fpr / 3.;
        runs.push(json!({"seed":seed,"model_path":fit,"weights_sha256":weight_hash,
            "checkpoint_sha256":checkpoint_hash,"fit_sha256":fit_hash,
            "development_report_path":dev,"development_report_sha256":b.add(&dev)?,
            "calibration_report_path":cal,"calibration_report_sha256":b.add(&cal)?,
            "rust_replay_path":rust,"rust_replay_sha256":b.add(&rust)?,
            "development_balanced_brier":score,"human_fpr_at_0_5":fpr,
            "share_mae_percentage_points":dr["model"]["share"]["mae_percentage_points"]["estimate"]}));
    }
    Ok(
        json!({"candidate":name,"numeric_path":numeric,"numeric_manifest_sha256":mh,
        "contract_file_sha256":ch,"contract_sha256":manifest["contract_sha256"],
        "active_input_dimension":active,"input_ablation":mode,
        "mean_balanced_development_brier":mean_brier,"mean_human_fpr_at_0_5":mean_fpr,
        "runs":runs}),
    )
}
fn require_curve(v: &Value) -> Result<()> {
    ensure!(
        v["schema"] == "slop_ninja_source_size_curve_results_v1"
            && v["complete"] == true
            && v["observed_curve_fits"] == 12
            && v["planned_curve_fits"] == 12
            && v["final_test_opened"] == false
            && v["seeds"] == json!([17, 29, 41]),
        "source-size comparison is incomplete"
    );
    Ok(())
}
fn run(cohort: &Path, scale: &Path, out: &Path, audit_existing: bool) -> Result<()> {
    ensure!(!out.exists(), "new immutable receipt required");
    let cohort = fs::canonicalize(cohort)?;
    let scale = fs::canonicalize(scale)?;
    let large = scale.join("large-batch-v1");
    let curve_path = large.join("results-curve-v1.json");
    let prior_path = cohort.join("selection-v2.json");
    let prior = read(&prior_path)?;
    ensure!(
        prior["schema"] == "slop_ninja_balanced_a_selection_freeze_v2"
            && prior["final_test_opened"] == false
            && prior["evaluation_seeds"] == json!([17, 29, 41]),
        "invalid prior selection"
    );
    let mut bindings = Bindings::default();
    let prior_hash = bindings.add(&prior_path)?;
    for (path, hash) in prior["protocol_sha256"]
        .as_object()
        .context("prior protocol hashes")?
    {
        bindings.expected(&cohort.join(path), hash)?;
    }
    let dataset_path = cohort.join("dataset-v1/manifest.json");
    let dataset = read(&dataset_path)?;
    ensure!(dataset["test_text_opened"] == false, "original Test opened");
    bindings.expected(&dataset_path, &prior["dataset_manifest_sha256"])?;
    // Hash the original fitting data without using its prose for model selection.
    bindings.expected(
        &cohort.join("dataset-v1/records.jsonl"),
        &prior["fitting_records_sha256"],
    )?;
    let mut candidates = Vec::new();
    for old in prior["candidates"].as_array().context("prior candidates")? {
        let numeric = Path::new(text(old, "numeric_path")?);
        let fits = Path::new(text(&old["runs"][0], "model_path")?)
            .parent()
            .context("fits root")?;
        let new = candidate(text(old, "candidate")?, numeric, fits, &mut bindings)?;
        for key in [
            "numeric_manifest_sha256",
            "contract_file_sha256",
            "contract_sha256",
            "mean_balanced_development_brier",
            "mean_human_fpr_at_0_5",
        ] {
            ensure!(new[key] == old[key], "prior candidate changed: {key}");
        }
        for (a, z) in new["runs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(old["runs"].as_array().context("old runs")?)
        {
            for key in [
                "seed",
                "weights_sha256",
                "checkpoint_sha256",
                "fit_sha256",
                "development_report_sha256",
                "rust_replay_sha256",
            ] {
                ensure!(a[key] == z[key], "prior fit changed: {key}");
            }
        }
        candidates.push(new);
    }
    ensure!(
        candidates.len() == 8,
        "all eight original configurations required"
    );
    for (prefix, fit_root) in [
        ("expanded", scale.join("fit-warm-v1")),
        ("mixture", scale.join("mixture-v1/fits")),
    ] {
        for mode in ["text", "combined"] {
            candidates.push(candidate(
                &format!("{prefix}_{mode}"),
                &scale.join("numeric-warm-v1").join(mode),
                &fit_root.join(mode),
                &mut bindings,
            )?);
        }
    }
    if audit_existing {
        let audit = json!({"schema":"slop_ninja_final_candidate_input_audit_v1",
            "complete":true,"candidate_configurations":12,"joint_fits":36,
            "final_selection_frozen":false,"test_acquisition_permitted":false,
            "final_test_opened":false,"pending_configurations":4,
            "candidates":candidates,"previous_selection_sha256":prior_hash,
            "binding_files":bindings.0.iter().map(|(p,h)|json!({"path":p,"sha256":h})).collect::<Vec<_>>()});
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(out)?;
        serde_json::to_writer_pretty(&mut f, &audit)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        println!(
            "{}",
            json!({"audited_configurations":12,"joint_fits":36,"test_acquisition_permitted":false})
        );
        return Ok(());
    }
    let curve = read(&curve_path)?;
    require_curve(&curve)?;
    for (level, mode) in [
        ("12", "text"),
        ("24", "text"),
        ("all", "text"),
        ("all", "combined"),
    ] {
        let c = candidate(
            &format!("scale_{level}_{mode}"),
            &large.join("numeric-warm-v1").join(mode),
            &large.join("fit-curve-v1").join(level).join(mode),
            &mut bindings,
        )?;
        let summary = &curve["fits"][format!("{level}:{mode}")]["per_seed"];
        ensure!(
            summary.as_array().is_some_and(|a| a.len() == 3),
            "curve fit roster"
        );
        for (r, s) in c["runs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(summary.as_array().unwrap())
        {
            ensure!(
                r["seed"] == s["seed"]
                    && r["fit_sha256"] == s["checksums"]["fit"]
                    && r["weights_sha256"] == s["checksums"]["weights"]
                    && r["development_report_sha256"]
                        == s["checksums"]["development"]["evaluation"]
                    && r["calibration_report_sha256"]
                        == s["checksums"]["calibration"]["evaluation"]
                    && r["rust_replay_sha256"] == s["checksums"]["development"]["rust_replay"],
                "source-size summary differs from candidate evidence"
            );
        }
        candidates.push(c);
    }
    for path in [
        scale.join("FIT_PROTOCOL.json"),
        scale.join("GENERATION_PROTOCOL.md"),
        scale.join("mixture-v1/PROTOCOL.json"),
        large.join("learning-curve-v1/PROTOCOL.txt"),
        large.join("pipeline-v1/FIT_PROTOCOL.json"),
        large.join("equivalence-v1.json"),
    ] {
        bindings.add(&path)?;
    }
    for level in ["12", "24", "all"] {
        bindings.add(&large.join(format!("learning-curve-v1/selections-v1/{level}.json")))?;
    }
    let source_plan = bindings.record(&cohort.join("proposed-sources.tsv"))?;
    bindings.add(&cohort.join("embeddings-v1/manifest.json"))?;
    let disposition =
        bindings.record(&large.join("final-evaluation-v1/CANDIDATE_DISPOSITION.md"))?;
    let source_results = bindings.record(&curve_path)?;
    let mapping_path = cohort.join("fit-warm-v1/text/joint-17/pangram-isotonic.json");
    let map = read(&mapping_path)?;
    ensure!(
        map["calibration"]["method"] == "isotonic_v1"
            && map["calibration"]["fit_split"] == "calibration"
            && map["calibration"]["provenance"]["development_used_for_fitting"] == false,
        "Pangram mapping was not fitted on CAL only"
    );
    let mut pangram_mapping = bindings.record(&mapping_path)?;
    pangram_mapping["method"] = json!("isotonic_v1");
    pangram_mapping["fit_split"] = json!("calibration");
    candidates.sort_by(rank);
    let receipt = json!({"schema":"slop_ninja_balanced_a_selection_freeze_v3",
        "selected_candidate":candidates[0]["candidate"],
        "selected_numeric_path":candidates[0]["numeric_path"],
        "selected_contract_sha256":candidates[0]["contract_sha256"],
        "representative_seed":17,"evaluation_seeds":[17,29,41],
        "selection":prior["selection"],"candidates":candidates,
        "previous_selection_sha256":prior_hash,
        "source_size_comparison_complete":true,"source_size_results":source_results,
        "source_plan":source_plan,"candidate_disposition":disposition,"pangram_mapping":pangram_mapping,
        "binding_files":bindings.0.iter().map(|(p,h)|json!({"path":p,"sha256":h})).collect::<Vec<_>>(),
        "planned_test":{"query_works":16,"reference_works":2,"human_queries":32,
            "generation_tasks":384,"requested_queries":416,"withheld_writer_references":4},
        "support_policy":{"v6_max_tokens_including_specials":2048,"truncation":false,
            "missing_predictions_imputed":false,"all_candidate_results_required":true,
            "preserve_requested_and_common_supported_populations":true,
            "cross_split_family_alias_text_or_prompt_collision":"stop; retain evidence; no silent Test-only removal"},
        "diagnostics_only":["single_task_controls","prior","availability_only"],
        "test_used_for_selection":false,"final_test_opened":false,
        "model_calibration_frozen":true,"grammar_frozen":true,
        "serving_qualified":false,"parity_established":false,
        "implementation_sha256":hex::encode(sha2::Sha256::digest(include_bytes!("freeze_final_selection.rs")))});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut file, &receipt)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(out.parent().context("output parent")?)?.sync_all()?;
    binding::validate_selection_receipt(out)?;
    println!(
        "{}",
        json!({"selected":receipt["selected_candidate"],"candidate_configurations":16,
        "joint_fits":48,"final_test_opened":false})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    let audit = a.first().is_some_and(|v| v == "--audit-existing");
    let args = if audit { &a[1..] } else { &a[..] };
    ensure!(
        args.len() == 3,
        "freeze_final_selection [--audit-existing] COHORT TRAIN_SCALE NEW_RECEIPT"
    );
    run(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        audit,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_curve_cannot_authorize_test() {
        assert!(require_curve(&json!({"complete":true})).is_err());
        let mut v = json!({"schema":"slop_ninja_source_size_curve_results_v1","complete":true,
            "observed_curve_fits":12,"planned_curve_fits":12,"final_test_opened":false,"seeds":[17,29,41]});
        require_curve(&v).unwrap();
        v["observed_curve_fits"] = json!(11);
        assert!(require_curve(&v).is_err());
    }
    #[test]
    fn prior_hash_changes_and_metric_missing_values_fail() {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("numeric.json");
        fs::write(&p, b"original").unwrap();
        let expected = json!(sha(&p).unwrap());
        fs::write(&p, b"mutated").unwrap();
        assert!(Bindings::default().expected(&p, &expected).is_err());
        assert!(metric(&Value::Null).is_err());
    }
    #[test]
    fn ranking_uses_mean_involvement_before_false_positives() {
        let a = json!({"candidate":"a","mean_balanced_development_brier":0.04,
            "mean_human_fpr_at_0_5":0.2,"active_input_dimension":768});
        let b = json!({"candidate":"b","mean_balanced_development_brier":0.05,
            "mean_human_fpr_at_0_5":0.0,"active_input_dimension":1});
        assert!(rank(&a, &b).is_lt());
        let mut c = a.clone();
        c["mean_human_fpr_at_0_5"] = json!(0.1);
        assert!(rank(&c, &a).is_lt());
    }
}
