//! Pangram CAL map (competitive A protocol v1.2, `calibration.pangram_map`,
//! `comparator`, `operating_point.pangram`, `failures_and_missing.pangram`).
//!
//! ```text
//! a_comp_pangram_cal_map --population CAL_SUBSAMPLE/population.jsonl \
//!   --design CAL_SUBSAMPLE/design.json --store STORE [--store STORE2 ...] \
//!   [--uncertain-job STORE_INDEX:JOB ...] --out OUT_DIR
//! ```
//!
//! - Reads the CAL subsample population written by `a_comp_pangram_cal_subsample`
//!   (`id`, `stratum`, `cell`, `balanced_weight`, `n_h`, `N_h`, `pi_h`,
//!   `selected`, `text_sha256`) and the `a_comp_pangram_bulk` stores of split
//!   `cal`, in dispatch order. No request is sent and no prose is read.
//! - Pins the version of the first successful CAL response
//!   (`pangram_store::PIN_RULE`). Other versions, failures (including echo
//!   mismatches beyond whitespace), not-sent, never-sent and uncertain items
//!   are missing.
//! - Refuses (writes a `refused_tier_c_not_evaluated` record and exits 3) if
//!   more than 2% of the selected CAL documents are missing or no success exists.
//! - Otherwise fits `isotonic_v1_clipped` on `min(1, fraction_ai +
//!   fraction_ai_assisted)` with fit weight `balanced_weight / pi_h` over the
//!   valid documents, clip bounds from their human and positive counts, and
//!   writes the map with Pangram's CAL 1% and 0.1% operating points (f64 bits).
//!
//! Outputs: `OUT_DIR/map.json` and `OUT_DIR/cal-observations.jsonl` (per
//! selected document: status, reason, pinned fraction and calibrated value).
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_a_baseline::pangram_map::{
    FitDoc, MAP_SCHEMA, bits, exceeds_missing_limit, fit, in_sample_brier, operating_points,
};
use grammar_a_baseline::pangram_store::{Outcome, PIN_RULE, missing_counts, outcomes_digest, resolve};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Fit the pre-registered Pangram CAL map (no network)")]
struct Args {
    #[arg(long)]
    population: PathBuf,
    #[arg(long)]
    design: PathBuf,
    /// Collector STORE of split cal, in dispatch order (repeatable).
    #[arg(long = "store", required = true)]
    stores: Vec<PathBuf>,
    /// STORE_INDEX:JOB declared uncertain after reconciliation (repeatable).
    #[arg(long = "uncertain-job")]
    uncertain: Vec<String>,
    #[arg(long)]
    out: PathBuf,
}

fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p).with_context(|| p.display().to_string())?)))
}

struct PopDoc {
    id: String,
    stratum: String,
    cell: String,
    balanced_weight: f64,
    pi_h: f64,
    selected: bool,
    text_sha256: String,
}

fn read_population(p: &Path) -> Result<Vec<PopDoc>> {
    let mut out = vec![];
    let mut per: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
    let mut wsum = 0.0;
    for (i, line) in fs::read_to_string(p)?.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).with_context(|| format!("population line {}", i + 1))?;
        let s = |k: &str| -> Result<String> { Ok(v[k].as_str().with_context(|| format!("line {}: {k}", i + 1))?.to_string()) };
        let d = PopDoc {
            id: s("id")?,
            stratum: s("stratum")?,
            cell: s("cell")?,
            balanced_weight: v["balanced_weight"].as_f64().context("balanced_weight")?,
            pi_h: v["pi_h"].as_f64().context("pi_h")?,
            selected: v["selected"].as_bool().context("selected")?,
            text_sha256: s("text_sha256")?,
        };
        ensure!(matches!(d.cell.as_str(), "human_only" | "model_only" | "mixed"), "{}: cell", d.id);
        ensure!(d.stratum.ends_with(&format!("|{}", d.cell)), "{}: stratum and cell differ", d.id);
        let (n_h, big) = (v["n_h"].as_u64().context("n_h")?, v["N_h"].as_u64().context("N_h")?);
        ensure!(d.pi_h == n_h as f64 / big as f64, "{}: pi_h != n_h / N_h", d.id);
        let e = per.entry(d.stratum.clone()).or_insert((n_h, big, 0, 0));
        ensure!(e.0 == n_h && e.1 == big, "{}: stratum sizes differ within {}", d.id, d.stratum);
        e.2 += 1;
        e.3 += d.selected as u64;
        wsum += d.balanced_weight;
        out.push(d);
    }
    for (k, (n_h, big, n, sel)) in &per {
        ensure!(n == big && sel == n_h, "stratum {k}: population file disagrees with N_h / n_h");
    }
    ensure!((wsum - 1.0).abs() < 1e-9, "balanced weights sum to {wsum}, not 1");
    let mut ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
    ids.sort();
    ensure!(ids.windows(2).all(|w| w[0] != w[1]), "duplicate population id");
    Ok(out)
}

fn write_new(p: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "output {} exists", a.out.display());
    let design: Value = serde_json::from_slice(&fs::read(&a.design)?)?;
    ensure!(design["schema"] == "slop_ninja_a_comp_pangram_cal_subsample_v1", "not a CAL subsample design");
    let pop = read_population(&a.population)?;
    let sel: Vec<&PopDoc> = pop.iter().filter(|d| d.selected).collect();
    ensure!(
        design["n_cal"].as_u64() == Some(sel.len() as u64) && design["population"].as_u64() == Some(pop.len() as u64),
        "design and population file disagree"
    );
    let selected: BTreeMap<String, String> = sel.iter().map(|d| (d.id.clone(), d.text_sha256.clone())).collect();
    let resolved = resolve(&selected, &a.stores, "cal", &a.uncertain)?;
    let mut versions: BTreeMap<String, usize> = BTreeMap::new();
    for o in resolved.outcomes.values() {
        if let Outcome::Valid(s) = o {
            *versions.entry(s.version.clone()).or_default() += 1;
        }
    }
    let mut inputs = json!({
        "population": a.population, "population_sha256": sha(&a.population)?,
        "design": a.design, "design_sha256": sha(&a.design)?,
        "stores": resolved.stores, "uncertain_jobs": a.uncertain,
    });
    let implementation = json!({
        "example_sha256": hex::encode(Sha256::digest(include_bytes!("a_comp_pangram_cal_map.rs"))),
        "pangram_map_sha256": hex::encode(Sha256::digest(include_bytes!("../src/pangram_map.rs"))),
        "pangram_store_sha256": hex::encode(Sha256::digest(include_bytes!("../src/pangram_store.rs"))),
        "involvement_calibration_sha256": hex::encode(Sha256::digest(include_bytes!("../src/involvement_calibration.rs"))),
    });
    fs::create_dir_all(&a.out)?;
    let refuse = |reason: String, extra: Value, inputs: &Value| -> Result<()> {
        let rec = json!({"schema": MAP_SCHEMA, "status": "refused_tier_c_not_evaluated", "reason": reason,
                         "detail": extra, "inputs": inputs, "implementation": implementation,
                         "network_calls": 0});
        write_new(&a.out.join("map.json"), &serde_json::to_vec_pretty(&rec)?)?;
        eprintln!("refused: {reason}");
        std::process::exit(3);
    };
    let Some((version, store, job, bulk_index, first_id)) = resolved.first_success.clone() else {
        return refuse("no successful CAL response".into(), json!({"missing_by_reason": missing_counts(&resolved.outcomes)}), &inputs);
    };
    let pinned = resolved.apply_pin(&version);
    let by_reason = missing_counts(&pinned);
    let missing: usize = by_reason.values().sum();
    let missing_json = json!({"count": missing, "selected": sel.len(), "fraction": missing as f64 / sel.len() as f64,
        "limit": "more than 2% of the selected CAL documents missing: Tier C not evaluated",
        "exceeds_2pct": exceeds_missing_limit(missing, sel.len()), "by_reason": by_reason,
        "duplicate_text_resolved_to_twin": resolved.duplicate_text_resolved, "uncertain_items": resolved.uncertainties});
    inputs["outcomes_digest"] = json!(outcomes_digest(&pinned));
    if exceeds_missing_limit(missing, sel.len()) {
        return refuse(format!("{missing} of {} selected CAL documents missing (> 2%)", sel.len()), missing_json, &inputs);
    }
    let docs: Vec<FitDoc> = sel
        .iter()
        .filter_map(|d| {
            pinned[&d.id].valid().map(|s| FitDoc {
                id: d.id.clone(),
                human: d.cell == "human_only",
                balanced_weight: d.balanced_weight,
                pi_h: d.pi_h,
                fraction: s.clamped_fraction(),
            })
        })
        .collect();
    let map = match fit(&version, &docs) {
        Ok(m) => m,
        Err(e) => return refuse(format!("mechanical fit failure: {e}"), missing_json, &inputs),
    };
    let ops = operating_points(&map, &docs)?;
    let n_h = docs.iter().filter(|d| d.human).count();
    let mut lines = Vec::new();
    for d in &sel {
        let o = &pinned[&d.id];
        let row = match o {
            Outcome::Valid(s) => {
                let x = s.clamped_fraction();
                let p = map.apply(x)?;
                json!({"id": d.id, "stratum": d.stratum, "cell": d.cell, "status": "valid", "version": s.version,
                       "echo": s.echo, "fraction": x, "fraction_bits": bits(x), "calibrated": p, "calibrated_bits": bits(p),
                       "fit_weight": d.balanced_weight / d.pi_h})
            }
            Outcome::Missing(r) => json!({"id": d.id, "stratum": d.stratum, "cell": d.cell, "status": "missing", "reason": r}),
        };
        lines.extend(serde_json::to_vec(&row)?);
        lines.push(b'\n');
    }
    write_new(&a.out.join("cal-observations.jsonl"), &lines)?;
    let rec = json!({
        "schema": MAP_SCHEMA, "status": "fitted",
        "protocol": "competitive A protocol v1.2: calibration.pangram_map, comparator, operating_point.pangram",
        "pinned_version": {"version": version, "rule": PIN_RULE,
                           "source": {"store": store, "job": job, "bulk_index": bulk_index, "id": first_id}},
        "versions_among_stored_successes": versions,
        "selected": sel.len(), "valid": docs.len(), "missing": missing_json,
        "support": {"n_h": n_h, "n_pos": docs.len() - n_h},
        "map": map.description(),
        "operating_points": ops,
        "operating_point_rule": "t = smallest v among the distinct calibrated values of the valid CAL subsample documents (any origin) plus +inf with at most alpha of the valid CAL subsample humans (each once, unweighted) at or above v; comp_eval::operating_threshold; flag when calibrated Test value >= t in f64",
        "in_sample_cal_brier_ht_diagnostic": in_sample_brier(&map, &docs)?,
        "inputs": inputs, "implementation": implementation, "network_calls": 0,
        "limits": ["CAL values are a fitted diagnostic, never a comparison.",
                   "Pangram is observed only on the pre-registered CAL subsample."],
    });
    write_new(&a.out.join("map.json"), &serde_json::to_vec_pretty(&rec)?)?;
    println!("{}", json!({"status": "fitted", "version": rec["pinned_version"]["version"], "valid": docs.len(),
        "missing": missing, "op_0.01_bits": ops["0.01"]["bits"], "op_0.001_bits": ops["0.001"]["bits"]}));
    Ok(())
}
