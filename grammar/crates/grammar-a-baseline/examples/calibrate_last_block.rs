//! Fit origin temperature and conditional named thresholds on CAL only.
//! FIT_BUNDLE RAW_CAL_RUN SEALED_SELECTION NEW_CALIBRATION.json
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("missing {key}"))
}
fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite(), "nonfinite scalar");
    Ok(n)
}
fn jsonl(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn softmax(logits: &Value, n: usize, temperature: f64) -> Result<Vec<f64>> {
    ensure!(
        temperature.is_finite() && temperature > 0.,
        "temperature bound"
    );
    let values = logits
        .as_array()
        .context("logits")?
        .iter()
        .map(number)
        .collect::<Result<Vec<_>>>()?;
    ensure!(values.len() == n && n > 1, "logit dimension");
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let values = values
        .into_iter()
        .map(|v| ((v - max) / temperature).exp())
        .collect::<Vec<_>>();
    let total = values.iter().sum::<f64>();
    Ok(values.into_iter().map(|v| v / total).collect())
}
fn criterion(rows: &[Value], pred: &BTreeMap<String, Value>, temperature: f64) -> Result<f64> {
    let mut cells = BTreeMap::<(&str, u64), (f64, usize)>::new();
    for r in rows {
        let Some(y) = r["targets"]["origin"].as_u64() else {
            continue;
        };
        ensure!(y < 3, "origin target");
        let p = softmax(&pred[text(r, "id")?]["origin_logits"], 3, temperature)?;
        let cell = cells.entry((text(r, "source_group")?, y)).or_default();
        cell.0 += (p[1] + p[2] - f64::from(y != 0)).powi(2);
        cell.1 += 1;
    }
    let mut families = BTreeMap::<&str, (f64, usize)>::new();
    for ((group, _), (sum, n)) in cells {
        let f = families.entry(group).or_default();
        f.0 += sum / n as f64;
        f.1 += 1;
    }
    ensure!(!families.is_empty(), "CAL lacks origin supervision");
    Ok(families
        .values()
        .map(|(sum, n)| sum / *n as f64)
        .sum::<f64>()
        / families.len() as f64)
}
fn threshold_objective(samples: &[(String, bool, bool, f64)], t: f64) -> Result<(f64, f64)> {
    let mut groups = [BTreeMap::<&str, (f64, usize)>::new(), BTreeMap::new()];
    for (g, known, correct, score) in samples {
        let index = usize::from(!known);
        let value = if *known {
            *correct && *score >= t
        } else {
            *score < t
        };
        let s = groups[index].entry(g).or_default();
        s.0 += f64::from(value);
        s.1 += 1;
    }
    ensure!(
        groups.iter().all(|g| !g.is_empty()),
        "known and withheld CAL families required"
    );
    let means =
        groups.map(|g| g.values().map(|(s, n)| s / *n as f64).sum::<f64>() / g.len() as f64);
    Ok((means[0], means[1]))
}
fn named(
    rows: &[Value],
    pred: &BTreeMap<String, Value>,
    role: &str,
    profiles: &[String],
    trained: bool,
) -> Result<Value> {
    let mut samples = Vec::new();
    let mut missing = 0;
    for r in rows {
        let label = &r["roles"][role];
        if label["present"] != true {
            continue;
        }
        let Some(identity) = label["identity"].as_str() else {
            missing += 1;
            continue;
        };
        let known = label["known"].as_bool().context("role known flag")?;
        ensure!(
            profiles.iter().any(|p| p == identity) == known,
            "roster/label mismatch"
        );
        let p = softmax(
            &pred[text(r, "id")?][format!("{role}_logits")],
            profiles.len(),
            1.,
        )?;
        let winner = (0..p.len())
            .max_by(|a, b| p[*a].total_cmp(&p[*b]).then_with(|| b.cmp(a)))
            .unwrap();
        samples.push((
            text(r, "source_group")?.to_owned(),
            known,
            profiles[winner] == identity,
            p[winner],
        ));
    }
    let groups = |known| {
        samples
            .iter()
            .filter(|(_, k, _, _)| *k == known)
            .map(|(g, _, _, _)| g)
            .collect::<BTreeSet<_>>()
            .len()
    };
    let qualified = trained && groups(true) > 0 && groups(false) > 0;
    let mut report = json!({"qualified":qualified,"threshold":1.,"known_families":groups(true),
        "withheld_families":groups(false),"known_rows":samples.iter().filter(|(_,k,_,_)|*k).count(),
        "withheld_rows":samples.iter().filter(|(_,k,_,_)|!*k).count(),"role_present_identity_missing":missing,
        "objective":"half_known_correct_named_acceptance_plus_half_withheld_rejection",
        "weighting":"equal_source_family_within_each_known_or_withheld_population",
        "presence_gate":"documented_role_presence_only; no_predicted_probability_cutoff",
        "tie_break":"higher_threshold","objective_value":null,
        "limit":"CAL qualification is not independent rejection performance; saturated softmax1 cannot be rejected by >=1"});
    if !qualified {
        return Ok(report);
    }
    let mut observed = samples.iter().map(|(_, _, _, s)| *s).collect::<Vec<_>>();
    observed.sort_by(f64::total_cmp);
    observed.dedup();
    let mut candidates = vec![0., 1.];
    candidates.extend(observed.windows(2).map(|p| (p[0] + p[1]) / 2.));
    let mut best = (-1., 1., (0., 0.));
    for t in &candidates {
        let rates = threshold_objective(&samples, *t)?;
        let score = (rates.0 + rates.1) / 2.;
        if score > best.0 || (score == best.0 && *t > best.1) {
            best = (score, *t, rates);
        }
    }
    report["threshold"] = json!(best.1);
    report["objective_value"] = json!(best.0);
    report["known_correct_named_rate"] = json!(best.2.0);
    report["withheld_rejection_rate"] = json!(best.2.1);
    report["candidate_count"] = json!(candidates.len());
    Ok(report)
}
fn run(data: &Path, raw: &Path, selected: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new calibration output required");
    let manifest = read(&data.join("manifest.json"))?;
    let contract = read(&data.join("contract.json"))?;
    let selection = read(selected)?;
    let receipt = read(&raw.join("manifest.json"))?;
    ensure!(
        manifest["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && manifest["complete"] == true
            && manifest["final_test_opened"] == false
            && manifest["contract_sha256"] == sha(&data.join("contract.json"))?,
        "dataset scope/binding"
    );
    ensure!(
        selection["schema"] == "slop_ninja_last_block_condition_selection_v1"
            && selection["complete"] == true
            && selection["selection_split"] == "development"
            && selection["final_test_opened"] == false
            && selection["calibration_read_for_selection"] == false
            && selection["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?,
        "sealed Development decision required"
    );
    ensure!(
        receipt["schema"] == "slop_ninja_last_block_raw_predictions_v1"
            && receipt["complete"] == true
            && receipt["partition"] == "calibration"
            && receipt["final_test_opened"] == false
            && receipt["selection_sha256"] == sha(selected)?
            && receipt["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?
            && receipt["contract_sha256"] == manifest["contract_sha256"]
            && receipt["origin_logits_semantics"]
                == "original_classifier_linear_divided_by_base_temperature; no_CAL_temperature",
        "raw predictions scope/binding"
    );
    let runs = selection["runs"]
        .as_array()
        .context("selected runs")?
        .iter()
        .filter(|r| r["condition"] == receipt["condition"] && r["seed"] == receipt["seed"])
        .collect::<Vec<_>>();
    ensure!(
        runs.len() == 1
            && runs[0]["selected_checkpoint_sha256"] == receipt["selected_checkpoint_sha256"],
        "checkpoint not frozen"
    );
    let rows = jsonl(&data.join("calibration.jsonl"))?;
    ensure!(
        manifest["files_sha256"]["calibration.jsonl"] == sha(&data.join("calibration.jsonl"))?
            && receipt["partition_sha256"] == manifest["files_sha256"]["calibration.jsonl"]
            && receipt["predictions_sha256"] == sha(&raw.join("predictions.jsonl"))?,
        "CAL population changed"
    );
    let mut pred = BTreeMap::new();
    for r in jsonl(&raw.join("predictions.jsonl"))? {
        ensure!(
            pred.insert(text(&r, "id")?.to_owned(), r).is_none(),
            "duplicate prediction"
        );
    }
    ensure!(
        rows.len() == pred.len()
            && receipt["requested"] == rows.len()
            && receipt["supported"] == rows.len()
            && receipt["unsupported"] == 0,
        "CAL coverage"
    );
    let mut seen = BTreeSet::new();
    for r in &rows {
        let id = text(r, "id")?;
        ensure!(
            r["frozen_split"] == "calibration"
                && r["purpose"] == "query"
                && seen.insert(id)
                && pred.get(id).context("missing prediction")?["prefix_sha256"]
                    == r["prefix"]["sha256"],
            "wrong or unbound CAL row"
        );
        ensure!(
            (0.0..=1.).contains(&number(&pred[id]["conditional_mixed_share"])?),
            "conditional share bounds"
        );
    }
    let trained = &runs[0]["trained_selected_heads"];
    let origin_calibrated = trained["origin"] == true;
    let initial = criterion(&rows, &pred, 1.)?;
    let mut best: (f64, f64) = (initial, 1.);
    if origin_calibrated {
        for k in -30..=30 {
            let t = (k as f64 * 0.1).exp();
            let score = criterion(&rows, &pred, t)?;
            if score < best.0 || (score == best.0 && (t - 1.).abs() < (best.1 - 1.).abs()) {
                best = (score, t);
            }
        }
    }
    let mut roles = BTreeMap::new();
    for role in ["human", "ai"] {
        let profiles = contract[format!("{role}_profiles")]
            .as_array()
            .context("roster")?
            .iter()
            .map(|v| Ok(v.as_str().context("profile")?.to_owned()))
            .collect::<Result<Vec<_>>>()?;
        roles.insert(
            role,
            named(&rows, &pred, role, &profiles, trained[role] == true)?,
        );
    }
    let report = json!({"schema":"slop_ninja_last_block_calibration_v1","complete":true,"fit_split":"calibration",
        "final_test_opened":false,"condition":receipt["condition"],"seed":receipt["seed"],
        "selected_checkpoint_sha256":receipt["selected_checkpoint_sha256"],"contract_sha256":manifest["contract_sha256"],
        "selection_sha256":sha(selected)?,"dataset_manifest_sha256":sha(&data.join("manifest.json"))?,
        "raw_manifest_sha256":sha(&raw.join("manifest.json"))?,"calibration_rows_sha256":sha(&data.join("calibration.jsonl"))?,
        "origin_calibrated":origin_calibrated,"temperature":best.1,"roles":roles,
        "origin_objective":"equal_family_then_equal_observed_origin_any_AI_Brier",
        "temperature_grid":"exp(k*0.1) for integer k=-30..30; explicit1; f64; nearest1 on exact tie",
        "uncalibrated_CAL_brier":initial,"calibrated_CAL_brier":best.0,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("calibrate_last_block.rs"))),
        "limits":["CAL scores are fitted diagnostics, never independent evaluation.",
            "Origin temperature calibrates involvement; share remains recomputed from calibrated origin and the frozen mixed-share head.",
            "Pangram content fractions are not targets for this calibration."]});
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"condition":receipt["condition"],"seed":receipt["seed"],"temperature":best.1,"origin_calibrated":origin_calibrated,
        "human_qualified":roles["human"]["qualified"],"ai_qualified":roles["ai"]["qualified"]})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "FIT_BUNDLE RAW_CAL_RUN SEALED_SELECTION NEW_CALIBRATION.json"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temperature_is_stable_for_large_logits_and_keeps_origin_order() {
        let p = softmax(&json!([10000., 10001., 9999.]), 3, 1.).unwrap();
        assert!((p.iter().sum::<f64>() - 1.).abs() < 1e-12);
        assert!(p[1] > p[0] && p[0] > p[2]);
        assert!(softmax(&json!([0., 1., 2.]), 3, 0.).is_err());
    }
    #[test]
    fn named_threshold_weights_families_and_requires_both_populations() {
        let s = vec![
            ("a".into(), true, true, 0.8),
            ("b".into(), true, false, 0.8),
            ("b".into(), true, false, 0.8),
            ("c".into(), false, false, 0.6),
        ];
        assert_eq!(threshold_objective(&s, 0.7).unwrap(), (0.5, 1.));
        assert!(threshold_objective(&s[..3], 0.7).is_err());
    }
}
