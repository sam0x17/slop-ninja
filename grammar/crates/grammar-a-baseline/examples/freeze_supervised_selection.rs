//! Select an input ablation using repeated-seed Development evidence, before Test.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 3,
        "freeze_supervised_selection FIT_ROOT FITTING_RECORDS NEW_FREEZE.json"
    );
    let root = Path::new(&a[0]);
    let mut comparisons = Vec::new();
    let mut candidates = Vec::new();
    for ablation in ["all", "structure", "leaf_grammar", "availability"] {
        let controls = if ablation == "all" {
            vec!["joint", "origin_only", "attribution_only"]
        } else {
            vec!["joint"]
        };
        for control in controls {
            let mut seeds = Vec::new();
            let mut scores = Vec::new();
            for seed in [17, 29, 41] {
                let tag = format!("{control}-{seed}");
                let fit = root.join(ablation).join(&tag);
                let report_path = root.join(ablation).join(format!("{tag}-development.json"));
                let report = read(&report_path)?;
                let f = read(&fit.join("fit.json"))?;
                ensure!(
                    report["split"] == "development"
                        && report["test_used_for_selection"] == false
                        && f["final_test_opened"] == false,
                    "Test used in selection"
                );
                let brier = report["model"]["involvement_brier"]
                    .as_f64()
                    .context("Brier")?;
                ensure!(brier.is_finite(), "Brier finite");
                scores.push(brier);
                seeds.push(json!({"seed":seed,"development_involvement_brier":brier,"human_fpr_at_0_5":report["model"]["human_fpr_at_0_5"],"share_mae":report["model"]["share"]["mae_percentage_points"]["estimate"],"human_top1":report["attribution"]["human"]["conditional_accuracy"]["estimate"],"ai_top1":report["attribution"]["ai"]["conditional_accuracy"]["estimate"],"report_sha256":sha(&report_path)?,"weights_sha256":sha(&fit.join("weights.json"))?,"fit_sha256":sha(&fit.join("fit.json"))?,"selected_epoch":f["selected_epoch"]}));
            }
            let mean = scores.iter().sum::<f64>() / scores.len() as f64;
            let sd = (scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>()
                / (scores.len() - 1) as f64)
                .sqrt();
            comparisons.push(json!({"ablation":ablation,"control":control,"mean_development_brier":mean,"seed_standard_deviation":sd,"runs":seeds}));
            if control == "joint" && ablation != "availability" {
                candidates.push((mean, ablation));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    let selected = candidates[0].1;
    let fit = root.join(selected).join("joint-17");
    let numeric = root.join(selected).join("numeric");
    let weights = read(&fit.join("weights.json"))?;
    let result = json!({"schema":"slop_ninja_a_selection_freeze_v1","selected_ablation":selected,"selected_control":"joint","representative_seed":17,"representative_seed_policy":"first predeclared seed, not best seed","selection_metric":"lowest mean family-weighted calibrated Development involvement Brier across seeds17,29,41; no Test","eligible_ablations":["all","structure","leaf_grammar"],"availability_only":"diagnostic measurement-status control, not a syntax/leaf candidate","fitting_records_sha256":sha(Path::new(&a[1]))?,"selected_model_path":fit,"selected_numeric_path":numeric,"selected_weights_sha256":sha(&fit.join("weights.json"))?,"selected_checkpoint_sha256":sha(&fit.join("model.pt"))?,"selected_calibration_sha256":sha(&fit.join("fit.json"))?,"numeric_contract_file_sha256":sha(&numeric.join("contract.json"))?,"numeric_contract_sha256":weights["contract_sha256"],"architecture":weights["architecture"],"comparisons":comparisons,"test_used_for_selection":false,"final_test_opened":false,"parity_established":false,"comparison_scope":"historical human proxies; explicit conservative grammar-development exposure; 16 Development source families"});
    ensure!(!Path::new(&a[2]).exists(), "freeze output exists");
    fs::write(&a[2], serde_json::to_vec_pretty(&result)?)?;
    println!(
        "{}",
        json!({"selected":selected,"mean_development_brier":candidates[0].0,"representative_seed":17,"freeze":a[2]})
    );
    Ok(())
}
