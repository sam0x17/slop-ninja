//! Publish aggregate-only evidence and paired family differences for the frozen pilot.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub(crate) fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
pub(crate) fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
pub(crate) fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("missing number")?;
    ensure!(n.is_finite(), "nonfinite value");
    Ok(n)
}
pub(crate) fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
pub(crate) fn selected(v: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(value) = v.get(*k) {
            out.insert((*k).into(), value.clone());
        }
    }
    Value::Object(out)
}
pub(crate) fn predictions(p: &Path) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for row in read(p)?.as_array().context("predictions")? {
        ensure!(
            out.insert(text(row, "id")?.into(), row["prediction"].clone())
                .is_none(),
            "duplicate prediction"
        );
    }
    Ok(out)
}
pub(crate) fn family_summary(values: &[(String, f64)]) -> Value {
    let mut groups = BTreeMap::<&str, (f64, usize)>::new();
    for (g, v) in values {
        let x = groups.entry(g).or_default();
        x.0 += v;
        x.1 += 1;
    }
    let means = groups
        .values()
        .map(|(s, n)| s / (*n as f64))
        .collect::<Vec<_>>();
    if means.is_empty() {
        return json!({"estimate":null,"ci95":null,"families":0,"documents":0});
    }
    let estimate = means.iter().sum::<f64>() / means.len() as f64;
    let ci = if means.len() < 2 {
        Value::Null
    } else {
        let mut seed = 20261026u64;
        let mut draws = Vec::new();
        for _ in 0..1000 {
            let mut total = 0.;
            for _ in &means {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                total += means[seed as usize % means.len()];
            }
            draws.push(total / means.len() as f64);
        }
        draws.sort_by(f64::total_cmp);
        json!([draws[24], draws[974]])
    };
    json!({"estimate":estimate,"ci95":ci,"families":means.len(),"documents":values.len()})
}
pub(crate) fn difference(
    rows: &[Value],
    a: &BTreeMap<String, Value>,
    b: &BTreeMap<String, Value>,
    kind: &str,
) -> Result<Vec<(String, f64)>> {
    ensure!(
        a.keys().eq(b.keys()) && a.len() == rows.len(),
        "paired population mismatch"
    );
    let mut out = Vec::new();
    for row in rows {
        let id = text(row, "id")?;
        let x = a.get(id).context("prediction missing")?;
        let y = &b[id];
        let value = if kind == "involvement_brier" {
            let target = number(&row["targets"]["origin"])?;
            let t = if target == 0. { 0. } else { 1. };
            (number(&x["ai_confidence_percent"])? / 100. - t).powi(2)
                - (number(&y["ai_confidence_percent"])? / 100. - t).powi(2)
        } else {
            let truth = &row["roles"][kind];
            if truth["present"] != true || truth["known"] != true || !truth["identity"].is_string()
            {
                continue;
            }
            let correct = |p: &Value| -> Result<f64> {
                let scores = p["attribution"][kind]["conditional_named_scores"]
                    .as_object()
                    .context("named scores")?;
                let mut best: Option<(&str, f64)> = None;
                for (name, v) in scores {
                    let score = number(v)?;
                    if best.is_none_or(|(_, b)| score > b) {
                        best = Some((name, score));
                    }
                }
                Ok(f64::from(
                    best.context("empty named scores")?.0 == text(truth, "identity")?,
                ))
            };
            correct(x)? - correct(y)?
        };
        out.push((text(row, "source_group")?.into(), value));
    }
    Ok(out)
}
pub(crate) fn balance_origins(
    rows: &[Value],
    values: &[(String, f64)],
) -> Result<Vec<(String, f64)>> {
    ensure!(
        rows.len() == values.len(),
        "balanced comparison support differs"
    );
    let mut cells = BTreeMap::<(String, u64), (f64, usize)>::new();
    for (row, (family, value)) in rows.iter().zip(values) {
        ensure!(
            text(row, "source_group")? == family,
            "balanced family differs"
        );
        let origin = row["targets"]["origin"]
            .as_u64()
            .context("missing origin")?;
        ensure!(origin < 3, "invalid origin");
        let cell = cells.entry((family.clone(), origin)).or_default();
        cell.0 += value;
        cell.1 += 1;
    }
    Ok(cells
        .into_iter()
        .map(|((g, _), (s, n))| (g, s / n as f64))
        .collect())
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 3 || args.len() == 4,
        "summarize_v6_fusion NUMERIC_BUNDLE FIT_ROOT NEW_REPORT [EXPOSURE_ROSTER]"
    );
    let data = Path::new(&args[0]);
    let fit = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    ensure!(!output.exists(), "new output required");
    let mut exposure = BTreeMap::new();
    if let Some(path) = args.get(3) {
        for line in fs::read_to_string(path)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            ensure!(
                exposure.insert(text(&row, "id")?.to_owned(), row).is_none(),
                "duplicate exposure row"
            );
        }
    }
    let mut reports = Vec::new();
    let mut preds = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut reference_rows = None;
    for modality in ["text", "combined", "cfg"] {
        let numeric = data.join(modality);
        let manifest = read(&numeric.join("manifest.json"))?;
        let dev_bytes = fs::read(numeric.join("development.jsonl"))?;
        let dev = std::str::from_utf8(&dev_bytes)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<Vec<Value>, _>>()?;
        let binding = manifest["splits"]
            .as_array()
            .context("split bindings")?
            .iter()
            .find(|b| b["split"] == "development")
            .context("Development binding")?;
        ensure!(
            binding["sha256"] == hex::encode(Sha256::digest(&dev_bytes)),
            "changed Development inputs"
        );
        let stripped = dev
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.as_object_mut().unwrap().remove("input");
                r
            })
            .collect::<Vec<_>>();
        if let Some(ref rows) = reference_rows {
            ensure!(
                *rows == stripped,
                "modalities differ outside learned inputs"
            );
        } else {
            reference_rows = Some(stripped);
        }
        let controls: &[&str] = if modality == "cfg" {
            &["joint"]
        } else {
            &["joint", "origin_only", "attribution_only"]
        };
        for control in controls {
            for seed in [17, 29, 41] {
                let name = format!("{control}-{seed}");
                let dir = fit.join(modality).join(&name);
                let report_path = fit.join(modality).join(format!("{name}-development.json"));
                let report = read(&report_path)?;
                let weights_sha = sha(&dir.join("weights.json"))?;
                ensure!(
                    report["fit"]["weights_sha256"] == weights_sha
                        && report["fit"]["data_manifest_sha256"]
                            == sha(&numeric.join("manifest.json"))?,
                    "fit binding differs"
                );
                let pred_path = dir.join("development-predictions.json");
                ensure!(
                    report["prediction_file_sha256"] == sha(&pred_path)?,
                    "predictions changed"
                );
                let rust_path = fit.join(modality).join(format!("{name}-rust.json"));
                let rust = read(&rust_path)?;
                ensure!(
                    number(&rust["max_absolute_delta_percentage_points"])? < 0.001,
                    "Rust reference replay failed"
                );
                let pg_path = fit.join(modality).join(format!("{name}-pangram.json"));
                let pg = read(&pg_path)?;
                ensure!(
                    pg["weights_sha256"] == weights_sha,
                    "Pangram comparison binding differs"
                );
                ensure!(
                    pg["paired"] == dev.len() && pg["requested"] == dev.len(),
                    "Pangram support differs"
                );
                let prediction_map = predictions(&pred_path)?;
                let mut by_exposure = serde_json::Map::new();
                if !exposure.is_empty() {
                    ensure!(
                        exposure.keys().eq(prediction_map.keys()),
                        "exposure population differs"
                    );
                    let mut strata = BTreeMap::<String, Vec<(String, f64)>>::new();
                    for row in &dev {
                        let id = text(row, "id")?;
                        let audit = &exposure[id];
                        ensure!(
                            row["source_group"] == audit["source_group"],
                            "exposure family differs"
                        );
                        let target = if row["targets"]["origin"] == 0 {
                            0.
                        } else {
                            1.
                        };
                        let p = number(&prediction_map[id]["ai_confidence_percent"])? / 100.;
                        strata
                            .entry(text(&audit["v6_exposure"], "tier")?.into())
                            .or_default()
                            .push((text(row, "source_group")?.into(), (p - target).powi(2)));
                    }
                    for (tier, values) in strata {
                        by_exposure.insert(tier, family_summary(&values));
                    }
                }
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
                                "documented_present",
                                "documented_absent_not_scored",
                                "present_identity_unspecified_not_scored",
                                "presence_unspecified_not_scored",
                                "presence_cutoff",
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
                reports.push(json!({"modality":modality,"control":control,"seed":seed,"development":report["model"],
                "prior_baseline":report["prior_baseline"],"family_origin_balanced":report["family_origin_balanced"],
                "uniform_origin_prior_balanced":report["uniform_origin_prior_balanced"],"attribution":attribution,"reference_matching":retrieval,
                "fit":selected(&report["fit"],&["selected_epoch","query_weighting","calibration_temperature","development_selection","supervision","training_status","elapsed_seconds","cuda_peak_allocated_bytes","torch","cuda","training_source_sha256","data_manifest_sha256","weights_sha256","checkpoint_sha256"]),
                "rust":selected(&rust,&["latency","artifact_json_bytes","loaded_numeric_weight_bytes","max_absolute_delta_percentage_points","runtime"]),
                "involvement_brier_by_v6_exposure":by_exposure,
                "pangram":selected(&pg,&["calibration","pangram","slop_ninja_paired","family_origin_balanced","paired_brier_gap_slop_minus_pangram","paired_brier_gap_95ci","requested","paired","calibration_requested","calibration_paired","by_realized_share","parity_established"])}));
                let key = format!("{modality}/{name}");
                preds.insert(key.clone(), prediction_map);
                hashes.insert(key,json!({"evaluation":sha(&report_path)?,"predictions":sha(&pred_path)?,"rust_replay":sha(&rust_path)?,"pangram_comparison":sha(&pg_path)?}));
            }
        }
    }
    let rows = reference_rows.context("no Development population")?;
    let mut paired = Vec::new();
    for (left, right, kind) in [
        ("combined/joint", "text/joint", "involvement_brier"),
        ("combined/joint", "cfg/joint", "involvement_brier"),
        ("text/joint", "text/origin_only", "involvement_brier"),
        (
            "combined/joint",
            "combined/origin_only",
            "involvement_brier",
        ),
        ("text/joint", "text/attribution_only", "human"),
        ("combined/joint", "combined/attribution_only", "human"),
        ("text/joint", "text/attribution_only", "ai"),
        ("combined/joint", "combined/attribution_only", "ai"),
    ] {
        for weighting in if kind == "involvement_brier" {
            vec!["family", "family_origin_balanced"]
        } else {
            vec!["family"]
        } {
            let mut samples = Vec::new();
            let mut seed_reports = Vec::new();
            for seed in [17, 29, 41] {
                let mut values = difference(
                    &rows,
                    &preds[&format!("{left}-{seed}")],
                    &preds[&format!("{right}-{seed}")],
                    kind,
                )?;
                if weighting == "family_origin_balanced" {
                    values = balance_origins(&rows, &values)?;
                }
                seed_reports.push(json!({"seed":seed,"difference":family_summary(&values)}));
                samples.push(values);
            }
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
                        samples.iter().map(|s| s[i].1).sum::<f64>() / 3.,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            paired.push(json!({"left":left,"right":right,"metric":kind,"weighting":weighting,"difference":"left_minus_right",
            "preferred_sign":if kind=="involvement_brier"{"negative"}else{"positive"},"seeds":seed_reports,
            "mean_over_fixed_seeds_then_family_bootstrap":family_summary(&averaged)}));
        }
    }
    let result = json!({"schema":"slop_ninja_v6_fusion_public_summary_v1","population":{"development_queries":rows.len(),"final_test_opened":false,"clean_holdout":false},
        "reports":reports,"paired":paired,"bindings":hashes,"bundle_manifest_sha256":sha(&data.join("manifest.json"))?,
        "summarizer_source_sha256":hex::encode(Sha256::digest(include_bytes!("summarize_v6_fusion.rs"))),
        "exposure_roster_sha256":args.get(3).map(|p|sha(Path::new(p))).transpose()?,
        "uncertainty":"1000 source-family bootstrap draws, seed20261026; conditional on checkpoint-selection Development and three fixed seeds; not final Test or rare-FP evidence. Balanced comparisons count origin cells as observations before family averaging.",
        "privacy":"aggregate-only; no texts, profile names, per-document identities, filesystem paths or live provider logs",
        "limitations":["Development used for checkpoint selection","CAL used for temperatures and named thresholds","cached embeddings require full frozen ModernBERT encoder at first-time inference","Rust replay qualifies the numeric head only","Pangram parity not established"]});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    f.write_all(&serde_json::to_vec_pretty(&result)?)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_family_means_are_not_document_weighted() {
        let result = family_summary(&[("a".into(), 1.), ("a".into(), 1.), ("b".into(), -1.)]);
        assert_eq!(result["estimate"], 0.);
        assert_eq!(result["families"], 2);
        assert!(family_summary(&[])["estimate"].is_null());
    }
    #[test]
    fn paired_balance_uses_equal_origin_mass_and_rejects_missing_labels() {
        let mut rows = vec![json!({"source_group":"work","targets":{"origin":0}})];
        let mut values = vec![("work".into(), 1.)];
        for _ in 0..20 {
            rows.push(json!({"source_group":"work","targets":{"origin":2}}));
            values.push(("work".into(), 0.));
        }
        assert_eq!(
            family_summary(&balance_origins(&rows, &values).unwrap())["estimate"],
            0.5
        );
        rows[0]["targets"]["origin"] = Value::Null;
        assert!(balance_origins(&rows, &values).is_err());
    }
}
