//! Retrospective paired V6/CFG/Pangram evaluation; no fitting or provider calls.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const LABELS: [&str; 3] = ["human_only", "model_only", "mixed"];
const V6_ID: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";
#[derive(Parser)]
struct Args {
    #[arg(long)]
    prepared: PathBuf,
    #[arg(long)]
    v6_predictions: PathBuf,
    #[arg(long)]
    v6_artifact: PathBuf,
    #[arg(long)]
    cfg_fit: PathBuf,
    #[arg(long)]
    pangram_comparison: PathBuf,
    #[arg(long)]
    pangram_results: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn sha(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn rows(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
fn keyed(input: Vec<Value>) -> Result<BTreeMap<String, Value>> {
    let mut map = BTreeMap::new();
    for row in input {
        ensure!(
            map.insert(text(&row, "id")?.to_owned(), row).is_none(),
            "duplicate prediction id"
        );
    }
    Ok(map)
}
fn probability(v: &Value) -> Result<f64> {
    let p = v.as_f64().context("probability")?;
    ensure!(
        p.is_finite() && (0.0..=1.0).contains(&p),
        "invalid probability"
    );
    Ok(p)
}
fn origin(v: &Value) -> Result<[f64; 3]> {
    let p = if let Some(a) = v.as_array() {
        ensure!(a.len() == 3, "origin shape");
        [
            probability(&a[0])?,
            probability(&a[1])?,
            probability(&a[2])?,
        ]
    } else {
        [
            probability(&v[LABELS[0]])?,
            probability(&v[LABELS[1]])?,
            probability(&v[LABELS[2]])?,
        ]
    };
    ensure!(
        (p.iter().sum::<f64>() - 1.).abs() < 1e-7,
        "origin probabilities do not sum to one"
    );
    Ok(p)
}
#[derive(Clone)]
struct Row {
    id: String,
    family: String,
    tier: String,
    target: usize,
    v6: [f64; 3],
    cfg: [f64; 3],
    pangram: f64,
}
fn means(values: &[(String, f64)]) -> Vec<f64> {
    let mut groups = BTreeMap::<&str, (f64, usize)>::new();
    for (g, v) in values {
        let e = groups.entry(g).or_default();
        e.0 += v;
        e.1 += 1;
    }
    groups.values().map(|(sum, n)| sum / (*n as f64)).collect()
}
fn metric(values: Vec<(String, f64)>) -> Value {
    let m = means(&values);
    if m.is_empty() {
        return json!({"estimate":null,"documents":0,"families":0,"ci95":null});
    }
    let estimate = m.iter().sum::<f64>() / m.len() as f64;
    let ci = if m.len() < 2 {
        Value::Null
    } else {
        let mut seed = 20261026u64;
        let mut draws = Vec::new();
        for _ in 0..1000 {
            let mut sum = 0.;
            for _ in &m {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                sum += m[seed as usize % m.len()];
            }
            draws.push(sum / m.len() as f64);
        }
        draws.sort_by(f64::total_cmp);
        json!([draws[24], draws[974]])
    };
    json!({"estimate":estimate,"documents":values.len(),"families":m.len(),"ci95":ci})
}
fn score(row: &Row, which: usize) -> f64 {
    match which {
        0 => row.v6[1] + row.v6[2],
        1 => row.cfg[1] + row.cfg[2],
        _ => row.pangram,
    }
}
fn model_metrics(rows: &[&Row], which: usize) -> Value {
    let values = |f: &dyn Fn(&Row) -> f64| {
        rows.iter()
            .map(|r| (r.family.clone(), f(r)))
            .collect::<Vec<_>>()
    };
    let mut result = json!({
        "involvement_brier":metric(values(&|r|(score(r,which)-f64::from(r.target!=0)).powi(2))),
        "involvement_log_loss":metric(values(&|r|{let p=score(r,which).clamp(1e-15,1.-1e-15);if r.target==0{-(1.-p).ln()}else{-p.ln()}})),
        "involvement_accuracy_at_0_5":metric(values(&|r|f64::from((score(r,which)>=0.5)==(r.target!=0)))),
        "human_false_positive_rate_at_0_5":metric(rows.iter().filter(|r|r.target==0).map(|r|(r.family.clone(),f64::from(score(r,which)>=0.5))).collect()),
        "ai_recall_at_0_5":metric(rows.iter().filter(|r|r.target!=0).map(|r|(r.family.clone(),f64::from(score(r,which)>=0.5))).collect()),
    });
    let family_counts = rows
        .iter()
        .fold(BTreeMap::<&str, usize>::new(), |mut m, r| {
            *m.entry(&r.family).or_default() += 1;
            m
        });
    let mut bins = [[0.0; 3]; 10];
    for r in rows {
        let p = score(r, which);
        let w = 1.0 / (family_counts[r.family.as_str()] as f64 * family_counts.len() as f64);
        let b = &mut bins[((p * 10.).floor() as usize).min(9)];
        b[0] += w;
        b[1] += w * p;
        b[2] += w * f64::from(r.target != 0);
    }
    result["involvement_ece10"] = if rows.is_empty() {
        Value::Null
    } else {
        json!(bins.iter().map(|b| (b[1] - b[2]).abs()).sum::<f64>())
    };
    result["calibration_bins"] = json!(
        bins.iter()
            .enumerate()
            .filter(|(_, b)| b[0] > 0.)
            .map(|(i, b)| json!({"bin":i,"weight":b[0],"predicted":b[1]/b[0],"observed":b[2]/b[0]}))
            .collect::<Vec<_>>()
    );
    if which < 2 {
        result["origin_accuracy"] = metric(values(&|r| {
            let p = if which == 0 { r.v6 } else { r.cfg };
            let best = (0..3)
                .max_by(|a, b| p[*a].total_cmp(&p[*b]).then_with(|| b.cmp(a)))
                .unwrap();
            f64::from(best == r.target)
        }));
        result["origin_brier_sum3"] = metric(values(&|r| {
            let p = if which == 0 { r.v6 } else { r.cfg };
            (0..3)
                .map(|i| (p[i] - f64::from(i == r.target)).powi(2))
                .sum()
        }));
    }
    if which == 0 {
        result["historical_operating_points"]=json!([0.8436493155285434,0.4287369114260753].map(|t|json!({"threshold":t,"comparison":"strictly_greater_than",
            "human_false_positive_rate":metric(rows.iter().filter(|r|r.target==0).map(|r|(r.family.clone(),f64::from(score(r,0)>t))).collect()),
            "ai_recall":metric(rows.iter().filter(|r|r.target!=0).map(|r|(r.family.clone(),f64::from(score(r,0)>t))).collect())})));
    }
    result
}
fn section(rows: Vec<&Row>) -> Value {
    let mut output = json!({"documents":rows.len(),"origin_counts":(0..3).map(|i|rows.iter().filter(|r|r.target==i).count()).collect::<Vec<_>>()});
    for (which, name) in ["v6", "cfg", "mapped_pangram"].into_iter().enumerate() {
        output[name] = model_metrics(&rows, which);
    }
    for (a, b, name) in [
        (0, 2, "v6_minus_pangram_brier"),
        (1, 0, "cfg_minus_v6_brier"),
        (1, 2, "cfg_minus_pangram_brier"),
    ] {
        output[name] = metric(
            rows.iter()
                .map(|r| {
                    let y = f64::from(r.target != 0);
                    (
                        r.family.clone(),
                        (score(r, a) - y).powi(2) - (score(r, b) - y).powi(2),
                    )
                })
                .collect(),
        );
    }
    output
}
fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "new output required");
    let binding = read(&a.prepared.join("manifest.json"))?;
    ensure!(
        binding["schema"] == "slop_ninja_reference_replay_request_v1",
        "unsupported request schema"
    );
    ensure!(
        binding["requests_sha256"] == sha(&a.prepared.join("requests.jsonl"))?
            && binding["roster_sha256"] == sha(&a.prepared.join("roster.jsonl"))?,
        "prepared files changed"
    );
    let split = text(&binding, "split")?;
    ensure!(matches!(split, "development" | "test"), "evaluation split");
    let roster = keyed(rows(&a.prepared.join("roster.jsonl"))?)?;
    let requests = keyed(rows(&a.prepared.join("requests.jsonl"))?)?;
    ensure!(roster.keys().eq(requests.keys()), "request roster differs");
    let artifact = read(&a.v6_artifact.join("manifest.json"))?;
    let artifact_id = text(&artifact, "artifact_id")?;
    if artifact_id != V6_ID {
        ensure!(
            read(&a.v6_artifact.join("RUNTIME_VARIANT.json"))?["parent_artifact_id"] == V6_ID,
            "not a bound V6 variant"
        );
    }
    let v6 = keyed(rows(&a.v6_predictions)?)?;
    ensure!(
        v6.keys().eq(roster.keys()),
        "V6 population mismatch; retain incomplete run as failure"
    );
    let cfg_path = a.cfg_fit.join(format!("{split}-predictions.json"));
    let cfg = keyed(
        read(&cfg_path)?
            .as_array()
            .context("CFG prediction array")?
            .clone(),
    )?;
    ensure!(cfg.keys().eq(roster.keys()), "CFG population mismatch");
    let comparison = read(&a.pangram_comparison)?;
    ensure!(
        comparison["split"] == split
            && comparison["weights_sha256"] == sha(&a.cfg_fit.join("weights.json"))?,
        "comparison model binding mismatch"
    );
    let pg = keyed(
        comparison["records"]
            .as_array()
            .context("Pangram paired records")?
            .clone(),
    )?;
    ensure!(
        pg.keys().eq(roster.keys()),
        "Pangram paired population mismatch"
    );
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for (id, row) in &roster {
        let group = text(row, "source_group")?;
        let text_hash = text(row, "text_sha256")?;
        ensure!(
            hex::encode(Sha256::digest(text(&requests[id], "text")?.as_bytes())) == text_hash,
            "request text differs from roster"
        );
        ensure!(
            v6[id]["artifact_id"] == artifact_id && v6[id]["labels"] == json!(LABELS),
            "V6 result identity/order mismatch"
        );
        ensure!(pg[id]["source_group"] == group, "Pangram family differs");
        let pg_path = a.pangram_results.join(id).join("result.json");
        let observed = read(&pg_path)?;
        ensure!(
            pg[id]["observation_sha256"] == sha(&pg_path)?
                && observed["text_sha256"] == text_hash
                && observed["source_group"] == group,
            "Pangram exact-text binding mismatch"
        );
        ensure!(
            observed["response"]["stage"] == "STAGE_SUCCESS",
            "Pangram observation not successful"
        );
        let c = origin(&cfg[id]["prediction"]["origin"])?;
        ensure!(
            (cfg[id]["prediction"]["ai_confidence_percent"]
                .as_f64()
                .context("CFG confidence")?
                / 100.
                - c[1]
                - c[2])
                .abs()
                < 1e-7,
            "CFG confidence differs from origin"
        );
        if v6[id]["status"] != "ok" {
            excluded.push(json!({"id":id,"source_group":group,"origin":row["origin"],"exposure":row["v6_exposure"],"status":v6[id]["status"],"reason":v6[id]["reason"]}));
            continue;
        }
        let p = origin(&v6[id]["probabilities"])?;
        ensure!(
            (probability(&v6[id]["model_or_assistance_probability"])? - p[1] - p[2]).abs() < 1e-7,
            "V6 involvement differs from origin"
        );
        let target = LABELS
            .iter()
            .position(|s| row["origin"] == *s)
            .context("origin target")?;
        included.push(Row {
            id: id.clone(),
            family: group.into(),
            tier: text(&row["v6_exposure"], "tier")?.into(),
            target,
            v6: p,
            cfg: c,
            pangram: probability(&pg[id]["mapped_pangram_involvement"])?,
        });
    }
    ensure!(!included.is_empty(), "no common support");
    let tiers = included
        .iter()
        .map(|r| r.tier.clone())
        .collect::<BTreeSet<_>>();
    let by_tier = tiers
        .iter()
        .map(|t| {
            (
                t,
                section(included.iter().filter(|r| &r.tier == t).collect()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let report = json!({"schema":"slop_ninja_v6_cfg_pangram_replay_v1","split":split,"requested":roster.len(),"paired":included.len(),"excluded":excluded,
        "common_support_all_origins":section(included.iter().collect()),
        "common_support_human_model_only":section(included.iter().filter(|r|r.target!=2).collect()),
        "common_support_mixed_only":section(included.iter().filter(|r|r.target==2).collect()),"by_v6_exposure":by_tier,
        "weighting":"equal source-family means within each metric's eligible rows; ECE bins use global equal-family weights",
        "uncertainty":"1000 paired source-family bootstrap draws; seed20261026; not rare-FP guarantees",
        "artifact_id":artifact_id,"parent_v6_artifact_id":V6_ID,"pangram_calibration":comparison["calibration"],
        "bindings":{"preparation_sha256":sha(&a.prepared.join("manifest.json"))?,"v6_predictions_sha256":sha(&a.v6_predictions)?,
            "v6_manifest_sha256":sha(&a.v6_artifact.join("manifest.json"))?,"cfg_predictions_sha256":sha(&cfg_path)?,
            "cfg_weights_sha256":sha(&a.cfg_fit.join("weights.json"))?,"pangram_comparison_sha256":sha(&a.pangram_comparison)?,
            "evaluator_sha256":hex::encode(Sha256::digest(include_bytes!("score_reference_replay.rs")))},
        "per_document":included.iter().map(|r|json!({"id":r.id,"source_group":r.family,"origin":LABELS[r.target],"v6_exposure":r.tier,
            "v6_involvement":score(r,0),"cfg_involvement":score(r,1),"mapped_pangram_involvement":r.pangram})).collect::<Vec<_>>(),
        "provider_calls":0,"test_used_for_selection":false,"clean_holdout":false,
        "limits":["V6 origin-only comparator has no trained share or attribution","historical V6 headline metrics exclude mixed and use a different cohort",
            "current Test is consumed retrospective evidence","unmatched rosters do not exclude pretraining exposure",
            "Pangram mapped content fraction is not native involvement confidence","unsupported rows excluded only from explicitly paired metrics; all remain in coverage"]});
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&a.out)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"requested":roster.len(),"paired":included.len(),"v6_brier":report["common_support_all_origins"]["v6"]["involvement_brier"]["estimate"],
        "cfg_brier":report["common_support_all_origins"]["cfg"]["involvement_brier"]["estimate"],"pangram_brier":report["common_support_all_origins"]["mapped_pangram"]["involvement_brier"]["estimate"]})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_population_is_unavailable_not_perfectly_calibrated() {
        let result = model_metrics(&[], 0);
        assert!(result["involvement_ece10"].is_null());
        assert!(result["involvement_brier"]["estimate"].is_null());
    }
    #[test]
    fn probability_validation_rejects_wrong_shape_and_mass() {
        assert!(origin(&json!([0.2, 0.2])).is_err());
        assert!(origin(&json!([0.8, 0.8, 0.1])).is_err());
        assert!(origin(&json!([0.2, 0.3, 0.5])).is_ok());
    }
    #[test]
    fn families_have_equal_weight_despite_row_counts() {
        let v = metric(vec![
            ("a".into(), 0.),
            ("a".into(), 0.),
            ("a".into(), 0.),
            ("b".into(), 1.),
        ]);
        assert_eq!(v["estimate"], 0.5);
        assert_eq!(v["families"], 2);
    }
    #[test]
    fn mixed_is_positive_for_involvement_but_distinct_for_origin() {
        let r = Row {
            id: "synthetic".into(),
            family: "one".into(),
            tier: "none".into(),
            target: 2,
            v6: [0.1, 0.8, 0.1],
            cfg: [0.1, 0.1, 0.8],
            pangram: 0.9,
        };
        let m = model_metrics(&[&r], 0);
        assert!((m["involvement_brier"]["estimate"].as_f64().unwrap() - 0.01).abs() < 1e-10);
        assert_eq!(m["origin_accuracy"]["estimate"], 0.);
        assert!(m["human_false_positive_rate_at_0_5"]["estimate"].is_null());
    }
}
