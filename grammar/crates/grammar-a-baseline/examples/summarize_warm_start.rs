//! Aggregate frozen warm-start evidence; no fitting, Test access or prose reads.
//! summarize_warm_start WARM_NUMERIC WARM_FITS OLD_NUMERIC OLD_FITS INIT EQUIVALENCE NEW_REPORT
#[allow(dead_code)]
#[path = "summarize_v6_fusion.rs"]
pub(crate) mod fusion;

use anyhow::{Context, Result, ensure};
use fusion::{
    balance_origins, difference, family_summary, number, predictions, read, selected, sha, text,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

const SEEDS: [u64; 3] = [17, 29, 41];
const PARTS: [&str; 2] = ["development", "calibration"];
type Predictions = BTreeMap<String, Value>;

pub(crate) fn population(root: &Path, part: &str) -> Result<(Vec<Value>, Value)> {
    ensure!(PARTS.contains(&part), "only Development/CAL are permitted");
    let manifest = read(&root.join("manifest.json"))?;
    ensure!(
        manifest["final_test_opened"] == false && manifest["test_used_for_selection"] == false,
        "Test input or selection forbidden"
    );
    ensure!(
        manifest["contract_file_sha256"] == sha(&root.join("contract.json"))?,
        "contract changed"
    );
    let path = root.join(format!("{part}.jsonl"));
    let input_sha = sha(&path)?;
    let bindings = manifest["splits"].as_array().context("split bindings")?;
    let matches = bindings
        .iter()
        .filter(|b| b["split"] == part)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1 && matches[0]["sha256"] == input_sha,
        "split binding differs"
    );
    let mut ids = BTreeSet::new();
    let mut rows = Vec::new();
    for line in fs::read_to_string(path)?.lines() {
        let mut row: Value = serde_json::from_str(line)?;
        ensure!(
            row["purpose"] == "query" && row["frozen_split"] == part,
            "query partition differs"
        );
        ensure!(
            ids.insert(text(&row, "id")?.to_owned()),
            "duplicate query ID"
        );
        ensure!(
            row["input"]["contract_sha256"] == manifest["contract_sha256"],
            "row contract differs"
        );
        row.as_object_mut().context("row object")?.remove("input");
        rows.push(row);
    }
    ensure!(
        !rows.is_empty() && matches[0]["rows"] == rows.len(),
        "population size differs"
    );
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok((
        rows,
        json!({"manifest_sha256":sha(&root.join("manifest.json"))?,
        "contract_file_sha256":manifest["contract_file_sha256"], "contract_sha256":manifest["contract_sha256"],
        "numeric_rows_sha256":input_sha}),
    ))
}

pub(crate) fn verify_report(
    report: &Value,
    fit: &Value,
    binding: &Value,
    part: &str,
    weights_sha: &str,
    prediction_sha: &str,
) -> Result<()> {
    ensure!(
        report["split"] == part
            && report["test_used_for_selection"] == false
            && fit["final_test_opened"] == false,
        "report uses Test or wrong split"
    );
    ensure!(
        report["fit"] == *fit
            && fit["weights_sha256"] == weights_sha
            && fit["data_manifest_sha256"] == binding["manifest_sha256"]
            && report["prediction_file_sha256"] == prediction_sha,
        "report/fit/prediction binding differs"
    );
    Ok(())
}

fn initial_check(value: &Value) -> Result<Value> {
    initial_check_population(value, 929)
}
pub(crate) fn initial_check_population(value: &Value, expected_rows: u64) -> Result<Value> {
    ensure!(
        value["passed"] == true
            && value["rows"] == expected_rows
            && number(&value["tolerance"])? == 1e-6,
        "incomplete initial equivalence check"
    );
    let error = number(&value["max_absolute_probability_error"])?;
    ensure!(
        (0.0..=1e-6).contains(&error),
        "initial probability equivalence failed"
    );
    Ok(selected(
        value,
        &[
            "rows",
            "passed",
            "tolerance",
            "max_absolute_probability_error",
            "device",
        ],
    ))
}

pub(crate) fn evaluation_metrics(report: &Value) -> Value {
    // Deliberately omit per-identity scores, gallery membership and query diagnostics.
    let mut attribution = serde_json::Map::new();
    let mut retrieval = serde_json::Map::new();
    for role in ["human", "ai"] {
        attribution.insert(
            role.into(),
            selected(
                &report["attribution"][role],
                &[
                    "training_status",
                    "conditional_accuracy",
                    "conditional_ce",
                    "conditional_mrr",
                    "conditional_top3",
                    "open_set",
                    "threshold",
                    "documented_present",
                    "documented_absent_not_scored",
                    "present_identity_unspecified_not_scored",
                    "presence_unspecified_not_scored",
                    "presence_cutoff",
                    "fallback_asserts_role_presence_or_gallery_absence",
                ],
            ),
        );
        retrieval.insert(
            role.into(),
            selected(
                &report["reference_matching"]["metrics"][role],
                &[
                    "diagnostic_only",
                    "gallery_identities",
                    "top1",
                    "top3",
                    "mrr",
                    "head_known_top1",
                    "head_withheld_top1",
                    "target_not_in_gallery",
                    "query_encoding_missing",
                    "zero_norm_queries",
                ],
            ),
        );
    }
    json!({"model":selected(&report["model"], &[
        "documents", "origin_labelled_documents", "source_families", "origin_order", "origin_counts",
        "origin_accuracy", "origin_brier_sum3", "involvement_brier", "involvement_brier_95ci", "ece10",
        "calibration_bins", "confusion_counts_truth_by_predicted", "human_fpr_at_0_5", "human_false_positive_rates",
        "false_positive_guarantee", "share"]),
        "family_origin_balanced":report["family_origin_balanced"],
        "uniform_origin_prior_balanced":report["uniform_origin_prior_balanced"],
        "attribution":attribution, "reference_matching":retrieval,
        "reference_contract":selected(&report["reference_matching"]["contract"], &[
            "schema", "reference_rows", "reference_used_for_training_or_normalization",
            "identity_authentication", "open_set_threshold_calibrated", "evaluation_partition"]),
        "weighting":"equal_source_family_within_each_metric_eligible_rows"})
}

pub(crate) fn pangram_metrics(pg: &Value) -> Value {
    let mut out = selected(
        pg,
        &[
            "split",
            "requested",
            "paired",
            "calibration_requested",
            "calibration_paired",
            "pangram",
            "slop_ninja_paired",
            "family_origin_balanced",
            "paired_brier_gap_slop_minus_pangram",
            "paired_brier_gap_95ci",
            "by_realized_share",
            "parity_established",
        ],
    );
    out["calibration"] = selected(
        &pg["calibration"],
        &[
            "method",
            "algorithm",
            "fit_split",
            "objective",
            "weighting",
            "weighted_CAL_brier_in_sample",
            "native_pangram_confidence",
            "meaning",
        ],
    );
    out
}

pub(crate) fn share_difference(
    rows: &[Value],
    a: &Predictions,
    b: &Predictions,
) -> Result<Vec<(String, f64)>> {
    ensure!(
        a.keys().eq(b.keys()) && a.len() == rows.len(),
        "paired share population differs"
    );
    rows.iter()
        .map(|row| {
            let id = text(row, "id")?;
            let truth = number(&row["targets"]["share"])?;
            ensure!((0.0..=1.0).contains(&truth), "invalid share target");
            let left = number(&a.get(id).context("left prediction missing")?["ai_share_percent"])?;
            let right =
                number(&b.get(id).context("right prediction missing")?["ai_share_percent"])?;
            ensure!(
                (0.0..=100.000001).contains(&left) && (0.0..=100.000001).contains(&right),
                "invalid share prediction"
            );
            Ok((
                text(row, "source_group")?.to_owned(),
                (left - truth * 100.).abs() - (right - truth * 100.).abs(),
            ))
        })
        .collect()
}

pub(crate) fn fixed_seed_summary(samples: &[Vec<(String, f64)>]) -> Result<Value> {
    ensure!(
        samples.len() == SEEDS.len() && !samples[0].is_empty(),
        "fixed three-seed support required"
    );
    ensure!(
        samples.iter().all(|s| s.len() == samples[0].len()),
        "seed support differs"
    );
    let averaged = (0..samples[0].len())
        .map(|i| {
            ensure!(
                samples.iter().all(|s| s[i].0 == samples[0][i].0),
                "seed family differs"
            );
            Ok((
                samples[0][i].0.clone(),
                samples.iter().map(|s| s[i].1).sum::<f64>() / SEEDS.len() as f64,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(family_summary(&averaged))
}

pub(crate) fn public_boundary(value: &Value) -> Result<()> {
    // Additional fail-closed guard on the intentionally selected aggregate fields.
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                ensure!(
                    ![
                        "id",
                        "identity",
                        "profile",
                        "profiles",
                        "source_group",
                        "root",
                        "path",
                        "records",
                        "predictions",
                        "query_diagnostics",
                        "conditional_top1_by_identity",
                        "per_identity_reference_rows",
                        "input_binding"
                    ]
                    .contains(&key.as_str()),
                    "private output field: {key}"
                );
                ensure!(key != "text" || !v.is_string(), "prose output field");
                public_boundary(v)?;
            }
        }
        Value::Array(values) => {
            for v in values {
                public_boundary(v)?;
            }
        }
        Value::String(v) => ensure!(
            !v.starts_with('/')
                && !v.contains("/home/")
                && !v.contains("grammar-private/")
                && !v.contains("author-pg")
                && !v.contains("author_work_v2:")
                && !v.starts_with("local-"),
            "private output string"
        ),
        _ => {}
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let scale = args.iter().any(|s| s == "--train-scale");
    let mixture = args.iter().any(|s| s == "--cohort-mixture");
    args.retain(|s| s != "--train-scale" && s != "--cohort-mixture");
    ensure!(
        args.len() == 7,
        "summarize_warm_start [--train-scale] [--cohort-mixture] WARM_NUMERIC WARM_FITS OLD_NUMERIC OLD_FITS INIT EQUIVALENCE NEW_REPORT"
    );
    let (data, fits, old_data, old_fits, init_path, gate_path, output) = (
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
        Path::new(&args[4]),
        Path::new(&args[5]),
        Path::new(&args[6]),
    );
    ensure!(!output.exists(), "new output required");
    let init = read(init_path)?;
    let init_sha = sha(init_path)?;
    let gate = read(gate_path)?;
    let gate_sha = sha(gate_path)?;
    let expected_rows = if scale || mixture {
        let m = read(&data.join("text/manifest.json"))?;
        let ext = &m["training_extension"];
        let n = ext["total_rows"].as_u64().context("scale rows")?;
        ensure!(
            n > 929
                && ext["original_rows"] == 929
                && ext["added_train_rows"] == n - 929
                && (if scale {
                    ext["parent_manifest_sha256"] == sha(&old_data.join("text/manifest.json"))?
                } else {
                    sha(&data.join("text/manifest.json"))?
                        == sha(&old_data.join("text/manifest.json"))?
                })
                && gate["schema"] == "slop_ninja_v6_residual_initial_equivalence_v2",
            "scale parent/gate differs"
        );
        n
    } else {
        929
    };
    ensure!(
        init["schema"] == "slop_ninja_v6_classifier_init_v1"
            && init["original_dependency_and_reference_checks_passed"] == true
            && init["final_test_opened"] == false
            && gate["initialization_sha256"] == init_sha
            && gate["passed"] == true
            && gate["rows"] == expected_rows
            && gate["final_test_opened"] == false,
        "initializer/gate differs"
    );
    let mut cpu_equivalence = serde_json::Map::new();
    let mut inputs = BTreeMap::new();
    let mut rosters = BTreeMap::<String, Vec<Value>>::new();
    let mut all_predictions = BTreeMap::<String, Predictions>::new();
    let mut hashes = BTreeMap::new();
    let mut reports = Vec::new();
    for mode in ["text", "combined"] {
        cpu_equivalence.insert(
            mode.into(),
            initial_check_population(&gate["results"][mode], expected_rows)?,
        );
        if scale || mixture {
            let m = read(&data.join(mode).join("manifest.json"))?;
            ensure!(
                m["training_extension"]["total_rows"] == expected_rows
                    && (if scale {
                        m["training_extension"]["parent_manifest_sha256"]
                            == sha(&old_data.join(mode).join("manifest.json"))?
                    } else {
                        sha(&data.join(mode).join("manifest.json"))?
                            == sha(&old_data.join(mode).join("manifest.json"))?
                    }),
                "scale modality parent differs"
            );
        }
        for (family, root) in [("warm", data), ("old", old_data)] {
            for part in PARTS {
                let numeric = root.join(mode);
                let (rows, binding) = population(&numeric, part)?;
                if let Some(previous) = rosters.get(part) {
                    ensure!(
                        *previous == rows,
                        "populations differ outside learned inputs"
                    );
                } else {
                    rosters.insert(part.to_owned(), rows);
                }
                if family == "warm" {
                    ensure!(
                        gate["bindings"][mode]["manifest_sha256"] == binding["manifest_sha256"]
                            && gate["bindings"][mode]["files_sha256"][format!("{part}.jsonl")]
                                == binding["numeric_rows_sha256"],
                        "numeric inputs changed after equivalence"
                    );
                }
                inputs.insert(format!("{family}:{mode}:{part}"), binding);
            }
        }
        for seed in SEEDS {
            let dir = fits.join(mode).join(format!("joint-{seed}"));
            let fit = read(&dir.join("fit.json"))?;
            let weights_sha = sha(&dir.join("weights.json"))?;
            let weights = read(&dir.join("weights.json"))?;
            ensure!(
                weights["schema"] == "slop_ninja_v6_residual_a_weights_v1"
                    && weights["input_mode"] == mode
                    && weights["initialization_sha256"] == init_sha
                    && fit["initialization_sha256"] == init_sha
                    && fit["equivalence_receipt_sha256"] == gate_sha
                    && fit["seed"] == seed
                    && fit["epochs"] == 250
                    && fit["control"] == "joint"
                    && fit["implementations"] == gate["implementations"]
                    && fit["query_weighting"] == "family_origin_balanced",
                "warm fit protocol differs"
            );
            if mixture {
                let receipt = read(&dir.join("cohort-mixture.json"))?;
                let protocol = read(&dir.join("mixture-protocol.json"))?;
                ensure!(
                    fit["cohort_mixture"] == receipt
                        && receipt["schema"] == "slop_ninja_v6_residual_cohort_mixture_v1"
                        && receipt["protocol_sha256"] == sha(&dir.join("mixture-protocol.json"))?
                        && receipt["original_cohort_mass"] == 0.5
                        && receipt["added_cohort_mass"] == 0.5
                        && receipt["apply_to"] == "train_only"
                        && receipt["normalization_changed"] == false
                        && receipt["development_calibration_weights_changed"] == false
                        && receipt["final_test_opened"] == false
                        && protocol["numeric_manifest_sha256"][mode]
                            == gate["bindings"][mode]["manifest_sha256"]
                        && protocol["initialization_sha256"] == init_sha,
                    "mixture receipt differs"
                );
            } else {
                ensure!(
                    fit["cohort_mixture"].is_null(),
                    "mixture fit requires explicit summary mode"
                );
            }
            let gpu_check = initial_check_population(&fit["initial_equivalence"], expected_rows)?;
            ensure!(
                read(&dir.join("initial-equivalence.json"))? == fit["initial_equivalence"],
                "initial check receipt differs"
            );
            let mut report = json!({"modality":mode, "control":"joint", "seed":seed, "initial_equivalence":gpu_check,
                "fit":selected(&fit, &["epochs", "selected_epoch", "query_weighting", "calibration_temperature", "base_temperature",
                    "development_selection", "supervision", "training_status", "elapsed_seconds", "cuda_peak_allocated_bytes",
                    "cuda_memory_fraction_limit", "torch", "cuda", "device", "implementations", "data_manifest_sha256",
                    "weights_sha256", "checkpoint_sha256", "initialization_sha256", "equivalence_receipt_sha256", "history", "cohort_mixture"])});
            for part in PARTS {
                let rows = &rosters[part];
                let report_path = dir.join(format!("{part}-report.json"));
                let prediction_path = dir.join(format!("{part}-predictions.json"));
                let eval = read(&report_path)?;
                let prediction_sha = sha(&prediction_path)?;
                let binding = &inputs[&format!("warm:{mode}:{part}")];
                verify_report(&eval, &fit, binding, part, &weights_sha, &prediction_sha)?;
                ensure!(
                    weights["contract_sha256"] == binding["contract_sha256"],
                    "weight contract differs"
                );
                let preds = predictions(&prediction_path)?;
                ensure!(
                    preds.len() == rows.len()
                        && rows
                            .iter()
                            .all(|r| preds.contains_key(r["id"].as_str().unwrap())),
                    "prediction population differs"
                );
                let rust_path = dir.join(format!("rust-{part}.json"));
                let rust = read(&rust_path)?;
                ensure!(
                    number(&rust["max_absolute_delta_percentage_points"])? < 0.001
                        && rust["predictions"]
                            .as_array()
                            .context("Rust replay rows")?
                            .len()
                            == rows.len(),
                    "Rust replay failed/incomplete"
                );
                let rust_ids = rust["predictions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| text(r, "id"))
                    .collect::<Result<BTreeSet<_>>>()?;
                ensure!(
                    rust_ids.len() == rows.len()
                        && rust_ids
                            .iter()
                            .copied()
                            .eq(preds.keys().map(String::as_str)),
                    "Rust replay IDs differ"
                );
                report[part] = evaluation_metrics(&eval);
                report[part]["rust"] = selected(
                    &rust,
                    &[
                        "latency",
                        "artifact_json_bytes",
                        "loaded_numeric_weight_bytes",
                        "max_absolute_delta_percentage_points",
                        "runtime",
                    ],
                );
                let key = format!("warm:{mode}:{seed}:{part}");
                all_predictions.insert(key.clone(), preds);
                hashes.insert(key, json!({"weights_sha256":weights_sha, "fit_sha256":sha(&dir.join("fit.json"))?,
                    "evaluation_sha256":sha(&report_path)?, "predictions_sha256":prediction_sha,
                    "rust_replay_sha256":sha(&rust_path)?, "initial_check_sha256":sha(&dir.join("initial-equivalence.json"))?}));

                let old_dir = old_fits.join(mode).join(format!("joint-{seed}"));
                let old_fit = read(&old_dir.join("fit.json"))?;
                let old_weights_sha = sha(&old_dir.join("weights.json"))?;
                let old_report_path = if scale || mixture {
                    old_dir.join(format!("{part}-report.json"))
                } else {
                    old_fits
                        .join(mode)
                        .join(format!("joint-{seed}-{part}.json"))
                };
                let old_report = read(&old_report_path)?;
                let old_prediction_path = old_dir.join(format!("{part}-predictions.json"));
                let old_prediction_sha = sha(&old_prediction_path)?;
                verify_report(
                    &old_report,
                    &old_fit,
                    &inputs[&format!("old:{mode}:{part}")],
                    part,
                    &old_weights_sha,
                    &old_prediction_sha,
                )?;
                ensure!(
                    old_fit["seed"] == seed && old_fit["control"] == "joint",
                    "old matched fit differs"
                );
                if scale || mixture {
                    let old_weights = read(&old_dir.join("weights.json"))?;
                    ensure!(
                        old_weights["schema"] == weights["schema"]
                            && old_weights["architecture"] == weights["architecture"]
                            && old_weights["text_mean"] == weights["text_mean"]
                            && old_weights["text_scale"] == weights["text_scale"]
                            && old_weights["initialization_sha256"] == init_sha
                            && old_fit["epochs"] == fit["epochs"]
                            && old_fit["cohort_mixture"].is_null()
                            && old_fit["query_weighting"] == fit["query_weighting"],
                        "unmatched scale capacity/budget/normalization"
                    );
                }
                let key = format!("old:{mode}:{seed}:{part}");
                all_predictions.insert(key.clone(), predictions(&old_prediction_path)?);
                hashes.insert(key, json!({"weights_sha256":old_weights_sha, "fit_sha256":sha(&old_dir.join("fit.json"))?,
                    "evaluation_sha256":sha(&old_report_path)?, "predictions_sha256":old_prediction_sha}));
            }
            let pg_path = dir.join("pangram-isotonic.json");
            let pg = read(&pg_path)?;
            ensure!(
                pg["weights_sha256"] == weights_sha
                    && pg["contract_sha256"] == weights["contract_sha256"]
                    && pg["split"] == "development"
                    && pg["test_used_for_selection"] == false
                    && pg["paired"] == rosters["development"].len()
                    && pg["requested"] == rosters["development"].len()
                    && pg["calibration_requested"] == rosters["calibration"].len()
                    && pg["calibration_paired"] == rosters["calibration"].len(),
                "Pangram bindings/support differ"
            );
            report["pangram_development"] = pangram_metrics(&pg);
            report["pangram_comparison_sha256"] = json!(sha(&pg_path)?);
            reports.push(report);
        }
    }
    let mut paired = Vec::new();
    for part in PARTS {
        let rows = &rosters[part];
        for (left, right, metric) in [
            ("warm:combined", "warm:text", "involvement_brier"),
            ("warm:text", "old:text", "involvement_brier"),
            ("warm:combined", "old:combined", "involvement_brier"),
            ("warm:combined", "warm:text", "share_mae_percentage_points"),
        ] {
            let mut samples = Vec::new();
            let mut seeds = Vec::new();
            for seed in SEEDS {
                let a = &all_predictions[&format!("{left}:{seed}:{part}")];
                let b = &all_predictions[&format!("{right}:{seed}:{part}")];
                let values = if metric == "involvement_brier" {
                    balance_origins(rows, &difference(rows, a, b, metric)?)?
                } else {
                    share_difference(rows, a, b)?
                };
                seeds.push(json!({"seed":seed, "difference":family_summary(&values)}));
                samples.push(values);
            }
            paired.push(json!({"split":part, "left":left, "right":right, "metric":metric,
                "weighting":if metric == "involvement_brier" {"family_origin_balanced"} else {"family"},
                "difference":"left_minus_right", "preferred_sign":"negative", "seeds":seeds,
                "mean_over_fixed_seeds_then_family_bootstrap":fixed_seed_summary(&samples)?}));
        }
    }
    let result = json!({"schema":if mixture {"slop_ninja_cohort_mixture_public_summary_v1"} else if scale {"slop_ninja_train_scale_public_summary_v1"} else {"slop_ninja_warm_start_public_summary_v1"}, "reports":reports, "paired":paired,
        "comparison":if mixture && scale {"warm = expanded Train with original/added cohort mass 50/50; old = original Train; identical residual architecture, initialization, normalization and optimizer-step budget"}
            else if mixture {"warm = original/added cohort mass 50/50; old = uniform family weighting over expanded Train; identical data, residual architecture, initialization, normalization and optimizer-step budget"}
            else if scale {"warm = expanded Train; old = original Train; identical residual architecture, initialization, normalization and optimizer-step budget"} else {"warm = V6 residual; old = earlier shared MLP"},
        "population":{"development_queries":rosters["development"].len(), "calibration_queries":rosters["calibration"].len(),
            "all_numeric_rows":expected_rows,"added_train_rows":expected_rows-929,
            "independent_train_reference_rows":12, "fixed_seeds":SEEDS, "warm_fits":6,
            "final_test_opened":false, "pretraining_exposure":"unknown", "clean_holdout":false},
        "initialization":{"sha256":init_sha, "equivalence_receipt_sha256":gate_sha, "cpu_equivalence":cpu_equivalence,
            "provenance":selected(&init, &["schema", "artifact_id", "artifact_manifest_sha256", "parent_artifact_id",
                "parent_manifest_sha256", "weight_file_sha256", "origin_order", "base_temperature", "dimension", "dtype",
                "runtime_profile", "original_dependency_and_reference_checks_passed", "exporter_sha256", "loader_bridge_sha256"])},
        "input_bindings":inputs, "artifact_bindings":hashes,
        "warm_bundle_manifest_sha256":sha(&data.join("manifest.json"))?, "old_bundle_manifest_sha256":sha(&old_data.join("manifest.json"))?,
        "summarizer_source_sha256":hex::encode(Sha256::digest(include_bytes!("summarize_warm_start.rs"))),
        "bootstrap_source_sha256":hex::encode(Sha256::digest(include_bytes!("summarize_v6_fusion.rs"))),
        "uncertainty":"1000 source-family bootstrap draws, seed20261026; average paired differences over the three fixed seeds before family resampling. Brier balances observed origin cells within each family; share MAE uses equal family weights. Bootstrap documents counts origin cells for balanced Brier, actual documents for share. Does not estimate seed-distribution uncertainty.",
        "privacy":"aggregate-only; no texts, profile names, per-document identities, filesystem paths or provider logs",
        "limitations":["Development selected checkpoints; CAL selected temperatures and profile thresholds; neither is final Test",
            "Historical author labels are proxies; no clean production history or pretraining exclusion established",
            if mixture && !scale {"Only Train cohort weighting differs in this comparison; the same expanded population retains historical V6 exposure, domain and rewrite-history limitations"}
                else if scale {"Additional historical source roots were exposed in original V6 Train; larger Train differs in size, domain composition and rewrite recipe; this is not a pure sample-count intervention"}
                else {"Warm and earlier shared models differ in initialization and architecture capacity"},
            "Rust replay qualifies numeric residual adapter and heads; full V6 encoder and end-to-end serving remain unqualified",
            "Retrieval uses independent Train references only; partition-withheld galleries are not qualified for this warm model",
            "Pangram comparison uses retained observations and a CAL-fitted isotonic mapping; no live calls or parity claim"]});
    public_boundary(&result)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(&result)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_share_is_percentage_point_mae_and_preserves_family_weights() {
        let rows = vec![
            json!({"id":"x", "source_group":"a", "targets":{"share":0.5}}),
            json!({"id":"y", "source_group":"b", "targets":{"share":0.0}}),
        ];
        let a = BTreeMap::from([
            ("x".into(), json!({"ai_share_percent":40.})),
            ("y".into(), json!({"ai_share_percent":10.})),
        ]);
        let b = BTreeMap::from([
            ("x".into(), json!({"ai_share_percent":30.})),
            ("y".into(), json!({"ai_share_percent":5.})),
        ]);
        let values = share_difference(&rows, &a, &b).unwrap();
        assert_eq!(family_summary(&values)["estimate"], -2.5);
        assert!(share_difference(&rows, &a, &BTreeMap::new()).is_err());
    }
    #[test]
    fn fixed_seed_averaging_does_not_treat_seeds_as_independent_families() {
        let samples = vec![
            vec![("a".into(), 1.), ("b".into(), -1.)],
            vec![("a".into(), 0.), ("b".into(), -2.)],
            vec![("a".into(), 2.), ("b".into(), 0.)],
        ];
        let result = fixed_seed_summary(&samples).unwrap();
        assert_eq!(result["estimate"], 0.);
        assert_eq!(result["families"], 2);
        assert_eq!(result["documents"], 2);
        assert_eq!(
            result,
            family_summary(&[("a".into(), 1.), ("b".into(), -1.)])
        );
        assert!(fixed_seed_summary(&samples[..2]).is_err());
    }
    #[test]
    fn projection_excludes_identity_diagnostics_paths_and_calibration_knots() {
        let private = json!({"model":{"documents":1}, "fit":{"path":"/home/private"},
            "attribution":{"human":{"conditional_accuracy":{"estimate":0.5}, "conditional_top1_by_identity":{"private_writer":1}}},
            "reference_matching":{"contract":{"per_identity_reference_rows":{"private_writer":2}},
                "metrics":{"human":{"top1":{"estimate":0.5}, "query_diagnostics":[{"id":"private_row"}]}}}});
        let public = evaluation_metrics(&private);
        public_boundary(&public).unwrap();
        assert!(!public.to_string().contains("private"));
        let pg = pangram_metrics(
            &json!({"records":[{"id":"private_row"}], "calibration":{"method":"isotonic_v1", "provenance":{"path":"/home/private"}, "knots":[0.1]}}),
        );
        public_boundary(&pg).unwrap();
        assert!(!pg.to_string().contains("private"));
        assert!(public_boundary(&json!({"id":"private_row"})).is_err());
        assert!(public_boundary(&json!({"value":"/home/private/file"})).is_err());
    }
    #[test]
    fn initial_gate_and_evaluation_bindings_fail_closed() {
        assert!(initial_check(&json!({"rows":928,"passed":true,"tolerance":1e-6,"max_absolute_probability_error":0.})).is_err());
        assert!(initial_check(&json!({"rows":929,"passed":true,"tolerance":1e-6,"max_absolute_probability_error":2e-6})).is_err());
        let extended = json!({"rows":1978,"passed":true,"tolerance":1e-6,"max_absolute_probability_error":1e-7});
        initial_check_population(&extended, 1978).unwrap();
        assert!(initial_check(&extended).is_err());
        assert!(initial_check_population(&extended, 1977).is_err());
        let fit =
            json!({"weights_sha256":"w", "data_manifest_sha256":"m", "final_test_opened":false});
        let mut report = json!({"fit":fit, "split":"development", "test_used_for_selection":false, "prediction_file_sha256":"p"});
        let binding = json!({"manifest_sha256":"m"});
        verify_report(&report, &fit, &binding, "development", "w", "p").unwrap();
        report["prediction_file_sha256"] = json!("changed");
        assert!(verify_report(&report, &fit, &binding, "development", "w", "p").is_err());
    }
}
