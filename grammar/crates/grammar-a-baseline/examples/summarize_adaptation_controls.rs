//! Selected warm residual vs matched single-task adaptation; numeric inputs only.
//! NUMERIC_TEXT JOINT_FITS_TEXT CONTROL_FITS CONTROL_GATE NEW_REPORT
#[allow(dead_code)]
#[path = "summarize_warm_start.rs"]
mod warm;

use anyhow::{Context, Result, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};
use warm::fusion::{balance_origins, difference, number, predictions, read, selected, sha};

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 5,
        "NUMERIC_TEXT JOINT_FITS_TEXT CONTROL_FITS CONTROL_GATE NEW_REPORT"
    );
    let (numeric, joint, controls, gate_path, output) = (
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
        Path::new(&args[4]),
    );
    ensure!(!output.exists(), "new report required");
    let gate = read(gate_path)?;
    let gate_sha = sha(gate_path)?;
    ensure!(
        gate["passed"] == true
            && gate["rows"] == 929
            && gate["final_test_opened"] == false
            && gate["bindings"]["text"]["manifest_sha256"] == sha(&numeric.join("manifest.json"))?,
        "unqualified control gate"
    );
    let mut reports = Vec::new();
    let mut rosters = BTreeMap::new();
    let mut bindings = BTreeMap::new();
    let mut all = BTreeMap::new();
    let mut architecture = None;
    let mut artifacts = BTreeMap::new();
    let mut control_protocol_sha = None;
    for part in ["development", "calibration"] {
        let (rows, binding) = warm::population(numeric, part)?;
        bindings.insert(part, binding.clone());
        for control in ["joint", "origin_only", "attribution_only"] {
            for seed in [17, 29, 41] {
                let dir = if control == "joint" {
                    joint.join(format!("joint-{seed}"))
                } else {
                    controls.join(format!("{control}-{seed}"))
                };
                let fit = read(&dir.join("fit.json"))?;
                let weights = read(&dir.join("weights.json"))?;
                let weights_sha = sha(&dir.join("weights.json"))?;
                ensure!(
                    fit["control"] == control
                        && fit["seed"] == seed
                        && fit["epochs"] == 250
                        && fit["query_weighting"] == "family_origin_balanced"
                        && fit["cohort_mixture"].is_null()
                        && weights["control"] == control
                        && weights["input_mode"] == "text"
                        && weights["schema"] == "slop_ninja_v6_residual_a_weights_v1"
                        && weights["contract_sha256"] == binding["contract_sha256"]
                        && fit["initialization_sha256"] == gate["initialization_sha256"]
                        && weights["initialization_sha256"] == gate["initialization_sha256"],
                    "unmatched adaptation fit"
                );
                let shape = json!([
                    weights["architecture"],
                    weights["text_mean"],
                    weights["text_scale"],
                    weights["base_temperature"],
                    weights["profiles"]
                ]);
                if let Some(ref previous) = architecture {
                    ensure!(*previous == shape, "capacity/profile/normalization differs");
                } else {
                    architecture = Some(shape);
                }
                ensure!(
                    fit["initial_equivalence"]["passed"] == true
                        && fit["initial_equivalence"]["rows"] == 929
                        && number(&fit["initial_equivalence"]["max_absolute_probability_error"])?
                            <= 1e-6,
                    "initial equivalence differs"
                );
                if control != "joint" {
                    let protocol = read(&dir.join("control-protocol.json"))?;
                    let protocol_sha = sha(&dir.join("control-protocol.json"))?;
                    ensure!(
                        fit["adaptation_control"] == true
                            && fit["outputs_diagnostic_only"] == true
                            && fit["control_based_test_selection"] == false
                            && fit["equivalence_receipt_sha256"] == gate_sha
                            && fit["implementations"] == gate["implementations"]
                            && fit["control_protocol_sha256"] == protocol_sha
                            && weights["control_protocol_sha256"] == protocol_sha
                            && protocol["equivalence_receipt_sha256"] == gate_sha
                            && protocol["status"] == "frozen"
                            && protocol["selected_by"] == "joint_Development_only"
                            && protocol["final_test_opened"] == false
                            && protocol["data_manifest_sha256"] == binding["manifest_sha256"],
                        "unbound adaptation protocol"
                    );
                    if let Some(ref previous) = control_protocol_sha {
                        ensure!(*previous == protocol_sha, "control protocols differ");
                    } else {
                        control_protocol_sha = Some(protocol_sha);
                    }
                }
                let eval = read(&dir.join(format!("{part}-report.json")))?;
                let pred_path = dir.join(format!("{part}-predictions.json"));
                warm::verify_report(&eval, &fit, &binding, part, &weights_sha, &sha(&pred_path)?)?;
                let preds = predictions(&pred_path)?;
                ensure!(
                    preds.len() == rows.len()
                        && rows
                            .iter()
                            .all(|r| preds.contains_key(r["id"].as_str().unwrap())),
                    "prediction support differs"
                );
                let rust_path = dir.join(format!("rust-{part}.json"));
                let rust = read(&rust_path)?;
                let rust_rows = rust["predictions"].as_array().context("Rust predictions")?;
                let rust_ids = rust_rows
                    .iter()
                    .map(|r| r["id"].as_str().context("Rust id"))
                    .collect::<Result<std::collections::BTreeSet<_>>>()?;
                ensure!(
                    number(&rust["max_absolute_delta_percentage_points"])? < 0.001
                        && rust_rows.len() == rows.len()
                        && rust_ids.len() == rows.len()
                        && rust_ids
                            .iter()
                            .copied()
                            .eq(preds.keys().map(String::as_str)),
                    "incomplete Rust replay"
                );
                let tag = format!("{control}:{seed}:{part}");
                all.insert(tag.clone(), preds);
                artifacts.insert(tag, json!({"weights_sha256":weights_sha,"fit_sha256":sha(&dir.join("fit.json"))?,
                    "predictions_sha256":sha(&pred_path)?,"evaluation_sha256":sha(&dir.join(format!("{part}-report.json")))?,
                    "rust_replay_sha256":sha(&rust_path)?}));
                reports.push(json!({"split":part,"control":control,"seed":seed,
                    "fit":selected(&fit,&["selected_epoch","epochs","supervision","available_train_supervision","training_status",
                        "development_selection","calibration_temperature","origin_calibrated","outputs_diagnostic_only",
                        "control_protocol_sha256","task_weights","initialization_scope","history"]),
                    "metrics":warm::evaluation_metrics(&eval),
                    "rust":selected(&rust,&["runtime","max_absolute_delta_percentage_points","latency","loaded_numeric_weight_bytes"])}));
            }
        }
        rosters.insert(part, rows);
    }
    let mut paired = Vec::new();
    for part in ["development", "calibration"] {
        for (control, metric) in [
            ("origin_only", "involvement_brier"),
            ("attribution_only", "human"),
            ("attribution_only", "ai"),
        ] {
            let mut samples = Vec::new();
            for seed in [17, 29, 41] {
                let values = difference(
                    &rosters[part],
                    &all[&format!("joint:{seed}:{part}")],
                    &all[&format!("{control}:{seed}:{part}")],
                    metric,
                )?;
                samples.push(if metric == "involvement_brier" {
                    balance_origins(&rosters[part], &values)?
                } else {
                    values
                });
            }
            paired.push(
                json!({"split":part,"comparison":format!("joint_minus_{control}"),"metric":metric,
                "preferred_sign":if metric=="involvement_brier" {"negative"} else {"positive"},
                "mean_over_fixed_seeds_then_family_bootstrap":warm::fixed_seed_summary(&samples)?}),
            );
        }
    }
    let result = json!({"schema":"slop_ninja_selected_residual_adaptation_summary_v1",
        "reports":reports,"paired":paired,"artifact_bindings":artifacts,"input_bindings":bindings,
        "control_equivalence_sha256":gate_sha,"control_protocol_sha256":control_protocol_sha,
        "source_sha256":hex::encode(Sha256::digest(include_bytes!("summarize_adaptation_controls.rs"))),
        "population":{"development":rosters["development"].len(),"calibration":rosters["calibration"].len(),
            "seeds":[17,29,41],"new_control_fits":6,"final_test_opened":false,"selection_changed":false},
        "limitations":["All controls inherit frozen V6 origin-supervised pretraining; attribution-only is adaptation-only",
            "Nonjoint heads have untrained outputs and are diagnostic controls, not complete A candidates",
            "Development selects checkpoints and CAL fits permitted calibration; neither is final Test",
            "Original Train has only 215 queries from 12 works; historical human proxies and pretrained exposure remain",
            "Paired intervals average fixed seeds before 1000 whole-family bootstrap draws; no rare-error guarantee",
            "No new detector calls or final-Test access; source mixture and independent data growth remain separate"]});
    warm::public_boundary(&result)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(&result)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
