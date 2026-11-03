//! Tier C / Tier I evaluation of the competitive A protocol v1.2 on the
//! sealed Test: one run, no code change after the seal.
//!
//! ```text
//! a_comp_tierc_evaluate --protocol protocol-v1.json --seal SEAL.json \
//!   --bundle TEST_BUNDLE_DIR --consume-sealed-split test \
//!   --pred Faux:17=P --pred Faux:29=P --pred Faux:41=P \
//!   --pred Stage3:17=P --pred Stage3:29=P --pred Stage3:41=P \
//!   --pred E0:17=P ... --pred V6:0=P \
//!   --ops Faux:17=OPS ... (one per --pred; a thresholds JSON or the fit's CAL evaluation.json) \
//!   --claimant Faux --frozen Stage3 \
//!   --pangram-map CAL_MAP_DIR/map.json --pangram-store STORE [--pangram-store STORE2] \
//!   [--pangram-uncertain-job N:JOB] --test-subsample TEST_SUBSAMPLE_DIR --n-test N \
//!   --held-out-generator OLMO2_LABEL [--frontier-label L ...] \
//!   [--replay Faux:17=RUST_CPU_REPLAY.jsonl] --out OUT_DIR
//! ```
//!
//! Gates (`tierc::verdicts`, on the pinned `pangram_subsample` estimator):
//! condition 1 noninferiority, 2 human FPR at the CAL 1% point per seed,
//! 3 gated slices, 4 execution, and Tier I against the frozen stage-3 A, for
//! the recipe (seeds 17, 29, 41) and the released artifact (seed 17).
//! Reported metrics: `comp_eval::evaluate` over every system (A arms,
//! frozen comparators and CAL-mapped Pangram on its subsample) with the
//! claimant against every other system and every A system against Pangram.
//!
//! Tier C is not evaluated when the balance check failed (`n_test` below
//! n_min), the CAL map was refused, or more than 2% of the selected primary
//! Test documents lack a valid pinned-version Pangram observation; Tier I is
//! evaluated regardless. Missing Pangram documents score involvement = label.
//!
//! Writes `OUT_DIR/report.json`, `OUT_DIR/summary.txt` and
//! `OUT_DIR/evaluation.md`; reads no text field and calls no provider.
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use grammar_a_baseline::comp_eval::{
    self, Missing, Obs, Options, Prediction, Row, SeedInput, SystemInput,
};
use grammar_a_baseline::pangram_map::{MAP_SCHEMA, PangramMap};
use grammar_a_baseline::pangram_store::{Outcome, missing_counts, outcomes_digest, resolve};
use grammar_a_baseline::pangram_subsample as ps;
use grammar_a_baseline::tierc::{self, Fit, NotEvaluated, PangramTest};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const PROTOCOL_SHA256: &str = "9ca96f5228c8a40671bb7c2c64d125cb587c7bfe534c3974cb0bf502966c8fab";
const PANGRAM: &str = "Pangram";
const SEEDS: [u64; 3] = [17, 29, 41];

#[derive(Parser)]
#[command(about = "Tier C / Tier I evaluation on the sealed Test (no network)")]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    /// Written seal (its existence and hash are recorded).
    #[arg(long)]
    seal: PathBuf,
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    consume_sealed_split: Option<String>,
    /// NAME:SEED=PATH calibrated Test predictions (repeatable).
    #[arg(long = "pred", required = true)]
    preds: Vec<String>,
    /// NAME:SEED=PATH CAL operating points of that fit (repeatable).
    #[arg(long = "ops")]
    ops: Vec<String>,
    #[arg(long, default_value = "Faux")]
    claimant: String,
    #[arg(long, default_value = "Stage3")]
    frozen: String,
    #[arg(long, default_value_t = 17)]
    released_seed: u64,
    #[arg(long)]
    pangram_map: PathBuf,
    #[arg(long = "pangram-store")]
    pangram_stores: Vec<PathBuf>,
    #[arg(long = "pangram-uncertain-job")]
    pangram_uncertain: Vec<String>,
    #[arg(long)]
    test_subsample: PathBuf,
    #[arg(long)]
    n_test: usize,
    #[arg(long = "held-out-generator", required = true)]
    held_out_generators: Vec<String>,
    #[arg(long = "frontier-label")]
    frontier_labels: Vec<String>,
    /// NAME:SEED=PATH Rust CPU replay predictions to compare with that fit.
    #[arg(long = "replay")]
    replays: Vec<String>,
    #[arg(long)]
    out: PathBuf,
}

fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p).with_context(|| p.display().to_string())?)))
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)
        .with_context(|| p.display().to_string())
}

fn name_seed_path(s: &str) -> Result<(String, u64, PathBuf)> {
    let (k, p) = s.split_once('=').with_context(|| format!("expected NAME:SEED=PATH, got {s}"))?;
    let (n, seed) = k.split_once(':').with_context(|| format!("expected NAME:SEED, got {k}"))?;
    ensure!(!n.is_empty() && n != PANGRAM, "bad system name {n}");
    Ok((n.to_string(), seed.parse().context("seed")?, PathBuf::from(p)))
}

#[derive(Deserialize)]
struct HashRow {
    id: String,
    split: String,
    text_sha256: String,
}

/// The `rows.jsonl` digest a manifest records, when it records one.
fn manifest_rows_digests(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "rows.jsonl"
                    && let Some(g) = x.as_str().or_else(|| x.get("sha256").and_then(Value::as_str))
                {
                    out.push(g.to_string());
                }
                manifest_rows_digests(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| manifest_rows_digests(x, out)),
        _ => {}
    }
}

struct Sub {
    /// Selected id -> (pi_h, text_sha256).
    selected: BTreeMap<String, (f64, String)>,
    design_sha256: String,
    population_sha256: String,
}

/// Check the Test subsample files against the bundle's primary Test rows:
/// identical documents, strata, balanced weights, `pi_h = n_h / N_h` and
/// selection, and `n = min(n_test, population)`.
fn check_subsample(dir: &Path, rows: &[Row], hashes: &BTreeMap<String, String>, n_test: usize) -> Result<Sub> {
    let design = read_json(&dir.join("design.json"))?;
    ensure!(design["schema"] == "slop_ninja_a_comp_pangram_test_subsample_v1", "not a Test subsample design");
    ensure!(design["protocol_sha256"] == PROTOCOL_SHA256, "Test subsample drawn under another protocol file");
    ensure!(design["n_test_sealed"].as_u64() == Some(n_test as u64), "Test subsample n_test differs from --n-test");
    let primary = tierc::primary_indices(rows);
    let docs: Vec<ps::EvalDoc> = primary
        .iter()
        .map(|&i| Ok(ps::EvalDoc { group: rows[i].group.clone(), cell: rows[i].cell, stratum: rows[i].stratum()?, value: 0.0 }))
        .collect::<Result<_>>()?;
    let w = ps::balanced_weights(&docs);
    let want: BTreeMap<&str, (usize, f64, &str)> =
        primary.iter().enumerate().map(|(k, &i)| (rows[i].id.as_str(), (k, w[k], docs[k].stratum.as_str()))).collect();
    let pop_p = dir.join("population.jsonl");
    let mut seen = BTreeSet::new();
    let mut selected = BTreeMap::new();
    let mut per: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for line in fs::read_to_string(&pop_p)?.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line)?;
        let id = v["id"].as_str().context("population id")?;
        let &(_, wd, st) = want.get(id).with_context(|| format!("subsample population names {id}, not a primary Test document"))?;
        ensure!(seen.insert(id.to_string()), "duplicate population id {id}");
        ensure!(v["stratum"] == st, "{id}: stratum differs from the bundle");
        let bw = v["balanced_weight"].as_f64().context("balanced_weight")?;
        ensure!((bw - wd).abs() <= 1e-12 * wd.max(1e-300) + 1e-18, "{id}: balanced weight differs");
        let sha = v["text_sha256"].as_str().context("text_sha256")?;
        ensure!(hashes.get(id).map(String::as_str) == Some(sha), "{id}: text hash differs from the bundle");
        let e = per.entry(st.to_string()).or_default();
        e.0 += 1;
        if v["selected"] == true {
            e.1 += 1;
            let pi = v["pi_h"].as_f64().context("pi_h")?;
            selected.insert(id.to_string(), (pi, sha.to_string()));
        }
    }
    ensure!(seen.len() == want.len(), "subsample population has {} of {} primary Test documents", seen.len(), want.len());
    for (id, (pi, _)) in &selected {
        let (n, s) = per[want[id.as_str()].2];
        ensure!(*pi == s as f64 / n as f64, "{id}: pi_h != n_h / N_h");
    }
    ensure!(selected.len() == n_test.min(want.len()), "selected {} != min(n_test, population)", selected.len());
    Ok(Sub { selected, design_sha256: sha(&dir.join("design.json"))?, population_sha256: sha(&pop_p)? })
}

fn main() -> Result<()> {
    let a = Args::parse();
    comp_eval::check_split_permission("test", a.consume_sealed_split.as_deref())?;
    ensure!(a.seal.is_file(), "the seal {} does not exist: Test is sealed", a.seal.display());
    ensure!(!a.out.join("report.json").exists(), "{} exists: Test is evaluated once", a.out.join("report.json").display());
    let proto_sha = sha(&a.protocol)?;
    ensure!(proto_sha == PROTOCOL_SHA256, "protocol sha256 {proto_sha} is not the pinned v1.2 file");
    let mut inputs = json!({"protocol": a.protocol, "protocol_sha256": proto_sha,
        "seal": a.seal, "seal_sha256": sha(&a.seal)?, "files": {}});

    // Bundle (metadata only).
    let rows_p = a.bundle.join("rows.jsonl");
    let rows_sha = sha(&rows_p)?;
    let manifest = read_json(&a.bundle.join("MANIFEST.json"))?;
    let mut digests = vec![];
    manifest_rows_digests(&manifest, &mut digests);
    for d in &digests {
        ensure!(*d == rows_sha, "MANIFEST rows.jsonl sha256 {d} != actual {rows_sha}");
    }
    let raw = fs::read_to_string(&rows_p)?;
    let rows: Vec<Row> = comp_eval::parse_rows(&raw)?.into_iter().filter(|r| r.split == "test").collect();
    ensure!(!rows.is_empty(), "bundle has no Test rows");
    let mut hashes = BTreeMap::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let h: HashRow = serde_json::from_str(line)?;
        if h.split == "test" {
            hashes.insert(h.id, h.text_sha256);
        }
    }
    inputs["bundle"] = json!({"dir": a.bundle, "rows_jsonl_sha256": rows_sha,
        "manifest_json_sha256": sha(&a.bundle.join("MANIFEST.json"))?, "manifest_rows_digest_checks": digests.len()});
    let held: BTreeSet<String> = a.held_out_generators.iter().cloned().collect();
    ensure!(
        rows.iter().any(|r| r.primary && r.label.as_ref().is_some_and(|l| held.contains(l))),
        "no primary Test positive carries a --held-out-generator label: the gated held_out_generator slice would be empty"
    );

    // A predictions and CAL operating points.
    let mut ops: BTreeMap<(String, u64), PathBuf> = BTreeMap::new();
    for o in &a.ops {
        let (n, s, p) = name_seed_path(o)?;
        ensure!(ops.insert((n.clone(), s), p).is_none(), "duplicate --ops {n}:{s}");
    }
    let mut systems: BTreeMap<String, Vec<(u64, BTreeMap<String, Obs>, f64, f64)>> = BTreeMap::new();
    for p in &a.preds {
        let (n, s, path) = name_seed_path(p)?;
        let preds = comp_eval::parse_predictions(&fs::read_to_string(&path)?)
            .with_context(|| format!("predictions {}", path.display()))?;
        let op = ops.remove(&(n.clone(), s)).with_context(|| format!("--pred {n}:{s} needs --ops {n}:{s}"))?;
        let (t1, t01) = tierc::load_operating_points(&read_json(&op)?, &n, s)
            .with_context(|| format!("operating points {}", op.display()))?;
        inputs["files"][format!("pred:{n}:{s}")] = json!({"path": path, "sha256": sha(&path)?});
        inputs["files"][format!("ops:{n}:{s}")] = json!({"path": op, "sha256": sha(&op)?,
            "0.01_bits": format!("0x{:016x}", t1.to_bits()), "0.001_bits": format!("0x{:016x}", t01.to_bits())});
        let v = systems.entry(n.clone()).or_default();
        ensure!(v.iter().all(|x| x.0 != s), "duplicate --pred {n}:{s}");
        v.push((s, preds, t1, t01));
    }
    if let Some(((n, s), _)) = ops.iter().next() {
        bail!("--ops {n}:{s} names no --pred");
    }
    let seeds_of = |n: &str| -> Result<BTreeSet<u64>> {
        Ok(systems.get(n).with_context(|| format!("no predictions for {n}"))?.iter().map(|x| x.0).collect())
    };
    ensure!(seeds_of(&a.claimant)? == SEEDS.into(), "claimant {} needs exactly seeds 17, 29, 41", a.claimant);
    ensure!(seeds_of(&a.frozen)? == SEEDS.into(), "frozen stage-3 A {} needs exactly seeds 17, 29, 41", a.frozen);
    ensure!(a.released_seed == 17, "the released artifact is seed 17 (protocol released_artifact_seed)");
    let fits_of = |n: &str| -> Result<Vec<Fit>> {
        let mut v: Vec<Fit> = systems[n].iter().map(|(s, p, t1, t01)| Fit::new(&rows, *s, p, *t1, *t01)).collect::<Result<_>>()?;
        v.sort_by_key(|f| f.seed);
        Ok(v)
    };
    let claimant = fits_of(&a.claimant)?;
    let frozen = fits_of(&a.frozen)?;

    // Pangram: CAL map, balance check, Test subsample and observations.
    let mut not_eval = NotEvaluated::default();
    if a.n_test < tierc::N_MIN {
        not_eval.0.push(format!("balance check: n_test {} below n_min {}", a.n_test, tierc::N_MIN));
    }
    let map_rec = read_json(&a.pangram_map)?;
    inputs["files"]["pangram_map"] = json!({"path": a.pangram_map, "sha256": sha(&a.pangram_map)?});
    ensure!(map_rec["schema"] == MAP_SCHEMA, "not a Pangram CAL map record");
    let map = if map_rec["status"] == "fitted" {
        let m = PangramMap::from_description(&map_rec["map"])?;
        ensure!(map_rec["pinned_version"]["version"] == m.version.as_str(), "map version binding");
        let t1 = comp_eval::parse_threshold(&map_rec["operating_points"]["0.01"])?;
        let t01 = comp_eval::parse_threshold(&map_rec["operating_points"]["0.001"])?;
        Some((m, t1, t01))
    } else {
        not_eval.0.push(format!("Pangram CAL map refused: {}", map_rec["reason"].as_str().unwrap_or("unknown")));
        None
    };
    let mut pangram_json = json!({"map_status": map_rec["status"]});
    let mut pangram_test: Option<PangramTest> = None;
    let mut pangram_preds: BTreeMap<String, Obs> = BTreeMap::new();
    let mut subsample_ids: Option<BTreeSet<String>> = None;
    if let (Some((m, t1, t01)), true) = (&map, a.n_test >= tierc::N_MIN) {
        ensure!(!a.pangram_stores.is_empty(), "--pangram-store required");
        let sub = check_subsample(&a.test_subsample, &rows, &hashes, a.n_test)?;
        let sel: BTreeMap<String, String> = sub.selected.iter().map(|(k, v)| (k.clone(), v.1.clone())).collect();
        let res = resolve(&sel, &a.pangram_stores, "test", &a.pangram_uncertain)?;
        let pinned = res.apply_pin(&m.version);
        let by_reason = missing_counts(&pinned);
        let pos: BTreeMap<&str, usize> = rows.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
        let mut pi = vec![None; rows.len()];
        let mut inv = vec![None; rows.len()];
        for (id, (p, _)) in &sub.selected {
            let i = pos[id.as_str()];
            pi[i] = Some(*p);
            if let Outcome::Valid(s) = &pinned[id] {
                let x = s.clamped_fraction();
                let v = m.apply(x)?;
                inv[i] = Some(v);
                // comp_eval form: involvement carried exactly by p_model_only,
                // share = Pangram's raw clamped fraction (no three-way output).
                pangram_preds.insert(id.clone(), Obs::Ok(Prediction {
                    p_human: 1.0 - v, p_model_only: v, p_mixed: 0.0, share: x,
                    attribution: BTreeMap::new(), roles: BTreeMap::new(),
                }));
            }
        }
        let p = PangramTest { pi, inv, t_01: *t1, t_001: *t01 };
        pangram_json = json!({"map_status": "fitted", "pinned_version": m.version,
            "selected": p.selected(), "valid": p.selected() - p.missing(), "missing": p.missing(),
            "missing_by_reason": by_reason, "missing_rule": "a missing selected document scores Pangram involvement = label; more than 2% missing: Tier C not evaluated",
            "exceeds_2pct": grammar_a_baseline::pangram_map::exceeds_missing_limit(p.missing(), p.selected()),
            "outcomes_digest": outcomes_digest(&pinned), "stores": res.stores,
            "uncertain_jobs": a.pangram_uncertain, "duplicate_text_resolved_to_twin": res.duplicate_text_resolved,
            "operating_points_bits": {"0.01": format!("0x{:016x}", t1.to_bits()), "0.001": format!("0x{:016x}", t01.to_bits())}});
        inputs["files"]["test_subsample"] = json!({"dir": a.test_subsample, "design_sha256": sub.design_sha256, "population_sha256": sub.population_sha256});
        subsample_ids = Some(sub.selected.keys().cloned().collect());
        pangram_test = Some(p);
    }

    // Gates.
    let verdicts = tierc::verdicts(&rows, &claimant, a.released_seed, &frozen, pangram_test.as_ref(), &not_eval, &held)?;

    // Reported metrics: comp_eval over every system.
    let mut sys_in: Vec<SystemInput> = systems
        .iter()
        .map(|(n, v)| SystemInput {
            name: n.clone(),
            seeds: v.iter().map(|(s, p, t1, t01)| SeedInput { seed: *s, predictions: p.clone(),
                operating_points: Some([("0.01", *t1), ("0.001", *t01)].into_iter().collect()) }).collect(),
            subsample: None,
            missing: Missing::Worst,
        })
        .collect();
    let mut comparisons: Vec<(String, String)> =
        systems.keys().filter(|n| **n != a.claimant).map(|n| (a.claimant.clone(), n.clone())).collect();
    if let (Some(ids), Some((_, t1, t01))) = (&subsample_ids, &map) {
        sys_in.push(SystemInput {
            name: PANGRAM.into(),
            seeds: vec![SeedInput { seed: 0, predictions: pangram_preds,
                operating_points: Some([("0.01", *t1), ("0.001", *t01)].into_iter().collect()) }],
            subsample: Some(ids.clone()),
            missing: Missing::Best,
        });
        comparisons.extend(systems.keys().map(|n| (n.clone(), PANGRAM.to_string())));
    }
    let opts = Options {
        released_seed: a.released_seed,
        held_out_generators: a.held_out_generators.clone(),
        frontier_labels: a.frontier_labels.clone(),
        ..Options::default()
    };
    let metrics = comp_eval::evaluate(&rows, &sys_in, &comparisons, &opts)?;

    // Cross-checks between the gate path and comp_eval.
    let mut cross = vec![];
    if let Some(c) = metrics["comparisons"].as_array().and_then(|v| {
        v.iter().find(|x| x["a"] == a.claimant.as_str() && x["b"] == PANGRAM)
    }) {
        for (vn, key) in [("recipe", "recipe"), ("released", "released_artifact")] {
            let g = &verdicts["tier_c"]["variants"][key]["condition_1_noninferiority"];
            if g.is_null() {
                continue;
            }
            let e = &c["variants"][vn];
            let d1 = (e["balanced_brier_difference"].as_f64().unwrap_or(f64::NAN) - g["estimate"].as_f64().unwrap()).abs();
            let d2 = (e["balanced_brier_difference_95ci"]["upper"].as_f64().unwrap_or(f64::NAN) - g["upper_draws_974"].as_f64().unwrap_or(f64::NAN)).abs();
            cross.push(json!({"check": format!("{vn}: condition 1 estimate and draws[974] vs comp_eval"),
                "abs_difference_estimate": d1, "abs_difference_upper": d2, "agree": d1 <= 1e-9 && d2 <= 1e-9}));
            let gs = e["gated_slices"].as_array().cloned().unwrap_or_default();
            let mine = verdicts["tier_c"]["variants"][key]["condition_3_slices"]["slices"].as_array().cloned().unwrap_or_default();
            let mut disagree = 0;
            for s in &mine {
                let other = gs.iter().find(|x| x["slice"] == s["slice"]);
                let st_ok = other.is_some_and(|o| o["status"] == s["realised"]["status"]);
                let est = other.and_then(|o| o["balanced_brier_difference"].as_f64());
                let mine_est = s["gap_point_estimate"].as_f64();
                let est_ok = match (est, mine_est) {
                    (Some(x), Some(y)) => (x - y).abs() <= 1e-9,
                    (None, None) => true,
                    _ => false,
                };
                if !(st_ok && est_ok) {
                    disagree += 1;
                    cross.push(json!({"check": format!("{vn}: slice {}", s["slice"]), "agree": false,
                        "gate_status": s["realised"]["status"], "comp_eval": other}));
                }
            }
            cross.push(json!({"check": format!("{vn}: gated slice status and gap estimates vs comp_eval"),
                "slices_compared": mine.len(), "disagreements": disagree, "agree": disagree == 0}));
        }
    }
    let cross_ok = cross.iter().all(|x| x["agree"] == true);

    // Rust CPU replay against its fit.
    let mut replay = vec![];
    for r in &a.replays {
        let (n, s, path) = name_seed_path(r)?;
        let rep = comp_eval::parse_predictions(&fs::read_to_string(&path)?)?;
        let base = &systems.get(&n).and_then(|v| v.iter().find(|x| x.0 == s)).with_context(|| format!("--replay {n}:{s} names no fit"))?.1;
        let (mut maxd, mut compared, mut absent) = (0.0f64, 0usize, 0usize);
        for row in &rows {
            match (base.get(&row.id), rep.get(&row.id)) {
                (Some(Obs::Ok(x)), Some(Obs::Ok(y))) => {
                    compared += 1;
                    for (p, q) in [(x.p_human, y.p_human), (x.p_model_only, y.p_model_only), (x.p_mixed, y.p_mixed), (x.share, y.share)] {
                        maxd = maxd.max((p - q).abs());
                    }
                }
                _ => absent += 1,
            }
        }
        inputs["files"][format!("replay:{n}:{s}")] = json!({"path": path, "sha256": sha(&path)?});
        replay.push(json!({"fit": format!("{n}:{s}"), "documents_compared": compared, "documents_not_compared": absent,
            "max_abs_probability_difference": maxd}));
    }

    let report = json!({
        "schema": tierc::REPORT_SCHEMA,
        "protocol": "competitive A protocol v1.2 (docs/a-competitive-protocol.md; protocol-v1.json)",
        "split": "test",
        "claimant": a.claimant, "frozen_stage3": a.frozen, "released_seed": a.released_seed,
        "verdicts": verdicts,
        "pangram": pangram_json,
        "cross_checks": {"all_agree": cross_ok, "checks": cross},
        "rust_cpu_replay": replay,
        "decisions": [
            "Pangram version pin: pangram_store::PIN_RULE (first store, first job in PLAN order with a stored success, lowest bulk_index).",
            "Pangram missing on Test: involvement = label in Delta_d; the 2% limit counts every missing selected primary Test document.",
            "A operating-point flags read 1 - p_human; a failed human is flagged and a failed positive missed whatever the threshold.",
            "Recipe: per-document A losses averaged over seeds 17, 29, 41 before group values; the FPR gate holds for each seed.",
            "Tier I recipe: frozen stage-3 A losses averaged over its three seeds; FPR at 0.5 as the mean over fits; released artifact: seed 17 against frozen seed 17.",
            "Condition 4 counts execution failures of the claimant's gated fits (recipe: three seeds; released: seed 17); other systems' failures are reported.",
            "Gated slices: realised minimum from pipeline counts (15 groups, 150 humans, frontier 150 frontier positives); a gated slice with no selected document fails condition 3.",
            "Pangram in comp_eval: p_model_only = calibrated involvement, p_mixed = 0, share = raw clamped fraction; its three-way and attribution figures are not meaningful.",
        ],
        "metrics": metrics,
        "inputs": inputs,
        "implementation": {
            "example_sha256": hex::encode(Sha256::digest(include_bytes!("a_comp_tierc_evaluate.rs"))),
            "tierc_sha256": hex::encode(Sha256::digest(include_bytes!("../src/tierc.rs"))),
            "comp_eval_sha256": hex::encode(Sha256::digest(include_bytes!("../src/comp_eval.rs"))),
            "pangram_subsample_sha256": hex::encode(Sha256::digest(include_bytes!("../src/pangram_subsample.rs"))),
            "pangram_map_sha256": hex::encode(Sha256::digest(include_bytes!("../src/pangram_map.rs"))),
            "pangram_store_sha256": hex::encode(Sha256::digest(include_bytes!("../src/pangram_store.rs"))),
        },
        "network_calls": 0,
    });
    ensure!(
        report["implementation"]["pangram_subsample_sha256"] == "837ad5bf31009cfd0a4e04d5d6d0540bc18564ccaf375ab34d8f078ce0287374",
        "pangram_subsample.rs differs from the protocol-pinned module"
    );
    fs::create_dir_all(&a.out)?;
    let summary = summary_text(&report);
    for (name, bytes) in [
        ("report.json", serde_json::to_vec(&report)?),
        ("evaluation.md", comp_eval::markdown(&report["metrics"]).into_bytes()),
        ("summary.txt", summary.clone().into_bytes()),
    ] {
        let mut f = fs::OpenOptions::new().create_new(true).write(true).open(a.out.join(name))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    print!("{summary}");
    Ok(())
}

fn num(v: &Value) -> String {
    v.as_f64().map_or("n/a".into(), |x| format!("{x:.5}"))
}

fn summary_text(r: &Value) -> String {
    let v = &r["verdicts"];
    let mut s = format!(
        "Tier C: {}\nTier I: {}\n",
        v["tier_c"]["status"].as_str().unwrap_or("?"),
        v["tier_i"]["status"].as_str().unwrap_or("?")
    );
    if let Some(reasons) = v["tier_c"]["reasons"].as_array() {
        for x in reasons {
            s += &format!("  Tier C not evaluated: {}\n", x.as_str().unwrap_or("?"));
        }
    }
    for k in ["recipe", "released_artifact"] {
        let c = &v["tier_c"]["variants"][k];
        let c1 = &c["condition_1_noninferiority"];
        s += &format!(
            "{k}: C1 {} (estimate {}, draws[974] {}); C2 {} (largest flags {}); C3 {}; C4 {}\n",
            c1["pass"], num(&c1["estimate"]), num(&c1["upper_draws_974"]),
            c["condition_2_human_fpr"]["pass"], c["condition_2_human_fpr"]["headline_largest_flag_count"],
            c["condition_3_slices"]["pass"], c["condition_4_execution"]["pass"],
        );
        let t = &v["tier_i"]["variants"][k];
        s += &format!(
            "{k}: Tier I Brier diff {} [{}, {}] pass {}; FPR@0.5 {} vs {} pass {}\n",
            num(&t["brier_difference_a_minus_frozen"]["estimate"]),
            num(&t["brier_difference_a_minus_frozen"]["lower_draws_24"]),
            num(&t["brier_difference_a_minus_frozen"]["upper_draws_974"]),
            t["brier_difference_a_minus_frozen"]["pass"],
            num(&t["human_fpr_at_0_5"]["a_fpr_mean"]), num(&t["human_fpr_at_0_5"]["frozen_fpr_mean"]),
            t["human_fpr_at_0_5"]["pass"],
        );
    }
    s += &format!(
        "Pangram: version {}, selected {}, missing {}\nCross-checks agree: {}\n",
        r["pangram"]["pinned_version"], r["pangram"]["selected"], r["pangram"]["missing"], r["cross_checks"]["all_agree"]
    );
    s
}
