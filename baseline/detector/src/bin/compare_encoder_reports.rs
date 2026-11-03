//! Paired source-family uncertainty for already frozen encoder evaluations.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    metrics::{self, ProbabilityRow},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    baseline: PathBuf,
    #[arg(long)]
    candidate: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn paired(before: &[ProbabilityRow], after: &[ProbabilityRow]) -> Result<Value> {
    paired_objective(before, after, false)
}

fn paired_objective(
    before: &[ProbabilityRow],
    after: &[ProbabilityRow],
    binary: bool,
) -> Result<Value> {
    metrics::summarize(before)?;
    metrics::summarize(after)?;
    ensure!(before.len() == after.len(), "Prediction counts differ");
    let before: BTreeMap<_, _> = before.iter().map(|r| (r.id.as_str(), r)).collect();
    let after: BTreeMap<_, _> = after.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut groups = BTreeMap::<&str, ([f64; 3], usize)>::new();
    for (id, a) in &before {
        let b = after.get(id).context("Paired prediction ID is missing")?;
        ensure!(
            a.group == b.group && a.label == b.label,
            "Paired family or origin label differs"
        );
        if binary && a.label == 2 {
            continue;
        }
        let score = |row: &ProbabilityRow| -> Result<[f64; 3]> {
            if binary {
                let m = metrics::summarize_human_model(std::slice::from_ref(row))?
                    .context("Missing human/model row")?;
                Ok([m.log_loss, m.binary_brier, m.accuracy_at_half])
            } else {
                let m = metrics::summarize(std::slice::from_ref(row))?;
                Ok([m.log_loss, m.multiclass_brier, m.accuracy])
            }
        };
        let (ma, mb) = (score(a)?, score(b)?);
        let delta = std::array::from_fn::<_, 3, _>(|i| mb[i] - ma[i]);
        let (sum, n) = groups.entry(&a.group).or_default();
        for i in 0..3 {
            sum[i] += delta[i];
        }
        *n += 1;
    }
    let values: Vec<_> = groups.into_values().collect();
    ensure!(
        !values.is_empty(),
        "No rows for the selected comparison objective"
    );
    let mean = |sample: &[([f64; 3], usize)]| -> [f64; 3] {
        let n = sample.iter().map(|v| v.1).sum::<usize>() as f64;
        std::array::from_fn(|i| sample.iter().map(|v| v.0[i]).sum::<f64>() / n)
    };
    let point = mean(&values);
    let mut draws: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    if values.len() >= 2 {
        let mut rng = 0x243f6a8885a308d3_u64;
        for _ in 0..512 {
            let sample: Vec<_> = (0..values.len())
                .map(|_| {
                    rng = rng.wrapping_add(0x9e3779b97f4a7c15);
                    let mut bits = rng;
                    bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
                    bits ^= bits >> 31;
                    values[(bits % values.len() as u64) as usize]
                })
                .collect();
            let estimate = mean(&sample);
            for i in 0..3 {
                draws[i].push(estimate[i]);
            }
        }
    }
    let mut deltas = serde_json::Map::new();
    let names = if binary {
        ["log_loss", "binary_brier", "accuracy_at_half"]
    } else {
        ["log_loss", "multiclass_brier", "accuracy"]
    };
    for (i, name) in names.iter().enumerate() {
        draws[i].sort_by(f64::total_cmp);
        let (interval, status) = if draws[i].is_empty() {
            (None, "insufficient_source_groups")
        } else if (draws[i][499] - draws[i][12]).abs() < 1e-12 {
            (None, "degenerate_observed_outcomes")
        } else {
            (
                Some([draws[i][12], draws[i][499]]),
                "paired_source_group_percentile",
            )
        };
        deltas.insert(
            (*name).into(),
            json!({"candidate_minus_baseline":point[i],
            "group_bootstrap_percentile_95":interval,"uncertainty_status":status,
            "improvement_direction":if i == 2 {"positive"} else {"negative"}}),
        );
    }
    Ok(
        json!({"rows":values.iter().map(|v| v.1).sum::<usize>(),"source_groups":values.len(),"bootstrap_replicates":draws[0].len(),
        "objective":if binary { "human_model" } else { "three_class" },
        "bootstrap_seed":"243f6a8885a308d3","deltas":deltas,
        "method":"Sample complete source families with replacement, using the same draw for both detectors; average all sampled rows. Sorted IDs/groups and a fixed SplitMix64 stream make the calculation reproducible.",
        "limits":"Intervals assume independent source families and do not cover arbitrary new registers, generators or pretraining contamination. Degenerate intervals are omitted. Different frozen thresholds do not establish matched-FPR superiority."}),
    )
}

fn paired_evaluation_view(
    a: &Value,
    b: &Value,
    before: &[ProbabilityRow],
    after: &[ProbabilityRow],
) -> Result<Option<Value>> {
    let (first, second) = match (a.get("evaluation_view"), b.get("evaluation_view")) {
        (None, None) => return Ok(None),
        (Some(first), Some(second)) => (first, second),
        _ => anyhow::bail!("Both reports must bind the same evaluation view"),
    };
    ensure!(first == second, "Evaluation view bindings differ");
    for (report, rows) in [(a, before), (b, after)] {
        let view = report["evaluation_view"]
            .as_object()
            .context("Evaluation view binding must be a non-null object")?;
        ensure!(view.len() == 5, "Unexpected evaluation view binding fields");
        for key in ["sha256", "records_sha256"] {
            let digest = view
                .get(key)
                .and_then(Value::as_str)
                .context("Missing evaluation view SHA256")?;
            ensure!(
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "Invalid evaluation view SHA256"
            );
        }
        ensure!(
            view["records_sha256"] == report["records_sha256"],
            "Evaluation view corpus differs from its report"
        );
        let split = view
            .get("split")
            .and_then(Value::as_str)
            .context("Missing evaluation view split")?;
        ensure!(
            matches!(split, "development" | "test") && report["split"] == split,
            "Evaluation view partition differs from its report"
        );
        let selected_rows = view
            .get("selected_rows")
            .and_then(Value::as_u64)
            .context("Invalid evaluation view row count")?;
        let source_groups = view
            .get("source_groups")
            .and_then(Value::as_u64)
            .context("Invalid evaluation view family count")?;
        ensure!(
            source_groups > 0
                && source_groups.checked_mul(3) == Some(selected_rows)
                && selected_rows == u64::try_from(rows.len())?,
            "Evaluation view requires three predictions per declared family"
        );
        let mut ids = BTreeSet::new();
        let mut groups = BTreeMap::<&str, [usize; 3]>::new();
        for row in rows {
            ensure!(
                ids.insert(row.id.as_str()) && !row.group.is_empty() && row.label < 3,
                "Invalid evaluation view prediction identity or origin"
            );
            groups.entry(row.group.as_str()).or_default()[row.label] += 1;
        }
        ensure!(
            u64::try_from(groups.len())? == source_groups
                && groups.values().all(|counts| *counts == [1, 1, 1]),
            "Evaluation view requires one prediction per origin in each family"
        );
    }
    Ok(Some(first.clone()))
}

fn paired_observation_policy(
    a: &Value,
    b: &Value,
    before: &[ProbabilityRow],
    after: &[ProbabilityRow],
) -> Result<Option<Value>> {
    let (first, second) = match (a.get("observation_policy"), b.get("observation_policy")) {
        (None, None) => return Ok(None),
        (Some(first), Some(second)) => (first, second),
        _ => anyhow::bail!("Both reports must use the same observation policy"),
    };
    ensure!(
        first.as_str() == Some("all_human_roots_v1") && first == second,
        "Unknown or unequal observation policies"
    );
    ensure!(
        a.get("evaluation_view").is_none() && b.get("evaluation_view").is_none(),
        "Human-root observations cannot also bind an evaluation view"
    );
    for rows in [before, after] {
        ensure!(!rows.is_empty(), "Human-root observations are empty");
        let mut ids = BTreeSet::new();
        let mut groups = BTreeSet::new();
        ensure!(
            rows.iter().all(|row| row.label == 0
                && !row.id.is_empty()
                && !row.group.is_empty()
                && ids.insert(row.id.as_str())
                && groups.insert(row.group.as_str())),
            "Human-root observations require one unique human prediction per source family"
        );
    }
    Ok(Some(first.clone()))
}

fn compare_reports(
    a: &Value,
    b: &Value,
    baseline_report_sha256: String,
    candidate_report_sha256: String,
) -> Result<Value> {
    for report in [a, b] {
        ensure!(
            report["schema"] == "slop_ninja_frozen_encoder_evaluation_v1",
            "Unexpected report schema"
        );
        ensure!(
            report["artifact_id"] == report["report"]["artifact_sha256"],
            "Artifact identity mismatch"
        );
        ensure!(
            report["split"] == "development"
                || (report["split"] == "test" && report["final_test_opened"] == true),
            "Unexpected evaluation partition or Test state"
        );
    }
    ensure!(
        a["records_sha256"] == b["records_sha256"] && a["split"] == b["split"],
        "Corpora or partitions differ"
    );
    ensure!(
        a["artifact_id"] != b["artifact_id"],
        "Comparison requires distinct detectors"
    );
    let before: Vec<ProbabilityRow> = serde_json::from_value(a["report"]["predictions"].clone())?;
    let after: Vec<ProbabilityRow> = serde_json::from_value(b["report"]["predictions"].clone())?;
    let observation_policy = paired_observation_policy(a, b, &before, &after)?;
    let evaluation_view = paired_evaluation_view(a, b, &before, &after)?;
    let comparison = paired(&before, &after)?;
    let human_model = paired_objective(&before, &after, true)?;
    let mut report = json!({"schema":"slop_ninja_paired_encoder_comparison_v1",
        "baseline_report_sha256":baseline_report_sha256,"candidate_report_sha256":candidate_report_sha256,
        "baseline_artifact_id":a["artifact_id"],"candidate_artifact_id":b["artifact_id"],
        "records_sha256":a["records_sha256"],"split":a["split"],"comparison":comparison,
        "human_model_comparison":human_model,
        "baseline_metrics":a["report"]["model"],"candidate_metrics":b["report"]["model"],
        "baseline_operating_points":a["report"]["operating_points"],"candidate_operating_points":b["report"]["operating_points"],
        "baseline_near_cutoffs":a["near_cutoffs"],"candidate_near_cutoffs":b["near_cutoffs"]});
    if let Some(view) = evaluation_view {
        report["evaluation_view"] = view;
    }
    if let Some(policy) = observation_policy {
        report["observation_policy"] = policy;
    }
    Ok(report)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output.exists(), "Use a new comparison output");
    let baseline_bytes = fs::read(&args.baseline)?;
    let candidate_bytes = fs::read(&args.candidate)?;
    let a: Value = serde_json::from_slice(&baseline_bytes)?;
    let b: Value = serde_json::from_slice(&candidate_bytes)?;
    let report = compare_reports(&a, &b, sha256(baseline_bytes), sha256(candidate_bytes))?;
    let mut output = fs::File::create_new(args.output)?;
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    println!("{}", serde_json::to_string_pretty(&report["comparison"])?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluation_reports() -> (Value, Value, Value) {
        let rows: Vec<_> = (0..6)
            .map(|i| ProbabilityRow {
                id: format!("synthetic-view-{i}"),
                group: format!("synthetic-view-family-{}", i / 3),
                label: i % 3,
                probabilities: std::array::from_fn(|label| if label == i % 3 { 0.6 } else { 0.2 }),
            })
            .collect();
        let baseline = json!({
            "schema":"slop_ninja_frozen_encoder_evaluation_v1",
            "artifact_id":sha256("synthetic baseline"),
            "records_sha256":sha256("synthetic lineage archive"),
            "split":"development", "final_test_opened":false,
            "report":{"artifact_sha256":sha256("synthetic baseline"),"predictions":rows},
        });
        let mut candidate = baseline.clone();
        candidate["artifact_id"] = json!(sha256("synthetic candidate"));
        candidate["report"]["artifact_sha256"] = candidate["artifact_id"].clone();
        let view = json!({
            "sha256":sha256("synthetic selected exposure manifest"),
            "records_sha256":baseline["records_sha256"], "split":"development",
            "selected_rows":6, "source_groups":2,
        });
        (baseline, candidate, view)
    }

    fn compare_fixture(a: &Value, b: &Value) -> Result<Value> {
        compare_reports(a, b, sha256("baseline report"), sha256("candidate report"))
    }

    fn human_root_reports() -> (Value, Value) {
        let (mut baseline, mut candidate, _) = evaluation_reports();
        for (report, human_probability) in [(&mut baseline, 0.6), (&mut candidate, 0.8)] {
            let rows: Vec<ProbabilityRow> =
                serde_json::from_value(report["report"]["predictions"].clone()).unwrap();
            let rows: Vec<_> = rows
                .into_iter()
                .filter(|row| row.label == 0)
                .map(|row| ProbabilityRow {
                    probabilities: [
                        human_probability,
                        (1.0 - human_probability) / 2.0,
                        (1.0 - human_probability) / 2.0,
                    ],
                    ..row
                })
                .collect();
            let points = metrics::select_operating_points(&rows, &[0.01, 0.05]).unwrap();
            report["report"] = serde_json::to_value(
                metrics::evaluate_probability_rows(
                    report["artifact_id"].as_str().unwrap(),
                    &rows,
                    [1.0 / 3.0; 3],
                    &points,
                )
                .unwrap(),
            )
            .unwrap();
            report["observation_policy"] = json!("all_human_roots_v1");
        }
        (baseline, candidate)
    }

    #[test]
    fn human_root_policy_is_bound_without_inventing_sensitivity_estimates() {
        let (mut baseline, mut candidate) = human_root_reports();
        let mut report = compare_fixture(&baseline, &candidate).unwrap();
        assert_eq!(report["observation_policy"], "all_human_roots_v1");
        assert!(report.get("evaluation_view").is_none());
        assert_eq!(report["comparison"]["rows"], 2);
        assert_eq!(report["human_model_comparison"]["rows"], 2);
        assert_eq!(report["human_model_comparison"]["source_groups"], 2);
        assert!(
            (report["human_model_comparison"]["deltas"]["log_loss"]["candidate_minus_baseline"]
                .as_f64()
                .unwrap()
                - (0.6_f64 / 0.8).ln())
            .abs()
                < 1e-12
        );
        for detector in ["baseline_operating_points", "candidate_operating_points"] {
            for point in report[detector].as_array().unwrap() {
                for sensitivity in [
                    "model_or_mixed_sensitivity",
                    "model_only_sensitivity",
                    "mixed_sensitivity",
                ] {
                    assert_eq!(point[sensitivity]["rows"], 0);
                    assert_eq!(point[sensitivity]["rate"], Value::Null);
                    assert_eq!(
                        point[sensitivity]["group_bootstrap_percentile_95"],
                        Value::Null
                    );
                }
            }
        }
        for value in [&mut baseline, &mut candidate, &mut report] {
            value.as_object_mut().unwrap().remove("observation_policy");
        }
        assert_eq!(report, compare_fixture(&baseline, &candidate).unwrap());
    }

    #[test]
    fn rejects_mixed_unknown_or_view_bound_human_observation_policies() {
        let (baseline, candidate) = human_root_reports();
        let mut ordinary = candidate.clone();
        ordinary
            .as_object_mut()
            .unwrap()
            .remove("observation_policy");
        assert!(compare_fixture(&baseline, &ordinary).is_err());
        assert!(compare_fixture(&ordinary, &baseline).is_err());
        let (_, trio, view) = evaluation_reports();
        assert!(compare_fixture(&baseline, &trio).is_err());
        for policy in [Value::Null, json!("unknown-policy"), json!({})] {
            let mut a = baseline.clone();
            let mut b = candidate.clone();
            a["observation_policy"] = policy.clone();
            b["observation_policy"] = policy;
            assert!(compare_fixture(&a, &b).is_err());
        }
        for binding in [Value::Null, view] {
            let mut a = baseline.clone();
            let mut b = candidate.clone();
            a["evaluation_view"] = binding.clone();
            b["evaluation_view"] = binding;
            assert!(compare_fixture(&a, &b).is_err());
        }
    }

    #[test]
    fn rejects_non_human_duplicate_or_empty_root_observations() {
        let (baseline, candidate) = human_root_reports();
        for (field, value) in [
            ("label", json!(1)),
            ("label", json!(2)),
            ("group", json!("")),
            ("group", json!("synthetic-view-family-1")),
            ("id", json!("")),
            ("id", json!("synthetic-view-3")),
        ] {
            let mut a = baseline.clone();
            let mut b = candidate.clone();
            a["report"]["predictions"][0][field] = value.clone();
            b["report"]["predictions"][0][field] = value;
            assert!(compare_fixture(&a, &b).is_err());
        }
        let mut a = baseline;
        let mut b = candidate;
        a["report"]["predictions"] = json!([]);
        b["report"]["predictions"] = json!([]);
        assert!(compare_fixture(&a, &b).is_err());
    }

    #[test]
    fn preserves_legacy_output_and_binds_identical_evaluation_views() {
        let (mut baseline, mut candidate, view) = evaluation_reports();
        let legacy = compare_fixture(&baseline, &candidate).unwrap();
        assert!(legacy.get("evaluation_view").is_none());
        assert!(legacy.get("observation_policy").is_none());
        baseline["evaluation_view"] = view.clone();
        candidate["evaluation_view"] = view.clone();
        let mut bound = compare_fixture(&baseline, &candidate).unwrap();
        assert_eq!(bound["evaluation_view"], view);
        bound.as_object_mut().unwrap().remove("evaluation_view");
        assert_eq!(bound, legacy);
    }

    #[test]
    fn rejects_absent_null_and_non_object_evaluation_view_bindings() {
        let (mut baseline, mut candidate, view) = evaluation_reports();
        baseline["evaluation_view"] = view;
        assert!(compare_fixture(&baseline, &candidate).is_err());
        assert!(compare_fixture(&candidate, &baseline).is_err());
        for malformed in [Value::Null, json!([]), json!("view")] {
            baseline["evaluation_view"] = malformed.clone();
            candidate["evaluation_view"] = malformed;
            assert!(compare_fixture(&baseline, &candidate).is_err());
        }
    }

    #[test]
    fn rejects_different_views_and_bindings_to_other_corpora_or_partitions() {
        let (baseline, candidate, view) = evaluation_reports();
        let mut a = baseline.clone();
        let mut b = candidate.clone();
        a["evaluation_view"] = view.clone();
        b["evaluation_view"] = view.clone();
        b["evaluation_view"]["sha256"] = json!(sha256("another exposure selection"));
        assert!(compare_fixture(&a, &b).is_err());
        for (key, value) in [
            ("sha256", json!("not-a-sha256")),
            ("records_sha256", json!(sha256("another corpus"))),
            ("split", json!("test")),
        ] {
            let mut changed = view.clone();
            changed[key] = value;
            a["evaluation_view"] = changed.clone();
            b["evaluation_view"] = changed;
            assert!(compare_fixture(&a, &b).is_err());
        }
        let mut missing = view.clone();
        missing.as_object_mut().unwrap().remove("sha256");
        a["evaluation_view"] = missing.clone();
        b["evaluation_view"] = missing;
        assert!(compare_fixture(&a, &b).is_err());
    }

    #[test]
    fn rejects_incorrect_view_counts_and_non_triplet_predictions() {
        let (baseline, candidate, view) = evaluation_reports();
        for (rows, groups) in [
            (json!(5), json!(2)),
            (json!(3), json!(1)),
            (json!(0), json!(0)),
            (json!(6), json!(u64::MAX)),
            (json!("6"), json!(2)),
        ] {
            let mut a = baseline.clone();
            let mut b = candidate.clone();
            let mut changed = view.clone();
            changed["selected_rows"] = rows;
            changed["source_groups"] = groups;
            a["evaluation_view"] = changed.clone();
            b["evaluation_view"] = changed;
            assert!(compare_fixture(&a, &b).is_err());
        }
        for (field, value) in [
            ("group", json!("synthetic-third-family")),
            ("label", json!(1)),
            ("id", json!("synthetic-view-1")),
        ] {
            let mut a = baseline.clone();
            let mut b = candidate.clone();
            a["evaluation_view"] = view.clone();
            b["evaluation_view"] = view.clone();
            a["report"]["predictions"][0][field] = value.clone();
            b["report"]["predictions"][0][field] = value;
            assert!(compare_fixture(&a, &b).is_err());
        }
    }

    #[test]
    fn paired_rows_require_matching_families_and_labels_and_omit_degenerate_intervals() {
        let before: Vec<_> = (0..6)
            .map(|i| ProbabilityRow {
                id: format!("synthetic-{i}"),
                group: format!("synthetic-family-{}", i / 3),
                label: i % 3,
                probabilities: std::array::from_fn(|j| if j == i % 3 { 0.6 } else { 0.2 }),
            })
            .collect();
        let after: Vec<_> = before
            .iter()
            .map(|r| ProbabilityRow {
                probabilities: std::array::from_fn(|j| if j == r.label { 0.8 } else { 0.1 }),
                ..r.clone()
            })
            .collect();
        let result = paired(&before, &after).unwrap();
        let binary = paired_objective(&before, &after, true).unwrap();
        assert_eq!(binary["rows"], 4);
        assert!(
            (binary["deltas"]["log_loss"]["candidate_minus_baseline"]
                .as_f64()
                .unwrap()
                - ((0.6_f64 / 0.8).ln() + (0.8_f64 / 0.9).ln()) / 2.0)
                .abs()
                < 1e-12
        );
        assert!(
            (result["deltas"]["log_loss"]["candidate_minus_baseline"]
                .as_f64()
                .unwrap()
                - (0.6_f64 / 0.8).ln())
            .abs()
                < 1e-12
        );
        assert_eq!(
            result["deltas"]["accuracy"]["uncertainty_status"],
            "degenerate_observed_outcomes"
        );
        let mut changed = after.clone();
        changed[0].group = "wrong-family".into();
        assert!(paired(&before, &changed).is_err());
        changed[0] = after[0].clone();
        changed[0].label = 1;
        assert!(paired(&before, &changed).is_err());
    }
}
