//! Contamination and era measurements for the pilot pool.
//!
//! - `sample`: a stratified, keyed draw of admitted pilot documents by crawl
//!   year × cohort domain, with a nested Pangram subsample. It writes the
//!   request shards for the released A runtime and the inputs of the Pangram
//!   bulk collector (split `cc_pilot`).
//! - `report`: joins A predictions and Pangram observations to the sample and
//!   writes flag-rate tables with intervals, year trends, the A human-score
//!   drift and the Tier C human comparison.
//!
//! No network, model or detector call happens here. Detector outputs are
//! measurements only: nothing here labels, filters or removes a document.
use crate::store;
use anyhow::{Context, Result, ensure};
use grammar_a_pools::splits::rank;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

pub const SAMPLE_SCOPE: &str = "cc_contamination_sample_v1";
pub const SPLIT: &str = "cc_pilot";
pub const COLLECTION: &str = "cc_pilot_v1";
pub const EARLY: (u16, u16) = (2013, 2016);
pub const LATE: (u16, u16) = (2020, 2022);

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn crawl_year(crawl: &str) -> Result<u16> {
    let y = crawl.strip_prefix("CC-MAIN-").and_then(|s| s.get(..4)).with_context(|| format!("crawl id {crawl}"))?;
    Ok(y.parse()?)
}

/// The admitted-pool fields these measurements read.
#[derive(Deserialize)]
pub struct PoolRow {
    pub id: String,
    pub crawl: String,
    pub era: String,
    pub domain: String,
    pub group: String,
    pub host: String,
    pub url: String,
    pub warc_date: String,
    pub text: String,
    #[serde(default)]
    pub text_redacted: Option<String>,
    pub text_sha256: String,
}

impl PoolRow {
    /// The text a cohort would carry: the redacted form when contact details
    /// were redacted, else the exact excerpt.
    pub fn scored_text(&self) -> (&str, &'static str) {
        match &self.text_redacted {
            Some(t) => (t, "text_redacted"),
            None => (&self.text, "text"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SampleRow {
    pub id: String,
    pub year: u16,
    pub crawl: String,
    pub era: String,
    pub domain: String,
    pub group: String,
    pub host: String,
    pub url: String,
    pub warc_date: String,
    pub words: usize,
    pub text_variant: String,
    pub text: String,
    pub text_sha256: String,
    pub pool_text_sha256: String,
    pub rank: String,
    /// 1: first document of its group in the cell; 2: fill after the group cap.
    pub pass: u8,
    pub pangram: bool,
}

pub fn read_pool(path: &Path) -> Result<Vec<PoolRow>> {
    store::rows(path)
}

/// Plain JSONL, written atomically. Returns the SHA-256 of the bytes.
pub fn write_jsonl<T: Serialize>(path: &Path, rows: &[T]) -> Result<String> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    let mut h = Sha256::new();
    {
        let mut w = BufWriter::new(File::create(&tmp)?);
        for r in rows {
            let mut b = serde_json::to_vec(r)?;
            b.push(b'\n');
            h.update(&b);
            w.write_all(&b)?;
        }
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(hex::encode(h.finalize()))
}

pub fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let r = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut out = Vec::new();
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&l).with_context(|| format!("{} line {}", path.display(), i + 1))?);
    }
    Ok(out)
}

pub fn file_sha256(path: &Path) -> Result<String> {
    Ok(sha256_hex(&fs::read(path).with_context(|| format!("read {}", path.display()))?))
}

// ------------------------------------------------------------------ draw

#[derive(Debug, PartialEq)]
pub struct Pick {
    pub index: usize,
    pub pass: u8,
    pub pangram: bool,
}

/// Per cell, candidates in ascending (rank, index) order: first at most one
/// per group, then the remainder, up to `per_cell`. The first
/// `pangram_per_cell` picks of each cell form the nested Pangram subsample.
/// `cands` holds (cell, rank, group) per candidate.
pub fn draw(cands: &[(String, u64, String)], per_cell: usize, pangram_per_cell: usize) -> Vec<Pick> {
    let mut cells: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, c) in cands.iter().enumerate() {
        cells.entry(c.0.as_str()).or_default().push(i);
    }
    let mut out = Vec::new();
    for (_, mut ix) in cells {
        ix.sort_by_key(|&i| (cands[i].1, i));
        let mut groups = HashSet::new();
        let mut taken = HashSet::new();
        let mut picks = Vec::new();
        for &i in &ix {
            if picks.len() == per_cell {
                break;
            }
            if groups.insert(cands[i].2.as_str()) {
                taken.insert(i);
                picks.push((i, 1u8));
            }
        }
        for &i in &ix {
            if picks.len() == per_cell {
                break;
            }
            if !taken.contains(&i) {
                picks.push((i, 2u8));
            }
        }
        for (n, (i, pass)) in picks.into_iter().enumerate() {
            out.push(Pick { index: i, pass, pangram: n < pangram_per_cell });
        }
    }
    out
}

pub struct SampleArgs<'a> {
    pub pool: &'a Path,
    pub out: &'a Path,
    pub seed: &'a str,
    pub per_cell: usize,
    pub pangram_per_cell: usize,
    pub shards: usize,
}

pub fn sample(a: &SampleArgs) -> Result<Value> {
    ensure!(!a.out.join("SAMPLE.json").exists(), "{} already holds a sample", a.out.display());
    ensure!(a.shards > 0 && a.pangram_per_cell <= a.per_cell, "bad sizes");
    let pool = read_pool(a.pool)?;
    let mut cands = Vec::with_capacity(pool.len());
    let mut available: BTreeMap<String, usize> = BTreeMap::new();
    for r in &pool {
        let cell = format!("{}|{}", crawl_year(&r.crawl)?, r.domain);
        *available.entry(cell.clone()).or_default() += 1;
        cands.push((cell, rank(a.seed, SAMPLE_SCOPE, &r.id), r.group.clone()));
    }
    let picks = draw(&cands, a.per_cell, a.pangram_per_cell);
    let mut rows = Vec::with_capacity(picks.len());
    for p in &picks {
        let r = &pool[p.index];
        let (text, variant) = r.scored_text();
        ensure!(variant == "text_redacted" || sha256_hex(text.as_bytes()) == r.text_sha256, "{}: pool text hash", r.id);
        rows.push(SampleRow {
            id: r.id.clone(),
            year: crawl_year(&r.crawl)?,
            crawl: r.crawl.clone(),
            era: r.era.clone(),
            domain: r.domain.clone(),
            group: r.group.clone(),
            host: r.host.clone(),
            url: r.url.clone(),
            warc_date: r.warc_date.clone(),
            words: text.split_whitespace().count(),
            text_variant: variant.into(),
            text: text.to_owned(),
            text_sha256: sha256_hex(text.as_bytes()),
            pool_text_sha256: r.text_sha256.clone(),
            rank: format!("{:016x}", cands[p.index].1),
            pass: p.pass,
            pangram: p.pangram,
        });
    }
    fs::create_dir_all(a.out)?;
    let sample_sha = write_jsonl(&a.out.join("sample.jsonl"), &rows)?;
    // Request shards for the A runtime, balanced in rank order.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&x, &y| rows[x].rank.cmp(&rows[y].rank));
    let mut shards: Vec<Vec<Value>> = vec![Vec::new(); a.shards];
    for (n, &i) in order.iter().enumerate() {
        shards[n % a.shards].push(json!({"id": rows[i].id, "text": rows[i].text}));
    }
    let mut shard_sha = BTreeMap::new();
    for (s, v) in shards.iter().enumerate() {
        let p = a.out.join("requests").join(format!("shard-{s:02}.jsonl"));
        shard_sha.insert(format!("shard-{s:02}.jsonl"), json!({"rows": v.len(), "sha256": write_jsonl(&p, v)?}));
    }
    // Pangram collector inputs.
    let pg: Vec<&SampleRow> = rows.iter().filter(|r| r.pangram).collect();
    let pdir = a.out.join("pangram-input");
    let sub: Vec<Value> = pg.iter().map(|r| json!({"id": r.id, "text_sha256": r.text_sha256})).collect();
    let prow: Vec<Value> = pg
        .iter()
        .map(|r| json!({"id": r.id, "split": SPLIT, "collection": COLLECTION, "text": r.text, "text_sha256": r.text_sha256}))
        .collect();
    let sub_sha = write_jsonl(&pdir.join("subsample.jsonl"), &sub)?;
    let rows_sha = write_jsonl(&pdir.join("rows.jsonl"), &prow)?;
    store::write_json(&pdir.join("collections.json"), &json!({"collections": [COLLECTION]}))?;
    let units: u64 = pg.iter().map(|r| (r.words as u64).div_ceil(100).max(1)).sum();
    // Cell table.
    let mut cells: BTreeMap<String, Value> = BTreeMap::new();
    for (cell, n) in &available {
        let taken: Vec<&SampleRow> = rows.iter().filter(|r| format!("{}|{}", r.year, r.domain) == *cell).collect();
        cells.insert(
            cell.clone(),
            json!({"available": n, "taken": taken.len(), "pass2": taken.iter().filter(|r| r.pass == 2).count(),
                "groups": taken.iter().map(|r| r.group.as_str()).collect::<HashSet<_>>().len(),
                "pangram": taken.iter().filter(|r| r.pangram).count()}),
        );
    }
    let summary = json!({
        "schema": "slop_ninja_cc_human_contamination_sample_v1",
        "pool": a.pool, "pool_sha256": file_sha256(a.pool)?, "pool_rows": pool.len(),
        "seed": a.seed, "scope": SAMPLE_SCOPE,
        "rule": "per crawl-year x domain cell, ascending rank(seed, scope, id): at most one document per group first, then fill; the first pangram_per_cell picks of each cell form the nested Pangram subsample",
        "per_cell": a.per_cell, "pangram_per_cell": a.pangram_per_cell,
        "text_rule": "text_redacted when present (contact details redacted), else the exact excerpt",
        "selected": rows.len(), "pangram_selected": pg.len(), "pangram_planned_units": units,
        "text_redacted_selected": rows.iter().filter(|r| r.text_variant == "text_redacted").count(),
        "files": {"sample.jsonl": sample_sha, "requests": shard_sha,
            "pangram-input/subsample.jsonl": sub_sha, "pangram-input/rows.jsonl": rows_sha},
        "cells": cells,
    });
    store::write_json(&a.out.join("SAMPLE.json"), &summary)?;
    Ok(json!({"selected": rows.len(), "pangram_selected": pg.len(), "pangram_planned_units": units}))
}

// ------------------------------------------------------------------ statistics

const Z: f64 = 1.959963984540054;

/// Wilson score interval (95%).
pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let (k, n) = (k as f64, n as f64);
    let p = k / n;
    let d = 1.0 + Z * Z / n;
    let c = (p + Z * Z / (2.0 * n)) / d;
    let h = Z * ((p * (1.0 - p) / n + Z * Z / (4.0 * n * n)).sqrt()) / d;
    ((c - h).max(0.0), (c + h).min(1.0))
}

/// Newcombe hybrid-score interval for p1 - p2 (95%).
pub fn newcombe(k1: usize, n1: usize, k2: usize, n2: usize) -> (f64, f64, f64) {
    let (p1, p2) = (k1 as f64 / n1.max(1) as f64, k2 as f64 / n2.max(1) as f64);
    let (l1, u1) = wilson(k1, n1);
    let (l2, u2) = wilson(k2, n2);
    let d = p1 - p2;
    let lo = d - ((p1 - l1).powi(2) + (u2 - p2).powi(2)).sqrt();
    let hi = d + ((u1 - p1).powi(2) + (p2 - l2).powi(2)).sqrt();
    (d, lo, hi)
}

pub fn rate_json(k: usize, n: usize) -> Value {
    let (lo, hi) = wilson(k, n);
    json!({"n": n, "flagged": k, "rate": if n > 0 { k as f64 / n as f64 } else { f64::NAN }, "wilson95": [lo, hi]})
}

/// Mantel (1963) extension test for a linear trend of a binary outcome on an
/// ordered score, stratified. Input per stratum: (score, n, flagged) rows.
/// Returns (pooled within-stratum slope per score unit, its SE under H0, z).
pub fn mantel_trend(strata: &[Vec<(f64, usize, usize)>]) -> (f64, f64, f64) {
    let (mut t, mut v, mut s) = (0.0, 0.0, 0.0);
    for rows in strata {
        let nn: f64 = rows.iter().map(|r| r.1 as f64).sum();
        let xx: f64 = rows.iter().map(|r| r.2 as f64).sum();
        if nn < 2.0 {
            continue;
        }
        let sy: f64 = rows.iter().map(|r| r.0 * r.1 as f64).sum();
        let syy: f64 = rows.iter().map(|r| r.0 * r.0 * r.1 as f64).sum();
        let ybar = sy / nn;
        t += rows.iter().map(|r| (r.0 - ybar) * r.2 as f64).sum::<f64>();
        let ss = syy - sy * sy / nn;
        s += ss;
        v += xx * (nn - xx) / (nn * (nn - 1.0)) * ss;
    }
    let slope = if s > 0.0 { t / s } else { f64::NAN };
    let se = if s > 0.0 { v.sqrt() / s } else { f64::NAN };
    (slope, se, if v > 0.0 { t / v.sqrt() } else { f64::NAN })
}

/// Pooled within-stratum least-squares slope of a continuous value on a score.
/// Input: (stratum, score, value).
pub fn within_slope(rows: &[(usize, f64, f64)]) -> f64 {
    let mut m: HashMap<usize, (f64, f64, f64)> = HashMap::new();
    for &(s, y, v) in rows {
        let e = m.entry(s).or_default();
        e.0 += 1.0;
        e.1 += y;
        e.2 += v;
    }
    let (mut num, mut den) = (0.0, 0.0);
    for &(s, y, v) in rows {
        let e = m[&s];
        let (yb, vb) = (e.1 / e.0, e.2 / e.0);
        num += (y - yb) * (v - vb);
        den += (y - yb) * (y - yb);
    }
    if den > 0.0 { num / den } else { f64::NAN }
}

/// Mann–Whitney AUC: P(late > early) + ½ P(tie).
pub fn auc(late: &[f64], early: &[f64]) -> f64 {
    if late.is_empty() || early.is_empty() {
        return f64::NAN;
    }
    let mut all: Vec<(f64, bool)> = late.iter().map(|&x| (x, true)).chain(early.iter().map(|&x| (x, false))).collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut i, mut rank_sum) = (0usize, 0.0f64);
    while i < all.len() {
        let mut j = i;
        while j + 1 < all.len() && all[j + 1].0 == all[i].0 {
            j += 1;
        }
        let r = (i + j) as f64 / 2.0 + 1.0;
        rank_sum += r * all[i..=j].iter().filter(|x| x.1).count() as f64;
        i = j + 1;
    }
    let (nl, ne) = (late.len() as f64, early.len() as f64);
    (rank_sum - nl * (nl + 1.0) / 2.0) / (nl * ne)
}

/// SplitMix64, for deterministic bootstrap draws.
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

pub fn percentile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let v: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return f64::NAN;
    }
    let pos = q * (v.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
}

/// Group-cluster bootstrap of a statistic over documents: groups are drawn
/// with replacement and each drawn group contributes all its documents.
pub fn cluster_boot<F: Fn(&[usize]) -> f64>(groups: &[Vec<usize>], draws: usize, seed: u64, stat: F) -> (f64, f64) {
    let mut rng = Rng(seed);
    let mut vals = Vec::with_capacity(draws);
    for _ in 0..draws {
        let mut idx = Vec::new();
        for _ in 0..groups.len() {
            idx.extend_from_slice(&groups[rng.below(groups.len())]);
        }
        vals.push(stat(&idx));
    }
    (percentile(&mut vals, 0.025), percentile(&mut vals, 0.975))
}

// ------------------------------------------------------------------ Pangram map

/// The Tier C Pangram CAL map: linear interpolation between adjacent knots of
/// `min(1, fraction_ai + fraction_ai_assisted)`, constant outside the knots
/// (`involvement_calibration::isotonic_predict_score`).
pub struct PangramMap {
    pub knots: Vec<(f64, f64)>,
    pub op_1pct: f64,
}

impl PangramMap {
    pub fn load(path: &Path) -> Result<Self> {
        let v: Value = store::read_json(path)?;
        let knots: Vec<(f64, f64)> = v["map"]["knots"]
            .as_array()
            .context("knots")?
            .iter()
            .map(|k| Ok((k["fraction"].as_f64().context("fraction")?, k["involvement"].as_f64().context("involvement")?)))
            .collect::<Result<_>>()?;
        ensure!(!knots.is_empty() && knots.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 <= w[1].1), "map knots");
        let op = v["operating_points"]["0.01"]["value"].as_f64().context("1% point")?;
        Ok(PangramMap { knots, op_1pct: op })
    }
    pub fn apply(&self, x: f64) -> f64 {
        let (f, l) = (self.knots[0], self.knots[self.knots.len() - 1]);
        if x <= f.0 {
            return f.1;
        }
        if x >= l.0 {
            return l.1;
        }
        let u = self.knots.partition_point(|k| k.0 <= x);
        let (a, b) = (self.knots[u - 1], self.knots[u]);
        (a.1 + (b.1 - a.1) * ((x - a.0) / (b.0 - a.0))).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug)]
pub struct PangramObs {
    pub raw: f64,
    pub fraction_ai: f64,
    pub label: String,
    pub headline: String,
    pub version: String,
}

/// Successful observations of a collector store, keyed by id; failures are
/// counted. Each id must have at most one success.
/// Successful observations by id: (echoed text SHA-256, observation).
pub type PangramObsMap = HashMap<String, (String, PangramObs)>;

pub fn pangram_store(store_dir: &Path) -> Result<(PangramObsMap, usize)> {
    let mut out = HashMap::new();
    let mut failures = 0;
    let obs = store_dir.join("obs");
    if !obs.exists() {
        return Ok((out, 0));
    }
    for e in fs::read_dir(&obs)? {
        let d = e?.path();
        for f in fs::read_dir(&d)? {
            let f = f?.path();
            let name = f.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with("observation-") {
                continue;
            }
            let o: Value = store::read_json(&f)?;
            match o["status"].as_str() {
                Some("success") => {
                    let id = o["id"].as_str().context("id")?.to_owned();
                    let fa = o["fraction_ai"].as_f64().context("fraction_ai")?;
                    let faa = o["fraction_ai_assisted"].as_f64().context("fraction_ai_assisted")?;
                    let ob = PangramObs {
                        raw: (fa + faa).min(1.0),
                        fraction_ai: fa,
                        label: o["response"]["prediction_short"].as_str().unwrap_or("").to_owned(),
                        headline: o["response"]["headline"].as_str().unwrap_or("").to_owned(),
                        version: o["version"].as_str().unwrap_or("").to_owned(),
                    };
                    let sha = o["text_sha256"].as_str().context("text_sha256")?.to_owned();
                    ensure!(out.insert(id.clone(), (sha, ob)).is_none(), "{id}: two successes");
                }
                _ => failures += 1,
            }
        }
    }
    Ok((out, failures))
}

// ------------------------------------------------------------------ report

pub struct ReportArgs {
    pub sample: PathBuf,
    pub a_pred: Vec<PathBuf>,
    pub threshold: f64,
    pub pangram_store: Option<PathBuf>,
    pub pangram_map: PathBuf,
    pub tierc_rows: Option<PathBuf>,
    pub tierc_pred: Option<PathBuf>,
    pub tierc_pangram: Option<PathBuf>,
    pub pool_cells: PathBuf,
    pub out: PathBuf,
    pub draws: usize,
}

#[derive(Clone)]
pub struct Doc {
    pub s: SampleRow,
    pub a_inv: Option<f64>,
    pub a_share: Option<f64>,
    pub a_mixed: Option<f64>,
    pub a_failure: Option<String>,
    pub p: Option<PangramObs>,
    pub p_cal: Option<f64>,
}

impl Doc {
    pub fn a_flag(&self, t: f64) -> Option<bool> {
        self.a_inv.map(|v| v >= t)
    }
    pub fn p_flag_cal(&self, t: f64) -> Option<bool> {
        self.p_cal.map(|v| v >= t)
    }
    pub fn p_flag_native(&self) -> Option<bool> {
        self.p.as_ref().map(|p| p.label != "Human")
    }
}

type FlagFn<'a> = &'a dyn Fn(&Doc) -> Option<bool>;

fn table_by<K: Ord + Clone + Serialize>(docs: &[Doc], key: impl Fn(&Doc) -> K, flag: FlagFn) -> Vec<Value> {
    let mut m: BTreeMap<K, (usize, usize)> = BTreeMap::new();
    for d in docs {
        if let Some(f) = flag(d) {
            let e = m.entry(key(d)).or_default();
            e.0 += 1;
            e.1 += f as usize;
        }
    }
    m.into_iter()
        .map(|(k, (n, x))| {
            let mut v = rate_json(x, n);
            v["key"] = json!(k);
            v
        })
        .collect()
}

/// Pool-weighted margin rate: cell rates weighted by the pool's cell sizes,
/// with a stratified normal 95% interval.
fn pool_weighted(docs: &[Doc], pool: &BTreeMap<(u16, String), usize>, margin: impl Fn(u16, &str) -> String, flag: FlagFn) -> BTreeMap<String, Value> {
    let mut cell: BTreeMap<(u16, String), (usize, usize)> = BTreeMap::new();
    for d in docs {
        if let Some(f) = flag(d) {
            let e = cell.entry((d.s.year, d.s.domain.clone())).or_default();
            e.0 += 1;
            e.1 += f as usize;
        }
    }
    let mut acc: BTreeMap<String, (f64, f64, f64)> = BTreeMap::new();
    for ((y, dom), (n, x)) in &cell {
        let w = *pool.get(&(*y, dom.clone())).unwrap_or(&0) as f64;
        let p = *x as f64 / *n as f64;
        let e = acc.entry(margin(*y, dom)).or_default();
        e.0 += w;
        e.1 += w * p;
        e.2 += w * w * p * (1.0 - p) / *n as f64;
    }
    acc.into_iter()
        .map(|(k, (w, wp, wv))| {
            let (p, se) = (wp / w, wv.sqrt() / w);
            (k, json!({"rate": p, "normal95": [(p - Z * se).max(0.0), (p + Z * se).min(1.0)], "pool_documents": w}))
        })
        .collect()
}

fn early(y: u16) -> bool {
    (EARLY.0..=EARLY.1).contains(&y)
}
fn late(y: u16) -> bool {
    (LATE.0..=LATE.1).contains(&y)
}

fn era_group(e: &str) -> &'static str {
    match e {
        "E0" | "E1" => "E0-E1",
        _ => "E2-E5",
    }
}

/// Every flag analysis for one flag definition over `docs`.
fn flag_section(docs: &[Doc], pool: &BTreeMap<(u16, String), usize>, flag: FlagFn) -> Value {
    let n = docs.iter().filter(|d| flag(d).is_some()).count();
    let k = docs.iter().filter(|d| flag(d) == Some(true)).count();
    let by_year = table_by(docs, |d| d.s.year, flag);
    let by_domain = table_by(docs, |d| d.s.domain.clone(), flag);
    let by_era = table_by(docs, |d| d.s.era.clone(), flag);
    let by_cell = table_by(docs, |d| format!("{}|{}", d.s.year, d.s.domain), flag);
    let by_era_domain = table_by(docs, |d| format!("{}|{}", era_group(&d.s.era), d.s.domain), flag);
    // Early (2013-2016) vs late (2020-2022), and E2-E5 vs E0-E1, overall and per domain.
    let mut contrasts = BTreeMap::new();
    let mut domains: Vec<String> = docs.iter().map(|d| d.s.domain.clone()).collect();
    domains.sort();
    domains.dedup();
    for dom in std::iter::once("all".to_string()).chain(domains.iter().cloned()) {
        let sel = |d: &&Doc| dom == "all" || d.s.domain == dom;
        let cnt = |p: &dyn Fn(&Doc) -> bool| {
            let v: Vec<bool> = docs.iter().filter(sel).filter(|d| p(d)).filter_map(flag).collect();
            (v.iter().filter(|x| **x).count(), v.len())
        };
        let (kl, nl) = cnt(&|d| late(d.s.year));
        let (ke, ne) = cnt(&|d| early(d.s.year));
        let (k5, n5) = cnt(&|d| era_group(&d.s.era) == "E2-E5");
        let (k1, n1) = cnt(&|d| era_group(&d.s.era) == "E0-E1");
        let (d1, lo1, hi1) = newcombe(kl, nl, ke, ne);
        let (d2, lo2, hi2) = newcombe(k5, n5, k1, n1);
        contrasts.insert(
            dom.clone(),
            json!({"late_2020_2022": rate_json(kl, nl), "early_2013_2016": rate_json(ke, ne),
                "late_minus_early": {"diff": d1, "newcombe95": [lo1, hi1]},
                "e2_e5": rate_json(k5, n5), "e0_e1": rate_json(k1, n1),
                "excess_e2_e5_over_e0_e1": {"diff": d2, "newcombe95": [lo2, hi2], "upper_above_2_points": hi2 > 0.02}}),
        );
    }
    // Year trend stratified by domain (Mantel extension).
    let mut strata = Vec::new();
    for dom in &domains {
        let mut m: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
        for d in docs.iter().filter(|d| &d.s.domain == dom) {
            if let Some(f) = flag(d) {
                let e = m.entry(d.s.year).or_default();
                e.0 += 1;
                e.1 += f as usize;
            }
        }
        strata.push(m.into_iter().map(|(y, (n, x))| (y as f64, n, x)).collect::<Vec<_>>());
    }
    let (slope, se, z) = mantel_trend(&strata);
    json!({
        "documents": n, "flagged": k, "overall": rate_json(k, n),
        "overall_pool_weighted": pool_weighted(docs, pool, |_, _| "all".into(), flag).remove("all"),
        "by_year": by_year, "by_year_pool_weighted": pool_weighted(docs, pool, |y, _| y.to_string(), flag),
        "by_domain": by_domain, "by_domain_pool_weighted": pool_weighted(docs, pool, |_, d| d.to_string(), flag),
        "by_era": by_era, "by_era_group_domain": by_era_domain, "by_year_domain": by_cell,
        "contrasts": contrasts,
        "year_trend_within_domain": {"slope_per_year": slope, "se_h0": se, "z": z,
            "method": "Mantel (1963) extension test, strata = domains, score = crawl year"},
    })
}

fn groups_of(docs: &[Doc], keep: impl Fn(&Doc) -> bool) -> Vec<Vec<usize>> {
    let mut m: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, d) in docs.iter().enumerate() {
        if keep(d) {
            m.entry(d.s.group.as_str()).or_default().push(i);
        }
    }
    m.into_values().collect()
}

/// Drift of A's continuous human scores with crawl year.
fn drift_section(docs: &[Doc], draws: usize) -> Value {
    let mut domains: Vec<String> = docs.iter().map(|d| d.s.domain.clone()).collect();
    domains.sort();
    domains.dedup();
    let di: HashMap<&str, usize> = domains.iter().enumerate().map(|(i, d)| (d.as_str(), i)).collect();
    let mut out = serde_json::Map::new();
    type Metric = (&'static str, fn(&Doc) -> Option<f64>);
    let metrics: [Metric; 3] = [
        ("involvement_probability", |d| d.a_inv),
        ("ai_share_percent", |d| d.a_share),
        ("origin_mixed", |d| d.a_mixed),
    ];
    for (name, get) in metrics {
        let mut by_year: BTreeMap<u16, Vec<f64>> = BTreeMap::new();
        for d in docs {
            if let Some(v) = get(d) {
                by_year.entry(d.s.year).or_default().push(v);
            }
        }
        let table: Vec<Value> = by_year
            .iter()
            .map(|(y, v)| {
                let mean = v.iter().sum::<f64>() / v.len() as f64;
                let mut c = v.clone();
                json!({"year": y, "n": v.len(), "mean": mean, "median": percentile(&mut c, 0.5), "p90": percentile(&mut v.clone(), 0.9)})
            })
            .collect();
        let slope_of = |idx: &[usize]| {
            let rows: Vec<(usize, f64, f64)> =
                idx.iter().filter_map(|&i| get(&docs[i]).map(|v| (di[docs[i].s.domain.as_str()], docs[i].s.year as f64, v))).collect();
            within_slope(&rows)
        };
        let all: Vec<usize> = (0..docs.len()).collect();
        let groups = groups_of(docs, |d| get(d).is_some());
        let slope = slope_of(&all);
        let ci = cluster_boot(&groups, draws, 0x5eed_0001, slope_of);
        // Domain-balanced AUC: late (2020-2022) above early (2013-2016).
        let auc_of = |idx: &[usize]| {
            let mut s = Vec::new();
            for dom in &domains {
                let l: Vec<f64> = idx.iter().filter(|&&i| &docs[i].s.domain == dom && late(docs[i].s.year)).filter_map(|&i| get(&docs[i])).collect();
                let e: Vec<f64> = idx.iter().filter(|&&i| &docs[i].s.domain == dom && early(docs[i].s.year)).filter_map(|&i| get(&docs[i])).collect();
                let a = auc(&l, &e);
                if a.is_finite() {
                    s.push(a);
                }
            }
            s.iter().sum::<f64>() / s.len().max(1) as f64
        };
        let a0 = auc_of(&all);
        let aci = cluster_boot(&groups, draws, 0x5eed_0002, auc_of);
        out.insert(
            name.into(),
            json!({"by_year": table, "within_domain_slope_per_year": slope, "slope_group_bootstrap95": [ci.0, ci.1],
                "late_vs_early_auc_domain_balanced": a0, "auc_group_bootstrap95": [aci.0, aci.1]}),
        );
    }
    Value::Object(out)
}

fn agreement(docs: &[Doc], fa: FlagFn, fb: FlagFn) -> Value {
    let (mut both, mut a_only, mut b_only, mut neither) = (0, 0, 0, 0);
    for d in docs {
        match (fa(d), fb(d)) {
            (Some(true), Some(true)) => both += 1,
            (Some(true), Some(false)) => a_only += 1,
            (Some(false), Some(true)) => b_only += 1,
            (Some(false), Some(false)) => neither += 1,
            _ => {}
        }
    }
    json!({"both": both, "a_only": a_only, "pangram_only": b_only, "neither": neither})
}

/// Tier C Test humans: the released A (calibrated `1 - p_human`) and Pangram
/// at its CAL 1% point, by Test domain, for the primary population (cohorts
/// `human:*`, the evaluator's 3,286 humans) and the diagnostic cohorts apart.
fn tierc_section(a: &ReportArgs, map: &PangramMap) -> Result<Value> {
    let (Some(rows_p), Some(pred_p)) = (&a.tierc_rows, &a.tierc_pred) else { return Ok(Value::Null) };
    let mut human: HashMap<String, (String, &'static str)> = HashMap::new();
    let r = BufReader::new(File::open(rows_p)?);
    for l in r.lines() {
        let v: Value = serde_json::from_str(&l?)?;
        if v["origin"].as_str() == Some("human_only") {
            let pop = if v["cohort_id"].as_str().is_some_and(|c| c.starts_with("human:")) { "primary" } else { "diagnostic" };
            human.insert(v["id"].as_str().context("id")?.into(), (v["domain"].as_str().unwrap_or("").into(), pop));
        }
    }
    type Counts = BTreeMap<&'static str, (BTreeMap<String, (usize, usize)>, (usize, usize))>;
    let bump = |c: &mut Counts, pop: &'static str, dom: &str, f: bool| {
        let e = c.entry(pop).or_default();
        let d = e.0.entry(dom.to_string()).or_default();
        d.0 += 1;
        d.1 += f as usize;
        e.1.0 += 1;
        e.1.1 += f as usize;
    };
    let mut a_cnt = Counts::new();
    for v in read_jsonl(pred_p)? {
        let id = v["id"].as_str().context("id")?;
        let Some((dom, pop)) = human.get(id) else { continue };
        let f = match v["p_human"].as_f64() {
            Some(ph) => (1.0 - ph).clamp(0.0, 1.0) >= a.threshold,
            None => true, // execution failure: flagged by the protocol rule
        };
        bump(&mut a_cnt, pop, dom, f);
    }
    let (mut p_cnt, mut p_native) = (Counts::new(), Counts::new());
    if let Some(pp) = &a.tierc_pangram {
        let (obs, _) = pangram_store(pp)?;
        for (id, (_, o)) in &obs {
            let Some((dom, pop)) = human.get(id) else { continue };
            bump(&mut p_cnt, pop, dom, map.apply(o.raw) >= map.op_1pct);
            bump(&mut p_native, pop, dom, o.label != "Human");
        }
    }
    let table = |c: &Counts| {
        c.iter()
            .map(|(pop, (m, (n, x)))| {
                (pop.to_string(), json!({"all": rate_json(*x, *n),
                    "by_domain": m.iter().map(|(k, (n, x))| (k.clone(), rate_json(*x, *n))).collect::<BTreeMap<_, _>>()}))
            })
            .collect::<BTreeMap<_, _>>()
    };
    Ok(json!({
        "a_released_seed17_at_cal_1pct": table(&a_cnt),
        "pangram_at_cal_1pct": table(&p_cnt),
        "pangram_native_not_human": table(&p_native),
        "note": "Tier C Test humans (consumed 2026-10-30): primary = corpus-v1 human cohorts (all pre-cutoff by source date), diagnostic = ELLIPSE learner essays and SBCSAE; Pangram on its stratified Test subsample, unweighted counts",
    }))
}

pub const REVIEW_N: usize = 50;
pub const REVIEW_PANGRAM_ONLY: usize = 8;

/// Flagged documents for manual review: every document flagged by A and by
/// Pangram (either rule), then at most `pangram_only` Pangram-only documents,
/// then A-only documents, each list round-robin over domains in rank order.
pub fn review_pick(docs: &[Doc], t: f64, pt: f64, n: usize, pangram_only: usize) -> Vec<(usize, &'static str)> {
    let pf = |d: &Doc| d.p_flag_native() == Some(true) || d.p_flag_cal(pt) == Some(true);
    let robin = |idx: Vec<usize>| {
        let mut by: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for i in idx {
            by.entry(docs[i].s.domain.as_str()).or_default().push(i);
        }
        for v in by.values_mut() {
            v.sort_by(|&x, &y| docs[x].s.rank.cmp(&docs[y].s.rank));
            v.reverse();
        }
        let mut out = Vec::new();
        while by.values().any(|v| !v.is_empty()) {
            for v in by.values_mut() {
                if let Some(i) = v.pop() {
                    out.push(i);
                }
            }
        }
        out
    };
    let af = |d: &Doc| d.a_flag(t) == Some(true);
    let both = robin((0..docs.len()).filter(|&i| af(&docs[i]) && pf(&docs[i])).collect());
    let p_only = robin((0..docs.len()).filter(|&i| !af(&docs[i]) && pf(&docs[i])).collect());
    let a_only = robin((0..docs.len()).filter(|&i| af(&docs[i]) && !pf(&docs[i])).collect());
    let mut out: Vec<(usize, &'static str)> = both.into_iter().map(|i| (i, "a_and_pangram")).take(n).collect();
    out.extend(p_only.into_iter().take(pangram_only.min(n - out.len())).map(|i| (i, "pangram_only")));
    let room = n - out.len();
    out.extend(a_only.into_iter().take(room).map(|i| (i, "a_only")));
    out
}

pub fn report(a: &ReportArgs) -> Result<Value> {
    let sample: Vec<SampleRow> = read_jsonl(&a.sample)?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>()?;
    let mut docs: Vec<Doc> =
        sample.into_iter().map(|s| Doc { s, a_inv: None, a_share: None, a_mixed: None, a_failure: None, p: None, p_cal: None }).collect();
    let index: HashMap<String, usize> = docs.iter().enumerate().map(|(i, d)| (d.s.id.clone(), i)).collect();
    // A predictions (Rust runtime): calibrated attribution.ai.involvement_probability.
    let mut a_seen = 0;
    let mut a_consistency_max = 0.0f64;
    for p in &a.a_pred {
        for v in read_jsonl(p)? {
            let id = v["id"].as_str().context("pred id")?;
            let i = *index.get(id).with_context(|| format!("{id}: prediction outside the sample"))?;
            ensure!(docs[i].a_inv.is_none() && docs[i].a_failure.is_none(), "{id}: two predictions");
            a_seen += 1;
            ensure!(v["text_sha256"].as_str() == Some(docs[i].s.text_sha256.as_str()), "{id}: scored text differs from the sample");
            if v["status"].as_str() == Some("ok") {
                let pr = &v["prediction"];
                let inv = pr["attribution"]["ai"]["involvement_probability"].as_f64().context("involvement")?;
                let ho = pr["origin"]["human_only"].as_f64().context("human_only")?;
                a_consistency_max = a_consistency_max.max((1.0 - ho - inv).abs());
                docs[i].a_inv = Some(inv);
                docs[i].a_share = pr["ai_share_percent"].as_f64();
                docs[i].a_mixed = pr["origin"]["mixed"].as_f64();
            } else {
                docs[i].a_failure = Some(v["reason"].as_str().unwrap_or("failure").into());
            }
        }
    }
    let map = PangramMap::load(&a.pangram_map)?;
    let mut p_fail = 0;
    let mut versions: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(sp) = &a.pangram_store {
        let (obs, f) = pangram_store(sp)?;
        p_fail = f;
        for (id, (sha, o)) in obs {
            let i = *index.get(&id).with_context(|| format!("{id}: Pangram observation outside the sample"))?;
            ensure!(docs[i].s.pangram, "{id}: Pangram observation outside the subsample");
            ensure!(sha == docs[i].s.text_sha256, "{id}: Pangram text hash");
            *versions.entry(o.version.clone()).or_default() += 1;
            docs[i].p_cal = Some(map.apply(o.raw));
            docs[i].p = Some(o);
        }
    }
    let pool_cells: Value = store::read_json(&a.pool_cells)?;
    let mut pool: BTreeMap<(u16, String), usize> = BTreeMap::new();
    for (k, v) in pool_cells["cells"].as_object().context("cells")? {
        let (y, d) = k.split_once('|').context("cell key")?;
        pool.insert((y.parse()?, d.into()), v["available"].as_u64().context("available")? as usize);
    }
    let t = a.threshold;
    let pt = map.op_1pct;
    let fa = |d: &Doc| d.a_flag(t);
    let fpc = |d: &Doc| d.p_flag_cal(pt);
    let fpn = |d: &Doc| d.p_flag_native();
    let sub: Vec<Doc> = docs.iter().filter(|d| d.s.pangram).cloned().collect();
    let out = json!({
        "schema": "slop_ninja_cc_human_contamination_report_v1",
        "inputs": {"sample": a.sample, "sample_sha256": file_sha256(&a.sample)?,
            "a_predictions": a.a_pred.iter().map(|p| Ok(json!({"path": p, "sha256": file_sha256(p)?}))).collect::<Result<Vec<_>>>()?,
            "pangram_store": a.pangram_store, "pangram_map": a.pangram_map, "pangram_map_sha256": file_sha256(&a.pangram_map)?},
        "a": {"model": "released A seed 17 (Faux / E1aux), Rust FP64 runtime full_a-2314d433, calibrated",
            "flag": "attribution.ai.involvement_probability >= threshold (CAL 1% operating point)",
            "threshold": t, "threshold_bits": format!("{:#018x}", t.to_bits()),
            "documents": docs.len(), "predictions": a_seen,
            "failures": docs.iter().filter(|d| d.a_failure.is_some()).count(),
            "max_abs_1_minus_human_only_minus_involvement": a_consistency_max,
            "flags": flag_section(&docs, &pool, &fa),
            "on_pangram_subsample": flag_section(&sub, &pool, &fa),
            "human_score_drift": drift_section(&docs, a.draws)},
        "pangram": {"selected": sub.len(), "successes": sub.iter().filter(|d| d.p.is_some()).count(), "failures": p_fail,
            "versions": versions,
            "cal_1pct": {"rule": "Tier C CAL map applied to min(1, fraction_ai + fraction_ai_assisted); flag when >= the map's 1% point",
                "threshold": pt, "flags": flag_section(&sub, &pool, &fpc)},
            "native_not_human": {"rule": "prediction_short is not Human (AI or Mixed)", "flags": flag_section(&sub, &pool, &fpn)},
            "labels": sub.iter().filter_map(|d| d.p.as_ref().map(|p| format!("{} / {}", p.label, p.headline)))
                .fold(BTreeMap::<String, usize>::new(), |mut m, k| { *m.entry(k).or_default() += 1; m }),
            "agreement_a_vs_pangram_cal_1pct": agreement(&sub, &fa, &fpc),
            "agreement_a_vs_pangram_native": agreement(&sub, &fa, &fpn)},
        "tier_c_test_humans": tierc_section(a, &map)?,
    });
    fs::create_dir_all(&a.out)?;
    store::write_json(&a.out.join("report.json"), &out)?;
    // Per-document scores (ids, strata and scores only; the texts stay in the sample).
    let per_doc: Vec<Value> = docs
        .iter()
        .map(|d| {
            json!({"id": d.s.id, "year": d.s.year, "era": d.s.era, "domain": d.s.domain, "group": d.s.group, "host": d.s.host,
                "url": d.s.url, "pangram_subsample": d.s.pangram, "a_involvement": d.a_inv, "a_flag": d.a_flag(t),
                "a_share_percent": d.a_share, "a_failure": d.a_failure,
                "pangram_raw": d.p.as_ref().map(|p| p.raw), "pangram_fraction_ai": d.p.as_ref().map(|p| p.fraction_ai),
                "pangram_label": d.p.as_ref().map(|p| p.label.clone()), "pangram_headline": d.p.as_ref().map(|p| p.headline.clone()),
                "pangram_cal": d.p_cal, "pangram_flag_cal_1pct": d.p_flag_cal(pt), "pangram_flag_native": d.p_flag_native()})
        })
        .collect();
    write_jsonl(&a.out.join("per-document.jsonl"), &per_doc)?;
    // Manual-review sample of flagged documents (texts stay in ignored storage).
    let review = review_pick(&docs, t, pt, REVIEW_N, REVIEW_PANGRAM_ONLY);
    let rv: Vec<Value> = review
        .iter()
        .map(|(i, why)| {
            let d = &docs[*i];
            json!({"id": d.s.id, "why": why, "year": d.s.year, "domain": d.s.domain, "host": d.s.host, "url": d.s.url,
                "a_involvement": d.a_inv, "a_share_percent": d.a_share,
                "pangram_label": d.p.as_ref().map(|p| p.label.clone()), "pangram_fraction_ai": d.p.as_ref().map(|p| p.fraction_ai),
                "words": d.s.words, "text": d.s.text})
        })
        .collect();
    write_jsonl(&a.out.join("review-sample.jsonl"), &rv)?;
    Ok(json!({"documents": docs.len(), "a_predictions": a_seen, "pangram_successes": sub.iter().filter(|d| d.p.is_some()).count()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_caps_groups_first_then_fills_and_nests_pangram() {
        let c = |cell: &str, r: u64, g: &str| (cell.to_string(), r, g.to_string());
        let cands = vec![c("a", 5, "g1"), c("a", 1, "g1"), c("a", 3, "g2"), c("a", 4, "g3"), c("b", 2, "g9"), c("a", 2, "g1")];
        let p = draw(&cands, 3, 2);
        // Cell a: ranks 1(g1) 2(g1) 3(g2) 4(g3) 5(g1) -> pass 1 takes 1, 3, 4.
        assert_eq!(p[0], Pick { index: 1, pass: 1, pangram: true });
        assert_eq!(p[1], Pick { index: 2, pass: 1, pangram: true });
        assert_eq!(p[2], Pick { index: 3, pass: 1, pangram: false });
        assert_eq!(p[3], Pick { index: 4, pass: 1, pangram: true });
        assert_eq!(p.len(), 4);
        let p = draw(&cands, 4, 0);
        assert_eq!(p[3], Pick { index: 5, pass: 2, pangram: false });
    }

    #[test]
    fn wilson_and_newcombe_match_reference_values() {
        let (lo, hi) = wilson(5, 100);
        assert!((lo - 0.021543).abs() < 1e-5 && (hi - 0.111750).abs() < 1e-5, "{lo} {hi}");
        let (d, lo, hi) = newcombe(56, 70, 48, 80);
        // Newcombe (1998) example 1: 0.2, [0.0524, 0.3339].
        assert!((d - 0.2).abs() < 1e-12 && (lo - 0.0524).abs() < 1e-3 && (hi - 0.3339).abs() < 1e-3, "{lo} {hi}");
        assert_eq!(wilson(0, 0), (0.0, 1.0));
    }

    #[test]
    fn mantel_trend_detects_a_rising_rate_and_auc_handles_ties() {
        let rising = vec![vec![(0.0, 100, 1), (1.0, 100, 5), (2.0, 100, 10)]];
        let (slope, se, z) = mantel_trend(&rising);
        // Cochran-Armitage by hand: T = 9, Var = 16*284/(300*299)*200, z = 2.8275.
        assert!((slope - 0.045).abs() < 1e-12 && se > 0.0 && (z - 2.827512748878495).abs() < 1e-9, "{slope} {se} {z}");
        let flat = vec![vec![(0.0, 100, 5), (1.0, 100, 5)], vec![(0.0, 50, 0), (1.0, 50, 0)]];
        assert_eq!(mantel_trend(&flat).0, 0.0);
        assert_eq!(auc(&[2.0, 3.0], &[1.0, 1.0]), 1.0);
        assert_eq!(auc(&[1.0], &[1.0]), 0.5);
        assert!((within_slope(&[(0, 0.0, 0.0), (0, 1.0, 2.0), (1, 0.0, 5.0), (1, 1.0, 7.0)]) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn pangram_map_interpolates_and_clamps() {
        let m = PangramMap { knots: vec![(0.0, 0.1), (0.5, 0.3), (1.0, 0.9)], op_1pct: 0.8 };
        assert_eq!(m.apply(-1.0), 0.1);
        assert!((m.apply(0.25) - 0.2).abs() < 1e-12);
        assert!((m.apply(0.75) - 0.6).abs() < 1e-12);
        assert_eq!(m.apply(1.0), 0.9);
    }

    fn doc(id: &str, dom: &str, rank: &str, inv: f64, label: Option<&str>) -> Doc {
        let s = SampleRow { id: id.into(), year: 2015, crawl: "CC-MAIN-2015-11".into(), era: "E0".into(), domain: dom.into(),
            group: id.into(), host: String::new(), url: String::new(), warc_date: String::new(), words: 200,
            text_variant: "text".into(), text: String::new(), text_sha256: String::new(), pool_text_sha256: String::new(),
            rank: rank.into(), pass: 1, pangram: label.is_some() };
        let p = label.map(|l| PangramObs { raw: 0.0, fraction_ai: 0.0, label: l.into(), headline: String::new(), version: "4.1".into() });
        Doc { s, a_inv: Some(inv), a_share: None, a_mixed: None, a_failure: None, p_cal: p.as_ref().map(|_| 0.0), p }
    }

    #[test]
    fn review_pick_orders_both_then_pangram_only_then_a_only_round_robin() {
        let docs = vec![
            doc("a1", "news", "01", 0.95, None),
            doc("a2", "news", "00", 0.95, None),
            doc("a3", "legal", "05", 0.95, None),
            doc("b1", "legal", "09", 0.95, Some("AI")),
            doc("p1", "news", "03", 0.10, Some("Mixed")),
            doc("p2", "news", "02", 0.10, Some("AI")),
            doc("n1", "news", "04", 0.10, Some("Human")),
        ];
        let got: Vec<(&str, &str)> = review_pick(&docs, 0.9, 0.8, 5, 1).iter().map(|(i, w)| (docs[*i].s.id.as_str(), *w)).collect();
        assert_eq!(got, vec![("b1", "a_and_pangram"), ("p2", "pangram_only"), ("a3", "a_only"), ("a2", "a_only"), ("a1", "a_only")]);
    }

    #[test]
    fn crawl_year_parses_crawl_ids() {
        assert_eq!(crawl_year("CC-MAIN-2013-20").unwrap(), 2013);
        assert!(crawl_year("CC-NEWS").is_err());
    }
}
