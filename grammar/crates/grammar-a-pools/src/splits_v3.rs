//! Split extension v3: keep every assignment of a base split directory
//! byte for byte, add Train and Train-mining documents from newly admitted
//! sources, and reserve two new evaluation domains (FineWeb, Global Voices)
//! from metadata-only scans of their reserved contributor buckets.
//!
//! * **Base.** Every base file is verified against its `SHA256SUMS` and
//!   copied unchanged; Train, Train-mining and held files keep their base
//!   lines first and append new rows. Evaluation manifests of the base
//!   domains are copied unchanged.
//! * **New Train side.** Rows of `pool` (one dedup output against a
//!   successor index that registers the base pool and every acquired
//!   evaluation candidate) are Train-side only: FineWeb, Global Voices,
//!   Stack Exchange and prose-licence PLOS. A prose-licence PLOS article
//!   with an author (folded name) of any reserved PLOS evaluation group
//!   (Test, CAL, Dev-select, Dev-iter or reserved-unassigned) is held.
//!   Train takes split components in seeded rank order per domain up to a
//!   quota and a per-contributor cap; the rest is Train-mining.
//! * **New evaluation domains.** Groups are contributors (registered
//!   domain; WordPress posting account) from contributor-bucket ranges
//!   declared per domain and split (default `reserved_lo..1000`; ranges of one
//!   domain are equal or disjoint, and its Train side lies below all of
//!   them), units are pages or posts. Groups of one range are allocated only
//!   to the splits that draw on it. Groups are allocated by deficit in seeded
//!   rank order until each split reaches `target * oversample` expected
//!   capped documents (expected documents per unit = the domain's Train-side
//!   yield). No backfill across domains; unreached and spare groups stay
//!   reserved and unused.
use crate::{
    io::{self, JsonlWriter},
    splits::{self, EVAL_SPLITS, Group, RESERVATION_SCHEMA, GROUP_SCHEMA, DOCUMENT_SCHEMA, Unit, allocate, rank},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_split_extension_summary_v3";
pub const NEW_EVAL_DOMAINS: [&str; 2] = ["fineweb", "global_voices"];
/// Train-side domains of new rows; `plos_prose` is PLOS whose licence was a prose statement.
pub const NEW_TRAIN_DOMAINS: [&str; 4] = ["fineweb", "global_voices", "stack_exchange", "plos_prose"];

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn new_domain(r: &Value) -> Result<usize> {
    Ok(match s(r, "collection") {
        "fineweb_cc_historical" => 0,
        "global_voices_historical" => 1,
        "stackexchange_historical" => 2,
        "plos_historical" if r["rights"]["licence_basis"] == "prose_statement_without_link" => 3,
        c => bail!("row {} of collection {c} is not a new Train-side source", s(r, "id")),
    })
}

/// Contributor key used for the bucket and Train caps.
fn contributor(d: usize, r: &Value) -> Option<String> {
    match d {
        0 | 1 | 2 => r["writer_key"].as_str().map(str::to_owned),
        _ => None,
    }
}

struct Cfg {
    raw: Value,
    seed: String,
    base: PathBuf,
    out: PathBuf,
    pool: PathBuf,
    reserved_lo: u32,
    /// Contributor-bucket range per new evaluation domain and split.
    ranges: [[(u32, u32); 4]; 2],
    scans: [PathBuf; 2],
    plos_scan: PathBuf,
    targets: [[f64; 4]; 2],
    /// Group cap per new domain and split; splits sharing a bucket range share a cap.
    caps: [[f64; 4]; 2],
    oversample: [f64; 2],
    quota: [u64; 4],
    train_cap: [Option<u64>; 4],
    train_total_cap: u64,
}

fn load(path: &Path) -> Result<Cfg> {
    let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let p = |k: &str| -> Result<PathBuf> { Ok(PathBuf::from(v[k].as_str().with_context(|| format!("config {k}"))?)) };
    let mut targets = [[0.0; 4]; 2];
    let (mut caps, mut over) = ([[0.0; 4]; 2], [0.0; 2]);
    for (i, d) in NEW_EVAL_DOMAINS.iter().enumerate() {
        for (j, sp) in EVAL_SPLITS.iter().enumerate() {
            targets[i][j] = v["targets"][d][sp].as_f64().with_context(|| format!("target {d} {sp}"))?;
        }
        for (j, sp) in EVAL_SPLITS.iter().enumerate() {
            let cv = &v["max_docs_per_group"][d];
            caps[i][j] = if cv.is_object() { cv[sp].as_f64() } else { cv.as_f64() }.with_context(|| format!("cap {d} {sp}"))?;
            ensure!(caps[i][j] >= 1.0 && caps[i][j].fract() == 0.0, "cap for {d} {sp}");
        }
        over[i] = v["oversample"][d].as_f64().context("oversample")?;
        ensure!(over[i] >= 1.0, "oversample for {d}");
    }
    let mut quota = [0u64; 4];
    let mut train_cap = [None; 4];
    for (i, d) in NEW_TRAIN_DOMAINS.iter().enumerate() {
        quota[i] = v["train_quota"][d].as_u64().with_context(|| format!("train_quota {d}"))?;
        train_cap[i] = v["train_max_docs_per_contributor"][d].as_u64();
    }
    Ok(Cfg {
        seed: v["seed"].as_str().context("seed")?.to_owned(),
        base: p("base")?,
        out: p("out")?,
        pool: p("pool")?,
        reserved_lo: v["reserved_buckets"][0].as_u64().context("reserved_buckets")? as u32,
        ranges: {
            let lo = v["reserved_buckets"][0].as_u64().context("reserved_buckets")? as u32;
            let mut r = [[(lo, 1000); 4]; 2];
            for (i, d) in NEW_EVAL_DOMAINS.iter().enumerate() {
                for (j, sp) in EVAL_SPLITS.iter().enumerate() {
                    if let Some(a) = v["split_buckets"][d][sp].as_array() {
                        let (x, y) = (a[0].as_u64().context("range")? as u32, a[1].as_u64().context("range")? as u32);
                        ensure!(x < y && y <= 1000, "bad range for {d} {sp}");
                        r[i][j] = (x, y);
                    }
                }
                for a in r[i] {
                    for b in r[i] {
                        ensure!(a == b || a.1 <= b.0 || b.1 <= a.0, "ranges of {d} must be equal or disjoint");
                    }
                }
            }
            r
        },
        scans: [p("fineweb_scan")?, p("global_voices_scan")?],
        plos_scan: p("plos_scan")?,
        targets,
        caps,
        oversample: over,
        quota,
        train_cap,
        train_total_cap: v["train_total_cap"].as_u64().context("train_total_cap")?,
        raw: v,
    })
}

/// Verify every `SHA256SUMS` line of the base directory.
fn verify_base(base: &Path) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for line in std::fs::read_to_string(base.join("SHA256SUMS"))?.lines() {
        let Some((h, p)) = line.split_once("  ") else { continue };
        let rel = p.trim_start_matches("./").to_owned();
        let (got, _) = io::file_sha(&base.join(&rel))?;
        ensure!(got == h, "base file {rel} differs from SHA256SUMS");
        m.insert(rel, h.to_owned());
    }
    ensure!(!m.is_empty(), "empty base SHA256SUMS");
    Ok(m)
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    ensure!(!to.exists(), "{} exists", to.display());
    std::fs::copy(from, to)?;
    Ok(())
}

/// Append-mode writer that starts with an exact copy of a base file.
struct Appender {
    w: std::io::BufWriter<std::fs::File>,
    rows: u64,
    base_rows: u64,
}

impl Appender {
    fn start(base: &Path, out: &Path) -> Result<Self> {
        copy_file(base, out)?;
        let base_rows = std::io::BufReader::new(std::fs::File::open(out)?).lines().count() as u64;
        let f = std::fs::OpenOptions::new().append(true).open(out)?;
        Ok(Appender { w: std::io::BufWriter::with_capacity(1 << 20, f), rows: base_rows, base_rows })
    }
    fn write(&mut self, v: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.w, v)?;
        self.w.write_all(b"\n")?;
        self.rows += 1;
        Ok(())
    }
    fn finish(mut self) -> Result<(u64, u64)> {
        self.w.flush()?;
        Ok((self.base_rows, self.rows))
    }
}

struct NewDoc {
    id: String,
    domain: usize,
    family: String,
    component: String,
    contributor: Option<String>,
    writer: Option<String>,
    words: u64,
    hashes: [String; 3],
    rank: u64,
    held: Vec<String>,
}

fn scan_rows(dir: &Path) -> Result<Vec<Value>> {
    let rep: Value = serde_json::from_slice(&std::fs::read(dir.join("report.json")).with_context(|| format!("{}", dir.display()))?)?;
    ensure!(rep["complete"] == true && rep["metadata_only"].is_object() && rep["candidates"] == 0, "{} is not a complete metadata-only scan", dir.display());
    let (sha, _) = io::file_sha(&dir.join("reserved-metadata.jsonl"))?;
    ensure!(rep["metadata_only"]["sha256"] == sha.as_str(), "{} metadata changed since its report", dir.display());
    io::jsonl_rows(&dir.join("reserved-metadata.jsonl"))?.collect()
}

pub fn run(config: &Path) -> Result<Value> {
    let c = load(config)?;
    let started = io::now_utc();
    let base_sums = verify_base(&c.base)?;
    io::fresh_dir(&c.out)?;
    let base_cfg: Value = serde_json::from_slice(&std::fs::read(c.base.join("config.json"))?)?;
    ensure!(base_cfg["seed"] == c.seed.as_str(), "seed differs from the base split");
    let mut notes: Vec<String> = Vec::new();

    // ---------------------------------------------------------------- base keys
    let mut base_ids: HashSet<String> = HashSet::new();
    let mut base_train_side = [0u64; 3];
    for (i, sp) in ["train", "train-mining", "held"].iter().enumerate() {
        for r in io::jsonl_rows(&c.base.join(sp).join("manifest.jsonl"))? {
            let r = r?;
            base_ids.insert(s(&r, "id").to_owned());
            base_train_side[i] += 1;
        }
    }
    let mut eval_families: HashSet<String> = HashSet::new();
    let mut plos_reserved_author_sha: HashSet<String> = HashSet::new();
    let mut plos_unassigned: HashSet<String> = HashSet::new();
    for sp in EVAL_SPLITS {
        let dir = c.base.join(sp.replace('_', "-"));
        for l in std::fs::read_to_string(dir.join("families.txt"))?.lines() {
            eval_families.insert(l.trim().to_owned());
        }
        for g in io::jsonl_rows(&dir.join("groups.jsonl"))? {
            let g = g?;
            if g["domain"] == "plos" {
                for h in g["contributor_key_sha256"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    plos_reserved_author_sha.insert(h.to_owned());
                }
            }
        }
    }
    for g in io::jsonl_rows(&c.base.join("reserved-unassigned").join("groups.jsonl"))? {
        let g = g?;
        for f in g["family_keys"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            eval_families.insert(f.to_owned());
            if g["domain"] == "plos" {
                plos_unassigned.insert(f.to_owned());
            }
        }
    }
    let mut plos_scan_rows = 0u64;
    for r in io::jsonl_rows(&c.plos_scan.join("reserved-metadata.jsonl"))? {
        let r = r?;
        plos_scan_rows += 1;
        if plos_unassigned.contains(s(&r, "family_key")) {
            for a in r["authors"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(k) = splits::author_key(a) {
                    plos_reserved_author_sha.insert(text::sha_hex(k.as_bytes()));
                }
            }
        }
    }

    // ---------------------------------------------------------------- new Train-side rows
    let mut docs: Vec<NewDoc> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut train_contributors: [HashSet<String>; 2] = Default::default();
    let mut admitted = [0u64; 4];
    let mut pool_rows = 0u64;
    for r in io::jsonl_rows(&c.pool)? {
        let r = r?;
        pool_rows += 1;
        ensure!(r["pool_status"] == "admitted", "pool row not admitted");
        let d = new_domain(&r)?;
        let id = s(&r, "id").to_owned();
        ensure!(!base_ids.contains(&id), "pool id {id} already assigned in the base split");
        ensure!(ids.insert(id.clone()), "duplicate pool id {id}");
        let family = s(&r, "source_group").to_owned();
        ensure!(!eval_families.contains(&family), "pool family {family} is reserved for evaluation in the base split");
        let ctr = contributor(d, &r);
        let bucket_key = match d {
            0 | 1 => ctr.clone().context("FineWeb and Global Voices rows carry a contributor key")?,
            _ => family.clone(),
        };
        let train_hi = if d < 2 { c.ranges[d].iter().map(|r| r.0).min().unwrap_or(c.reserved_lo) } else { c.reserved_lo };
        ensure!(crate::adapter::bucket(&bucket_key) < train_hi, "pool row {id} lies in a reserved bucket range");
        if d < 2 {
            train_contributors[d].insert(bucket_key.clone());
        }
        let mut held = Vec::new();
        if d == 3 {
            let blocked = r["provenance"]["authors"].as_array().into_iter().flatten().filter_map(Value::as_str)
                .filter_map(splits::author_key).any(|k| plos_reserved_author_sha.contains(&text::sha_hex(k.as_bytes())));
            if blocked {
                held.push("plos_author_of_reserved_evaluation_group".to_owned());
            }
        }
        admitted[d] += 1;
        docs.push(NewDoc {
            rank: rank(&c.seed, "train_document_v3", &id),
            component: r["pool_admission"]["split_component"].as_str().unwrap_or(&family).to_owned(),
            writer: r["writer_key"].as_str().map(str::to_owned),
            contributor: ctr,
            words: r["word_count"].as_u64().unwrap_or(0),
            hashes: [s(&r, "text_sha256").to_owned(), s(&r, "normalized_text_sha256").to_owned(), s(&r, "folded_text_sha256").to_owned()],
            id,
            domain: d,
            family,
            held,
        });
    }

    // ---------------------------------------------------------------- new evaluation domains
    let mut groups: [Vec<Group>; 2] = Default::default();
    let mut yields = serde_json::Map::new();
    let mut universe = serde_json::Map::new();
    for (i, dom) in NEW_EVAL_DOMAINS.iter().enumerate() {
        let rows = scan_rows(&c.scans[i])?;
        let (mut eligible_train, mut reserved_seen, mut reserved_eligible) = (0u64, 0u64, 0u64);
        let mut blocked: BTreeMap<&str, u64> = BTreeMap::new();
        let mut by: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let train_hi = c.ranges[i].iter().map(|r| r.0).min().unwrap_or(c.reserved_lo);
        for r in rows {
            let b = r["bucket"].as_u64().context("bucket")? as u32;
            ensure!(r["bucket_basis"] == "group_key", "{dom} scan rows must be bucketed by contributor");
            let eligible = r["eligible_metadata"] == true;
            if b < train_hi {
                if eligible {
                    eligible_train += 1;
                }
                continue;
            }
            if !c.ranges[i].iter().any(|&(x, y)| x <= b && b < y) {
                *blocked.entry("bucket_between_train_side_and_reserved_ranges_unused").or_default() += 1;
                continue;
            }
            reserved_seen += 1;
            if !eligible {
                *blocked.entry("metadata_not_eligible").or_default() += 1;
                continue;
            }
            reserved_eligible += 1;
            let g = s(&r, "group_key").to_owned();
            if train_contributors[i].contains(&g) {
                bail!("{dom} contributor {g} has Train-side text but a reserved bucket");
            }
            by.entry(g).or_default().push(r);
        }
        let rate = if eligible_train == 0 { 0.0 } else { admitted[i] as f64 / eligible_train as f64 };
        yields.insert((*dom).to_owned(), json!({"eligible_train_range_units": eligible_train, "admitted_train_side_docs": admitted[i], "docs_per_unit": rate}));
        for (g, units) in by {
            let mut fams = BTreeSet::new();
            let mut us = Vec::new();
            for r in units {
                let f = s(&r, "family_key").to_owned();
                if !fams.insert(f.clone()) {
                    *blocked.entry("unit_repeated_in_scan_counted_once").or_default() += 1;
                    continue;
                }
                let mut meta = r.clone();
                if let Some(m) = meta.as_object_mut() {
                    for k in ["schema", "prose_opened", "family_key", "group_key", "bucket", "bucket_basis", "eligible_metadata"] {
                        m.remove(k);
                    }
                }
                meta["acquisition"] = json!(match i {
                    0 => "fineweb adapter on the bound parquet shards with --buckets over the reserved range and --families",
                    _ => "global-voices adapter on the bound REST pages with --buckets over the reserved range and --families",
                });
                us.push(Unit { family_key: f, expected_docs: rate, meta });
            }
            groups[i].push(Group { rank: rank(&c.seed, dom, &g), contributors: BTreeSet::from([g.clone()]), key: g, units: us, train_side_docs: 0 });
        }
        universe.insert((*dom).to_owned(), json!({"reserved_units_seen": reserved_seen, "reserved_units_eligible": reserved_eligible, "blocked": blocked, "eligible_groups": groups[i].len()}));
    }
    let mut assigned: [[Vec<Group>; 4]; 2] = Default::default();
    let mut spare: [Vec<Group>; 2] = Default::default();
    for i in 0..2 {
        let mut gs = std::mem::take(&mut groups[i]);
        gs.sort_by(|a, b| (a.rank, &a.key).cmp(&(b.rank, &b.key)));
        let mut distinct: Vec<(u32, u32)> = c.ranges[i].to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        for rg in distinct {
            let in_range: Vec<Group> = gs.iter().filter(|g| (rg.0..rg.1).contains(&crate::adapter::bucket(&g.key))).cloned().collect();
            let mut t = c.targets[i];
            for sx in 0..4 {
                if c.ranges[i][sx] != rg {
                    t[sx] = 0.0;
                }
            }
            let users: Vec<usize> = (0..4).filter(|&sx| c.ranges[i][sx] == rg).collect();
            let cap = c.caps[i][users[0]];
            ensure!(users.iter().all(|&sx| c.caps[i][sx] == cap), "{}: splits sharing a bucket range must share a group cap", NEW_EVAL_DOMAINS[i]);
            let exp: Vec<f64> = in_range.iter().map(|g| g.expected(cap)).collect();
            for (g, a) in in_range.into_iter().zip(allocate(&exp, &t, c.oversample[i])) {
                match a {
                    Some(sx) => assigned[i][sx].push(g),
                    None => spare[i].push(g),
                }
            }
        }
    }
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut cross = 0u64;
    for i in 0..2 {
        for sx in 0..4 {
            for g in &assigned[i][sx] {
                for k in g.contributors.iter().cloned().chain(g.units.iter().map(|u| u.family_key.clone())) {
                    if owner.insert(k, sx).is_some_and(|p| p != sx) {
                        cross += 1;
                    }
                }
            }
        }
    }
    ensure!(cross == 0, "new evaluation splits share {cross} contributor or family keys");

    // ---------------------------------------------------------------- Train / Train-mining
    let mut comp: BTreeMap<(usize, String), Vec<u32>> = BTreeMap::new();
    for (i, d) in docs.iter().enumerate() {
        if d.held.is_empty() {
            comp.entry((d.domain, d.component.clone())).or_default().push(i as u32);
        }
    }
    let mut comps: Vec<((usize, String), Vec<u32>)> = comp.into_iter().collect();
    comps.sort_by_key(|((d, k), _)| (*d, rank(&c.seed, "train_component_v3", k), k.clone()));
    let quota_total: u64 = c.quota.iter().sum();
    ensure!(base_train_side[0] + quota_total <= c.train_total_cap, "base Train {} plus new quotas {quota_total} exceed the Train cap {}", base_train_side[0], c.train_total_cap);
    let mut in_train: HashSet<u32> = HashSet::new();
    let mut train_n = [0u64; 4];
    let mut per: HashMap<String, u64> = HashMap::new();
    let mut cap_skipped = [0u64; 4];
    for ((d, _), members) in &comps {
        if train_n[*d] + members.len() as u64 > c.quota[*d] {
            continue;
        }
        if let Some(cap) = c.train_cap[*d] {
            let mut need: HashMap<String, u64> = HashMap::new();
            for m in members {
                if let Some(k) = &docs[*m as usize].contributor {
                    *need.entry(k.clone()).or_default() += 1;
                }
            }
            if need.iter().any(|(k, n)| per.get(k).copied().unwrap_or(0) + n > cap) {
                cap_skipped[*d] += members.len() as u64;
                continue;
            }
            for (k, n) in need {
                *per.entry(k).or_default() += n;
            }
        }
        train_n[*d] += members.len() as u64;
        in_train.extend(members.iter().copied());
    }
    let mut concentration = serde_json::Map::new();
    for d in 0..4 {
        let mut by: HashMap<&str, u64> = HashMap::new();
        for i in &in_train {
            let x = &docs[*i as usize];
            if x.domain == d {
                if let Some(k) = &x.contributor {
                    *by.entry(k).or_default() += 1;
                }
            }
        }
        let n: u64 = by.values().sum();
        if n > 0 {
            let simpson: f64 = by.values().map(|&x| (x as f64 / n as f64).powi(2)).sum();
            concentration.insert(NEW_TRAIN_DOMAINS[d].to_owned(), json!({"documents_with_key": n, "distinct_keys": by.len(), "max_docs_per_key": by.values().max(),
                "effective_number_inverse_simpson": (10.0 / simpson).round() / 10.0, "cap": c.train_cap[d], "documents_skipped_by_cap": cap_skipped[d]}));
        }
    }

    // ---------------------------------------------------------------- outputs
    let out = &c.out;
    // Base evaluation and spare files, unchanged.
    let mut copied = Vec::new();
    for rel in base_sums.keys() {
        let top = rel.split('/').next().unwrap_or("");
        if ["test", "cal", "dev-select", "dev-iter", "reserved-unassigned"].contains(&top) {
            copy_file(&c.base.join(rel), &out.join(rel))?;
            copied.push(rel.clone());
        }
    }
    for rel in ["config.json", "summary.json", "counts.tsv", "ACQUISITION.json", "overlap-receipts.jsonl", "prior-index-currency.json", "attempts.jsonl", "SHA256SUMS"] {
        if c.base.join(rel).exists() {
            copy_file(&c.base.join(rel), &out.join("base-v2").join(rel))?;
        }
    }
    // New-domain reservations.
    let mut eval_summary = serde_json::Map::new();
    let mut shortfalls = Vec::new();
    for (sx, sname) in EVAL_SPLITS.iter().enumerate() {
        let dir = out.join("new-domains").join(sname.replace('_', "-"));
        io::fresh_dir(&dir)?;
        let mut uw = JsonlWriter::create(&dir.join("manifest.jsonl"))?;
        let mut gw = JsonlWriter::create(&dir.join("groups.jsonl"))?;
        let mut fams = String::new();
        let mut per_dom = serde_json::Map::new();
        let status = match *sname {
            "dev_iter" => "not_acquired_acquirable_now_exposed_by_design",
            "test" => "sealed_acquire_only_after_protocol_seal",
            _ => "sealed_not_acquired_until_protocol_step",
        };
        for i in 0..2 {
            let dom = NEW_EVAL_DOMAINS[i];
            let (target, cap) = (c.targets[i][sx], c.caps[i][sx]);
            let (mut exp_sum, mut nunits) = (0.0, 0u64);
            for (order, g) in assigned[i][sx].iter().enumerate() {
                let e = g.expected(cap);
                exp_sum += e;
                nunits += g.units.len() as u64;
                gw.write(&json!({"schema": GROUP_SCHEMA, "split": sname, "domain": dom, "group_key": g.key, "group_rank": format!("{:016x}", g.rank),
                    "reserved_bucket_range": [c.ranges[i][sx].0, c.ranges[i][sx].1],
                    "order_in_split_domain": order, "units": g.units.len(), "expected_docs_capped": e, "max_docs_per_group": cap,
                    "contributor_key_sha256": g.contributors.iter().map(|k| text::sha_hex(k.as_bytes())).collect::<Vec<_>>(),
                    "federal_register_train_side_docs_held": null}))?;
                let mut us: Vec<&Unit> = g.units.iter().collect();
                us.sort_by_key(|u| (rank(&c.seed, "unit", &u.family_key), u.family_key.clone()));
                for (uo, u) in us.into_iter().enumerate() {
                    uw.write(&json!({"schema": RESERVATION_SCHEMA, "split": sname, "domain": dom, "group_key": g.key, "group_order": order,
                        "unit_order_in_group": uo, "family_key": u.family_key, "bucket": crate::adapter::bucket(&g.key), "bucket_basis": "group_key",
                        "expected_docs": u.expected_docs, "source_unit": u.meta, "prose_status": status, "text_sha256": null,
                        "text_hashes_note": "excerpt text hashes are recorded at acquisition; source-unit hashes above bind the reserved inputs"}))?;
                    fams.push_str(&u.family_key);
                    fams.push('\n');
                }
            }
            if exp_sum < target * c.oversample[i] * 0.999 {
                shortfalls.push(json!({"split": sname, "domain": dom, "target_human_docs": target, "expected_capped_capacity": exp_sum,
                    "issue": "expected capacity below target x oversample (reserved groups exhausted)"}));
            }
            per_dom.insert(dom.to_owned(), json!({"groups": assigned[i][sx].len(), "source_units": nunits, "target_human_docs": target,
                "reserved_bucket_range": [c.ranges[i][sx].0, c.ranges[i][sx].1],
                "expected_capped_capacity": exp_sum, "capacity_over_target": exp_sum / target.max(1.0), "max_docs_per_group": cap, "reserve_oversample": c.oversample[i]}));
        }
        let (usha, urows) = uw.finish()?;
        let (gsha, grows) = gw.finish()?;
        std::fs::write(dir.join("families.txt"), &fams)?;
        let seal = json!({"split": sname, "prose_status": status, "prose_opened": false, "text_read_or_printed": false,
            "domains": NEW_EVAL_DOMAINS,
            "rule": match *sname {
                "test" => "Test prose is acquired only after the protocol seal; manifest is metadata only",
                "dev_iter" => "Dev-iter is exposed by design and may be acquired for iterative decisions; nothing is acquired by this reservation",
                "dev_select" => "Dev-select prose stays sealed until finalists are bound; opened once to choose among at most four finalists",
                _ => "CAL prose stays sealed until the protocol calibration step",
            },
            "acquisition_rule": "process groups in group_order; within a group take units in unit_order_in_group; keep at most max_docs_per_group passing documents per group; stop a domain at its target; record every skip; no backfill (declared before any prose is opened): a domain that exhausts its reserved groups is evaluated at its realised size; unreached groups stay reserved and unused (never Train, never mined, never mirror topics)",
            "base_domains": "the five base domains of this split keep their splits-v2 reservation unchanged in ../../<split>/",
            "manifest_sha256": usha, "manifest_rows": urows, "groups_sha256": gsha, "groups_rows": grows});
        io::write_json(&dir.join("SEALED.json"), &seal)?;
        eval_summary.insert((*sname).to_owned(), json!({"by_domain": per_dom, "manifest_sha256": usha, "groups_sha256": gsha, "prose_status": status}));
    }
    {
        let dir = out.join("new-domains").join("reserved-unassigned");
        io::fresh_dir(&dir)?;
        let mut w = JsonlWriter::create(&dir.join("groups.jsonl"))?;
        let mut per_dom = serde_json::Map::new();
        for i in 0..2 {
            for g in &spare[i] {
                w.write(&json!({"schema": GROUP_SCHEMA, "split": "reserved_unassigned", "domain": NEW_EVAL_DOMAINS[i], "group_key": g.key,
                    "group_rank": format!("{:016x}", g.rank), "units": g.units.len(), "family_keys": g.units.iter().map(|u| &u.family_key).collect::<Vec<_>>()}))?;
            }
            per_dom.insert(NEW_EVAL_DOMAINS[i].to_owned(), json!({"groups": spare[i].len(), "source_units": spare[i].iter().map(|g| g.units.len()).sum::<usize>()}));
        }
        let (sha, rows) = w.finish()?;
        eval_summary.insert("reserved_unassigned".into(), json!({"by_domain": per_dom, "groups_sha256": sha, "rows": rows}));
    }
    // Train-side manifests: base lines, then new rows.
    let split_of = |i: usize| -> &'static str {
        if !docs[i].held.is_empty() { "held" } else if in_train.contains(&(i as u32)) { "train" } else { "train_mining" }
    };
    let mut ts = serde_json::Map::new();
    for name in ["train", "train_mining", "held"] {
        let dn = name.replace('_', "-");
        let mut w = Appender::start(&c.base.join(&dn).join("manifest.jsonl"), &out.join(&dn).join("manifest.jsonl"))?;
        let mut order: Vec<usize> = (0..docs.len()).filter(|&i| split_of(i) == name).collect();
        order.sort_by_key(|&i| (docs[i].domain, docs[i].rank, i));
        let mut by: BTreeMap<&str, (u64, HashSet<&str>, HashSet<&str>, u64)> = BTreeMap::new();
        for i in order {
            let d = &docs[i];
            let e = by.entry(NEW_TRAIN_DOMAINS[d.domain]).or_default();
            e.0 += 1;
            e.1.insert(&d.family);
            if let Some(k) = &d.contributor {
                e.2.insert(k);
            }
            e.3 += d.words;
            w.write(&json!({"schema": DOCUMENT_SCHEMA, "split": name, "id": d.id, "domain": NEW_TRAIN_DOMAINS[d.domain], "source_group": d.family,
                "split_component": d.component, "writer_key": d.writer, "word_count": d.words, "text_sha256": d.hashes[0],
                "normalized_text_sha256": d.hashes[1], "folded_text_sha256": d.hashes[2], "rank": format!("{:016x}", d.rank),
                "held_reasons": if d.held.is_empty() { json!(null) } else { json!(d.held) }, "splits_version": "v3"}))?;
        }
        let (base_rows, rows) = w.finish()?;
        ts.insert(name.to_owned(), json!({"base_rows": base_rows, "rows": rows, "new_rows": rows - base_rows,
            "new_by_domain": by.iter().map(|(k, v)| ((*k).to_owned(), json!({"documents": v.0, "source_families": v.1.len(), "contributor_keys": v.2.len(), "mean_words": v.3 as f64 / v.0.max(1) as f64}))).collect::<serde_json::Map<_, _>>()}));
    }
    // Text rows: base lines, then new rows from the pool.
    {
        let pos: HashMap<&str, usize> = docs.iter().enumerate().map(|(i, d)| (d.id.as_str(), i)).collect();
        let mut wt = Appender::start(&c.base.join("train").join("documents.jsonl"), &out.join("train").join("documents.jsonl"))?;
        let mut wm = Appender::start(&c.base.join("train-mining").join("documents.jsonl"), &out.join("train-mining").join("documents.jsonl"))?;
        let se_basis = c.raw["stack_exchange_admission"].clone();
        for r in io::jsonl_rows(&c.pool)? {
            let mut r = r?;
            let i = pos[s(&r, "id")];
            let sp = split_of(i);
            if sp == "held" {
                continue;
            }
            r["frozen_split"] = json!(sp);
            r["split_assigned"] = json!(true);
            r["split_assignment"] = json!({"schema": DOCUMENT_SCHEMA, "split": sp, "seed": c.seed, "splits_version": "v3",
                "rule": "read only splits-v3 train/documents.jsonl or train-mining/documents.jsonl; pool files include held documents and are not training inputs"});
            if docs[i].domain == 2 {
                r["rights"]["admission_decision"] = se_basis.clone();
            }
            if sp == "train" { wt.write(&r)? } else { wm.write(&r)? }
        }
        let (tb, tr) = wt.finish()?;
        let (mb, mr) = wm.finish()?;
        ensure!(tr - tb == ts["train"]["new_rows"].as_u64().unwrap_or(u64::MAX), "train text rows differ from manifest");
        ensure!(mr - mb == ts["train_mining"]["new_rows"].as_u64().unwrap_or(u64::MAX), "train-mining text rows differ from manifest");
        ensure!(tb == base_train_side[0] && mb == base_train_side[1], "base text rows differ from base manifests");
    }
    let total_train = ts["train"]["rows"].as_u64().unwrap_or(0);
    ensure!(total_train <= c.train_total_cap, "Train has {total_train} rows, above the cap");
    if base_train_side[0] + quota_total > total_train {
        notes.push(format!("Train quotas not all reached: {} of {} new Train documents placed", total_train - base_train_side[0], quota_total));
    }
    let held_reasons: BTreeMap<&str, u64> = docs.iter().flat_map(|d| d.held.iter()).fold(BTreeMap::new(), |mut m, r| {
        *m.entry(r.as_str()).or_default() += 1;
        m
    });
    ts.insert("new_contributor_concentration_train".into(), json!(concentration));
    ts.insert("new_held_reasons".into(), json!(held_reasons));
    ts.insert("new_train_quota".into(), json!(NEW_TRAIN_DOMAINS.iter().zip(c.quota).map(|(d, q)| ((*d).to_owned(), json!(q))).collect::<serde_json::Map<_, _>>()));
    let summary = json!({
        "schema": SUMMARY_SCHEMA,
        "started_utc": started,
        "finished_utc": io::now_utc(),
        "config": c.raw,
        "base": {"dir": c.base, "files_verified": base_sums.len(), "base_eval_files_copied_unchanged": copied, "train_side_rows": {"train": base_train_side[0], "train_mining": base_train_side[1], "held": base_train_side[2]}},
        "pool": {"path": c.pool, "rows": pool_rows, "admitted_by_domain": NEW_TRAIN_DOMAINS.iter().zip(admitted).map(|(d, n)| ((*d).to_owned(), json!(n))).collect::<serde_json::Map<_, _>>()},
        "new_domain_evaluation": eval_summary,
        "yields": yields,
        "evaluation_universe": universe,
        "train_side": ts,
        "checks": {
            "new_ids_disjoint_from_base": true,
            "new_families_disjoint_from_base_evaluation_and_spare_families": true,
            "new_train_rows_outside_reserved_buckets": true,
            "new_evaluation_cross_split_conflicts": cross,
            "plos_reserved_author_keys": plos_reserved_author_sha.len(),
            "plos_scan_rows_read": plos_scan_rows,
        },
        "shortfalls": shortfalls,
        "notes": notes,
        "detector_calls": 0,
        "model_calls": 0,
        "evaluation_prose_opened": false,
    });
    io::write_json(&out.join("summary.json"), &summary)?;
    io::write_json(&out.join("config.json"), &c.raw)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::bucket;

    fn jsonl(p: &Path, rows: &[Value]) -> String {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut w = JsonlWriter::create(p).unwrap();
        for r in rows {
            w.write(r).unwrap();
        }
        w.finish().unwrap().0
    }

    fn key_in(prefix: &str, lo: u32, hi: u32, skip: usize) -> String {
        (0..).map(|i| format!("{prefix}{i}")).filter(|k| (lo..hi).contains(&bucket(k))).nth(skip).unwrap()
    }

    fn row(id: &str, coll: &str, fam: &str, writer: Option<&str>, extra: Value) -> Value {
        let t = format!("text of {id}");
        let mut r = json!({"id": id, "collection": coll, "source_group": fam, "writer_key": writer, "pool_status": "admitted", "word_count": 200,
            "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "normalized_text_sha256": "n", "folded_text_sha256": "f",
            "pool_admission": {"split_component": fam}, "rights": {"license": "x"}, "provenance": {}});
        if let (Some(m), Some(e)) = (r.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                if k == "rights" || k == "provenance" {
                    for (k2, v2) in v.as_object().unwrap() {
                        m.get_mut(k).unwrap()[k2] = v2.clone();
                    }
                } else {
                    m.insert(k.clone(), v.clone());
                }
            }
        }
        r
    }

    fn scan(dir: &Path, rows: &[Value]) {
        let sha = jsonl(&dir.join("reserved-metadata.jsonl"), rows);
        io::write_json(&dir.join("report.json"), &json!({"complete": true, "candidates": 0, "metadata_only": {"sha256": sha}})).unwrap();
    }

    #[test]
    fn extension_keeps_base_bytes_reserves_new_domains_and_holds_reserved_plos_authors() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base");
        let mut sums = String::new();
        let mut add = |rel: &str, sha: String| sums.push_str(&format!("{sha}  ./{rel}\n"));
        add("train/manifest.jsonl", jsonl(&base.join("train/manifest.jsonl"), &[json!({"id": "b1", "split": "train"})]));
        add("train/documents.jsonl", jsonl(&base.join("train/documents.jsonl"), &[json!({"id": "b1", "text": "base text"})]));
        add("train-mining/manifest.jsonl", jsonl(&base.join("train-mining/manifest.jsonl"), &[json!({"id": "b2"})]));
        add("train-mining/documents.jsonl", jsonl(&base.join("train-mining/documents.jsonl"), &[json!({"id": "b2", "text": "mined"})]));
        add("held/manifest.jsonl", jsonl(&base.join("held/manifest.jsonl"), &[json!({"id": "b3"})]));
        let reserved_author = splits::author_key("Jane Roe").unwrap();
        for sp in ["test", "cal", "dev-select", "dev-iter"] {
            let g = if sp == "cal" { json!({"domain": "plos", "group_key": "doi:e1", "contributor_key_sha256": [text::sha_hex(reserved_author.as_bytes())]}) } else { json!({"domain": "wikipedia", "group_key": "w"}) };
            add(&format!("{sp}/groups.jsonl"), jsonl(&base.join(sp).join("groups.jsonl"), &[g]));
            std::fs::write(base.join(sp).join("families.txt"), format!("fam:{sp}\n")).unwrap();
            add(&format!("{sp}/families.txt"), io::file_sha(&base.join(sp).join("families.txt")).unwrap().0);
        }
        add("reserved-unassigned/groups.jsonl", jsonl(&base.join("reserved-unassigned/groups.jsonl"), &[json!({"domain": "plos", "family_keys": ["doi:u1"]})]));
        let cfg_sha = io::write_json(&base.join("config.json"), &json!({"seed": "s"})).unwrap();
        add("config.json", cfg_sha);
        std::fs::write(base.join("SHA256SUMS"), &sums).unwrap();
        let plos_scan = d.path().join("plos-scan");
        scan(&plos_scan, &[json!({"family_key": "doi:u1", "authors": ["Max Spare"]})]);

        // Scans: Train-range and reserved contributors.
        let site_t = key_in("fineweb_site:t", 0, 900, 0);
        let gv_t = key_in("global_voices_author:", 0, 700, 0);
        let mut fw_rows = vec![json!({"family_key": "fineweb_page:t1", "group_key": site_t, "bucket": bucket(&site_t), "bucket_basis": "group_key", "eligible_metadata": true}),
                               json!({"family_key": "fineweb_page:t2", "group_key": site_t, "bucket": bucket(&site_t), "bucket_basis": "group_key", "eligible_metadata": true})];
        let mut gv_rows = vec![json!({"family_key": "global_voices_post:1", "group_key": gv_t, "bucket": bucket(&gv_t), "bucket_basis": "group_key", "eligible_metadata": true})];
        for i in 0..40 {
            let s = key_in("fineweb_site:r", 900, 1000, i);
            fw_rows.push(json!({"family_key": format!("fineweb_page:r{i}"), "group_key": s, "bucket": bucket(&s), "bucket_basis": "group_key", "eligible_metadata": true, "url": "u"}));
            for (lo, hi, tag) in [(900, 1000, "r"), (700, 900, "m")] {
                let a = key_in("global_voices_author:", lo, hi, i);
                for j in 0..3 {
                    gv_rows.push(json!({"family_key": format!("global_voices_post:{tag}{i}_{j}"), "group_key": a, "bucket": bucket(&a), "bucket_basis": "group_key", "eligible_metadata": j != 2}));
                }
            }
        }
        let (fw_scan, gv_scan) = (d.path().join("fw-scan"), d.path().join("gv-scan"));
        scan(&fw_scan, &fw_rows);
        scan(&gv_scan, &gv_rows);
        let se_fam = key_in("stackexchange:x:question:", 0, 900, 0);
        let pool = d.path().join("pool.jsonl");
        jsonl(&pool, &[
            row("f1", "fineweb_cc_historical", "fineweb_page:t1", Some(&site_t), json!({})),
            row("g1", "global_voices_historical", "global_voices_post:1", Some(&gv_t), json!({})),
            row("s1", "stackexchange_historical", &se_fam, Some("stackexchange:x:user:1"), json!({})),
            row("p1", "plos_historical", &key_in("doi:ok", 0, 900, 0), None, json!({"rights": {"licence_basis": "prose_statement_without_link"}, "provenance": {"authors": ["A Writer"]}})),
            row("p2", "plos_historical", &key_in("doi:cal", 0, 900, 0), None, json!({"rights": {"licence_basis": "prose_statement_without_link"}, "provenance": {"authors": ["Jane Roe"]}})),
            row("p3", "plos_historical", &key_in("doi:sp", 0, 900, 0), None, json!({"rights": {"licence_basis": "prose_statement_without_link"}, "provenance": {"authors": ["Max Spare"]}})),
        ]);
        let out = d.path().join("out");
        let cfg = d.path().join("cfg.json");
        let t = json!({"test": 4, "cal": 3, "dev_select": 3, "dev_iter": 2});
        io::write_json(&cfg, &json!({"seed": "s", "base": base, "out": out, "pool": pool, "reserved_buckets": [900, 1000],
            "fineweb_scan": fw_scan, "global_voices_scan": gv_scan, "plos_scan": plos_scan,
            "targets": {"fineweb": t, "global_voices": t}, "max_docs_per_group": {"fineweb": 1, "global_voices": {"test": 3, "cal": 2, "dev_select": 2, "dev_iter": 2}}, "oversample": {"fineweb": 1.3, "global_voices": 1.3},
            "train_quota": {"fineweb": 1, "global_voices": 0, "stack_exchange": 1, "plos_prose": 1}, "train_max_docs_per_contributor": {"fineweb": 1},
            "train_total_cap": 10, "stack_exchange_admission": "se basis",
            "split_buckets": {"global_voices": {"test": [900, 1000], "cal": [700, 900], "dev_select": [700, 900], "dev_iter": [700, 900]}}})).unwrap();
        let s = run(&cfg).unwrap();
        // Base bytes first, unchanged.
        let tr = std::fs::read_to_string(out.join("train/documents.jsonl")).unwrap();
        assert!(tr.starts_with(&std::fs::read_to_string(base.join("train/documents.jsonl")).unwrap()));
        assert_eq!(std::fs::read(out.join("cal/groups.jsonl")).unwrap(), std::fs::read(base.join("cal/groups.jsonl")).unwrap());
        // Reserved PLOS authors (CAL group and reserved-unassigned) are held.
        assert_eq!(s["train_side"]["new_held_reasons"]["plos_author_of_reserved_evaluation_group"], 2);
        assert_eq!(s["train_side"]["train"]["new_rows"], 3);
        assert_eq!(s["train_side"]["train_mining"]["new_rows"], 1);
        let rows: Vec<Value> = io::jsonl_rows(&out.join("train/documents.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert!(rows.iter().any(|r| r["id"] == "s1" && r["rights"]["admission_decision"] == "se basis" && r["frozen_split"] == "train"));
        // FineWeb yield: 1 admitted / 2 eligible Train-range pages; GV: 1 / 1.
        assert_eq!(s["yields"]["fineweb"]["docs_per_unit"], 0.5);
        assert_eq!(s["evaluation_universe"]["global_voices"]["blocked"]["metadata_not_eligible"], 80);
        // Global Voices Test draws on 900..1000 only (cap 3); the other splits on 700..900 only (cap 2).
        for sp in ["test", "cal", "dev-select", "dev-iter"] {
            for g in io::jsonl_rows(&out.join("new-domains").join(sp).join("groups.jsonl")).unwrap() {
                let g = g.unwrap();
                let b = bucket(g["group_key"].as_str().unwrap());
                if g["domain"] == "global_voices" {
                    assert_eq!(sp == "test", b >= 900, "{sp} {b}");
                    assert_eq!(g["max_docs_per_group"], if sp == "test" { 3.0 } else { 2.0 });
                    assert!(b >= 700);
                } else {
                    assert!(b >= 900);
                }
            }
        }
        // Capacity reaches target x 1.3 in every split (enough reserved groups).
        assert!(s["shortfalls"].as_array().unwrap().is_empty(), "{}", s["shortfalls"]);
        let seal: Value = serde_json::from_slice(&std::fs::read(out.join("new-domains/test/SEALED.json")).unwrap()).unwrap();
        assert_eq!(seal["manifest_sha256"], io::file_sha(&out.join("new-domains/test/manifest.jsonl")).unwrap().0.as_str());
        // A rerun into a fresh directory is byte-identical.
        let out2 = d.path().join("out2");
        let mut c2: Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
        c2["out"] = json!(out2);
        let cfg2 = d.path().join("cfg2.json");
        io::write_json(&cfg2, &c2).unwrap();
        run(&cfg2).unwrap();
        for f in ["train/documents.jsonl", "new-domains/cal/manifest.jsonl", "new-domains/reserved-unassigned/groups.jsonl", "held/manifest.jsonl"] {
            assert_eq!(std::fs::read(out.join(f)).unwrap(), std::fs::read(out2.join(f)).unwrap(), "{f}");
        }
    }
}
