//! Apply the frozen prepare_ordered_cfg four-bin projection, never normalize-fit.
use super::*;
use serde::Deserialize;
use std::io::{BufRead, BufReader};

const BINS: usize = 4;
fn vector(value: &Value, n: usize) -> Result<Vec<f64>> {
    let values = value.as_array().context("frozen ordered vector")?;
    ensure!(values.len() == n, "ordered vector dimension");
    values
        .iter()
        .map(|v| {
            let x = v.as_f64().context("ordered number")?;
            ensure!(x.is_finite(), "nonfinite ordered value");
            Ok(x)
        })
        .collect()
}
pub(crate) fn validate(parent: &Parent, leaf: &Parent, name: &str) -> Result<()> {
    let c = &parent.contract;
    let lc = &leaf.contract;
    let keys = lc["coordinates"].as_array().context("leaf coordinates")?;
    let n = keys
        .len()
        .checked_mul(BINS)
        .context("ordered dimension overflow")?;
    ensure!(
        lc["ablation"] == "leaf_grammar"
            && !keys.is_empty()
            && keys.iter().all(|k| k
                .as_str()
                .is_some_and(|k| k.split('/').nth(1) == Some("leaf"))),
        "grammatical-leaf parent required"
    );
    ensure!(
        c["schema"] == "slop_ninja_shared_a_input_v1"
            && c["feature_policy"] == "slop_ninja_four_bin_cfg_leaves_v1"
            && c["variant"]
                == if name == "ordered_cfg" {
                    "ordered"
                } else {
                    "global"
                }
            && c["bins"] == BINS
            && c["input_dimension"] == n * 2
            && parent.manifest["feature_dimension"] == n * 2
            && c["coordinates"] == lc["coordinates"]
            && c["parent_contract_sha256"] == leaf.manifest["contract_sha256"]
            && c["parent_manifest_sha256"] == sha(&fs::read(leaf.path.join("manifest.json"))?)
            && c["normalization"]["fit"]
                == "Train queries only, equal source family, masks explicit"
            && c["normalization"]["scale_floor"] == 0.01
            && c["layout"]
                == "bin-major standardized log1p counts divided by document tokens, then centered masks"
            && c["position"]
                == "each parser window assigned by token midpoint; no within-window token order"
            && c["global_control"]
                == "one-quarter of document counts broadcast into each bin; same dimension"
            && c["source_sha256"] == sha(include_bytes!("../prepare_ordered_cfg.rs")),
        "unsupported or substituted frozen ordered contract"
    );
    for field in [
        "human_profiles",
        "ai_profiles",
        "origin_order",
        "grammar_identities",
        "document_policy",
        "split_assignments",
    ] {
        ensure!(c[field] == lc[field], "ordered parent {field} differs");
    }
    vector(&c["normalization"]["mean"], n)?;
    ensure!(
        vector(&c["normalization"]["scale"], n)?
            .iter()
            .all(|x| *x > 0.)
            && vector(&c["normalization"]["mask_center"], n)?
                .iter()
                .all(|x| (0.0..=1.).contains(x)),
        "invalid frozen ordered scales/masks"
    );
    Ok(())
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

fn project(
    path: &Path,
    keys: &[String],
    total: usize,
    global: &Value,
    ordered: bool,
) -> Result<(Vec<f64>, Vec<f64>)> {
    let n = keys.len();
    let mut counts = vec![0.; n * BINS];
    let mut tokens = [0usize; BINS];
    let mut available = [true; BINS];
    let mut start = 0usize;
    for line in BufReader::new(fs::File::open(path)?).lines() {
        // Skip prose, forests and token values; retain only lengths and CFG counts.
        let w: Window = serde_json::from_str(&line?)?;
        let length = w.tokens.len();
        let bin = if total == 0 {
            0
        } else {
            (((2 * start as u128 + length as u128) * BINS as u128 / (2 * total as u128)) as usize)
                .min(BINS - 1)
        };
        start = start.checked_add(length).context("token count overflow")?;
        tokens[bin] = tokens[bin]
            .checked_add(length)
            .context("bin count overflow")?;
        available[bin] &= w.features.projection_available;
        for (i, key) in keys.iter().enumerate() {
            let x = w.features.counts.get(key).copied().unwrap_or(0.);
            ensure!(x.is_finite() && x >= 0., "invalid window count");
            counts[bin * n + i] += x;
        }
    }
    ensure!(start == total, "window/document token denominator mismatch");
    for (i, key) in keys.iter().enumerate() {
        let expected = match global.get(key) {
            Some(v) => number(v)?,
            None => 0.,
        };
        let observed = (0..BINS).map(|b| counts[b * n + i]).sum::<f64>();
        ensure!(
            (observed - expected).abs() <= 1e-7 * (1. + expected),
            "window/document count mismatch"
        );
        if !ordered {
            for bin in 0..BINS {
                counts[bin * n + i] = expected / BINS as f64;
            }
        }
    }
    for x in &mut counts {
        *x = if total == 0 {
            0.
        } else {
            (*x / total as f64).ln_1p()
        };
    }
    let masks = if ordered {
        (0..BINS)
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
            .collect()
    } else {
        vec![
            if total > 0 && available.iter().all(|v| *v) {
                1.
            } else {
                0.
            };
            n * BINS
        ]
    };
    Ok((counts, masks))
}

pub(crate) fn apply(
    contract: &Value,
    export: &Path,
    audit: &Value,
    rows: &mut Rows,
) -> Result<Vec<Value>> {
    let keys = contract["coordinates"]
        .as_array()
        .context("ordered coordinates")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).context("coordinate string"))
        .collect::<Result<Vec<_>>>()?;
    let n = keys.len() * BINS;
    let mean = vector(&contract["normalization"]["mean"], n)?;
    let scale = vector(&contract["normalization"]["scale"], n)?;
    let center = vector(&contract["normalization"]["mask_center"], n)?;
    let report_bytes = fs::read(export.join("report.json"))?;
    let raw_bytes = fs::read(export.join("rows.jsonl"))?;
    ensure!(
        sha(&report_bytes) == audit["source_report_sha256"]
            && sha(&raw_bytes) == audit["source_rows_sha256"],
        "export changed after frozen transformation"
    );
    let report: Value = serde_json::from_slice(&report_bytes)?;
    let raw = indexed(&raw_bytes)?;
    let mut prefixes = BTreeMap::new();
    for doc in report["documents"].as_array().context("documents")? {
        let prefix = str_at(doc, "prefix")?;
        ensure!(
            prefix.bytes().all(|b| b.is_ascii_digit())
                && prefixes.insert(str_at(doc, "id")?, prefix).is_none(),
            "invalid window binding"
        );
    }
    let ch = sha(&serde_json::to_vec(contract)?);
    let mut bindings = Vec::new();
    for (id, row) in rows {
        let source = raw.get(id).context("missing ordered export row")?;
        let prefix = prefixes.get(id.as_str()).context("missing window prefix")?;
        let path = export.join(format!("{prefix}.windows.jsonl"));
        let h = final_test_binding::sha(&path)?;
        ensure!(source["windows_sha256"] == h, "windows changed");
        let total = usize::try_from(counter(&source["receipt"]["summary"], "tokens")?)?;
        let (counts, masks) = project(
            &path,
            &keys,
            total,
            &source["receipt"]["features"]["counts"],
            contract["variant"] == "ordered",
        )?;
        ensure!(
            final_test_binding::sha(&path)? == h,
            "windows changed during projection"
        );
        let mut x = (0..n)
            .map(|i| {
                if masks[i] > 0. {
                    (counts[i] - mean[i]) / scale[i]
                } else {
                    0.
                }
            })
            .collect::<Vec<_>>();
        x.extend((0..n).map(|i| masks[i] - center[i]));
        ensure!(
            x.iter().all(|x| x.is_finite() && (*x as f32).is_finite()),
            "nonfinite ordered feature"
        );
        row["input"] = json!({"contract_sha256":ch,"x":x});
        bindings.push(json!({"id":id,"windows_sha256":h,"tokens":total}));
    }
    Ok(bindings)
}
