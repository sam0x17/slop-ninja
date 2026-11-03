//! Aggregate the fixed 12-fit source-size curve and original selected text fits.
//! summarize_source_size_curve LARGE_ROOT ORIGINAL_ROOT NEW_REPORT.json
//! Numeric/evaluation receipts only; no prose, Test, inference or provider access.
#[allow(dead_code)]
#[path = "summarize_warm_start.rs"]
mod warm;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
use warm::fusion::{
    balance_origins, difference, family_summary, number, predictions, read, selected, sha, text,
};
const SEEDS: [u64; 3] = [17, 29, 41];
const PARTS: [&str; 2] = ["development", "calibration"];
const NESTED_SIZE_CONTRASTS: [(&str, &str); 3] = [
    ("24:text", "12:text"),
    ("all:text", "24:text"),
    ("all:text", "12:text"),
];
const STRATA: [&str; 5] = [
    "cmu-books-historical",
    "hc3-wiki-historical",
    "plos_one",
    "wikinews",
    "project_gutenberg_historical_novel",
];
type Pred = BTreeMap<String, Value>;
#[path = "summarize_source_size_curve/evidence.rs"]
mod evidence;
#[cfg(test)]
#[path = "summarize_source_size_curve/tests.rs"]
mod tests;

fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().with_context(|| format!("array {k}"))
}
fn count(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().with_context(|| format!("count {k}"))
}
fn digest(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn exact_predictions(rows: &[Value], pred: &Pred) -> Result<()> {
    ensure!(
        rows.len() == pred.len()
            && rows
                .iter()
                .all(|r| r["id"].as_str().is_some_and(|id| pred.contains_key(id))),
        "prediction roster differs"
    );
    Ok(())
}

// Average estimates, never confidence-interval endpoints. Equal seed weights do
// not turn the three fixed seeds into independent families or a seed-population CI.
fn mean_values(values: &[Value]) -> Result<Value> {
    ensure!(values.len() == 3, "three fixed seeds required");
    let first = &values[0];
    if first.is_number() {
        if !values.iter().all(Value::is_number) {
            return Ok(Value::Null);
        }
        return Ok(json!(
            values
                .iter()
                .map(number)
                .collect::<Result<Vec<_>>>()?
                .iter()
                .sum::<f64>()
                / 3.
        ));
    }
    if let Some(object) = first.as_object() {
        let mut out = serde_json::Map::new();
        for key in object.keys() {
            if key.contains("ci95")
                || key.contains("95ci")
                || key == "brier_95ci"
                || key == "calibration_bins"
            {
                continue;
            }
            if values.iter().all(|v| v.get(key).is_some()) {
                out.insert(
                    key.clone(),
                    mean_values(&values.iter().map(|v| v[key].clone()).collect::<Vec<_>>())?,
                );
            }
        }
        return Ok(Value::Object(out));
    }
    if let Some(a) = first.as_array() {
        ensure!(
            values
                .iter()
                .all(|v| v.as_array().is_some_and(|v| v.len() == a.len())),
            "seed aggregate array shape differs"
        );
        return Ok(Value::Array(
            (0..a.len())
                .map(|i| mean_values(&values.iter().map(|v| v[i].clone()).collect::<Vec<_>>()))
                .collect::<Result<_>>()?,
        ));
    }
    Ok(if values.iter().all(|v| v == first) {
        first.clone()
    } else {
        Value::Null
    })
}
fn learning_curve(fit: &Value, require_train: bool) -> Result<Value> {
    let mut points = Vec::new();
    for (i, p) in array(fit, "history")?.iter().enumerate() {
        ensure!(p["epoch"] == i * 25, "unexpected learning-curve epoch grid");
        let dev = number(&p["development_involvement_brier"])?;
        let mut point = json!({"epoch":p["epoch"],"development_involvement_brier":dev});
        if require_train || p.get("train_involvement_brier").is_some() {
            let train = number(&p["train_involvement_brier"])?;
            let loss = number(&p["train_joint_loss"])?;
            ensure!(
                (0.0..=1.0).contains(&train) && loss >= 0.,
                "invalid Train curve"
            );
            point["train_involvement_brier"] = json!(train);
            point["train_joint_loss"] = json!(loss);
            point["development_minus_train_brier"] = json!(dev - train);
        }
        ensure!((0.0..=1.0).contains(&dev), "invalid Development curve");
        points.push(point);
    }
    ensure!(points.len() == 11, "incomplete 0..250 learning curve");
    let checkpoint = points.iter().find(|p| p["epoch"] == fit["selected_epoch"]);
    Ok(
        json!({"points":points,"selected_epoch":fit["selected_epoch"],
        "selected_epoch_gap":checkpoint.and_then(|p|p.get("development_minus_train_brier")),
        "train_history_available":points[0].get("train_involvement_brier").is_some(),
        "meaning":"uncalibrated logging-grid losses with each fit's Train sampling weights; Development weights fixed; absent checkpoint gap is not interpolated"}),
    )
}
fn pair(rows: &[Value], left: &[&Pred], right: &[&Pred], metric: &str) -> Result<Value> {
    ensure!(left.len() == 3 && right.len() == 3, "paired fixed seeds");
    // The legacy helper calls its input count "documents", even when its
    // inputs are balanced family-by-origin cells. Correct only this projection.
    let with_counts = |mut summary: Value, cells: usize| {
        if metric == "involvement_brier" {
            summary["documents"] = json!(rows.len());
            summary["balanced_cells"] = json!(cells);
        }
        summary
    };
    let mut samples = Vec::new();
    let mut per_seed = Vec::new();
    for (i, seed) in SEEDS.iter().enumerate() {
        let values = match metric {
            "involvement_brier" => {
                balance_origins(rows, &difference(rows, left[i], right[i], metric)?)?
            }
            "share_mae_percentage_points" => warm::share_difference(rows, left[i], right[i])?,
            "human" | "ai" => difference(rows, left[i], right[i], metric)?,
            _ => anyhow::bail!("unknown paired metric"),
        };
        per_seed.push(
            json!({"seed":seed,"difference":with_counts(family_summary(&values), values.len())}),
        );
        samples.push(values);
    }
    let summary = if samples.iter().all(Vec::is_empty) {
        family_summary(&[])
    } else {
        warm::fixed_seed_summary(&samples)?
    };
    Ok(
        json!({"metric":metric,"difference":"left_minus_right","preferred_sign":if ["human","ai"].contains(&metric){"positive"}else{"negative"},
        "weighting":if metric=="involvement_brier"{"family_origin_balanced"}else{"equal_eligible_family"},
        "count_semantics":"documents counts eligible paired documents once, not once per seed; balanced_cells (Brier only) counts nonempty family-by-origin cells. Families remain the bootstrap resampling units.",
        "per_seed":per_seed,"mean_over_fixed_seeds_then_family_bootstrap":with_counts(summary, samples[0].len())}),
    )
}

struct Run {
    public: Value,
    pred: BTreeMap<String, Pred>,
    pangram: Pred,
    pangram_observations: Value,
    shape: Value,
}
fn model_comparisons(
    rows: &[Value],
    runs: &BTreeMap<String, Vec<Run>>,
    part: &str,
) -> Result<Vec<Value>> {
    let mut comparisons = Vec::new();
    for (left, right) in NESTED_SIZE_CONTRASTS.into_iter().chain([
        ("12:text", "original:text"),
        ("24:text", "original:text"),
        ("all:text", "original:text"),
        ("all:combined", "original:text"),
        ("all:combined", "all:text"),
    ]) {
        for metric in [
            "involvement_brier",
            "share_mae_percentage_points",
            "human",
            "ai",
        ] {
            let mut p = pair(
                rows,
                &runs[left].iter().map(|r| &r.pred[part]).collect::<Vec<_>>(),
                &runs[right]
                    .iter()
                    .map(|r| &r.pred[part])
                    .collect::<Vec<_>>(),
                metric,
            )?;
            p["left"] = json!(left);
            p["right"] = json!(right);
            p["split"] = json!(part);
            p["comparison_role"] = json!(if NESTED_SIZE_CONTRASTS.contains(&(left, right)) {
                "primary_data_size_fixed_stratum_mixture"
            } else if right == "original:text" {
                "original_selected_comparator_also_changes_stratum_mixture"
            } else {
                "cfg_addition_at_fixed_size_and_mixture"
            });
            comparisons.push(p);
        }
    }
    Ok(comparisons)
}
fn fit_run(
    dir: &Path,
    numeric: &Path,
    seed: u64,
    mode: &str,
    gate: &Value,
    gate_sha: &str,
    init_sha: &str,
    expected_rows: u64,
    rosters: &BTreeMap<String, Vec<Value>>,
    bindings: &BTreeMap<String, Value>,
    sampling: Option<(&Value, &str, &Value)>,
) -> Result<Run> {
    let fit = read(&dir.join("fit.json"))?;
    let weights = read(&dir.join("weights.json"))?;
    let weights_sha = sha(&dir.join("weights.json"))?;
    ensure!(
        fit["schema"] == "slop_ninja_v6_residual_a_fit_v1"
            && fit["seed"] == seed
            && fit["epochs"] == 250
            && fit["control"] == "joint"
            && fit["final_test_opened"] == false
            && fit["cohort_mixture"].is_null()
            && fit["query_weighting"] == "family_origin_balanced"
            && fit["initialization_sha256"] == init_sha
            && fit["equivalence_receipt_sha256"] == gate_sha
            && fit["implementations"] == gate["implementations"]
            && fit["input_binding"] == gate["bindings"][mode]
            && fit["weights_sha256"] == weights_sha
            && fit["checkpoint_sha256"] == sha(&dir.join("model.pt"))?,
        "fit/gate/budget binding differs"
    );
    ensure!(
        fit["development_selection"]["metric"] == "involvement_brier"
            && fit["development_selection"]["split"] == "development"
            && count(&fit, "selected_epoch")? <= 250,
        "checkpoint selection differs"
    );
    ensure!(
        weights["schema"] == "slop_ninja_v6_residual_a_weights_v1"
            && weights["input_mode"] == mode
            && weights["control"] == "joint"
            && weights["initialization_sha256"] == init_sha
            && weights["contract_sha256"] == bindings["development"]["contract_sha256"],
        "weight artifact differs"
    );
    let initial = warm::initial_check_population(&fit["initial_equivalence"], expected_rows)?;
    ensure!(
        read(&dir.join("initial-equivalence.json"))? == fit["initial_equivalence"],
        "initial receipt changed"
    );
    let mut sampling_view = Value::Null;
    if let Some((protocol, hash, view)) = sampling {
        ensure!(
            sha(&dir.join("sampling-protocol.json"))? == hash
                && read(&dir.join("sampling-protocol.json"))? == *protocol,
            "fit used another sampling protocol"
        );
        let receipt = read(&dir.join("source-size-sampling.json"))?;
        ensure!(
            fit["source_size_sampling"] == receipt
                && receipt["protocol_sha256"] == hash
                && receipt["level"] == protocol["level"]
                && receipt["selected_train_rows"] == view["selected_train_rows"]
                && receipt["selected_train_families"] == view["selected_train_families"]
                && receipt["available_train_rows"] == protocol["total_train_rows"]
                && receipt["unselected_rows_used_for_optimizer"] == false
                && receipt["normalization_changed"] == false
                && receipt["development_calibration_weights_changed"] == false
                && receipt["final_test_opened"] == false,
            "sampling receipt differs"
        );
        sampling_view = selected(
            &receipt,
            &[
                "schema",
                "level",
                "protocol_sha256",
                "selected_train_rows",
                "selected_train_families",
                "available_train_rows",
                "masked_task_stratum_mass",
                "unselected_rows_used_for_optimizer",
                "normalization_changed",
                "development_calibration_weights_changed",
                "final_test_opened",
            ],
        );
    } else {
        ensure!(
            fit["source_size_sampling"].is_null(),
            "original comparator must be unsampled"
        );
    }
    let mut public = json!({"seed":seed,"modality":mode,"sampling":sampling_view,"initial_equivalence":initial,
        "fit":selected(&fit,&["epochs","selected_epoch","query_weighting","development_selection","calibration_temperature","base_temperature",
            "supervision","available_train_supervision","training_status","elapsed_seconds","cuda_peak_allocated_bytes","cuda_memory_fraction_limit",
            "device","implementations","data_manifest_sha256","weights_sha256","checkpoint_sha256","initialization_sha256","equivalence_receipt_sha256"]),
        "learning_curve":learning_curve(&fit,sampling.is_some())?,"checksums":{"fit":sha(&dir.join("fit.json"))?,"weights":weights_sha,
            "initial_equivalence":sha(&dir.join("initial-equivalence.json"))?}});
    let mut pred = BTreeMap::new();
    for part in PARTS {
        let eval_path = dir.join(format!("{part}-report.json"));
        let pred_path = dir.join(format!("{part}-predictions.json"));
        let report = read(&eval_path)?;
        let preds = predictions(&pred_path)?;
        warm::verify_report(
            &report,
            &fit,
            &bindings[part],
            part,
            &weights_sha,
            &sha(&pred_path)?,
        )?;
        exact_predictions(&rosters[part], &preds)?;
        let rust_path = dir.join(format!("rust-{part}.json"));
        let rust = read(&rust_path)?;
        evidence::replay(&rust, &preds)?;
        ensure!(
            rust["artifact_json_bytes"] == fs::metadata(dir.join("weights.json"))?.len(),
            "replay artifact size differs"
        );
        public[part] = warm::evaluation_metrics(&report);
        public[part]["rust"] = selected(
            &rust,
            &[
                "runtime",
                "latency",
                "artifact_json_bytes",
                "loaded_numeric_weight_bytes",
                "max_absolute_delta_percentage_points",
            ],
        );
        public["checksums"][part] = json!({"evaluation":sha(&eval_path)?,"predictions_sha256":sha(&pred_path)?,"rust_replay":sha(&rust_path)?});
        pred.insert(part.into(), preds);
    }
    let pg_path = dir.join("pangram-isotonic.json");
    let pg = read(&pg_path)?;
    let (pg_pred, observations) = evidence::pangram(
        &pg,
        &weights,
        &weights_sha,
        &sha(&dir.join("fit.json"))?,
        numeric,
        &rosters["development"],
        &pred["development"],
        rosters["calibration"].len(),
    )?;
    public["pangram_development"] = warm::pangram_metrics(&pg);
    public["checksums"]["pangram"] = json!(sha(&pg_path)?);
    let shape = json!({"architecture":weights["architecture"],"text_mean":weights["text_mean"],"text_scale":weights["text_scale"],
        "base_temperature":weights["base_temperature"],"profiles":weights["profiles"]});
    Ok(Run {
        public,
        pred,
        pangram: pg_pred,
        pangram_observations: observations,
        shape,
    })
}

fn run(large: &Path, original: &Path, output: &Path) -> Result<()> {
    ensure!(!output.exists(), "new report required");
    let data = large.join("numeric-warm-v1");
    let old = original.join("numeric-warm-v1");
    let init_path = original.join("warm-start-v1/init.json");
    let init = read(&init_path)?;
    let init_sha = sha(&init_path)?;
    ensure!(
        init["schema"] == "slop_ninja_v6_classifier_init_v1"
            && init["original_dependency_and_reference_checks_passed"] == true
            && init["final_test_opened"] == false,
        "unqualified initializer"
    );
    let gate_path = large.join("equivalence-v1.json");
    let gate = read(&gate_path)?;
    let gate_sha = sha(&gate_path)?;
    // The selected original fits used a retained versioned receipt. Resolve its
    // exact recorded digest, never silently substitute the unversioned receipt.
    let original_fit = read(&original.join("fit-warm-v1/text/joint-17/fit.json"))?;
    let old_gate_path = evidence::gate_by_hash(
        &original.join("warm-start-v1"),
        text(&original_fit, "equivalence_receipt_sha256")?,
    )?;
    let old_gate = read(&old_gate_path)?;
    let old_gate_sha = sha(&old_gate_path)?;
    let total = count(
        &read(&data.join("text/manifest.json"))?["training_extension"],
        "total_rows",
    )?;
    ensure!(total > 929, "enlarged numeric population required");
    for (g, n) in [(&gate, total), (&old_gate, 929)] {
        ensure!(
            g["initialization_sha256"] == init_sha
                && g["rows"] == n
                && g["passed"] == true
                && g["final_test_opened"] == false,
            "initial gate binding"
        );
    }
    let mut rosters = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let mut manifests = BTreeMap::new();
    for (label, root) in [("large", &data), ("original", &old)] {
        for mode in ["text", "combined"] {
            let m = read(&root.join(mode).join("manifest.json"))?;
            let g = if label == "large" { &gate } else { &old_gate };
            warm::initial_check_population(
                &g["results"][mode],
                if label == "large" { total } else { 929 },
            )?;
            if label == "large" {
                ensure!(
                    m["training_extension"]["original_rows"] == 929
                        && m["training_extension"]["total_rows"] == total
                        && m["training_extension"]["parent_manifest_sha256"]
                            == sha(&old.join(mode).join("manifest.json"))?,
                    "changed original parent"
                );
            }
            let mut by_part = BTreeMap::new();
            for part in PARTS {
                let (rows, binding) = warm::population(&root.join(mode), part)?;
                if let Some(prior) = rosters.get(part) {
                    ensure!(*prior == rows, "Dev/CAL supervision/populations differ");
                } else {
                    rosters.insert(part.to_owned(), rows);
                }
                ensure!(
                    g["bindings"][mode]["manifest_sha256"] == binding["manifest_sha256"]
                        && g["bindings"][mode]["files_sha256"][format!("{part}.jsonl")]
                            == binding["numeric_rows_sha256"],
                    "gate population changed"
                );
                by_part.insert(part.to_owned(), binding);
            }
            bindings.insert(format!("{label}:{mode}"), by_part);
            manifests.insert(format!("{label}:{mode}"), m);
        }
    }
    let mut selections = BTreeMap::new();
    let mut views = BTreeMap::new();
    let mut selected_groups = BTreeMap::new();
    let mut selection_hashes = BTreeMap::new();
    let design_sha = sha(&large.join("learning-curve-v1/PROTOCOL.txt"))?;
    for level in ["12", "24", "all"] {
        let path = large.join(format!("learning-curve-v1/selections-v1/{level}.json"));
        let p = read(&path)?;
        let (view, groups) =
            evidence::selection(&p, level, &manifests, &bindings, &init_sha, &design_sha)?;
        selections.insert(level, p);
        views.insert(level, view);
        selected_groups.insert(level, groups);
        selection_hashes.insert(level, sha(&path)?);
    }
    ensure!(
        selected_groups["12"].is_subset(&selected_groups["24"])
            && selected_groups["24"].is_subset(&selected_groups["all"]),
        "non-nested selections"
    );
    let basis = |p: &Value| -> Result<Value> {
        Ok(json!(
            array(p, "rows")?
                .iter()
                .map(|r| selected(r, &["id", "source_group", "stratum"]))
                .collect::<Vec<_>>()
        ))
    };
    ensure!(
        basis(&selections["12"])? == basis(&selections["24"])?
            && basis(&selections["12"])? == basis(&selections["all"])?,
        "sampling populations/strata differ"
    );
    let mut runs = BTreeMap::<String, Vec<Run>>::new();
    let mut shape = None;
    let mut observations = None;
    for (tag, mode, level) in [
        ("original:text", "text", None),
        ("12:text", "text", Some("12")),
        ("24:text", "text", Some("24")),
        ("all:text", "text", Some("all")),
        ("all:combined", "combined", Some("all")),
    ] {
        let mut set = Vec::new();
        for seed in SEEDS {
            let (dir, root, g, gsha, n, label) = if let Some(l) = level {
                (
                    large.join(format!("fit-curve-v1/{l}/{mode}/joint-{seed}")),
                    data.join(mode),
                    &gate,
                    &gate_sha,
                    total,
                    "large",
                )
            } else {
                (
                    original.join(format!("fit-warm-v1/text/joint-{seed}")),
                    old.join("text"),
                    &old_gate,
                    &old_gate_sha,
                    929,
                    "original",
                )
            };
            let sampling = level.map(|l| (&selections[l], selection_hashes[l].as_str(), &views[l]));
            let r = fit_run(
                &dir,
                &root,
                seed,
                mode,
                g,
                gsha,
                &init_sha,
                n,
                &rosters,
                &bindings[&format!("{label}:{mode}")],
                sampling,
            )?;
            if let Some(prior) = &shape {
                ensure!(
                    *prior == r.shape,
                    "architecture/normalization/profiles differ"
                );
            } else {
                shape = Some(r.shape.clone());
            }
            if let Some(prior) = &observations {
                ensure!(
                    *prior == r.pangram_observations,
                    "Pangram observations/mapping differ"
                );
            } else {
                observations = Some(r.pangram_observations.clone());
            }
            set.push(r);
        }
        runs.insert(tag.into(), set);
    }
    let (v6, v6_public) = evidence::frozen_v6(original, &init, &rosters)?;
    let mut comparisons = Vec::new();
    for part in PARTS {
        comparisons.extend(model_comparisons(&rosters[part], &runs, part)?);
        for tag in runs.keys() {
            let left = runs[tag].iter().map(|r| &r.pred[part]).collect::<Vec<_>>();
            let mut p = pair(&rosters[part], &left, &[&v6[part]; 3], "involvement_brier")?;
            p["left"] = json!(tag);
            p["right"] = json!("frozen_v6");
            p["split"] = json!(part);
            comparisons.push(p);
            if part == "development" {
                let mut p = pair(
                    &rosters[part],
                    &left,
                    &runs[tag].iter().map(|r| &r.pangram).collect::<Vec<_>>(),
                    "involvement_brier",
                )?;
                p["left"] = json!(tag);
                p["right"] = json!("cal_isotonic_pangram");
                p["split"] = json!(part);
                comparisons.push(p);
            }
        }
    }
    let mut summaries = BTreeMap::new();
    for (tag, set) in runs {
        let values = set.into_iter().map(|r| r.public).collect::<Vec<_>>();
        let aggregate = values
            .iter()
            .map(|v| {
                selected(
                    v,
                    &[
                        "development",
                        "calibration",
                        "pangram_development",
                        "learning_curve",
                    ],
                )
            })
            .collect::<Vec<_>>();
        summaries.insert(tag,json!({"per_seed":values,"equal_seed_mean":mean_values(&aggregate)?,"mean_semantics":"arithmetic mean of three fixed-seed aggregate estimates; confidence intervals are never averaged"}));
    }
    let result = json!({"schema":"slop_ninja_source_size_curve_results_v1","complete":true,"planned_curve_fits":12,"observed_curve_fits":12,
        "original_selected_text_comparator_fits":3,"seeds":SEEDS,"fits":summaries,"paired_comparisons":comparisons,"frozen_v6":v6_public,
        "population":{"available_numeric_rows":total,"original_numeric_rows":929,"added_train_rows":total-929,
            "development_queries":rosters["development"].len(),"calibration_queries":rosters["calibration"].len(),"selections":views,
            "pretraining_exposure":"unknown; retained historical roots were included in V6 Train; new book acquisition does not establish absence",
            "human_evidence":"historical proxies, not authenticated unaided workflows","clean_holdout":false,"final_test_opened":false},
        "bindings":{"numeric":bindings,"sampling_protocol_sha256":selection_hashes,"design_protocol_sha256":design_sha,
            "initializer_sha256":init_sha,"equivalence_sha256":gate_sha,"original_equivalence_sha256":old_gate_sha,
            "summarizer_source_sha256":digest(include_bytes!("summarize_source_size_curve.rs")),
            "evidence_helper_sha256":digest(include_bytes!("summarize_source_size_curve/evidence.rs")),
            "metric_helper_sha256":digest(include_bytes!("summarize_warm_start.rs")),"bootstrap_helper_sha256":digest(include_bytes!("summarize_v6_fusion.rs"))},
        "uncertainty":"1000 source-family bootstrap draws, seed20261026; average paired differences over seeds17/29/41 before resampling; Brier balances observed origin cells per family. No seed-distribution uncertainty estimate.",
        "data_size_interpretation":"The three direct nested text-level contrasts are primary evidence for the data-size question under fixed original/added stratum loss masses, architecture and optimizer budget; involvement Brier is primary. Comparisons against original selected text also change the stratum mixture.",
        "limitations":["Development selected checkpoints; CAL fitted temperatures and named thresholds; Test remains closed",
            "Nested-level evidence is conditional on these sampled source strata, historical exposure and rewrite recipes; it is not a clean-holdout or general causal claim",
            "Learning-curve Train weights change with the selected population; original fits may lack recorded Train losses; no missing checkpoint gap is invented",
            "V6 comparator supplies origin only; Pangram fractions are not replacement-span share ground truth or native involvement probabilities",
            "Retrieval uses the retained independent Train gallery; no additional partition-withheld gallery qualification is inferred",
            "Rust replay covers the numeric adapter/heads, not full encoder or serving qualification; no provider or inference calls occurred"],
        "final_test_opened":false,"fitting_performed":false,"provider_calls":0});
    warm::public_boundary(&result)?;
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    serde_json::to_writer_pretty(&mut f, &result)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "LARGE_ROOT ORIGINAL_ROOT NEW_REPORT.json");
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}
