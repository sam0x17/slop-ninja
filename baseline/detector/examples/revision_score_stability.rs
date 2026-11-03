//! Exploratory revision stability from completed public reports; no model calls.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    metrics::{ProbabilityRow, summarize_human_model},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn entropy(q: &[f64; 3]) -> f64 {
    q.iter().filter(|&&x| x > 0.0).map(|x| -x * x.ln()).sum()
}
fn measures(q: &[[f64; 3]; 3]) -> (f64, f64, f64) {
    let mean = std::array::from_fn(|i| q.iter().map(|p| p[i]).sum::<f64>() / 3.0);
    let js = (entropy(&mean) - q.iter().map(entropy).sum::<f64>() / 3.0).max(0.0);
    let scores = q.map(|p| (p[1] + p[2]).clamp(0.0, 1.0));
    let range = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - scores.iter().copied().fold(f64::INFINITY, f64::min);
    (js, 100.0 * range, 100.0 * (scores[2] - scores[0]))
}
fn descriptive(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    json!({"groups":n,"mean":values.iter().sum::<f64>()/n as f64,"median":if n%2==1 {values[n/2]} else {(values[n/2-1]+values[n/2])/2.0},"minimum":values[0],"maximum":values[n-1]})
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new result file");
    let plan = read(&args.plan)?;
    let root = PathBuf::from(string(&plan, "output")?);
    let completed = read(&root.join("completion.json"))?;
    ensure!(
        completed["public_diagnostics_complete"] == true && completed["error"].is_null(),
        "Public comparison must be complete"
    );
    let cases = plan["cases"].as_array().context("Missing cases")?;
    let mut bindings = BTreeMap::new();
    for p in [
        &args.plan,
        &root.join("completion.json"),
        &root.join("opening.json"),
    ] {
        bindings.insert(p.clone(), sha256(fs::read(p)?));
    }
    let mut results = Vec::new();
    for cohort in ["primary", "phi4"] {
        let mut reference_keys = None;
        for role in ["v6", "control", "consistency"] {
            let mut stages = Vec::new();
            let mut artifact = None;
            let mut thresholds = None;
            for depth in 0..3 {
                let name = format!("{cohort}-R{depth}");
                let selected: Vec<_> = cases.iter().filter(|c| c["name"] == name).collect();
                ensure!(selected.len() == 1, "Missing or duplicate stage");
                let p = if role == "v6" {
                    PathBuf::from(string(selected[0], "baseline")?)
                } else {
                    root.join(role).join(&name).join("report.json")
                };
                let r = read(&p)?;
                ensure!(
                    r["schema"] == "slop_ninja_frozen_encoder_evaluation_v1",
                    "Unexpected report schema"
                );
                ensure!(
                    r["split"] == "test" && r["evaluation_view"]["split"] == "test",
                    "Unexpected report partition"
                );
                let id = string(&r, "artifact_id")?.to_owned();
                ensure!(
                    artifact.as_ref().is_none_or(|old| old == &id),
                    "Artifact changes across stages"
                );
                artifact = Some(id);
                let th = string(&r, "thresholds_sha256")?.to_owned();
                ensure!(
                    thresholds.as_ref().is_none_or(|old| old == &th),
                    "Thresholds change across stages"
                );
                thresholds = Some(th);
                let rows: Vec<ProbabilityRow> =
                    serde_json::from_value(r["report"]["predictions"].clone())?;
                summarize_human_model(&rows)?;
                let mut model = BTreeMap::new();
                for row in rows.into_iter().filter(|r| r.label == 1) {
                    ensure!(
                        model.insert(row.group.clone(), row).is_none(),
                        "More than one model output per group/stage"
                    );
                }
                ensure!(!model.is_empty(), "No model outputs");
                bindings.insert(p.clone(), sha256(fs::read(&p)?));
                stages.push(model);
            }
            let keys = stages
                .iter()
                .map(|s| {
                    s.iter()
                        .map(|(group, row)| (group.clone(), row.id.clone()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            ensure!(
                reference_keys.as_ref().is_none_or(|old| old == &keys),
                "Source IDs differ across candidate reports"
            );
            reference_keys = Some(keys);
            let groups: Vec<_> = stages[0].keys().cloned().collect();
            ensure!(
                stages.iter().all(|s| s.keys().eq(groups.iter())),
                "Source groups differ across stages"
            );
            let mut divergences = Vec::new();
            let mut ranges = Vec::new();
            let mut changes = Vec::new();
            for group in groups {
                let ids: Vec<_> = stages.iter().map(|s| s[&group].id.as_str()).collect();
                ensure!(
                    ids[0] != ids[1] && ids[0] != ids[2] && ids[1] != ids[2],
                    "A revision reused the same record ID"
                );
                let q = std::array::from_fn(|i| stages[i][&group].probabilities);
                let (js, range, change) = measures(&q);
                divergences.push(js);
                ranges.push(range);
                changes.push(change);
            }
            results.push(json!({"cohort":cohort,"role":role,"artifact_id":artifact,"thresholds_sha256":thresholds,"three_stage_js_nats":descriptive(divergences),"combined_probability_range_pp":descriptive(ranges),"r2_minus_r0_pp":descriptive(changes)}));
        }
    }
    for (p, h) in &bindings {
        ensure!(sha256(fs::read(p)?) == *h, "Report changed during analysis");
    }
    let output = json!({"schema":"slop_ninja_revision_score_stability_v1","exploratory_posthoc":true,"new_model_calls":0,"provider_calls":0,"reference_promoted":false,"rows":results,"bindings":bindings,"interpretation":"Equal-weight three-way Jensen-Shannon divergence of calibrated three-class predictions and range of P(model_only)+P(mixed), within each source family's R0/R1/R2 sequence. Each family contributes once. Temperatures differ across artifacts. Constant wrong predictions can be perfectly stable; this is neither accuracy nor direct measurement of the uncalibrated training loss. No independent confirmation or model/threshold selection follows from this diagnostic."});
    let mut file = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut file, &output)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stability_does_not_imply_accuracy_or_agreement_between_origin_classes() {
        assert_eq!(measures(&[[1.0, 0.0, 0.0]; 3]), (0.0, 0.0, 0.0));
        let separate = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert!((measures(&separate).0 - 3.0_f64.ln()).abs() < 1e-12);
        assert_eq!(measures(&separate).1, 100.0);
        let origins = [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]];
        assert!(measures(&origins).0 > 0.0);
        assert_eq!(measures(&origins).1, 0.0);
        assert_eq!(measures(&origins).2, 0.0);
    }
}
