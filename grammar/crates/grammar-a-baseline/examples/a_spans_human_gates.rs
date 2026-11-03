//! Protocol spans-S2 human-document gates (H1, H2) on the Dev-iter primary
//! population. Metadata and predictions only; no text, no model.
//!
//! H1: the human-only involvement Brier with balanced weights recomputed over
//! the population's humans (the release evaluator's `human_mean_brier_balanced`),
//! artifact (released seed against released seed) and recipe (per-document loss
//! averaged over seeds) differences with the protocol source-group bootstrap;
//! pass when the 95% upper bound is at most `m_H1`.
//! H2: Dev-iter humans flagged at each fit's own CAL operating points (read from
//! the release `devit_op_diag` records); pass when the released seed's count and
//! the seed-mean count are at most the comparator's plus `m_H2`.
//!
//! usage: a_spans_human_gates CONFIG.json OUT.json

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use grammar_a_baseline::comp_eval::{
    Obs, Row, balanced_weights, group_values, parse_predictions, parse_rows,
};
use grammar_a_baseline::pangram_subsample::bootstrap_interval;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CONFIG_SCHEMA: &str = "slop_ninja_a_spans_s2_human_gates_config_v1";
const OUT_SCHEMA: &str = "slop_ninja_a_spans_s2_human_gates_v1";
const POINTS: [&str; 2] = ["0.01", "0.001"];

struct System {
    seed: u64,
    loss: Vec<f64>,
    brier: f64,
    at_half: usize,
    flags: Option<BTreeMap<String, u64>>,
}

fn sha256_file(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )))
}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("config field {k}"))
}

fn f(v: &Value, k: &str) -> Result<f64> {
    v[k].as_f64().with_context(|| format!("config field {k}"))
}

/// Per-human loss `(inv - y)^2` with `inv = min(p_model_only + p_mixed, 1)`, as the release evaluator scores it.
fn human_losses(rows: &[Row], humans: &[usize], path: &Path) -> Result<(Vec<f64>, usize)> {
    let preds = parse_predictions(
        &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )?;
    let mut loss = Vec::with_capacity(humans.len());
    let mut half = 0;
    for &i in humans {
        let inv = match preds.get(&rows[i].id) {
            Some(Obs::Ok(p)) => (p.p_model_only + p.p_mixed).min(1.0),
            Some(Obs::Failed(why)) => bail!(
                "{}: prediction failed for {} ({why})",
                path.display(),
                rows[i].id
            ),
            None => bail!("{}: no prediction for {}", path.display(), rows[i].id),
        };
        half += (inv >= 0.5) as usize;
        loss.push((inv - rows[i].y).powi(2));
    }
    Ok((loss, half))
}

fn pick(l: &[System], seed: u64) -> Option<&System> {
    l.iter().find(|x| x.seed == seed)
}

fn wmean(w: &[f64], x: &[f64]) -> f64 {
    w.iter().zip(x).map(|(a, b)| a * b).sum()
}

fn interval(rows: &[Row], humans: &[usize], w: &[f64], d: &[f64]) -> Value {
    let v = group_values(rows, humans, humans, w, d);
    let point = v.iter().sum::<f64>() / v.len() as f64;
    match bootstrap_interval(&v) {
        Some((lo, hi)) => {
            json!({"difference": point, "lower": lo, "upper": hi, "source_groups": v.len(),
                                 "method": "protocol_source_group_bootstrap_1000_seed20261026_draws24_974"})
        }
        None => {
            json!({"difference": point, "lower": null, "upper": null, "source_groups": v.len()})
        }
    }
}

fn run(cfg: &Value) -> Result<Value> {
    ensure!(cfg["schema"] == CONFIG_SCHEMA, "config schema");
    let rows_path = Path::new(s(cfg, "bundle_rows")?);
    let rows = parse_rows(&std::fs::read_to_string(rows_path).context("bundle rows")?)?;
    let humans: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].primary && rows[i].origin == 0)
        .collect();
    ensure!(!humans.is_empty(), "no primary humans");
    let w = balanced_weights(&rows, &humans);
    let released = cfg["released_seed"].as_u64().context("released_seed")?;
    let m_h1 = f(&cfg["margins"], "m_H1")?;
    let m_h2: BTreeMap<&str, f64> = POINTS
        .iter()
        .map(|p| {
            Ok((
                *p,
                cfg["margins"]["m_H2"][*p]
                    .as_f64()
                    .with_context(|| format!("m_H2 {p}"))?,
            ))
        })
        .collect::<Result<_>>()?;
    // Optional exact check against the release evaluator's per-seed values.
    let expected = &cfg["release_evaluator_values"];
    let mut systems: BTreeMap<String, Vec<System>> = BTreeMap::new();
    let mut inputs = Vec::new();
    for e in cfg["systems"].as_array().context("systems")? {
        let name = s(e, "name")?.to_string();
        let seed = e["seed"].as_u64().context("seed")?;
        let pred = Path::new(s(e, "predictions")?);
        let (loss, at_half) = human_losses(&rows, &humans, pred)?;
        let brier = wmean(&w, &loss);
        if let Some(x) = expected
            .get(format!("{name}:{seed}"))
            .and_then(Value::as_f64)
        {
            ensure!(
                x.to_bits() == brier.to_bits(),
                "{name}:{seed}: human-only Brier {brier} differs from the release evaluator's {x}"
            );
        }
        let flags = match e.get("devit_op").and_then(Value::as_str) {
            None => None,
            Some(p) => {
                let d: Value = serde_json::from_slice(
                    &std::fs::read(p).with_context(|| format!("read {p}"))?,
                )?;
                let mut m = BTreeMap::new();
                for pt in POINTS {
                    ensure!(
                        d[pt]["humans"].as_u64() == Some(humans.len() as u64),
                        "{p}: human count at {pt}"
                    );
                    m.insert(
                        pt.to_string(),
                        d[pt]["humans_flagged"]
                            .as_u64()
                            .with_context(|| format!("{p} {pt}"))?,
                    );
                }
                inputs.push(json!({"system": name, "seed": seed, "devit_op": p, "sha256": sha256_file(Path::new(p))?}));
                Some(m)
            }
        };
        inputs.push(json!({"system": name, "seed": seed, "predictions": pred, "sha256": sha256_file(pred)?}));
        let list = systems.entry(name.clone()).or_default();
        ensure!(
            list.iter().all(|x| x.seed != seed),
            "{name}: seed {seed} twice"
        );
        list.push(System {
            seed,
            loss,
            brier,
            at_half,
            flags,
        });
    }
    let mut per_system = serde_json::Map::new();
    for (name, list) in &systems {
        let seeds: Vec<Value> = list
            .iter()
            .map(|x| {
                json!({"seed": x.seed, "human_mean_brier_balanced": x.brier,
            "humans_at_0_5": x.at_half, "dev_iter_humans_flagged_at_cal_points": x.flags})
            })
            .collect();
        let mean = list.iter().map(|x| x.brier).sum::<f64>() / list.len() as f64;
        per_system.insert(
            name.clone(),
            json!({"seeds": seeds, "seed_mean_human_mean_brier_balanced": mean}),
        );
    }
    let mut comparisons = Vec::new();
    for c in cfg["comparisons"].as_array().cloned().unwrap_or_default() {
        let (a, b) = (s(&c, "a")?, s(&c, "b")?);
        let (sa, sb) = (
            systems.get(a).with_context(|| format!("system {a}"))?,
            systems.get(b).with_context(|| format!("system {b}"))?,
        );
        let artifact = match (pick(sa, released), pick(sb, released)) {
            (Some(x), Some(y)) => {
                let d: Vec<f64> = x.loss.iter().zip(&y.loss).map(|(p, q)| p - q).collect();
                Some(interval(&rows, &humans, &w, &d))
            }
            _ => None,
        };
        let recipe = if sa.len() >= 2 && sa.len() == sb.len() {
            let avg =
                |l: &[System], k: usize| l.iter().map(|x| x.loss[k]).sum::<f64>() / l.len() as f64;
            let d: Vec<f64> = (0..humans.len()).map(|k| avg(sa, k) - avg(sb, k)).collect();
            Some(interval(&rows, &humans, &w, &d))
        } else {
            None
        };
        let upper = |v: &Option<Value>| v.as_ref().and_then(|x| x["upper"].as_f64());
        let h1_artifact = upper(&artifact).map(|u| u <= m_h1);
        let h1_recipe = upper(&recipe).map(|u| u <= m_h1);
        let mut h2 = serde_json::Map::new();
        for pt in POINTS {
            let count = |x: &System| x.flags.as_ref().map(|m| m[pt] as f64);
            let art = match (
                pick(sa, released).and_then(count),
                pick(sb, released).and_then(count),
            ) {
                (Some(x), Some(y)) => Some(json!({"a": x, "b": y, "pass": x <= y + m_h2[pt]})),
                _ => None,
            };
            let mean = |l: &[System]| -> Option<f64> {
                let v: Option<Vec<f64>> = l.iter().map(count).collect();
                v.map(|v| v.iter().sum::<f64>() / v.len() as f64)
            };
            let rec = match (sa.len() >= 2 && sa.len() == sb.len(), mean(sa), mean(sb)) {
                (true, Some(x), Some(y)) => {
                    Some(json!({"a_mean": x, "b_mean": y, "pass": x <= y + m_h2[pt]}))
                }
                _ => None,
            };
            h2.insert(
                pt.to_string(),
                json!({"margin": m_h2[pt], "artifact": art, "recipe": rec}),
            );
        }
        let h2_artifact: Option<bool> = POINTS
            .iter()
            .map(|p| h2[*p]["artifact"]["pass"].as_bool())
            .collect::<Option<Vec<bool>>>()
            .map(|v| v.iter().all(|x| *x));
        let h2_recipe: Option<bool> = POINTS
            .iter()
            .map(|p| h2[*p]["recipe"]["pass"].as_bool())
            .collect::<Option<Vec<bool>>>()
            .map(|v| v.iter().all(|x| *x));
        comparisons.push(json!({"a": a, "b": b, "seeds_a": sa.iter().map(|x| x.seed).collect::<Vec<_>>(),
            "seeds_b": sb.iter().map(|x| x.seed).collect::<Vec<_>>(),
            "h1": {"margin": m_h1, "artifact": artifact, "recipe": recipe, "artifact_pass": h1_artifact, "recipe_pass": h1_recipe},
            "h2": {"points": h2, "artifact_pass": h2_artifact, "recipe_pass": h2_recipe}}));
    }
    Ok(
        json!({"schema": OUT_SCHEMA, "population": "dev_iter primary humans", "humans": humans.len(),
              "weights": "balanced over the primary humans: equal source-group mass, equal document mass within a group",
              "bundle_rows_sha256": sha256_file(rows_path)?, "released_seed": released, "systems": per_system,
              "comparisons": comparisons, "inputs": inputs}),
    )
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        args.len() == 3,
        "usage: a_spans_human_gates CONFIG.json OUT.json"
    );
    let out = Path::new(&args[2]);
    ensure!(!out.exists(), "new output required: {}", out.display());
    let cfg: Value = serde_json::from_slice(&std::fs::read(&args[1]).context("config")?)?;
    let mut value = run(&cfg)?;
    value["config_sha256"] = json!(sha256_file(Path::new(&args[1]))?);
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    std::fs::write(out, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, group: &str, origin: &str) -> String {
        let share = if origin == "human_only" { 0.0 } else { 1.0 };
        let label = if origin == "human_only" {
            Value::Null
        } else {
            json!("gen-a")
        };
        let op = if origin == "human_only" {
            Value::Null
        } else {
            json!("mirror")
        };
        json!({"id": id, "split": "dev_iter", "origin": origin, "share": share, "source_group": group,
               "domain": "d", "generator_label": label, "operation": op, "population": "primary"}).to_string()
    }

    fn pred(id: &str, inv: f64) -> String {
        json!({"id": id, "p_human": 1.0 - inv, "p_model_only": inv, "p_mixed": 0.0, "share": inv})
            .to_string()
    }

    #[test]
    fn human_brier_weights_and_gates() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            row("h1", "g1", "human_only"),
            row("h2", "g1", "human_only"),
            row("h3", "g2", "human_only"),
            row("m1", "g2", "model_only"),
        ]
        .join("\n");
        std::fs::write(dir.path().join("rows.jsonl"), rows).unwrap();
        let write = |name: &str, v: [f64; 4]| {
            let p = dir.path().join(name);
            let ids = ["h1", "h2", "h3", "m1"];
            std::fs::write(
                &p,
                ids.iter()
                    .zip(v)
                    .map(|(i, x)| pred(i, x))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
            p.to_string_lossy().to_string()
        };
        let op = |name: &str, a: u64, b: u64| {
            let p = dir.path().join(name);
            std::fs::write(&p, json!({"0.01": {"humans": 3, "humans_flagged": a}, "0.001": {"humans": 3, "humans_flagged": b}}).to_string()).unwrap();
            p.to_string_lossy().to_string()
        };
        let r17 = write("r17.jsonl", [0.1, 0.3, 0.2, 0.9]);
        let r29 = write("r29.jsonl", [0.2, 0.2, 0.2, 0.9]);
        let a17 = write("a17.jsonl", [0.1, 0.3, 0.6, 0.9]);
        let a29 = write("a29.jsonl", [0.2, 0.2, 0.2, 0.9]);
        // g1 humans carry 1/4 each, the g2 human 1/2 (balanced over the humans only).
        let expect_r17: f64 = [
            0.25 * 0.1f64.powi(2),
            0.25 * 0.3f64.powi(2),
            0.5 * 0.2f64.powi(2),
        ]
        .iter()
        .sum();
        let cfg = json!({"schema": CONFIG_SCHEMA, "bundle_rows": dir.path().join("rows.jsonl"), "released_seed": 17,
            "margins": {"m_H1": 0.1, "m_H2": {"0.01": 0.0, "0.001": 0.0}},
            "release_evaluator_values": {"R:17": expect_r17},
            "systems": [{"name": "R", "seed": 17, "predictions": r17, "devit_op": op("o1", 2, 0)},
                        {"name": "R", "seed": 29, "predictions": r29, "devit_op": op("o2", 1, 0)},
                        {"name": "A", "seed": 17, "predictions": a17, "devit_op": op("o3", 2, 1)},
                        {"name": "A", "seed": 29, "predictions": a29, "devit_op": op("o4", 1, 0)}],
            "comparisons": [{"a": "A", "b": "R"}]});
        let out = run(&cfg).unwrap();
        let r = &out["systems"]["R"]["seeds"][0];
        assert_eq!(
            r["human_mean_brier_balanced"].as_f64().unwrap().to_bits(),
            expect_r17.to_bits()
        );
        assert_eq!(out["systems"]["A"]["seeds"][0]["humans_at_0_5"], 1);
        let c = &out["comparisons"][0];
        // Only h3 differs at seed 17: 0.36 - 0.04 = 0.32 at weight 1/2.
        assert!((c["h1"]["artifact"]["difference"].as_f64().unwrap() - 0.16).abs() < 1e-12);
        assert!((c["h1"]["recipe"]["difference"].as_f64().unwrap() - 0.08).abs() < 1e-12);
        assert_eq!(c["h1"]["artifact_pass"], false);
        assert_eq!(c["h2"]["points"]["0.001"]["artifact"]["pass"], false);
        assert_eq!(c["h2"]["points"]["0.01"]["recipe"]["pass"], true);
        assert_eq!(c["h2"]["artifact_pass"], false);
        // A wrong expected value is refused.
        let mut bad = cfg.clone();
        bad["release_evaluator_values"]["R:17"] = json!(expect_r17 + 1e-15);
        assert!(run(&bad).is_err());
    }
}
