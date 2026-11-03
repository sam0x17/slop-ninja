//! Pre-registered Pangram Test subsample (competitive A protocol v1.2,
//! `pangram_subsample`): strata, Neyman allocation and selection.
//!
//! ```text
//! a_comp_pangram_test_subsample --protocol protocol-v1.json \
//!   --rows TEST_BUNDLE/rows.jsonl --n-test N --out OUT_DIR
//! ```
//!
//! - Population: the realised primary Test documents (bundle `population`
//!   `primary` or `frontier`; `comp_eval::PRIMARY_POPULATIONS`), after Test
//!   positives are generated and accepted. Every other row is counted and
//!   excluded.
//! - Strata: domain x origin cell x realised-share bin (`Row::stratum`, the
//!   evaluator's own key; a realised share below 10% is `(0,10)`), empty
//!   strata dropped.
//! - `W_h`: stratum share of the full primary Test balanced mass. `S_h`: the
//!   fixed planning SDs, checked bit for bit against the protocol file. Floor
//!   3, `pangram_subsample::neyman_allocation`, size `min(n_test, population)`.
//!   `n_test` is the figure bound in the seal; below n_min the tool refuses.
//! - Selection: the `n_h` smallest `selection_key(20261027, "test", id)` per
//!   stratum (`pangram_subsample::select`).
//!
//! Metadata is read through `comp_eval::parse_rows`; text is read only to
//! count words (Unicode whitespace) for billed units and to verify its hash.
//! Nothing is printed from it and no provider is called.
//!
//! Outputs `OUT_DIR/population.jsonl`, `OUT_DIR/subsample.jsonl` (the selected
//! rows; the collector's SUBSAMPLE_JSONL) and `OUT_DIR/design.json`.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_a_baseline::comp_eval::{PRIMARY_POPULATIONS, parse_rows};
use grammar_a_baseline::pangram_subsample::{STRATUM_FLOOR, SUBSAMPLE_SEED, selection_key};
use grammar_a_baseline::tierc::{N_MIN, PLANNING_SD, check_planning_sd, draw_test};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

const PROTOCOL_SHA256: &str = "9ca96f5228c8a40671bb7c2c64d125cb587c7bfe534c3974cb0bf502966c8fab";

#[derive(Parser)]
#[command(about = "Draw the pre-registered Pangram Test subsample (no network)")]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    rows: PathBuf,
    /// n_test bound in the seal at the balance check.
    #[arg(long)]
    n_test: usize,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Deserialize)]
struct TextRow {
    id: String,
    split: String,
    population: String,
    text: String,
    text_sha256: String,
}

fn units(words: u64) -> u64 {
    words.div_ceil(100).max(1)
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "output {} exists", a.out.display());
    let proto_bytes = fs::read(&a.protocol)?;
    let proto_sha = hex::encode(Sha256::digest(&proto_bytes));
    ensure!(
        proto_sha == PROTOCOL_SHA256,
        "protocol sha256 {proto_sha} is not the pinned v1.2 file"
    );
    let protocol: Value = serde_json::from_slice(&proto_bytes)?;
    check_planning_sd(&protocol)?;
    let raw = fs::read_to_string(&a.rows)?;
    let rows_sha = hex::encode(Sha256::digest(raw.as_bytes()));
    let all = parse_rows(&raw)?;
    let mut excluded: BTreeMap<String, usize> = BTreeMap::new();
    let mut other_splits = 0usize;
    let rows: Vec<_> = all
        .into_iter()
        .filter(|r| {
            if r.split != "test" {
                other_splits += 1;
                return false;
            }
            if !r.primary {
                *excluded.entry(r.population.clone()).or_default() += 1;
            }
            r.primary
        })
        .collect();
    ensure!(!rows.is_empty(), "no primary Test rows");
    // Text pass: hash check and word counts only.
    let mut words: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let t: TextRow = serde_json::from_str(line).with_context(|| format!("rows line {}", i + 1))?;
        if t.split != "test" || !PRIMARY_POPULATIONS.contains(&t.population.as_str()) {
            continue;
        }
        let h = hex::encode(Sha256::digest(t.text.as_bytes()));
        ensure!(h == t.text_sha256, "rows line {}: text_sha256 mismatch", i + 1);
        words.insert(t.id, (h, t.text.split_whitespace().count() as u64));
    }
    ensure!(words.len() == rows.len(), "text pass and metadata pass disagree");
    let d = draw_test(&rows, a.n_test)?;
    fs::create_dir_all(&a.out)?;
    let mut pop = fs::OpenOptions::new().create_new(true).write(true).open(a.out.join("population.jsonl"))?;
    let mut sub = fs::OpenOptions::new().create_new(true).write(true).open(a.out.join("subsample.jsonl"))?;
    let big: BTreeMap<&str, usize> = d.strata.iter().map(|s| (s.key.as_str(), s.population)).collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&x, &y| rows[x].id.cmp(&rows[y].id));
    let (mut u_sel, mut u_all) = (0u64, 0u64);
    let mut cells: BTreeMap<String, usize> = BTreeMap::new();
    for i in order {
        let r = &rows[i];
        let k = r.stratum()?;
        let (sha, w) = &words[&r.id];
        let (n_h, n_big) = (d.allocation[&k], big[k.as_str()]);
        let selected = d.selected.contains(&r.id);
        u_all += units(*w);
        if selected {
            u_sel += units(*w);
            *cells.entry(r.cell.name().to_string()).or_default() += 1;
        }
        let rec = json!({"id": r.id, "stratum": k, "domain": r.domain, "cell": r.cell.name(),
            "share": r.share, "source_group": r.group, "population": r.population,
            "text_sha256": sha, "words": w, "billed_units": units(*w), "balanced_weight": d.weights[i],
            "selection_key": hex::encode(selection_key(SUBSAMPLE_SEED, "test", &r.id)),
            "n_h": n_h, "N_h": n_big, "pi_h": n_h as f64 / n_big as f64, "selected": selected});
        writeln!(pop, "{rec}")?;
        if selected {
            writeln!(sub, "{rec}")?;
        }
    }
    pop.sync_all()?;
    sub.sync_all()?;
    let groups: BTreeSet<&str> = rows.iter().map(|r| r.group.as_str()).collect();
    let design = json!({
        "schema": "slop_ninja_a_comp_pangram_test_subsample_v1",
        "protocol": "competitive A protocol v1.2 pangram_subsample (strata.test, allocation.test, selection, estimator)",
        "protocol_sha256": proto_sha, "input_rows": a.rows, "input_rows_sha256": rows_sha,
        "population": rows.len(), "source_groups": groups.len(),
        "primary_populations": PRIMARY_POPULATIONS, "non_primary_test_rows_excluded": excluded,
        "rows_of_other_splits_ignored": other_splits,
        "seed": SUBSAMPLE_SEED, "split_key": "test", "floor": STRATUM_FLOOR, "n_min": N_MIN,
        "n_test_sealed": a.n_test, "n": d.n,
        "n_rule": "n = min(n_test, realised primary Test population); no resizing after Test prose",
        "planning_sd": PLANNING_SD.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
        "strata": d.strata.iter().map(|s| json!({"key": s.key, "N_h": s.population, "W_h": s.weight,
            "S_h": s.sd, "n_h": d.allocation[&s.key]})).collect::<Vec<_>>(),
        "strata_count": d.strata.len(),
        "subsample_cells": cells,
        "word_policy": "Unicode whitespace-separated tokens (str::split_whitespace) of the exact bundle text",
        "unit_policy": "max(1, ceil(words / 100)) per item",
        "billed_units_selected": u_sel, "billed_units_population": u_all,
        "reads": "comp_eval::parse_rows metadata (id, split, origin, share, source_group, domain, population and its validation fields); text for word count and sha256 only",
        "network_calls": 0,
    });
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(a.out.join("design.json"))?;
    f.write_all(&serde_json::to_vec_pretty(&design)?)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"population": rows.len(), "n": d.n, "strata": d.strata.len(), "cells": design["subsample_cells"], "billed_units_selected": u_sel}));
    Ok(())
}
