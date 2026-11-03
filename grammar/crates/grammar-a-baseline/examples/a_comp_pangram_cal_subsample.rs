//! Pre-registered Pangram CAL subsample (competitive A protocol v1.2,
//! `pangram_subsample`). CAL_ROWS_JSONL OUT_DIR [--budget-units U]
//!
//! Strata are domain x origin cell over the realised primary CAL documents;
//! rows of any other population are counted and excluded.
//! `W_h` is the stratum share of the full CAL balanced mass (equal mass per
//! source group, per observed origin cell within a group, per document within
//! a cell), `S_h = 1`, `n_cal = 600`, floor 3, allocated by
//! `pangram_subsample::neyman_allocation` and selected by the smallest
//! `selection_key(20261027, "cal", id)` within each stratum.
//!
//! Text is read only to count words (Unicode whitespace-separated tokens) and
//! to hash it; nothing is printed or written from it. Billed units are one
//! started 100-word block per item, at least one. With `--budget-units U`
//! (spendable units after the margin, computed privately) the tool also
//! applies the protocol's `n_test` rule. No currency figure is read or
//! written, and no provider is called.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::pangram_subsample::{
    Cell, EvalDoc, STRATUM_FLOOR, SUBSAMPLE_SEED, Stratum, balanced_weights, neyman_allocation,
    select, selection_key, stratum_key,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
};

const N_CAL: usize = 600;
const N_MIN: u64 = 1900;
const N_TEST_ROUNDING: u64 = 50;

struct Doc {
    id: String,
    group: String,
    cell: Cell,
    domain: String,
    stratum: String,
    text_sha256: String,
    words: u64,
    units: u64,
}

fn billed_units(words: u64) -> u64 {
    words.div_ceil(100).max(1)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2 || (args.len() == 4 && args[2] == "--budget-units"),
        "usage: CAL_ROWS_JSONL OUT_DIR [--budget-units U]"
    );
    let rows = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    let budget: Option<f64> = if args.len() == 4 {
        let u: f64 = args[3].parse().context("--budget-units")?;
        ensure!(u.is_finite() && u > 0.0, "bad budget units");
        Some(u)
    } else {
        None
    };
    ensure!(!out.exists(), "output {} exists", out.display());

    let raw = fs::read(&rows).with_context(|| rows.display().to_string())?;
    let rows_sha = hex::encode(Sha256::digest(&raw));
    let mut docs = Vec::new();
    let mut ids = BTreeSet::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for (i, line) in BufReader::new(&raw[..]).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).with_context(|| format!("line {}", i + 1))?;
        let s = |k: &str| -> Result<String> {
            Ok(v[k]
                .as_str()
                .with_context(|| format!("line {}: {k}", i + 1))?
                .to_string())
        };
        ensure!(s("split")? == "cal", "line {}: not a CAL row", i + 1);
        let population = s("population")?;
        if population != "primary" {
            // The design covers the realised primary documents only.
            *skipped.entry(population).or_default() += 1;
            continue;
        }
        let id = s("id")?;
        ensure!(ids.insert(id.clone()), "duplicate id {id}");
        let text = s("text")?;
        let text_sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        ensure!(
            text_sha256 == s("text_sha256")?,
            "line {}: text_sha256 mismatch",
            i + 1
        );
        let cell = Cell::parse(&s("origin")?)?;
        let domain = s("domain")?;
        // CAL strata are domain x origin cell (no share bins).
        let stratum = match cell {
            Cell::Mixed => {
                ensure!(!domain.is_empty() && !domain.contains('|'), "bad domain");
                format!("{domain}|mixed")
            }
            c => stratum_key(&domain, c, None)?,
        };
        let words = text.split_whitespace().count() as u64;
        docs.push(Doc {
            id,
            group: s("source_group")?,
            cell,
            domain,
            stratum,
            text_sha256,
            words,
            units: billed_units(words),
        });
    }
    ensure!(!docs.is_empty(), "no CAL rows");

    let eval: Vec<EvalDoc> = docs
        .iter()
        .map(|d| EvalDoc {
            group: d.group.clone(),
            cell: d.cell,
            stratum: d.stratum.clone(),
            value: 0.0,
        })
        .collect();
    let w = balanced_weights(&eval);
    let mut by: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for (d, w) in docs.iter().zip(&w) {
        let e = by.entry(d.stratum.clone()).or_default();
        e.0 += 1;
        e.1 += w;
    }
    let strata: Vec<Stratum> = by
        .iter()
        .map(|(k, (n, w))| Stratum {
            key: k.clone(),
            population: *n,
            weight: *w,
            sd: 1.0,
        })
        .collect();
    let alloc = neyman_allocation(&strata, N_CAL, STRATUM_FLOOR)?;
    let allocation: BTreeMap<String, usize> = strata
        .iter()
        .zip(&alloc)
        .map(|(s, &n)| (s.key.clone(), n))
        .collect();
    let pairs: Vec<(String, String)> = docs
        .iter()
        .map(|d| (d.id.clone(), d.stratum.clone()))
        .collect();
    let chosen = select(&pairs, &allocation, SUBSAMPLE_SEED, "cal")?;
    ensure!(chosen.len() == N_CAL, "selected {} != {N_CAL}", chosen.len());

    let u_cal: u64 = docs
        .iter()
        .filter(|d| chosen.contains(&d.id))
        .map(|d| d.units)
        .sum();
    let total_units: u64 = docs.iter().map(|d| d.units).sum();
    let u_test = total_units as f64 / docs.len() as f64;

    fs::create_dir_all(&out)?;
    let mut sel = fs::File::create(out.join("subsample.jsonl"))?;
    let mut pop = fs::File::create(out.join("population.jsonl"))?;
    let mut order: Vec<(&Doc, f64)> = docs.iter().zip(w.iter().copied()).collect();
    order.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    for (d, w) in &order {
        let n_h = allocation[&d.stratum];
        let big_n = by[&d.stratum].0;
        let rec = json!({
            "id": d.id,
            "stratum": d.stratum,
            "domain": d.domain,
            "cell": d.cell.name(),
            "source_group": d.group,
            "text_sha256": d.text_sha256,
            "words": d.words,
            "billed_units": d.units,
            "balanced_weight": w,
            "selection_key": hex::encode(selection_key(SUBSAMPLE_SEED, "cal", &d.id)),
            "n_h": n_h,
            "N_h": big_n,
            "pi_h": n_h as f64 / big_n as f64,
            "selected": chosen.contains(&d.id),
        });
        writeln!(pop, "{rec}")?;
        if chosen.contains(&d.id) {
            writeln!(sel, "{rec}")?;
        }
    }
    let n_test = budget.map(|u| {
        let afford = (u - u_cal as f64) / (N_TEST_ROUNDING as f64 * u_test);
        let n = if afford > 0.0 {
            N_TEST_ROUNDING * afford.floor() as u64
        } else {
            0
        };
        json!({
            "budget_units": u,
            "n_test_before_population_clamp": n,
            "rule": "n_test = min(realised primary Test population, 50 * floor((U - U_cal) / (50 * u_test))); below n_min no Pangram request and Tier C not evaluated",
            "at_or_above_n_min": n >= N_MIN,
        })
    });
    let report = json!({
        "schema": "slop_ninja_a_comp_pangram_cal_subsample_v1",
        "protocol": "competitive A protocol v1.2 pangram_subsample",
        "input_rows": args[0],
        "input_rows_sha256": rows_sha,
        "population": docs.len(),
        "non_primary_rows_excluded": skipped,
        "groups": docs.iter().map(|d| d.group.as_str()).collect::<BTreeSet<_>>().len(),
        "seed": SUBSAMPLE_SEED,
        "n_cal": N_CAL,
        "floor": STRATUM_FLOOR,
        "sd": 1.0,
        "strata": strata.iter().zip(&alloc).map(|(s, n)| json!({
            "key": s.key, "N_h": s.population, "W_h": s.weight, "n_h": n,
        })).collect::<Vec<_>>(),
        "word_policy": "Unicode whitespace-separated tokens (Rust str::split_whitespace) of the exact bundle text",
        "unit_policy": "max(1, ceil(words / 100)) per item",
        "U_cal": u_cal,
        "u_test": u_test,
        "cal_total_units": total_units,
        "n_min": N_MIN,
        "n_test_rounding": N_TEST_ROUNDING,
        "n_test": n_test,
        "subsample_cells": {
            "human_only": docs.iter().filter(|d| chosen.contains(&d.id) && d.cell == Cell::HumanOnly).count(),
            "model_only": docs.iter().filter(|d| chosen.contains(&d.id) && d.cell == Cell::ModelOnly).count(),
            "mixed": docs.iter().filter(|d| chosen.contains(&d.id) && d.cell == Cell::Mixed).count(),
        },
        "reads": "id, split, population, origin, domain, source_group, text (word count and sha256 only)",
    });
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    fs::write(out.join("design.json"), bytes)?;
    println!("{}", serde_json::to_string(&report["subsample_cells"])?);
    Ok(())
}
