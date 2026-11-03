//! Freeze the two-condition, three-seed decision using Development only.
//! FIT_BUNDLE FITS_ROOT NEW_SELECTION.json. Does not read CAL or Test examples.
#[allow(dead_code, unused_imports)]
#[path = "evaluate_supervised.rs"]
mod evaluation;
#[allow(dead_code, unused_imports)]
#[path = "summarize_warm_start.rs"]
mod warm;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
use warm::fusion::{balance_origins, difference, number, read, sha, text};
const SEEDS: [u64; 3] = [17, 29, 41];
const CONDITIONS: [&str; 2] = ["frozen_last_block", "adapt_last_block"];
type Pred = BTreeMap<String, Value>;

fn selected_epoch(history: &Value) -> Result<(u64, f64)> {
    let rows = history.as_array().context("learning history")?;
    ensure!(rows.len() == 13, "full frozen epoch budget required");
    let mut best = (0, f64::INFINITY);
    for (i, row) in rows.iter().enumerate() {
        ensure!(row["epoch"] == i, "history missing/repeated epoch");
        let score = number(&row["development_involvement_brier"])?;
        ensure!((0.0..=1.).contains(&score), "invalid Development score");
        if score < best.1 {
            best = (i as u64, score);
        }
    }
    Ok(best)
}
fn winner(means: &[f64; 2]) -> usize {
    usize::from(means[1] < means[0])
}

fn run(data: &Path, fits: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new selection required");
    let manifest = read(&data.join("manifest.json"))?;
    ensure!(
        manifest["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && manifest["complete"] == true
            && manifest["final_test_opened"] == false,
        "frozen non-Test bundle required"
    );
    ensure!(
        sha(&data.join("contract.json"))? == manifest["contract_sha256"]
            && sha(&data.join("protocol.json"))? == manifest["protocol_sha256"]
            && sha(&data.join("development.jsonl"))?
                == manifest["files_sha256"]["development.jsonl"],
        "dataset binding changed"
    );
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    for line in fs::read_to_string(data.join("development.jsonl"))?.lines() {
        let row: Value = serde_json::from_str(line)?;
        ensure!(
            row["purpose"] == "query"
                && row["frozen_split"] == "development"
                && ids.insert(text(&row, "id")?.to_owned()),
            "wrong/duplicate Development row"
        );
        rows.push(row);
    }
    ensure!(!rows.is_empty(), "empty Development");
    let mut runs = Vec::new();
    let mut predictions = BTreeMap::<(String, u64), Pred>::new();
    let mut means = [0.; 2];
    let mut source_binding = None;
    for (c, condition) in CONDITIONS.iter().enumerate() {
        for seed in SEEDS {
            let root = fits.join(format!("{condition}-seed-{seed}"));
            let fit = read(&root.join("fit.json"))?;
            let binding = read(&root.join("binding.json"))?;
            ensure!(
                fit["schema"] == "slop_ninja_last_block_a_fit_v1"
                    && fit["complete"] == true
                    && fit["real_data_fit"] == true
                    && fit["final_test_opened"] == false
                    && fit["calibrated"] == false
                    && fit["epochs"] == 12
                    && fit["binding"] == binding,
                "incomplete or wrongly scoped fit"
            );
            ensure!(
                binding["condition"] == *condition
                    && binding["seed"] == seed
                    && binding["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?
                    && binding["contract_sha256"] == manifest["contract_sha256"]
                    && binding["protocol_sha256"] == manifest["protocol_sha256"]
                    && fit["selected_checkpoint_sha256"] == sha(&root.join("selected.pt"))?
                    && fit["last_layer_sha256"] == sha(&root.join("last-layer.safetensors"))?
                    && fit["shared_heads_sha256"] == sha(&root.join("shared-heads.safetensors"))?,
                "checkpoint/input binding mismatch"
            );
            let mut common = binding.clone();
            common
                .as_object_mut()
                .context("binding")?
                .remove("condition");
            common.as_object_mut().unwrap().remove("seed");
            if let Some(ref expected) = source_binding {
                ensure!(*expected == common, "unmatched runtimes/implementations");
            } else {
                source_binding = Some(common);
            }
            let history = read(&root.join("history.json"))?;
            let (epoch, score) = selected_epoch(&history)?;
            ensure!(
                fit["selected_epoch"] == epoch
                    && (number(&fit["development_involvement_brier"])? - score).abs() < 1e-12,
                "epoch choice disagrees with frozen rule"
            );
            let supplied = read(&root.join("development-predictions.json"))?;
            let mut p = Pred::new();
            for row in supplied.as_array().context("predictions")? {
                ensure!(
                    row["calibrated"] == false
                        && p.insert(text(row, "id")?.to_owned(), row.clone()).is_none(),
                    "prediction duplicate/calibration mismatch"
                );
            }
            ensure!(
                p.keys().cloned().collect::<BTreeSet<_>>() == ids,
                "Development coverage differs"
            );
            let balanced = evaluation::balanced_origin_metrics(&rows, &p, None)?;
            ensure!(
                (number(&balanced["involvement_brier"])? - score).abs() < 1e-6,
                "recomputed Development criterion differs"
            );
            let metrics = evaluation::origin_share_metrics(&rows, &p, None)?;
            means[c] += score / 3.;
            runs.push(json!({"condition":condition,"seed":seed,"selected_epoch":epoch,
                "criterion":score,"family_origin_balanced":balanced,"family":metrics,
                "trained_selected_heads":fit["trained_selected_heads"],"learning_history":history,
                "fit_manifest_sha256":sha(&root.join("fit.json"))?,
                "selected_checkpoint_sha256":fit["selected_checkpoint_sha256"],
                "last_layer_sha256":fit["last_layer_sha256"],"shared_heads_sha256":fit["shared_heads_sha256"],
                "development_predictions_sha256":sha(&root.join("development-predictions.json"))?}));
            predictions.insert((condition.to_string(), seed), p);
        }
    }
    let mut paired = Vec::new();
    for seed in SEEDS {
        let diff = difference(
            &rows,
            &predictions[&(CONDITIONS[1].into(), seed)],
            &predictions[&(CONDITIONS[0].into(), seed)],
            "involvement_brier",
        )?;
        paired.push(balance_origins(&rows, &diff)?);
    }
    let mut comparison = warm::fixed_seed_summary(&paired)?;
    // The helper receives family/origin cells, not original documents.
    comparison["family_origin_cells"] = comparison["documents"].clone();
    comparison["documents"] = json!(rows.len());
    let report = json!({"schema":"slop_ninja_last_block_condition_selection_v1","complete":true,
        "selection_split":"development","final_test_opened":false,"calibration_read_for_selection":false,
        "conditions":CONDITIONS,"seeds":SEEDS,"selected_condition":CONDITIONS[winner(&means)],
        "mean_selected_development_brier":{CONDITIONS[0]:means[0],CONDITIONS[1]:means[1]},
        "selection_rule":"lowest_mean_over_three_fixed_seeds_of_minimum_epoch_Development_Brier; frozen_on_tie",
        "uncalibrated":true,"representative_seed":17,"runs":runs,
        "adapt_minus_frozen_paired_source_family_interval":comparison,
        "dataset_manifest_sha256":sha(&data.join("manifest.json"))?,
        "contract_sha256":manifest["contract_sha256"],"protocol_sha256":manifest["protocol_sha256"],
        "implementation_sha256":hex::encode(sha2::Sha256::digest(include_bytes!("select_last_block_condition.rs"))),
        "limits":["Development-selected evidence, not final-Test performance.",
            "Uncalibrated share and conditional outputs are diagnostic until CAL and independent evaluation.",
            "Bootstrap resamples source works after averaging fixed seeds; training/calibration uncertainty is not included."]});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"selected_condition":report["selected_condition"],"mean_development_brier":report["mean_selected_development_brier"]})
    );
    Ok(())
}
use sha2::Digest;
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "FIT_BUNDLE FITS_ROOT NEW_SELECTION.json");
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}
#[cfg(test)]
mod selector_tests {
    use super::*;
    #[test]
    fn fixed_epoch_rule_keeps_earliest_tie_and_rejects_partial_budget() {
        let mut h = (0..13)
            .map(|i| json!({"epoch":i,"development_involvement_brier":0.2}))
            .collect::<Vec<_>>();
        assert_eq!(selected_epoch(&json!(h)).unwrap(), (0, 0.2));
        h[4]["development_involvement_brier"] = json!(0.1);
        h[7]["development_involvement_brier"] = json!(0.1);
        assert_eq!(selected_epoch(&json!(h)).unwrap(), (4, 0.1));
        h.pop();
        assert!(selected_epoch(&json!(h)).is_err());
        assert_eq!(winner(&[0.1, 0.1]), 0);
        assert_eq!(winner(&[0.2, 0.1]), 1);
    }
}
