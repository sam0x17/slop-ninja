//! Domain supervision targets `r2_domain_supervision_v1` (protocol A2 §6.4;
//! A9, S0-8).
//!
//! The rule covers every Train-2 domain with evaluation humans in any round-2
//! population (`domains`, declared by the caller). Every count is over Train
//! rows with **positive weight**: a zero-weight row (ELLIPSE) counts for
//! nothing (r2 F18g). For a covered domain with H humans:
//!
//! 1. positives ≥ ⌈H × 37,396 / 49,179⌉ (R0's bundle ratio, 0.760);
//! 2. frontier records ≥ ⌈H × 10,713 / 49,179⌉ (0.218). S0-8 holds the
//!    domain's frontier total only: the added records' allocation over
//!    labels (label-balanced, Terra ≥ Astra, more to gpt-6.1-terra; §6.4,
//!    §4.5, user 2026-10-28) is B5b's top-up plan, which checks it
//!    (amendment 5 item 5.3). **Waived for Enron** (Enron text never goes to
//!    a hosted model, §2.7, §6.1); the waiver is recorded;
//! 3. every declared generator × domain cell ≥ 59 rows (R0's realised
//!    coverage floor);
//! 4. the domain's share of Train-2 rows ≥ its share m_d of the blended
//!    evaluation human mass: ½ × its share of the Tier C Test human mass plus
//!    ½ × the v5 CC shares of the strata mapped to it (only a CC stratum that
//!    is the same domain, i.e. legal, is mapped).
//!
//! **How the targets are composed** (rows are filled from Train-mining and
//! generation until each rule holds; supply limits are the planner's):
//! - New frontier records and the rule-3 cell top-ups are positives, so they
//!   count toward rule 1. A domain's positives target at H humans is
//!   P(H) = max(⌈r1·H⌉, P0 + ΔF(H) + ΔC), with ΔF(H) the rule-2 frontier
//!   additions and ΔC the rule-3 additions; its rows are H + P(H).
//! - Rule 4 adds humans: a domain below m_d × N (N = every positive-weight
//!   Train-2 row after the additions) takes the smallest H with
//!   H + P(H) ≥ ⌈m_d × N⌉, so its added rows are humans plus their positives
//!   at rules 1–3. N grows with every addition, so the targets are iterated
//!   from below to the least fixed point; Σ m_d < 1 over the covered domains
//!   is required, and the passes are bounded by the fixed point's size.
//! - `planned_humans` and `planned_positives` (per generator label) are rows
//!   that a fixed plan adds anyway, such as the Enron Train half (S0-9); they
//!   count as current.
//!
//! **Enron.** A domain with Enron humans must be listed in `enron_domains`,
//! and a listed domain may hold no other positive-weight human collection.
//!
//! **Input.** The Train split directory of an assembler build only: its
//! MANIFEST must name split `train` and the `rows.jsonl` digest, and every row
//! read must be a Train row, or the run stops (`for_train_rows`, shared with
//! the `ess` report). Only metadata fields are used; the output holds counts,
//! shares and digests, never text or ids.
use crate::bundle::{MANIFEST_SCHEMA, file_sha};
use crate::{ORIGINS, sha_hex};
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::pile::ENRON_COLLECTION;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub const SUPERVISION_RULE: &str = "r2_domain_supervision_v1";
pub const CONFIG_SCHEMA: &str = "slop_ninja_a_r2_supervision_config_v1";
pub const OUTPUT_SCHEMA: &str = "slop_ninja_a_r2_domain_supervision_v1";
/// Rule 1: R0's bundle ratio, 37,396 positives per 49,179 humans (protocol A2 §6.4, M).
pub const RULE1: (u64, u64) = (37_396, 49_179);
/// Rule 2: R0's 10,713 frontier records per 49,179 humans (protocol A2 §6.4, M).
pub const RULE2: (u64, u64) = (10_713, 49_179);
/// Rule 3: R0's realised coverage floor per generator × domain cell (inherited).
pub const RULE3_FLOOR: u64 = 59;
pub const ENRON_WAIVER: &str = "rule 2 waived: Enron text never goes to a hosted model (protocol A2 §2.7, §6.1)";

/// The caller's declaration: covered domains, evaluation masses, generators and plans.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisionConfig {
    pub schema: String,
    /// Train-2 domains with evaluation humans in any round-2 population.
    pub domains: Vec<String>,
    /// Tier C Test human mass by Train-2 domain (normalised here; domains not covered are ignored).
    pub tier_c_test_mass: BTreeMap<String, f64>,
    /// `cc_launch_shares_v5` by CC stratum (normalised here).
    pub cc_shares: BTreeMap<String, f64>,
    /// CC strata that are a covered Train-2 domain (legal only, by the protocol).
    #[serde(default)]
    pub cc_stratum_domains: BTreeMap<String, String>,
    /// Covered domains whose rule 2 is waived (Enron).
    #[serde(default)]
    pub enron_domains: Vec<String>,
    /// Generator labels of rule 3's cells.
    pub rule3_generators: Vec<String>,
    #[serde(default)]
    pub planned_humans: BTreeMap<String, u64>,
    /// Domain -> generator label -> planned non-frontier positives.
    #[serde(default)]
    pub planned_positives: BTreeMap<String, BTreeMap<String, u64>>,
}

/// Positive-weight Train counts of one domain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub humans: u64,
    pub humans_zero_weight: u64,
    pub positives: u64,
    pub positives_zero_weight: u64,
    pub frontier: u64,
    pub frontier_by_label: BTreeMap<String, u64>,
    /// AI-involved rows per generator label.
    pub by_generator: BTreeMap<String, u64>,
    /// Collections of the positive-weight humans.
    pub human_collections: BTreeSet<String>,
}

impl Counts {
    /// Count one bundle row (metadata fields only).
    pub fn add(&mut self, r: &Value) -> Result<()> {
        let w = r["weight"].as_f64().context("weight")?;
        ensure!(w.is_finite() && w >= 0.0, "weight must be finite and >= 0");
        let origin = r["origin"].as_str().context("origin")?;
        ensure!(ORIGINS.contains(&origin), "origin {origin}");
        let live = w > 0.0;
        if origin == "human_only" {
            if live {
                self.humans += 1;
                self.human_collections.insert(r["collection"].as_str().unwrap_or("").to_string());
            } else {
                self.humans_zero_weight += 1;
            }
            return Ok(());
        }
        if !live {
            self.positives_zero_weight += 1;
            return Ok(());
        }
        self.positives += 1;
        let label = r["generator_label"].as_str().context("generator label of a positive")?;
        *self.by_generator.entry(label.into()).or_default() += 1;
        if r["source_kind"] == "frontier" {
            self.frontier += 1;
            *self.frontier_by_label.entry(label.into()).or_default() += 1;
        }
        Ok(())
    }
    pub fn rows(&self) -> u64 {
        self.humans + self.positives
    }
}

/// Count the Train split directory `dir` of a build (see the module note on input).
pub fn count_train(dir: &Path) -> Result<(BTreeMap<String, Counts>, Value)> {
    let mut by_domain: BTreeMap<String, Counts> = BTreeMap::new();
    let input = for_train_rows(dir, |i, v| {
        let d = v["domain"].as_str().with_context(|| format!("line {i}: domain"))?;
        by_domain.entry(d.into()).or_default().add(v)
    })?;
    Ok((by_domain, input))
}

/// Read the rows of the Train split directory `dir` of a build: the manifest
/// must be an unsealed Train split whose `rows.jsonl` digest and row count
/// match, and a row that is not a Train row stops the read before any later
/// line. Calls `f(line number, row)`; returns the input record (digests only).
pub fn for_train_rows(dir: &Path, mut f: impl FnMut(usize, &Value) -> Result<()>) -> Result<Value> {
    let mpath = dir.join("MANIFEST.json");
    let mbytes = fs::read(&mpath).with_context(|| format!("read {}", mpath.display()))?;
    let m: Value = serde_json::from_slice(&mbytes).context("MANIFEST.json")?;
    ensure!(m["schema"] == MANIFEST_SCHEMA, "{} is not an assembler split manifest", mpath.display());
    ensure!(m["split"] == "train" && m["sealed"] != true, "{} is not an unsealed Train split", dir.display());
    let rows_path = dir.join("rows.jsonl");
    let rows_sha = file_sha(&rows_path)?;
    ensure!(m["files"]["rows.jsonl"] == rows_sha.as_str(), "{} differs from its manifest", rows_path.display());
    let rd = BufReader::with_capacity(1 << 20, fs::File::open(&rows_path)?);
    let mut n = 0usize;
    for (i, line) in rd.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).with_context(|| format!("rows.jsonl line {}", i + 1))?;
        if v["split"] != "train" {
            bail!("rows.jsonl line {} is not a Train row: stopped before reading further", i + 1);
        }
        f(i + 1, &v).with_context(|| format!("rows.jsonl line {}", i + 1))?;
        n += 1;
    }
    ensure!(n == m["rows"].as_u64().unwrap_or(u64::MAX) as usize, "rows.jsonl holds {n} rows, its manifest says {}", m["rows"]);
    Ok(json!({"train_dir": dir, "manifest_sha256": sha_hex(&mbytes), "rows_sha256": rows_sha, "rows": n,
        "build_id": m["build_id"], "row_schema": m["row_schema"]}))
}

/// The Kish ESS report of a build's Train split under the trainer's weighting
/// (`covariate::trainer_weighting`; protocol A2 V24, amendment 5 item 5.1).
/// A report, not a refusal: `passes_floor` says whether the split meets R0's floor.
pub fn train_ess(dir: &Path, code: &Value) -> Result<Value> {
    let mut rows: Vec<(usize, String, f64)> = Vec::new();
    let input = for_train_rows(dir, |_, v| {
        let o = v["origin"].as_str().context("origin")?;
        let o = ORIGINS.iter().position(|x| *x == o).with_context(|| format!("origin {o}"))?;
        let w = v["weight"].as_f64().context("weight")?;
        rows.push((o, v["cohort_id"].as_str().context("cohort_id")?.to_string(), w));
        Ok(())
    })?;
    let t: Vec<(usize, &str, f64)> = rows.iter().map(|(o, c, w)| (*o, c.as_str(), *w)).collect();
    let tw = crate::covariate::trainer_weighting(&t)?;
    Ok(json!({"schema": "slop_ninja_a_train_ess_report_v1", "kish_ess": crate::covariate::ess_value(&tw),
        "input": input, "code": code}))
}

fn ceil_ratio(x: u64, (num, den): (u64, u64)) -> u64 {
    ((x as u128 * num as u128).div_ceil(den as u128)) as u64
}

/// One covered domain's fixed inputs.
struct Domain {
    h0: u64,
    p0: u64,
    f0: u64,
    cells: BTreeMap<String, u64>,
    dc: u64,
    waived: bool,
    m: f64,
}

impl Domain {
    fn frontier(&self, h: u64) -> u64 {
        if self.waived { self.f0 } else { self.f0.max(ceil_ratio(h, RULE2)) }
    }
    fn positives(&self, h: u64) -> u64 {
        ceil_ratio(h, RULE1).max(self.p0 + (self.frontier(h) - self.f0) + self.dc)
    }
    fn rows(&self, h: u64) -> u64 {
        h + self.positives(h)
    }
    /// Smallest h >= self-current with rows(h) >= target.
    fn humans_for(&self, from: u64, target: u64) -> u64 {
        let (mut lo, mut hi) = (from, from.max(target));
        if self.rows(lo) >= target {
            return lo;
        }
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if self.rows(mid) >= target { hi = mid } else { lo = mid }
        }
        hi
    }
}

fn shares(m: &BTreeMap<String, f64>, what: &str) -> Result<BTreeMap<String, f64>> {
    let mut s = 0.0;
    for (k, v) in m {
        ensure!(v.is_finite() && *v >= 0.0, "{what} {k}: mass must be finite and >= 0");
        s += v;
    }
    ensure!(s > 0.0, "{what}: no mass");
    Ok(m.iter().map(|(k, v)| (k.clone(), v / s)).collect())
}

/// The targets of `cfg` over the Train counts `counts` (all domains of the bundle).
pub fn targets(counts: &BTreeMap<String, Counts>, cfg: &SupervisionConfig) -> Result<Value> {
    ensure!(cfg.schema == CONFIG_SCHEMA, "supervision config schema must be {CONFIG_SCHEMA}");
    let covered: BTreeSet<&str> = cfg.domains.iter().map(String::as_str).collect();
    ensure!(!covered.is_empty() && covered.len() == cfg.domains.len(), "covered domains must be distinct and non-empty");
    let gens: BTreeSet<&str> = cfg.rule3_generators.iter().map(String::as_str).collect();
    ensure!(!gens.is_empty() && gens.len() == cfg.rule3_generators.len(), "rule-3 generators must be distinct and non-empty");
    for d in cfg.enron_domains.iter().chain(cfg.planned_humans.keys()).chain(cfg.planned_positives.keys()) {
        ensure!(covered.contains(d.as_str()), "domain {d} is not covered");
    }
    for (d, c) in counts {
        if c.human_collections.contains(ENRON_COLLECTION) {
            ensure!(cfg.enron_domains.contains(d), "domain {d} holds {ENRON_COLLECTION} humans but is not an Enron domain");
        }
    }
    for d in &cfg.enron_domains {
        if let Some(c) = counts.get(d) {
            ensure!(c.human_collections.iter().all(|x| x == ENRON_COLLECTION),
                "Enron domain {d} holds humans of other collections: the rule-2 waiver is for Enron only");
        }
    }
    let tc = shares(&cfg.tier_c_test_mass, "Tier C Test mass")?;
    let cc = shares(&cfg.cc_shares, "CC shares")?;
    for (s, d) in &cfg.cc_stratum_domains {
        ensure!(cc.contains_key(s), "CC stratum {s} has no share");
        ensure!(covered.contains(d.as_str()), "CC stratum {s} maps to {d}, which is not covered");
    }
    let mass = |d: &str| -> (f64, f64) {
        let t = tc.get(d).copied().unwrap_or(0.0);
        let c: f64 = cfg.cc_stratum_domains.iter().filter(|(_, x)| x.as_str() == d).map(|(s, _)| cc[s]).sum();
        (0.5 * t, 0.5 * c)
    };
    let empty = Counts::default();
    let mut doms: BTreeMap<&str, Domain> = BTreeMap::new();
    for d in &covered {
        let c = counts.get(*d).unwrap_or(&empty);
        let planned = cfg.planned_positives.get(*d);
        let mut cells = BTreeMap::new();
        for g in &gens {
            let n = c.by_generator.get(*g).copied().unwrap_or(0) + planned.and_then(|p| p.get(*g)).copied().unwrap_or(0);
            cells.insert(g.to_string(), n);
        }
        let dc = cells.values().map(|n| RULE3_FLOOR.saturating_sub(*n)).sum();
        let (t, cm) = mass(d);
        doms.insert(d, Domain {
            h0: c.humans + cfg.planned_humans.get(*d).copied().unwrap_or(0),
            p0: c.positives + planned.map_or(0, |p| p.values().sum()),
            f0: c.frontier, cells, dc, waived: cfg.enron_domains.iter().any(|x| x == d), m: t + cm,
        });
    }
    let s: f64 = doms.values().map(|x| x.m).sum();
    ensure!(s < 1.0, "the covered domains' evaluation mass shares sum to {s} >= 1: rule 4 has no solution");
    let other: u64 = counts.iter().filter(|(d, _)| !covered.contains(d.as_str())).map(|(_, c)| c.rows()).sum();
    // Rules 1-3 at the current humans, then rule 4 from below to the least fixed point.
    let mut h: BTreeMap<&str, u64> = doms.iter().map(|(d, x)| (*d, x.h0)).collect();
    let n_of = |h: &BTreeMap<&str, u64>| other + doms.iter().map(|(d, x)| x.rows(h[d])).sum::<u64>();
    let a = n_of(&h);
    let bound = ((a + 3 * doms.len() as u64) as f64 / (1.0 - s)).ceil() as u64;
    let mut passes = 0u64;
    loop {
        let n = n_of(&h);
        ensure!(n <= bound && passes <= bound.saturating_sub(a) + 1, "rule 4 did not converge (internal fault)");
        passes += 1;
        let mut changed = false;
        let mut next = h.clone();
        for (d, x) in &doms {
            if x.m <= 0.0 {
                continue;
            }
            let target = (x.m * n as f64).ceil() as u64;
            if x.rows(h[d]) < target {
                next.insert(d, x.humans_for(h[d], target));
                changed = true;
            }
        }
        h = next;
        if !changed {
            break;
        }
    }
    let n_final = n_of(&h);
    let mut out = serde_json::Map::new();
    let mut totals = (0u64, 0u64, 0u64, 0u64);
    for (d, x) in &doms {
        let c = counts.get(*d).unwrap_or(&empty);
        let hd = h[d];
        let f = x.frontier(hd);
        let p = x.positives(hd);
        let add_f = f - x.f0;
        let floor_adds: BTreeMap<&String, u64> = x.cells.iter().map(|(g, n)| (g, RULE3_FLOOR.saturating_sub(*n))).filter(|(_, a)| *a > 0).collect();
        let (t, cm) = mass(d);
        totals.0 += hd - x.h0;
        totals.1 += p - x.p0;
        totals.2 += add_f;
        totals.3 += x.rows(hd);
        out.insert(d.to_string(), json!({
            "eval_mass_share": x.m, "tier_c_test_half": t, "cc_half": cm,
            "current": {"humans_positive_weight": c.humans, "humans_zero_weight": c.humans_zero_weight,
                "positives_positive_weight": c.positives, "positives_zero_weight": c.positives_zero_weight,
                "frontier_records": c.frontier, "frontier_by_label": c.frontier_by_label, "rows": c.rows(),
                "generator_rows": x.cells.iter().map(|(g, n)| (g.clone(), json!(n - cfg.planned_positives.get(*d).and_then(|p| p.get(g)).copied().unwrap_or(0)))).collect::<serde_json::Map<_, _>>()},
            "planned": {"humans": cfg.planned_humans.get(*d).copied().unwrap_or(0), "positives": cfg.planned_positives.get(*d)},
            "targets": {"humans": hd, "positives": p, "frontier_records": f, "rows": x.rows(hd),
                "rule1_min_positives": ceil_ratio(hd, RULE1),
                "rule2_min_frontier": if x.waived { Value::Null } else { json!(ceil_ratio(hd, RULE2)) },
                "rule3_min_rows_per_generator": RULE3_FLOOR, "rule4_min_rows": (x.m * n_final as f64).ceil() as u64,
                "positives_without_humans": hd == 0 && p > 0},
            "add": {"humans": hd - x.h0, "positives": p - x.p0, "frontier_records": add_f,
                "local_positives": p - x.p0 - add_f, "local_for_generator_floor": floor_adds},
            "binding": {"rule1": ceil_ratio(hd, RULE1) > x.p0 + add_f + x.dc, "rule2": if x.waived { json!("waived") } else { json!(add_f > 0) },
                "rule3": floor_adds.keys().collect::<Vec<_>>(), "rule4": hd > x.h0},
            "rule2_waiver": if x.waived { json!(ENRON_WAIVER) } else { Value::Null },
        }));
    }
    let uncovered: BTreeMap<&String, u64> = counts.iter().filter(|(d, _)| !covered.contains(d.as_str())).map(|(d, c)| (d, c.rows())).collect();
    let ignored_mass: Vec<&String> = cfg.tier_c_test_mass.keys().filter(|d| !covered.contains(d.as_str())).collect();
    let unmapped_cc: Vec<&String> = cfg.cc_shares.keys().filter(|s| !cfg.cc_stratum_domains.contains_key(*s)).collect();
    Ok(json!({
        "schema": OUTPUT_SCHEMA, "rule": SUPERVISION_RULE,
        "constants": {"rule1_positives_per_human": [RULE1.0, RULE1.1], "rule2_frontier_per_human": [RULE2.0, RULE2.1],
            "rule3_floor": RULE3_FLOOR, "counts": "Train rows with positive weight only"},
        "rule3_generators": cfg.rule3_generators,
        "frontier_label_allocation": "not part of S0-8: B5b's top-up plan allocates each domain's added frontier records over labels (label-balanced, Terra >= Astra, more to gpt-6.1-terra; protocol A2 §6.4, §4.5, amendment 5 item 5.3) and checks it",
        "enron_domains": cfg.enron_domains,
        "composition": "new frontier and rule-3 rows count toward rule 1; rule 4 adds humans with their rule 1-3 positives; least fixed point over N",
        "rows": {"current_all_domains": counts.values().map(Counts::rows).sum::<u64>(), "target_all_domains": n_final,
            "uncovered_domains": uncovered},
        "covered_eval_mass_share_sum": s, "rule4_passes": passes,
        "tier_c_test_domains_not_covered": ignored_mass, "cc_strata_not_mapped": unmapped_cc,
        "added": {"humans": totals.0, "positives": totals.1, "frontier_records": totals.2},
        "domains": out,
    }))
}

/// Count `train_dir`, compute the targets and write them once to `out` (mode 600).
pub fn run(train_dir: &Path, config: &Path, out: &Path, code: &Value) -> Result<Value> {
    ensure!(!out.exists(), "{} exists; the targets are written once", out.display());
    let cbytes = fs::read(config).with_context(|| format!("read {}", config.display()))?;
    let cfg: SupervisionConfig = serde_json::from_slice(&cbytes).context("supervision config")?;
    let (counts, input) = count_train(train_dir)?;
    let mut v = targets(&counts, &cfg)?;
    v["inputs"] = json!({"train": input, "config": {"path": config, "sha256": sha_hex(&cbytes)}});
    v["code"] = code.clone();
    let mut b = serde_json::to_vec_pretty(&v)?;
    b.push(b'\n');
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(out).with_context(|| format!("create {}", out.display()))?;
    f.write_all(&b)?;
    f.sync_all()?;
    Ok(json!({"out": out, "sha256": sha_hex(&b), "domains": cfg.domains.len(), "rows_target": v["rows"]["target_all_domains"],
        "added": v["added"]}))
}
