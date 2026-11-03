//! Protocol version 2 (PROTOCOL-v2.md, FROZEN-v2.json): verified bindings,
//! the three-slope class-balanced logistic fusion of arm F, and the helpers of
//! the reproduction gate (JSON comparison and a hash-only JSONL sink). The v1
//! modules are unchanged; v1's two-slope fit still serves arms D and E.
use crate::common::*;
use crate::run::{FUSION_F, LAMBDA, REPLICATES, SEED_STRING};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub const SCHEMA_V2: &str = "slop_ninja_cfg_vs_encoder_evaluation_v2";
pub const FROZEN_V2_SHA256: &str = "522b03633411a3f92a6eb507d2793d8926e7fea4da9352f13b0a5583469074b8";
pub const PROTOCOL_V2_SHA256: &str = "15df96d752b74121393374751082088b3904f5d1abfe18e80b39155716ed1e40";
pub const ADDENDUM_SHA256: &str = "08289539eab0fa7f5943af34a0ea16a2383bbdc4173eb4d569a6f4c3b5db8ee2";
pub const PRIMARY_METRIC: &str = "primary/T1/pooled/k5/auc";

/// Hash-checked v2 and v1 bindings, read from FROZEN-v2.json.
pub struct Bindings {
    pub frozen_v2: Value,
    pub v1_selection_sha256: String,
    pub v1_record_sha256: String,
    pub v1_results_sha256: String,
    pub v1_evaluator_record_sha256: String,
    pub v1_binary_sha256: String,
    pub expected_trials: u64,
    pub expected_genuine_trials: u64,
}

fn text<'a>(v: &'a Value, pointer: &str) -> Result<&'a str> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("FROZEN-v2.json missing {pointer}"))
}

/// Verifies FROZEN-v2.json, PROTOCOL-v2.md, the pooling addendum and the
/// unchanged v1 files (FROZEN.json, PROTOCOL.md, DEVIATIONS.md), and that
/// the constants this binary uses are the frozen ones.
pub fn bindings(root: &Path) -> Result<Bindings> {
    let bytes = fs::read(root.join("FROZEN-v2.json")).context("FROZEN-v2.json")?;
    ensure!(
        sha_hex(&bytes) == FROZEN_V2_SHA256,
        "FROZEN-v2.json does not match its frozen hash"
    );
    let f: Value = serde_json::from_slice(&bytes)?;
    ensure!(f["protocol_id"] == PROTOCOL_ID && f["version"] == 2, "FROZEN-v2.json protocol");
    ensure!(f["protocol_md"]["sha256"] == PROTOCOL_V2_SHA256, "PROTOCOL-v2.md binding");
    require_hash(&root.join("PROTOCOL-v2.md"), PROTOCOL_V2_SHA256)?;
    let ext = &f["extends_v1"];
    ensure!(ext["ADDENDUM-v1-pooling.md"] == ADDENDUM_SHA256, "addendum binding");
    require_hash(&root.join("ADDENDUM-v1-pooling.md"), ADDENDUM_SHA256)?;
    ensure!(ext["FROZEN.json"] == FROZEN_SHA256, "FROZEN.json binding");
    require_hash(&root.join("FROZEN.json"), FROZEN_SHA256)?;
    require_hash(&root.join("PROTOCOL.md"), text(ext, "/PROTOCOL.md")?)?;
    require_hash(&root.join("DEVIATIONS.md"), text(ext, "/DEVIATIONS.md/sha256")?)?;
    // Constants of this binary against the frozen values.
    let fit = &f["arms"]["F"]["fit"];
    ensure!(fit["lambda"].as_f64() == Some(LAMBDA), "frozen lambda");
    ensure!(
        f["arms"]["F"]["components"] == json!(FUSION_F.1.map(|c| c.to_string())),
        "frozen F component order"
    );
    ensure!(
        f["arms"]["D"]["components"] == json!(["B", "C"]),
        "frozen D components"
    );
    let bs = &f["statistics"]["bootstrap"];
    ensure!(bs["replicates"] == REPLICATES as u64, "frozen replicates");
    ensure!(bs["seed_string"] == SEED_STRING, "frozen seed string");
    ensure!(f["primary_endpoint"]["metric"] == PRIMARY_METRIC, "frozen primary metric");
    ensure!(f["primary_endpoint"]["contrast"] == "F - D", "frozen contrast");
    ensure!(
        f["pooling"]["name"] == "cfg_vs_encoder_v1_as_run_reference_unit_mean_cosine"
            && f["pooling"]["frozen_wording_run_in_v2"] == false,
        "frozen pooling"
    );
    let ev1 = &ext["eval_v1"];
    Ok(Bindings {
        v1_selection_sha256: text(ev1, "/selection~1selection.json")?.into(),
        v1_record_sha256: text(ev1, "/selection_record_sha256")?.into(),
        v1_results_sha256: text(ev1, "/results~1results.json")?.into(),
        v1_evaluator_record_sha256: text(ev1, "/EVALUATOR.json")?.into(),
        v1_binary_sha256: text(ev1, "/evaluator_binary_sha256")?.into(),
        expected_trials: fit["expected_trials"].as_u64().context("expected_trials")?,
        expected_genuine_trials: fit["expected_genuine_trials"]
            .as_u64()
            .context("expected_genuine_trials")?,
        frozen_v2: f,
    })
}

/// Reads a JSON file after checking its sha256.
pub fn read_bound_json(path: &Path, expected: &str) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        sha_hex(&bytes) == expected,
        "sha256 mismatch for {}",
        path.display()
    );
    Ok(serde_json::from_slice(&bytes)?)
}

/// Result of the three-slope fit.
#[derive(Clone, Copy, Debug)]
pub struct Fit3 {
    pub beta: [f64; 4],
    pub iterations: usize,
    pub converged: bool,
    pub last_max_abs_step: f64,
}

/// Arm F's fusion (PROTOCOL-v2.md section 4): class-balanced logistic
/// regression with an intercept and an L2 penalty on the three slopes,
/// minimize sum_i w'_i logloss_i + lambda (b1^2 + b2^2 + b3^2), each class's
/// weights rescaled to total 0.5. Newton from zero, at most 200 iterations,
/// stopping when the largest absolute step is below 1e-13. A singular Hessian
/// or a non-finite coefficient is a failure.
pub fn logistic_fit3(x: &[[f64; 3]], y: &[bool], w: &[f64], lambda: f64) -> Result<Fit3> {
    ensure!(x.len() == y.len() && y.len() == w.len() && !x.is_empty(), "fusion inputs");
    ensure!(
        x.iter().flatten().chain(w).all(|v| v.is_finite()),
        "non-finite fusion input"
    );
    let pos: f64 = y.iter().zip(w).filter(|(p, _)| **p).map(|(_, w)| w).sum();
    let neg: f64 = y.iter().zip(w).filter(|(p, _)| !**p).map(|(_, w)| w).sum();
    ensure!(pos > 0.0 && neg > 0.0, "fusion needs both classes");
    let wb: Vec<f64> = y
        .iter()
        .zip(w)
        .map(|(p, w)| if *p { 0.5 * w / pos } else { 0.5 * w / neg })
        .collect();
    let mut b = [0.0f64; 4];
    let mut last = f64::NAN;
    for it in 1..=200 {
        let mut g = [0.0f64; 4];
        let mut h = [[0.0f64; 4]; 4];
        for i in 0..x.len() {
            let f = [1.0, x[i][0], x[i][1], x[i][2]];
            let eta = b[0] + b[1] * f[1] + b[2] * f[2] + b[3] * f[3];
            let p = 1.0 / (1.0 + (-eta).exp());
            let r = p - if y[i] { 1.0 } else { 0.0 };
            let c = p * (1.0 - p);
            for a in 0..4 {
                g[a] += wb[i] * r * f[a];
                for bb in 0..4 {
                    h[a][bb] += wb[i] * c * f[a] * f[bb];
                }
            }
        }
        for a in 1..4 {
            g[a] += 2.0 * lambda * b[a];
            h[a][a] += 2.0 * lambda;
        }
        let step = solve4(h, g)?;
        ensure!(step.iter().all(|s| s.is_finite()), "non-finite fusion step");
        let mut delta = 0.0f64;
        for a in 0..4 {
            b[a] -= step[a];
            delta = delta.max(step[a].abs());
        }
        ensure!(b.iter().all(|v| v.is_finite()), "non-finite fusion coefficient");
        last = delta;
        if delta < 1e-13 {
            return Ok(Fit3 {
                beta: b,
                iterations: it,
                converged: true,
                last_max_abs_step: delta,
            });
        }
    }
    Ok(Fit3 {
        beta: b,
        iterations: 200,
        converged: false,
        last_max_abs_step: last,
    })
}

/// Gauss-Jordan elimination with partial pivoting; the singularity test is
/// v1's (`|pivot| > 1e-300`).
pub fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> Result<[f64; 4]> {
    for c in 0..4 {
        let piv = (c..4)
            .max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))
            .unwrap();
        ensure!(a[piv][c].abs() > 1e-300, "singular fusion Hessian");
        a.swap(c, piv);
        b.swap(c, piv);
        for r in 0..4 {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in 0..4 {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    Ok([b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2], b[3] / a[3][3]])
}

/// Pair-file sink: writes and hashes (as v1's JsonlWriter), or only hashes the
/// exact bytes that would be written.
pub enum PairSink {
    File(JsonlWriter),
    Hash { hash: Sha256, rows: usize },
}

impl PairSink {
    pub fn open(dir: Option<&Path>, name: &str) -> Result<Self> {
        Ok(match dir {
            Some(d) => PairSink::File(JsonlWriter::create(&d.join(name))?),
            None => PairSink::Hash {
                hash: Sha256::new(),
                rows: 0,
            },
        })
    }

    pub fn write<T: Serialize>(&mut self, value: &T) -> Result<()> {
        match self {
            PairSink::File(w) => w.write(value),
            PairSink::Hash { hash, rows } => {
                let mut line = serde_json::to_vec(value)?;
                line.push(b'\n');
                hash.update(&line);
                *rows += 1;
                Ok(())
            }
        }
    }

    pub fn finish(self) -> Result<(usize, String)> {
        match self {
            PairSink::File(w) => w.finish(),
            PairSink::Hash { hash, rows } => Ok((rows, hex::encode(hash.finalize()))),
        }
    }
}

/// Serializes and parses back, so values compare as written JSON.
pub fn roundtrip(v: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&serde_json::to_vec(v)?)?)
}

/// JSON pointers at which `a` and `b` differ (JSON value equality, numbers
/// compared as parsed JSON numbers), at most `limit`.
pub fn json_diff(a: &Value, b: &Value, path: &str, out: &mut Vec<String>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}/{}", k.replace('~', "~0").replace('/', "~1"));
                match (x.get(k), y.get(k)) {
                    (Some(u), Some(v)) => json_diff(u, v, &p, out, limit),
                    _ => out.push(p),
                }
                if out.len() >= limit {
                    return;
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                json_diff(u, v, &format!("{path}/{i}"), out, limit);
            }
        }
        _ => {
            if a != b {
                out.push(path.to_string());
            }
        }
    }
}

/// Gate comparison of a recomputed v1-format document against the recorded
/// one: equal after a round trip, with the differing pointers (no values).
pub fn gate_compare(recomputed: &Value, recorded: &Value) -> Result<Value> {
    let r = roundtrip(recomputed)?;
    let mut diffs = Vec::new();
    json_diff(&r, recorded, "", &mut diffs, 200);
    Ok(json!({"equal": diffs.is_empty() && r == *recorded, "differing_pointers": diffs}))
}

/// Removes arm F's scores from a v2 pair row, leaving the v1 row layout.
pub fn strip_f(row: &mut Value) -> Result<()> {
    let sc = row
        .get_mut("scores")
        .and_then(Value::as_object_mut)
        .context("pair row without scores")?;
    if sc.remove("F").is_some() {
        return Ok(());
    }
    for v in sc.values_mut() {
        let m: &mut Map<String, Value> = v.as_object_mut().context("pair row score layout")?;
        ensure!(m.remove("F").is_some(), "pair row without an F score");
    }
    Ok(())
}
