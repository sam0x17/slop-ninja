//! Four token-position bins of frozen CFG leaf expectations, plus matched global control.
//! Usage: prepare_ordered_cfg CFG_LEAF_NUMERIC EXPORT NEW_OUT
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};
const PARTS: [&str; 4] = ["train", "development", "calibration", "reference"];
const BINS: usize = 4;
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn file_sha(p: &Path) -> Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut b = [0u8; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(p: &Path) -> Result<Vec<Value>> {
    BufReader::new(File::open(p)?)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect()
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn jsonl(r: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for v in r {
        serde_json::to_writer(&mut b, v)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn bin(start: usize, len: usize, total: usize) -> usize {
    if total == 0 {
        0
    } else {
        ((2 * start + len) * BINS / (2 * total)).min(BINS - 1)
    }
}
#[derive(Deserialize)]
struct Features {
    counts: BTreeMap<String, f64>,
    projection_available: bool,
}
#[derive(Deserialize)]
struct Window {
    tokens: Vec<serde::de::IgnoredAny>,
    features: Features,
}
struct Raw {
    counts: Vec<f64>,
    masks: Vec<f64>,
}
fn project(windows: &Path, keys: &[String], total: usize, global: &Value) -> Result<(Raw, Raw)> {
    let n = keys.len();
    let mut counts = vec![0.; n * BINS];
    let mut tokens = [0usize; BINS];
    let mut available = [true; BINS];
    let mut start = 0;
    for line in BufReader::new(File::open(windows)?).lines() {
        // Unknown forest/source fields are skipped: no prose or lexical identities enter features.
        let window: Window = serde_json::from_str(&line?)?;
        let length = window.tokens.len();
        let b = bin(start, length, total);
        start += length;
        tokens[b] += length;
        available[b] &= window.features.projection_available;
        for (i, k) in keys.iter().enumerate() {
            let value = window.features.counts.get(k).copied().unwrap_or(0.);
            ensure!(value.is_finite() && value >= 0., "invalid window count");
            counts[b * n + i] += value;
        }
    }
    ensure!(start == total, "window/document token denominator mismatch");
    let mut broadcast = vec![0.; n * BINS];
    for (i, k) in keys.iter().enumerate() {
        let expected = global[k].as_f64().unwrap_or(0.);
        let observed = (0..BINS).map(|b| counts[b * n + i]).sum::<f64>();
        ensure!(
            (observed - expected).abs() <= 1e-7 * (1. + expected),
            "window/document count mismatch"
        );
        for b in 0..BINS {
            broadcast[b * n + i] = expected / BINS as f64;
        }
    }
    for v in counts.iter_mut().chain(&mut broadcast) {
        *v = if total == 0 {
            0.
        } else {
            (*v / total as f64).ln_1p()
        };
    }
    let mask = (0..BINS)
        .flat_map(|b| {
            std::iter::repeat_n(
                if tokens[b] > 0 && available[b] {
                    1.
                } else {
                    0.
                },
                n,
            )
        })
        .collect();
    Ok((
        Raw {
            counts,
            masks: mask,
        },
        Raw {
            counts: broadcast,
            masks: vec![
                if total > 0 && available.iter().all(|v| *v) {
                    1.
                } else {
                    0.
                };
                n * BINS
            ],
        },
    ))
}
fn normalize(
    train: &[Value],
    raw: &BTreeMap<String, Raw>,
    n: usize,
) -> Result<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    let mut families = BTreeMap::new();
    for r in train {
        ensure!(
            r["purpose"] == "query" && r["frozen_split"] == "train",
            "only Train queries fit"
        );
        *families.entry(s(r, "source_group")?).or_insert(0usize) += 1;
    }
    ensure!(!families.is_empty(), "empty Train");
    let (mut means, mut scales, mut centers) = (vec![0.; n], vec![1.; n], vec![0.; n]);
    for i in 0..n {
        let (mut sum, mut mass, mut all) = (0., 0., 0.);
        for r in train {
            let x = &raw[s(r, "id")?];
            let w = 1. / families[s(r, "source_group")?] as f64;
            all += w;
            mass += w * x.masks[i];
            sum += w * x.masks[i] * x.counts[i];
        }
        means[i] = if mass > 0. { sum / mass } else { 0. };
        centers[i] = mass / all;
        let mut variance = 0.;
        for r in train {
            let x = &raw[s(r, "id")?];
            variance += x.masks[i] * (x.counts[i] - means[i]).powi(2)
                / families[s(r, "source_group")?] as f64;
        }
        scales[i] = if mass > 0. {
            (variance / mass).sqrt().max(0.01)
        } else {
            1.
        };
    }
    Ok((means, scales, centers))
}
fn run(parent: &Path, export: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let manifest = read(&parent.join("manifest.json"))?;
    let contract = read(&parent.join("contract.json"))?;
    ensure!(
        manifest["contract_file_sha256"] == file_sha(&parent.join("contract.json"))?
            && manifest["contract_sha256"] == sha(&serde_json::to_vec(&contract)?),
        "parent contract changed"
    );
    ensure!(
        manifest["final_test_opened"] == false
            && manifest["excluded"] == 0
            && contract["ablation"] == "leaf_grammar",
        "complete non-Test grammatical-leaf parent required"
    );
    let keys = contract["coordinates"]
        .as_array()
        .context("coordinates")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).context("coordinate"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !keys.is_empty() && keys.iter().all(|k| k.split('/').nth(1) == Some("leaf")),
        "grammatical leaves only"
    );
    let report = read(&export.join("report.json"))?;
    ensure!(
        report["complete"] == true
            && report["rows_sha256"] == file_sha(&export.join("rows.jsonl"))?,
        "incomplete or changed export"
    );
    let source_contract = read(&export.join("contract.json"))?;
    ensure!(
        source_contract["engine_identity"] == contract["grammar_identities"]["document_engine"]
            && source_contract["parser_identity"]
                == contract["grammar_identities"]["native_parser"]
            && source_contract["policy"] == contract["document_policy"],
        "frozen parser mismatch"
    );
    ensure!(
        contract["source_rows_sha256"] == report["rows_sha256"],
        "different source rows"
    );
    ensure!(
        manifest["population_file_sha256"] == file_sha(&parent.join("population.json"))?,
        "population changed"
    );
    let population = read(&parent.join("population.json"))?;
    let allowed = population["rows"]
        .as_array()
        .context("population rows")?
        .iter()
        .map(|r| Ok((s(r, "id")?.to_owned(), r)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut parts = BTreeMap::new();
    let mut numeric_ids = BTreeSet::new();
    for part in PARTS {
        let path = parent.join(format!("{part}.jsonl"));
        let binding = manifest["splits"]
            .as_array()
            .context("splits")?
            .iter()
            .find(|b| b["split"] == part)
            .context("split binding")?;
        ensure!(binding["sha256"] == file_sha(&path)?, "split changed");
        let r = rows(&path)?;
        for row in &r {
            let id = s(row, "id")?;
            let p = allowed.get(id).context("numeric ID absent")?;
            ensure!(
                numeric_ids.insert(id.to_owned())
                    && row["input"]["contract_sha256"] == manifest["contract_sha256"]
                    && row["source_group"] == p["group"]
                    && row["frozen_split"] == p["split"]
                    && row["purpose"] == p["purpose"],
                "numeric population mismatch"
            );
            ensure!(
                if part == "reference" {
                    row["purpose"] == "reference" && row["frozen_split"] == "train"
                } else {
                    row["purpose"] == "query" && row["frozen_split"] == part
                },
                "partition/reference mismatch"
            );
        }
        parts.insert(part, r);
    }
    ensure!(
        numeric_ids == allowed.keys().cloned().collect(),
        "numeric ID set mismatch"
    );
    let mut ordered = BTreeMap::new();
    let mut unordered = BTreeMap::new();
    let mut bindings = Vec::new();
    for (index, line) in BufReader::new(File::open(export.join("rows.jsonl"))?)
        .lines()
        .enumerate()
    {
        let row: Value = serde_json::from_str(&line?)?;
        let id = s(&row, "id")?;
        let p = allowed.get(id).context("unexpected export ID")?;
        ensure!(
            row["metadata"]["frozen_split"] != "test"
                && row["receipt"]["source_sha256"] == p["source_sha256"]
                && sha(&serde_json::to_vec(&row["metadata"])?) == p["metadata_sha256"]
                && row["windows_sha256"] == p["windows_sha256"],
            "export population binding mismatch"
        );
        let path = export.join(format!("{index:05}.windows.jsonl"));
        let h = file_sha(&path)?;
        ensure!(row["windows_sha256"] == h, "windows changed");
        let total = row["receipt"]["summary"]["tokens"]
            .as_u64()
            .context("tokens")? as usize;
        let (a, b) = project(&path, &keys, total, &row["receipt"]["features"]["counts"])?;
        ensure!(
            ordered.insert(id.into(), a).is_none(),
            "duplicate export ID"
        );
        unordered.insert(id.into(), b);
        bindings.push(json!({"id":id,"windows_sha256":h}));
    }
    ensure!(
        ordered.keys().eq(allowed.keys()),
        "incomplete export roster"
    );
    fs::create_dir(out)?;
    for (variant, raw) in [("ordered", ordered), ("global", unordered)] {
        let n = keys.len() * BINS;
        let (mean, scale, center) = normalize(&parts["train"], &raw, n)?;
        let c = json!({"schema":"slop_ninja_shared_a_input_v1","feature_policy":"slop_ninja_four_bin_cfg_leaves_v1","variant":variant,"bins":BINS,"input_dimension":n*2,"origin_order":contract["origin_order"],"human_profiles":contract["human_profiles"],"ai_profiles":contract["ai_profiles"],"coordinates":keys,"normalization":{"fit":"Train queries only, equal source family, masks explicit","mean":mean,"scale":scale,"scale_floor":0.01,"mask_center":center},"layout":"bin-major standardized log1p counts divided by document tokens, then centered masks","position":"each parser window assigned by token midpoint; no within-window token order","global_control":"one-quarter of document counts broadcast into each bin; same dimension","grammar_identities":contract["grammar_identities"],"document_policy":contract["document_policy"],"parent_contract_sha256":manifest["contract_sha256"],"parent_manifest_sha256":file_sha(&parent.join("manifest.json"))?,"windows_bindings":bindings,"labels_not_inputs":true,"metadata_not_inputs":true,"final_test_opened":false,"split_assignments":contract["split_assignments"],"source_sha256":sha(include_bytes!("prepare_ordered_cfg.rs"))});
        let hash = sha(&serde_json::to_vec(&c)?);
        let dir = out.join(variant);
        fs::create_dir(&dir)?;
        let cb = serde_json::to_vec_pretty(&c)?;
        put(&dir.join("contract.json"), &cb)?;
        let mut splits = Vec::new();
        for part in PARTS {
            let mut output = Vec::new();
            for row in &parts[part] {
                let x = &raw[s(row, "id")?];
                let mut values = (0..n)
                    .map(|i| {
                        if x.masks[i] > 0. {
                            (x.counts[i] - mean[i]) / scale[i]
                        } else {
                            0.
                        }
                    })
                    .collect::<Vec<_>>();
                values.extend((0..n).map(|i| x.masks[i] - center[i]));
                ensure!(values.iter().all(|x| x.is_finite()), "nonfinite feature");
                let mut r = row.clone();
                r["input"] = json!({"contract_sha256":hash,"x":values});
                output.push(r);
            }
            let b = jsonl(&output)?;
            put(&dir.join(format!("{part}.jsonl")), &b)?;
            splits.push(json!({"split":part,"rows":output.len(),"sha256":sha(&b)}));
        }
        let pb = fs::read(parent.join("population.json"))?;
        put(&dir.join("population.json"), &pb)?;
        let mut m = manifest.clone();
        m["contract_sha256"] = hash.into();
        m["contract_file_sha256"] = sha(&cb).into();
        m["feature_dimension"] = (n * 2).into();
        m["splits"] = splits.into();
        m["parent_manifest_sha256"] = file_sha(&parent.join("manifest.json"))?.into();
        m["feature_policy"] = c["feature_policy"].clone();
        put(&dir.join("manifest.json"), &serde_json::to_vec_pretty(&m)?)?;
    }
    println!(
        "{}",
        json!({"documents":allowed.len(),"coordinates":keys.len(),"bins":BINS,"variants":["ordered","global"]})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 3,
        "prepare_ordered_cfg CFG_LEAF_NUMERIC EXPORT NEW_OUT"
    );
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_position_is_order_sensitive() {
        assert_eq!(bin(0, 2, 16), 0);
        assert_eq!(bin(14, 2, 16), 3);
        assert_eq!(bin(0, 0, 0), 0);
    }
    #[test]
    fn normalization_is_family_weighted_and_excludes_unpassed_rows() {
        let r = |id: &str, g: &str| json!({"id":id,"source_group":g,"purpose":"query","frozen_split":"train"});
        let raw = BTreeMap::from([
            (
                "a".into(),
                Raw {
                    counts: vec![0.],
                    masks: vec![1.],
                },
            ),
            (
                "b".into(),
                Raw {
                    counts: vec![2.],
                    masks: vec![1.],
                },
            ),
            (
                "c".into(),
                Raw {
                    counts: vec![10.],
                    masks: vec![1.],
                },
            ),
            (
                "dev".into(),
                Raw {
                    counts: vec![1e9],
                    masks: vec![1.],
                },
            ),
        ]);
        let (m, s, c) = normalize(&[r("a", "one"), r("b", "one"), r("c", "two")], &raw, 1).unwrap();
        assert_eq!(m, vec![5.5]);
        assert!((s[0] - 20.75f64.sqrt()).abs() < 1e-12);
        assert_eq!(c, vec![1.]);
    }
    #[test]
    fn swapped_windows_change_ordered_but_not_global() {
        let d = tempfile::tempdir().unwrap();
        let k = "native_complete/leaf/synthetic".to_string();
        let w = |v: f64| json!({"tokens":[{},{}],"features":{"counts":{k.clone():v},"projection_available":true},"forest":[{"sensitive":"ignored"}]});
        let a = d.path().join("a");
        let b = d.path().join("b");
        put(&a, &jsonl(&[w(1.), w(3.)]).unwrap()).unwrap();
        put(&b, &jsonl(&[w(3.), w(1.)]).unwrap()).unwrap();
        let global = json!({k.clone():4.});
        let (x, g) = project(&a, &[k.clone()], 4, &global).unwrap();
        let (y, h) = project(&b, &[k], 4, &global).unwrap();
        assert_ne!(x.counts, y.counts);
        assert_eq!(g.counts, h.counts);
        assert_eq!(x.masks, y.masks);
    }
}
