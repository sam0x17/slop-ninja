//! Freeze the predeclared fresh-family selection before acquiring final Test.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn n(v: &Value) -> Result<f64> {
    let x = v.as_f64().context("missing metric")?;
    ensure!(x.is_finite(), "nonfinite metric");
    Ok(x)
}
fn cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
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
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 2 || (args.len() == 3 && args[2] == "--include-warm-start"),
        "freeze_balanced_selection COHORT NEW_RECEIPT [--include-warm-start]"
    );
    let include_warm = args.len() == 3;
    let root = Path::new(&args[0]);
    let out = Path::new(&args[1]);
    ensure!(!out.exists(), "new receipt required");
    let dataset = read(&root.join("dataset-v1/manifest.json"))?;
    ensure!(
        dataset["test_text_opened"] == false
            && dataset["records_sha256"] == sha(&root.join("dataset-v1/records.jsonl"))?,
        "cohort changed or Test opened"
    );
    let mut hashes = serde_json::Map::new();
    for path in [
        "PROTOCOL.md",
        "ORDERED_PROTOCOL.md",
        "SELECTION_POLICY.md",
        "proposed-sources.tsv",
        "generator-identities.json",
        "ORDERED_GATE.json",
    ] {
        hashes.insert(path.into(), json!(sha(&root.join(path))?));
    }
    ensure!(
        read(&root.join("ORDERED_GATE.json"))?["criterion_satisfied"] == true,
        "ordered gate missing"
    );
    if include_warm {
        for path in [
            "WARM_START_PROTOCOL.md",
            "WARM_START_AMENDMENT.md",
            "selection-v1.json",
            "warm-start-v1/init.json",
            "warm-start-v1/equivalence-v2.json",
        ] {
            hashes.insert(path.into(), json!(sha(&root.join(path))?));
        }
        let gate = read(&root.join("warm-start-v1/equivalence-v2.json"))?;
        ensure!(
            gate["passed"] == true
                && gate["rows"] == 929
                && gate["final_test_opened"] == false
                && gate["initialization_sha256"] == sha(&root.join("warm-start-v1/init.json"))?,
            "initial equivalence gate missing or changed"
        );
        let prior = read(&root.join("selection-v1.json"))?;
        ensure!(
            prior["final_test_opened"] == false
                && prior["fitting_records_sha256"] == dataset["records_sha256"],
            "prior selection population changed"
        );
    }
    let mut candidates = Vec::new();
    let mut configurations = vec![
        ("cfg_all", "numeric-cfg-v1/all", "fit-cfg-v1/all", true),
        (
            "cfg_leaf",
            "numeric-cfg-v1/leaf_grammar",
            "fit-cfg-v1/leaf_grammar",
            true,
        ),
        (
            "text",
            "numeric-fusion-v1/text",
            "fit-fusion-v1/text",
            false,
        ),
        (
            "combined",
            "numeric-fusion-v1/combined",
            "fit-fusion-v1/combined",
            false,
        ),
        (
            "ordered_cfg",
            "numeric-ordered-v1/ordered",
            "fit-ordered-v1/ordered",
            false,
        ),
        (
            "global_cfg_control",
            "numeric-ordered-v1/global",
            "fit-ordered-v1/global",
            false,
        ),
    ];
    if include_warm {
        configurations.extend([
            (
                "warm_text",
                "numeric-warm-v1/text",
                "fit-warm-v1/text",
                true,
            ),
            (
                "warm_combined",
                "numeric-warm-v1/combined",
                "fit-warm-v1/combined",
                true,
            ),
        ]);
    }
    for (name, numeric_path, fit_path, inside) in configurations {
        let warm = name.starts_with("warm_");
        let numeric = root.join(numeric_path);
        let fits = root.join(fit_path);
        let manifest = read(&numeric.join("manifest.json"))?;
        ensure!(
            manifest["final_test_opened"] == false && manifest["excluded"] == 0,
            "incomplete or Test numeric inputs"
        );
        let contract = read(&numeric.join("contract.json"))?;
        ensure!(
            manifest["contract_file_sha256"] == sha(&numeric.join("contract.json"))?,
            "contract file changed"
        );
        let dim = contract["input_dimension"]
            .as_u64()
            .context("input dimension")?;
        // Text controls have padded zero coordinates. Their active block is the
        // fixed 768 V6 activations; the combined model uses both blocks.
        let active = if name == "text" || name == "warm_text" {
            768
        } else {
            dim
        };
        let mut runs = Vec::new();
        let mut brier = 0.;
        let mut fpr = 0.;
        for seed in [17, 29, 41] {
            let tag = format!("joint-{seed}");
            let fit = fits.join(&tag);
            let report_path = if inside {
                fit.join("development-report.json")
            } else {
                fits.join(format!("{tag}-development.json"))
            };
            let rust_path = if warm {
                fit.join("rust-development.json")
            } else if inside {
                fit.join("rust-reference.json")
            } else {
                fits.join(format!("{tag}-rust.json"))
            };
            let report = read(&report_path)?;
            let f = read(&fit.join("fit.json"))?;
            let weights = read(&fit.join("weights.json"))?;
            ensure!(
                f["seed"] == seed
                    && f["control"] == "joint"
                    && f["query_weighting"] == "family_origin_balanced"
                    && f["final_test_opened"] == false,
                "wrong fit protocol"
            );
            ensure!(
                f["weights_sha256"] == sha(&fit.join("weights.json"))?
                    && f["checkpoint_sha256"] == sha(&fit.join("model.pt"))?
                    && f["data_manifest_sha256"] == sha(&numeric.join("manifest.json"))?,
                "fit bytes changed"
            );
            ensure!(
                weights["architecture"]["hidden"] == 64
                    && weights["architecture"]["embedding"] == (if warm { 768 } else { 32 })
                    && weights["architecture"]["dimension"] == dim,
                "architecture differs"
            );
            ensure!(
                weights["contract_sha256"] == manifest["contract_sha256"],
                "weights/contract mismatch"
            );
            ensure!(
                f["training_status"]["origin"] == (if warm { "fine_tuned" } else { "trained" })
                    && f["training_status"]["share_interpolation"] == "trained_fractional"
                    && f["training_status"]["human"] == "trained"
                    && f["training_status"]["ai"] == "trained",
                "partially supervised candidate"
            );
            if warm {
                let gate = read(&root.join("warm-start-v1/equivalence-v2.json"))?;
                let mode = if name == "warm_text" {
                    "text"
                } else {
                    "combined"
                };
                ensure!(
                    weights["schema"] == "slop_ninja_v6_residual_a_weights_v1"
                        && weights["input_mode"] == mode
                        && weights["architecture"]["raw_text_dimension"] == 768
                        && f["initial_equivalence"]["passed"] == true
                        && f["initial_equivalence"]["rows"] == 929
                        && n(&f["initial_equivalence"]["max_absolute_probability_error"])? <= 1e-6
                        && f["equivalence_receipt_sha256"]
                            == sha(&root.join("warm-start-v1/equivalence-v2.json"))?
                        && weights["initialization_sha256"] == gate["initialization_sha256"]
                        && gate["bindings"][mode]["manifest_sha256"]
                            == sha(&numeric.join("manifest.json"))?,
                    "warm initialization/inputs differ"
                );
            }
            ensure!(
                report["split"] == "development"
                    && report["test_used_for_selection"] == false
                    && report["fit"] == f
                    && report["prediction_file_sha256"]
                        == sha(&fit.join("development-predictions.json"))?,
                "report binding differs"
            );
            let rust = read(&rust_path)?;
            ensure!(
                n(&rust["max_absolute_delta_percentage_points"])? < 0.001,
                "Rust replay failed"
            );
            let score = n(&report["family_origin_balanced"]["involvement_brier"])?;
            let fp = n(&report["model"]["human_fpr_at_0_5"])?;
            brier += score / 3.;
            fpr += fp / 3.;
            runs.push(json!({"seed":seed,"model_path":fit,"development_report_sha256":sha(&report_path)?,"weights_sha256":f["weights_sha256"],"checkpoint_sha256":f["checkpoint_sha256"],"fit_sha256":sha(&fit.join("fit.json"))?,"rust_replay_sha256":sha(&rust_path)?,"development_balanced_brier":score,"human_fpr_at_0_5":fp,"share_mae_percentage_points":report["model"]["share"]["mae_percentage_points"]["estimate"]}));
        }
        candidates.push(json!({"candidate":name,"numeric_path":numeric,"numeric_manifest_sha256":sha(&numeric.join("manifest.json"))?,"contract_file_sha256":sha(&numeric.join("contract.json"))?,"contract_sha256":manifest["contract_sha256"],"active_input_dimension":active,"mean_balanced_development_brier":brier,"mean_human_fpr_at_0_5":fpr,"runs":runs}));
    }
    candidates.sort_by(cmp);
    let receipt = json!({"schema":"slop_ninja_balanced_a_selection_freeze_v1","selected_candidate":candidates[0]["candidate"],"selected_numeric_path":candidates[0]["numeric_path"],"selected_contract_sha256":candidates[0]["contract_sha256"],"representative_seed":17,"evaluation_seeds":[17,29,41],"selection":"lowest arithmetic mean Development family_origin_balanced involvement Brier over three fixed seeds; exact ties: human FPR, active dimension, name","candidates":candidates,"protocol_sha256":hashes,"fitting_records_sha256":dataset["records_sha256"],"dataset_manifest_sha256":sha(&root.join("dataset-v1/manifest.json"))?,"test_used_for_selection":false,"final_test_opened":false,"model_calibration_frozen":true,"grammar_frozen":true,"serving_qualified":false,"parity_established":false,"implementation_sha256":hex::encode(Sha256::digest(include_bytes!("freeze_balanced_selection.rs")))});
    let mut receipt = receipt;
    if include_warm {
        receipt["schema"] = json!("slop_ninja_balanced_a_selection_freeze_v2");
        receipt["warm_start_included"] = json!(true);
        receipt["previous_selection_sha256"] = json!(sha(&root.join("selection-v1.json"))?);
        receipt["same_architecture_comparison"] = json!(false);
        receipt["qualification"] = json!(
            "warm text/combined are capacity matched; comparison with original MLP also changes architecture; final Test remains unopened"
        );
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    f.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"selected":receipt["selected_candidate"],"mean_balanced_brier":receipt["candidates"][0]["mean_balanced_development_brier"],"test_opened":false})
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn involvement_priority_precedes_fpr_and_ties_are_deterministic() {
        let a = json!({"candidate":"a","mean_balanced_development_brier":0.05,"mean_human_fpr_at_0_5":0.1,"active_input_dimension":768});
        let b = json!({"candidate":"b","mean_balanced_development_brier":0.06,"mean_human_fpr_at_0_5":0.,"active_input_dimension":100});
        assert!(cmp(&a, &b).is_lt());
        let mut c = a.clone();
        c["mean_human_fpr_at_0_5"] = json!(0.);
        assert!(cmp(&c, &a).is_lt());
    }
}
