//! Competitive-A evaluator CLI. Reads a training/evaluation bundle
//! (`rows.jsonl`, `weights.json`, `MANIFEST.json`) and one predictions file per
//! system and seed; writes `evaluation.json` and `evaluation.md`.
//!
//! It never reads text fields. Dev-select, CAL and Test need
//! `--consume-sealed-split <split>`, and Test needs the CAL-declared operating
//! points of every seed. See `docs/a-competitive-evaluator.md`.
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use grammar_a_baseline::comp_diagnostics::{self, DomainRule};
use grammar_a_baseline::comp_eval::{self, Missing, Options, SeedInput, SystemInput};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Evaluate competitive-A predictions against a bundle")]
struct Args {
    /// Assembler split directory with rows.jsonl (every row carries
    /// `population`), weights.json and MANIFEST.json.
    #[arg(long)]
    bundle: PathBuf,
    /// Split to evaluate (rows of other splits are ignored).
    #[arg(long)]
    split: String,
    /// NAME:SEED=PATH predictions.jsonl for one system fit (repeatable).
    #[arg(long = "pred", required = true)]
    preds: Vec<String>,
    /// NAME:SEED=PATH CAL-declared operating points {"0.01": t, "0.001": t}.
    #[arg(long = "ops")]
    ops: Vec<String>,
    /// NAME=PATH pre-registered subsample ids (one id per line or JSONL with "id");
    /// that system is scored best case where an observation is missing.
    #[arg(long = "subsample")]
    subsamples: Vec<String>,
    /// A,B paired comparison (A minus B), repeatable.
    #[arg(long = "compare")]
    compares: Vec<String>,
    #[arg(long, default_value_t = comp_eval::RELEASED_SEED)]
    released_seed: u64,
    /// Generator label forming the gated held_out_generator slice (repeatable).
    #[arg(long = "held-out-generator")]
    held_out_generators: Vec<String>,
    /// Frontier roster label pinned in the seal (repeatable); default: the
    /// labels of the split's frontier-population rows.
    #[arg(long = "frontier-label")]
    frontier_labels: Vec<String>,
    /// Consistency probe manifest (`slop_ninja_a_consistency_probe_v1`, from
    /// `slop_ninja_a_augment probes`): repeats and light perturbations of
    /// documents of the split. Ungated diagnostic.
    #[arg(long)]
    probes: Option<PathBuf>,
    /// NAME:SEED=PATH predictions on the probe rows (repeatable; needs --probes
    /// and a --pred of the same NAME:SEED).
    #[arg(long = "probe-pred")]
    probe_preds: Vec<String>,
    /// Train split MANIFEST.json of the same build: its domain counts define
    /// the out-of-domain human slice (domains without Train humans or rows).
    #[arg(long)]
    train_manifest: Option<PathBuf>,
    /// Domain declared out of domain for that slice (repeatable).
    #[arg(long = "ood-domain")]
    ood_domains: Vec<String>,
    /// Required, and must equal --split, to score dev_select, cal or test.
    #[arg(long)]
    consume_sealed_split: Option<String>,
    /// Output directory (created; evaluation.json and evaluation.md).
    #[arg(long)]
    out: PathBuf,
}

fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        fs::read(p).with_context(|| format!("read {}", p.display()))?,
    )))
}

fn split_kv(s: &str) -> Result<(&str, &str)> {
    s.split_once('=')
        .with_context(|| format!("expected KEY=VALUE, got {s}"))
}

fn name_seed(k: &str) -> Result<(String, u64)> {
    let (n, s) = k
        .split_once(':')
        .with_context(|| format!("expected NAME:SEED, got {k}"))?;
    ensure!(!n.is_empty(), "empty system name");
    Ok((n.to_string(), s.parse().context("seed")?))
}

/// Verify any `rows.jsonl` digest the manifest records (key `rows.jsonl`
/// mapping to a hex string or to an object with `sha256`).
fn check_manifest(manifest: &Value, rows_sha: &str) -> Result<usize> {
    fn walk(v: &Value, want: &str, n: &mut usize) -> Result<()> {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if k == "rows.jsonl" {
                        let got = x
                            .as_str()
                            .or_else(|| x.get("sha256").and_then(Value::as_str));
                        if let Some(g) = got {
                            ensure!(g == want, "MANIFEST rows.jsonl sha256 {g} != actual {want}");
                            *n += 1;
                        }
                    }
                    walk(x, want, n)?;
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, want, n)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut n = 0;
    walk(manifest, rows_sha, &mut n)?;
    Ok(n)
}

fn read_ids(p: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for line in fs::read_to_string(p)?.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let id = if l.starts_with('{') {
            serde_json::from_str::<Value>(l)?["id"]
                .as_str()
                .context("subsample row without id")?
                .to_string()
        } else {
            l.to_string()
        };
        ensure!(out.insert(id.clone()), "duplicate subsample id {id}");
    }
    Ok(out)
}

fn write_atomic(p: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, p)?;
    Ok(())
}

fn main() -> Result<()> {
    let a = Args::parse();
    comp_eval::check_split_permission(&a.split, a.consume_sealed_split.as_deref())?;
    let rows_p = a.bundle.join("rows.jsonl");
    let weights_p = a.bundle.join("weights.json");
    let manifest_p = a.bundle.join("MANIFEST.json");
    let rows_sha = sha(&rows_p)?;
    let manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_p)?).context("MANIFEST.json")?;
    let manifest_checks = check_manifest(&manifest, &rows_sha)?;
    let all = comp_eval::parse_rows(&fs::read_to_string(&rows_p)?)?;
    let rows: Vec<_> = all.into_iter().filter(|r| r.split == a.split).collect();
    ensure!(!rows.is_empty(), "bundle has no rows for split {}", a.split);

    let out_json = a.out.join("evaluation.json");
    if comp_eval::SEALED_SPLITS.contains(&a.split.as_str()) {
        ensure!(
            !out_json.exists(),
            "{} exists: a sealed split is evaluated once",
            out_json.display()
        );
    }

    let mut systems: BTreeMap<String, SystemInput> = BTreeMap::new();
    let mut inputs = BTreeMap::new();
    let mut ops: BTreeMap<(String, u64), PathBuf> = BTreeMap::new();
    for o in &a.ops {
        let (k, p) = split_kv(o)?;
        ensure!(
            ops.insert(name_seed(k)?, PathBuf::from(p)).is_none(),
            "duplicate --ops {k}"
        );
    }
    for p in &a.preds {
        let (k, path) = split_kv(p)?;
        let (name, seed) = name_seed(k)?;
        let path = PathBuf::from(path);
        let predictions = comp_eval::parse_predictions(&fs::read_to_string(&path)?)
            .with_context(|| format!("predictions {}", path.display()))?;
        let operating_points = match ops.remove(&(name.clone(), seed)) {
            Some(op) => {
                inputs.insert(
                    format!("ops:{name}:{seed}"),
                    json!({"path": op, "sha256": sha(&op)?}),
                );
                Some(comp_eval::parse_operating_points(&serde_json::from_slice(
                    &fs::read(&op)?,
                )?)?)
            }
            None => None,
        };
        inputs.insert(
            format!("pred:{name}:{seed}"),
            json!({"path": path, "sha256": sha(&path)?}),
        );
        systems
            .entry(name.clone())
            .or_insert_with(|| SystemInput {
                name,
                seeds: vec![],
                subsample: None,
                missing: Missing::Worst,
            })
            .seeds
            .push(SeedInput {
                seed,
                predictions,
                operating_points,
            });
    }
    if let Some(((n, s), _)) = ops.iter().next() {
        bail!("--ops {n}:{s} names no --pred");
    }
    for s in &a.subsamples {
        let (name, p) = split_kv(s)?;
        let sys = systems
            .get_mut(name)
            .with_context(|| format!("--subsample names unknown system {name}"))?;
        ensure!(sys.subsample.is_none(), "duplicate --subsample {name}");
        sys.subsample = Some(read_ids(Path::new(p))?);
        sys.missing = Missing::Best;
        inputs.insert(
            format!("subsample:{name}"),
            json!({"path": p, "sha256": sha(Path::new(p))?}),
        );
    }
    let comparisons: Vec<(String, String)> = a
        .compares
        .iter()
        .map(|c| {
            let (x, y) = c
                .split_once(',')
                .with_context(|| format!("--compare A,B, got {c}"))?;
            Ok((x.to_string(), y.to_string()))
        })
        .collect::<Result<_>>()?;
    let opts = Options {
        released_seed: a.released_seed,
        held_out_generators: a.held_out_generators.clone(),
        frontier_labels: a.frontier_labels.clone(),
        ..Options::default()
    };
    let systems: Vec<SystemInput> = systems.into_values().collect();
    comp_eval::check_operating_points(&a.split, &systems)?;
    let mut e = comp_eval::evaluate(&rows, &systems, &comparisons, &opts)?;
    // Ungated diagnostics: answer consistency and out-of-domain human FPR.
    let mut consistency = serde_json::Map::new();
    let mut ood = serde_json::Map::new();
    let probes = match &a.probes {
        Some(p) => {
            inputs.insert("probes".into(), json!({"path": p, "sha256": sha(p)?}));
            Some(comp_diagnostics::parse_probes(&fs::read_to_string(p)?, &rows).context("probes")?)
        }
        None => None,
    };
    ensure!(probes.is_some() || a.probe_preds.is_empty(), "--probe-pred needs --probes");
    let mut probe_preds: BTreeMap<(String, u64), BTreeMap<String, comp_eval::Obs>> = BTreeMap::new();
    for pp in &a.probe_preds {
        let (k, path) = split_kv(pp)?;
        let ns = name_seed(k)?;
        let path = PathBuf::from(path);
        let parsed = comp_eval::parse_predictions(&fs::read_to_string(&path)?).with_context(|| format!("probe predictions {}", path.display()))?;
        inputs.insert(format!("probe_pred:{}:{}", ns.0, ns.1), json!({"path": path, "sha256": sha(&path)?}));
        ensure!(probe_preds.insert(ns, parsed).is_none(), "duplicate --probe-pred {k}");
    }
    let rule = if a.train_manifest.is_some() || !a.ood_domains.is_empty() {
        let mut r = match &a.train_manifest {
            Some(p) => {
                inputs.insert("train_manifest".into(), json!({"path": p, "sha256": sha(p)?}));
                DomainRule::from_train_manifest(&serde_json::from_slice(&fs::read(p)?).context("train MANIFEST.json")?)?
            }
            // without a Train manifest only declared domains are out of domain
            None => DomainRule { train: rows.iter().map(|r| (r.domain.clone(), true)).collect(), ..DomainRule::default() },
        };
        r.declared = a.ood_domains.iter().cloned().collect();
        Some(r)
    } else {
        None
    };
    for sys in &systems {
        for si in &sys.seeds {
            let th = comp_diagnostics::seed_thresholds(&e, &sys.name, si.seed)?;
            let key = format!("{}:{}", sys.name, si.seed);
            if let (Some(pr), Some(pp)) = (&probes, probe_preds.remove(&(sys.name.clone(), si.seed))) {
                consistency.insert(key.clone(), comp_diagnostics::consistency(&rows, pr, &si.predictions, &pp, &th)?);
            }
            if let Some(r) = &rule {
                ood.insert(key, comp_diagnostics::ood_human_fpr(&rows, &si.predictions, r, &th));
            }
        }
    }
    if let Some(((n, s), _)) = probe_preds.iter().next() {
        bail!("--probe-pred {n}:{s} names no --pred");
    }
    e["diagnostics"] = json!({"consistency": consistency, "out_of_domain_humans": ood,
        "ood_declared_domains": a.ood_domains, "status": "ungated; outside the primary metric, every gate and the Pangram strata"});
    e["inputs"] = json!({
        "bundle": a.bundle,
        "rows_jsonl_sha256": rows_sha,
        "weights_json_sha256": sha(&weights_p)?,
        "manifest_json_sha256": sha(&manifest_p)?,
        "manifest_rows_digest_checks": manifest_checks,
        "files": inputs,
        "released_seed": a.released_seed,
        "held_out_generators": a.held_out_generators,
        "frontier_labels": a.frontier_labels,
        "consumed_sealed_split": a.consume_sealed_split,
    });
    fs::create_dir_all(&a.out)?;
    write_atomic(&out_json, &serde_json::to_vec(&e)?)?;
    let md = comp_eval::markdown(&e) + &comp_diagnostics::markdown(&e["diagnostics"]);
    write_atomic(&a.out.join("evaluation.md"), md.as_bytes())?;
    println!(
        "{}",
        json!({"split": a.split, "documents": rows.len(), "systems": systems.len(), "comparisons": comparisons.len(), "evaluation_json_sha256": sha(&out_json)?})
    );
    Ok(())
}
