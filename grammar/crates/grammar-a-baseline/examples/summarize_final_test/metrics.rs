//! Existing metrics/bootstrap, with explicit document-versus-origin-cell counts.
use super::*;

pub(super) fn equal(a: &Value, b: &Value) -> Result<()> {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => ensure!(
            (number(a)? - number(b)?).abs() < 1e-10,
            "aggregate numeric mismatch"
        ),
        (Value::Array(a), Value::Array(b)) => {
            ensure!(a.len() == b.len(), "aggregate shape");
            for (x, y) in a.iter().zip(b) {
                equal(x, y)?;
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            ensure!(a.keys().eq(b.keys()), "aggregate fields");
            for (k, v) in a {
                equal(v, &b[k])?;
            }
        }
        _ => ensure!(a == b, "aggregate value mismatch"),
    }
    Ok(())
}
pub(super) fn common(rows: &Pred, preds: &[&Pred]) -> BTreeSet<String> {
    rows.keys()
        .filter(|id| preds.iter().all(|p| p.contains_key(*id)))
        .cloned()
        .collect()
}
pub(super) fn rows(rows: &Pred, ids: &BTreeSet<String>) -> Vec<Value> {
    ids.iter().map(|id| rows[id].clone()).collect()
}
pub(super) fn families(rows: &[Value]) -> usize {
    rows.iter()
        .filter_map(|r| r["source_group"].as_str())
        .collect::<BTreeSet<_>>()
        .len()
}
fn restrict(rows: &[Value], pred: &Pred) -> Result<Pred> {
    rows.iter()
        .map(|r| {
            let id = b::text(r, "id")?;
            Ok((
                id.into(),
                pred.get(id).context("common prediction")?.clone(),
            ))
        })
        .collect()
}

pub(super) fn comparison(
    left_name: &str,
    right_name: &str,
    rows: &[Value],
    left: &[&Pred],
    right: &[&Pred],
) -> Result<Value> {
    ensure!(
        left.len() == 3 && right.len() == 3,
        "all three fixed seeds required"
    );
    let mut family = Vec::new();
    let mut balanced = Vec::new();
    let mut seeds = Vec::new();
    for (i, seed) in SEEDS.iter().enumerate() {
        let a = restrict(rows, left[i])?;
        let b = restrict(rows, right[i])?;
        let gaps = difference(rows, &a, &b, "involvement_brier")?;
        let cells = balance_origins(rows, &gaps)?;
        let mut s = family_summary(&cells);
        s["documents"] = json!(rows.len());
        s["balanced_cells"] = json!(cells.len());
        seeds.push(json!({"seed":seed,"family":family_summary(&gaps),"family_origin_balanced":s}));
        family.push(gaps);
        balanced.push(cells);
    }
    let summarize = |samples: &[Vec<(String, f64)>]| -> Result<Value> {
        if samples[0].is_empty() {
            Ok(family_summary(&[]))
        } else {
            warm::fixed_seed_summary(samples)
        }
    };
    let mut balanced_result = summarize(&balanced)?;
    balanced_result["documents"] = json!(rows.len());
    balanced_result["balanced_cells"] = json!(balanced[0].len());
    Ok(
        json!({"left":left_name,"right":right_name,"metric":"involvement_brier","difference":"left_minus_right",
        "negative_favors_left":true,"documents":rows.len(),"source_families":families(rows),
        "per_seed":seeds,"mean_over_fixed_seeds_then_family_bootstrap":{
            "family":summarize(&family)?,"family_origin_balanced":balanced_result}}),
    )
}

// A scalar allow-list prevents dynamic identity keys and arbitrary report fields
// from reaching the public artifact. Missing diagnostics remain explicit nulls.
const METRICS: [(&str, &str); 13] = [
    ("involvement_brier", "/model/involvement_brier"),
    (
        "balanced_involvement_brier",
        "/family_origin_balanced/involvement_brier",
    ),
    ("human_fpr_at_0_5", "/model/human_fpr_at_0_5"),
    ("ece10", "/model/ece10"),
    ("balanced_ece10", "/family_origin_balanced/ece10"),
    ("origin_accuracy", "/model/origin_accuracy/estimate"),
    ("origin_brier_sum3", "/model/origin_brier_sum3/estimate"),
    (
        "balanced_origin_accuracy",
        "/family_origin_balanced/origin_accuracy",
    ),
    (
        "balanced_origin_brier_sum3",
        "/family_origin_balanced/origin_brier_sum3",
    ),
    (
        "prior_involvement_brier",
        "/prior_baseline/involvement_brier",
    ),
    (
        "share_mae_percentage_points",
        "/model/share/mae_percentage_points/estimate",
    ),
    (
        "share_mse_percentage_points_squared",
        "/model/share/mse_percentage_points_squared/estimate",
    ),
    (
        "share_bias_percentage_points",
        "/model/share/bias_percentage_points/estimate",
    ),
];
const SHARE_BINS: [&str; 6] = ["0", "(0,25]", "(25,50]", "(50,75]", "(75,100)", "100"];
const SHARE_BIN_METRICS: [&str; 3] = [
    "mae_percentage_points",
    "mse_percentage_points_squared",
    "bias_percentage_points",
];
const REFERENCE_METRICS: [&str; 5] = [
    "top1",
    "top3",
    "mrr",
    "head_known_top1",
    "head_withheld_top1",
];
const ROLE_METRICS: [(&str, &str); 10] = [
    ("conditional_accuracy", "conditional_accuracy/estimate"),
    ("conditional_ce", "conditional_ce/estimate"),
    ("conditional_mrr", "conditional_mrr/estimate"),
    ("conditional_top3", "conditional_top3/estimate"),
    (
        "known_correct_named",
        "open_set/known_correct_named/estimate",
    ),
    ("known_fallback", "open_set/known_fallback/estimate"),
    ("known_wrong_named", "open_set/known_wrong_named/estimate"),
    ("withheld_rejection", "open_set/withheld_rejection/estimate"),
    (
        "accepted_named_precision",
        "open_set/accepted_named_precision/estimate",
    ),
    (
        "balanced_correct_named_vs_withheld_rejection",
        "open_set/balanced_correct_named_vs_withheld_rejection",
    ),
];
fn scalar(v: &Value) -> Result<Value> {
    if v.is_null() {
        Ok(Value::Null)
    } else {
        Ok(json!(number(v)?))
    }
}
fn at(v: &Value, p: &str) -> Result<Value> {
    scalar(v.pointer(p).unwrap_or(&Value::Null))
}
fn scalar_table(report: &Value) -> Result<Value> {
    let mut m = serde_json::Map::new();
    for (name, path) in METRICS {
        m.insert(name.into(), at(report, path)?);
    }
    for bin in SHARE_BINS {
        let values = &report["model"]["share"]["by_realized_share"][bin];
        for metric in SHARE_BIN_METRICS {
            m.insert(
                format!("share_bin_{bin}_{metric}"),
                scalar(&values[metric]["estimate"])?,
            );
        }
        // These three losses use the same eligible rows in the evaluator.
        m.insert(
            format!("share_bin_{bin}_documents"),
            scalar(&values["mae_percentage_points"]["documents"])?,
        );
    }
    for role in ["human", "ai"] {
        for (name, path) in ROLE_METRICS {
            m.insert(
                format!("{role}_{name}"),
                at(report, &format!("/attribution/{role}/{path}"))?,
            );
        }
        let reference = if report["reference_matching"]["available"] == true {
            &report["reference_matching"]["metrics"][role]
        } else {
            &Value::Null
        };
        for metric in REFERENCE_METRICS {
            m.insert(
                format!("reference_{role}_{metric}"),
                scalar(&reference[metric]["estimate"])?,
            );
            m.insert(
                format!("reference_{role}_{metric}_documents"),
                scalar(&reference[metric]["documents"])?,
            );
        }
        m.insert(
            format!("reference_{role}_gallery_identities"),
            scalar(&reference["gallery_identities"])?,
        );
    }
    Ok(Value::Object(m))
}
pub(super) fn repeated(values: &[Value]) -> Result<Value> {
    ensure!(values.len() == 3, "three fixed seeds");
    let available = values
        .iter()
        .filter(|v| !v.is_null())
        .map(number)
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"available_seeds":available.len(),"required_seeds":3,
        "mean":(available.len()==3).then(||available.iter().sum::<f64>()/3.),
        "min":(available.len()==3).then(||available.iter().copied().fold(f64::INFINITY,f64::min)),
        "max":(available.len()==3).then(||available.iter().copied().fold(f64::NEG_INFINITY,f64::max))}))
}
fn calibration(v: &Value) -> Result<Value> {
    let Some(a) = v.as_array() else {
        ensure!(v.is_null(), "calibration bins array");
        return Ok(Value::Null);
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for bin in a {
        let i = b::count(bin, "bin")?;
        ensure!(i < 10 && seen.insert(i), "calibration bin index");
        out.push(
            json!({"bin":i,"documents":b::count(bin,"documents")?,"weight":number(&bin["weight"])?,
            "mean_score":number(&bin["mean_score"])?,"observed_ai":number(&bin["observed_ai"])?}),
        );
    }
    Ok(json!(out))
}
fn ci(v: &Value) -> Result<Value> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    Ok(json!({"lower":number(&v["lower"])?,"upper":number(&v["upper"])?}))
}
fn diagnostics(report: &Value) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for role in ["human", "ai"] {
        let r = &report["attribution"][role];
        let mut counts = serde_json::Map::new();
        for key in [
            "documented_present",
            "documented_absent_not_scored",
            "present_identity_unspecified_not_scored",
            "presence_unspecified_not_scored",
        ] {
            counts.insert(key.into(), scalar(&r[key])?);
        }
        for (name, path) in [
            (
                "conditional_labelled_documents",
                "conditional_accuracy/documents",
            ),
            (
                "known_open_set_documents",
                "open_set/known_correct_named/documents",
            ),
            (
                "withheld_open_set_documents",
                "open_set/withheld_rejection/documents",
            ),
        ] {
            counts.insert(name.into(), at(r, &format!("/{path}"))?);
        }
        counts.insert(
            "conditional_metric_available".into(),
            json!(r["conditional_accuracy"]["estimate"].is_number()),
        );
        counts.insert(
            "withheld_metric_available".into(),
            json!(r["open_set"]["withheld_rejection"]["estimate"].is_number()),
        );
        out.insert(role.into(), Value::Object(counts));
    }
    Ok(
        json!({"attribution":out,"share_labelled_documents":scalar(&report["model"]["share"]["labelled_documents"])?,
        "reference_gallery_available":report["reference_matching"]["available"]==true,
        "missing_diagnostics_are_not_zero_error":true}),
    )
}
pub(super) fn table(runs: &[Run]) -> Result<Value> {
    ensure!(
        runs.len() == 3 && runs.iter().map(|r| r.seed).eq(SEEDS),
        "candidate fixed seed order"
    );
    let values = runs
        .iter()
        .map(|r| scalar_table(&r.report))
        .collect::<Result<Vec<_>>>()?;
    let mut stats = serde_json::Map::new();
    for key in values[0].as_object().context("metric table")?.keys() {
        stats.insert(
            key.clone(),
            repeated(&values.iter().map(|v| v[key].clone()).collect::<Vec<_>>())?,
        );
    }
    let mut per_seed = Vec::new();
    for (r, m) in runs.iter().zip(values) {
        per_seed.push(json!({"seed":r.seed,"metrics":m,"support":r.support,"diagnostics":diagnostics(&r.report)?,
            "calibration_bins":calibration(&r.report["model"]["calibration_bins"])?,
            "balanced_calibration_bins":calibration(&r.report["family_origin_balanced"]["calibration_bins"])?,
            "involvement_brier_ci95":ci(&r.report["model"]["involvement_brier_95ci"])?,
            "balanced_involvement_brier_ci95":ci(&r.report["family_origin_balanced"]["involvement_brier_95ci"])?,
            "pangram_on_this_run_paired_support":{
                "family_involvement_brier":scalar(&r.pg_report["family"]["pangram_brier"])?,
                "balanced_involvement_brier":scalar(&r.pg_report["family_origin_balanced"]["pangram_brier"])?,
                "human_fpr_at_0_5":scalar(&r.pg_report["human_fpr_at_0_5"]["pangram"])?,
                "fraction_mae_percentage_points_diagnostic":scalar(&r.pg_report["share"]["pangram_fraction_mae_percentage_points"])?,
                "fraction_is_not_span_ground_truth":true},"bindings":r.evidence}));
    }
    Ok(
        json!({"candidate":runs[0].candidate,"per_seed":per_seed,"fixed_seed_statistics":stats,
        "seed_statistics_use_all_three_or_null":true,"no_CI_endpoint_averaging":true}),
    )
}
pub(super) fn v6_public(report: &Value) -> Result<Value> {
    let mut strata = serde_json::Map::new();
    for name in ["all_origins", "human_model_only", "mixed_only"] {
        let r = &report[name];
        let wrap =
            json!({"model":r["family"],"family_origin_balanced":r["family_origin_balanced"]});
        let mut metrics = serde_json::Map::new();
        for (key, path) in METRICS
            .into_iter()
            .filter(|(k, _)| !k.starts_with("share_"))
        {
            metrics.insert(key.into(), at(&wrap, path)?);
        }
        strata.insert(name.into(),json!({"metrics":metrics,"documents":scalar(&r["family"]["documents"])?,
            "calibration_bins":calibration(&r["family"]["calibration_bins"])?,
            "balanced_calibration_bins":calibration(&r["family_origin_balanced"]["calibration_bins"])?}));
    }
    Ok(
        json!({"requested":b::count(report,"requested")?,"supported":b::count(report,"supported")?,
        "unsupported":b::count(report,"unsupported")?,"failed":b::count(report,"failed")?,
        "origin_metrics":strata,"share_available":false,"attribution_available":false,
        "temperature_frozen":true,"max_tokens_including_special_tokens":2048,"truncation":false}),
    )
}
