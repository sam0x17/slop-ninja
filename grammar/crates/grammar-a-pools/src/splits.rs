//! Split reservation for the competitive A protocol.
//!
//! Evaluation splits (Test, CAL, Dev-select, Dev-iter) are reserved from
//! metadata only: families in the reserved buckets (900..1000) that no
//! adapter has rendered, listed by `--metadata-only` scans, the Gutenberg
//! writer-level plan and the Wikipedia multistream index. Contributor
//! closure (PG writer, Stack Exchange network account, PLOS author name,
//! Federal Register agency/sub-agency unit) forms source groups; a group
//! that shares a contributor with admitted Train-side text is either
//! blocked (Stack Exchange, PLOS) or, for Federal Register units chosen for
//! evaluation, its Train-side documents are held out. Groups are assigned
//! in seeded hash order with per-domain targets, so every evaluation split
//! covers every domain. Admitted (already rendered) pool documents are
//! split into Train and Train-mining by whole dedup component, after a
//! re-run of the prior-snapshot checks and a cross-source duplicate pass.
//!
//! No evaluation prose is opened here: evaluation manifests carry
//! identifiers, family keys and hashes of source units only.
use crate::{
    io::{self, JsonlWriter},
    protected::Index,
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::BufRead,
    path::{Path, PathBuf},
};

pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_split_summary_v1";
pub const RESERVATION_SCHEMA: &str = "slop_ninja_a_competitive_split_reservation_v1";
pub const GROUP_SCHEMA: &str = "slop_ninja_a_competitive_split_group_v1";
pub const DOCUMENT_SCHEMA: &str = "slop_ninja_a_competitive_split_document_v1";
pub const WIKIPEDIA_INDEX_SCHEMA: &str = "slop_ninja_a_pool_wikipedia_index_reservation_v1";

// The seeded rank (and its domain string) lives in `grammar_a_form::rank`, the
// one source the form path shares; it is re-exported here under its existing path.
use grammar_a_form::rank::RANK_DOMAIN;
pub use grammar_a_form::rank::rank;

pub const DOMAINS: [&str; 6] = ["project_gutenberg", "wikipedia", "wikinews", "plos", "us_federal_register", "stack_exchange"];
/// Every evaluation domain, including the two added in splits-v3 (FineWeb
/// and Global Voices); indices 0..6 agree with `DOMAINS`.
pub const ALL_DOMAINS: [&str; 8] = ["project_gutenberg", "wikipedia", "wikinews", "plos", "us_federal_register", "stack_exchange", "fineweb", "global_voices"];
pub const EVAL_SPLITS: [&str; 4] = ["test", "cal", "dev_select", "dev_iter"];

pub fn domain_of_collection(c: &str) -> Option<usize> {
    Some(match c {
        "project_gutenberg_historical" => 0,
        "wikipedia_enwiki_historical" => 1,
        "wikinews_enwikinews_historical" => 2,
        "plos_historical" => 3,
        "us_federal_register_historical" => 4,
        "stackexchange_historical" => 5,
        _ => return None,
    })
}

/// `domain_of_collection` over `ALL_DOMAINS`.
pub fn domain_of_collection_all(c: &str) -> Option<usize> {
    domain_of_collection(c).or(match c {
        "fineweb_cc_historical" => Some(6),
        "global_voices_historical" => Some(7),
        _ => None,
    })
}

/// Union-find over interned string keys.
#[derive(Default)]
pub struct Dsu {
    ids: HashMap<String, u32>,
    keys: Vec<String>,
    parent: Vec<u32>,
}

impl Dsu {
    pub fn id(&mut self, k: &str) -> u32 {
        if let Some(&i) = self.ids.get(k) {
            return i;
        }
        let i = self.keys.len() as u32;
        self.ids.insert(k.to_owned(), i);
        self.keys.push(k.to_owned());
        self.parent.push(i);
        i
    }
    pub fn find(&mut self, mut i: u32) -> u32 {
        while self.parent[i as usize] != i {
            let p = self.parent[i as usize];
            self.parent[i as usize] = self.parent[p as usize];
            i = p;
        }
        i
    }
    pub fn union(&mut self, a: &str, b: &str) {
        let (x, y) = (self.id(a), self.id(b));
        let (rx, ry) = (self.find(x), self.find(y));
        if rx != ry {
            // Smallest key is the root, so component keys are stable.
            if self.keys[rx as usize] < self.keys[ry as usize] {
                self.parent[ry as usize] = rx;
            } else {
                self.parent[rx as usize] = ry;
            }
        }
    }
    pub fn root_key(&mut self, k: &str) -> String {
        let i = self.id(k);
        let r = self.find(i);
        self.keys[r as usize].clone()
    }
}

/// One reservable source unit (a work, article, thread or document).
#[derive(Clone, Debug)]
pub struct Unit {
    pub family_key: String,
    pub expected_docs: f64,
    pub meta: Value,
}

/// A source group: the split and bootstrap unit.
#[derive(Clone, Debug)]
pub struct Group {
    pub key: String,
    pub rank: u64,
    pub units: Vec<Unit>,
    pub contributors: BTreeSet<String>,
    pub train_side_docs: u64,
}

impl Group {
    pub fn expected(&self, cap: f64) -> f64 {
        self.units.iter().map(|u| u.expected_docs).sum::<f64>().min(cap)
    }
}

/// Deficit allocation in rank order: each group goes to the split whose
/// reserve target is least filled (relative), until every split reaches
/// `target * oversample` or groups run out. Returns the split index per
/// group (None: reserved but unassigned spare).
pub fn allocate(expected: &[f64], targets: &[f64; 4], oversample: f64) -> Vec<Option<usize>> {
    let goal: Vec<f64> = targets.iter().map(|t| t * oversample).collect();
    let mut have = [0.0f64; 4];
    expected
        .iter()
        .map(|&e| {
            let mut best: Option<(usize, f64)> = None;
            for s in 0..4 {
                if goal[s] <= 0.0 || have[s] >= goal[s] {
                    continue;
                }
                let deficit = (goal[s] - have[s]) / goal[s];
                if best.is_none_or(|(_, d)| deficit > d) {
                    best = Some((s, deficit));
                }
            }
            best.map(|(s, _)| {
                have[s] += e;
                s
            })
        })
        .collect()
}

/// Wikipedia multistream index lines (`offset:page_id:title`) to text-free
/// reservation rows for article-like titles in `[lo, hi)`; counts per range.
const NAMESPACES: &[&str] = &[
    "Talk", "User", "User talk", "Wikipedia", "Wikipedia talk", "WP", "File", "File talk", "MediaWiki", "MediaWiki talk",
    "Template", "Template talk", "Help", "Help talk", "Category", "Category talk", "Portal", "Portal talk", "Draft",
    "Draft talk", "TimedText", "TimedText talk", "Module", "Module talk", "Book", "Book talk", "Education Program",
    "Education Program talk", "Gadget", "Gadget talk", "Gadget definition", "Gadget definition talk", "Topic", "Image",
    "Image talk", "Special", "Media",
];

pub fn article_like(title: &str) -> bool {
    if let Some((ns, _)) = title.split_once(':') {
        if NAMESPACES.contains(&ns) {
            return false;
        }
    }
    !(title.starts_with("List of") || title.starts_with("Lists of") || title.contains("(disambiguation)"))
}

pub fn wikipedia_index_reserve<R: BufRead>(input: R, lo: u32, hi: u32, out: &Path) -> Result<Value> {
    io::fresh_dir(out)?;
    let mut w = JsonlWriter::create(&out.join("reserved-metadata.jsonl"))?;
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut seen = HashSet::new();
    for line in input.lines() {
        let line = line?;
        let mut parts = line.splitn(3, ':');
        let (Some(off), Some(id), Some(title)) = (parts.next(), parts.next(), parts.next()) else {
            *counts.entry("malformed_lines").or_default() += 1;
            continue;
        };
        *counts.entry("index_lines").or_default() += 1;
        if !article_like(title) {
            *counts.entry("not_article_like").or_default() += 1;
            continue;
        }
        if !seen.insert(id.to_owned()) {
            *counts.entry("repeated_page_id").or_default() += 1;
            continue;
        }
        let key = format!("wikipedia:page:{id}");
        let b = crate::adapter::bucket(&key);
        *counts
            .entry(match b {
                0..100 => "article_like_buckets_0_100",
                100..900 => "article_like_buckets_100_900",
                _ => "article_like_buckets_900_1000",
            })
            .or_default() += 1;
        if lo <= b && b < hi {
            w.write(&json!({"schema": crate::adapter::RESERVED_METADATA_SCHEMA, "family_key": key, "bucket": b, "page_id": id,
                            "index_offset": off, "title_sha256": text::sha_hex(title.as_bytes()), "prose_opened": false}))?;
        }
    }
    let (sha, rows) = w.finish()?;
    let rep = json!({"schema": WIKIPEDIA_INDEX_SCHEMA, "finished_utc": io::now_utc(), "buckets": [lo, hi], "rows": rows,
                     "reserved_metadata_sha256": sha, "counts": counts,
                     "rule": "multistream index only (offset, page id, title); titles kept as SHA-256; namespace prefixes, List of/Lists of and (disambiguation) titles excluded; redirects are not visible in the index and are screened at acquisition",
                     "prose_opened": false});
    io::write_json(&out.join("report.json"), &rep)?;
    Ok(rep)
}

// ---------------------------------------------------------------- config

struct Config {
    raw: Value,
    seed: String,
    out: PathBuf,
    index: PathBuf,
    pools: Vec<PathBuf>,
    gutenberg_reserved: PathBuf,
    gutenberg_yield_works: f64,
    wikipedia_reserved: PathBuf,
    wikipedia_yield_denominator: f64,
    wikinews_scan: PathBuf,
    plos_scan: PathBuf,
    fr_scans: Vec<PathBuf>,
    se_scans: Vec<PathBuf>,
    se_aliases: PathBuf,
    targets: [[f64; 4]; 6],
    caps: [f64; 6],
    oversample: [f64; 6],
    train_quota: [u64; 6],
    fr_max_train_docs: u64,
    reserve_lo: u32,
    excluded: [bool; 6],
    train_cap: [Option<u64>; 6],
    minimum: [f64; 4],
    backfill_domain: Option<usize>,
}

fn cfg_str(v: &Value, k: &str) -> Result<String> {
    v[k].as_str().map(str::to_owned).with_context(|| format!("config: {k}"))
}
fn cfg_paths(v: &Value, k: &str) -> Result<Vec<PathBuf>> {
    v[k].as_array().with_context(|| format!("config: {k}"))?.iter().map(|p| p.as_str().map(PathBuf::from).with_context(|| format!("config: {k}"))).collect()
}
fn per_domain(v: &Value, k: &str) -> Result<[f64; 6]> {
    let mut out = [0.0; 6];
    for (i, d) in DOMAINS.iter().enumerate() {
        out[i] = v[k][d].as_f64().with_context(|| format!("config: {k}.{d}"))?;
    }
    Ok(out)
}

impl Config {
    fn load(path: &Path) -> Result<Self> {
        let raw: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut targets = [[0.0; 4]; 6];
        for (i, d) in DOMAINS.iter().enumerate() {
            for (j, s) in EVAL_SPLITS.iter().enumerate() {
                targets[i][j] = raw["targets"][d][s].as_f64().with_context(|| format!("config: targets.{d}.{s}"))?;
            }
        }
        let quota = per_domain(&raw, "train_quota")?;
        Ok(Config {
            seed: cfg_str(&raw, "seed")?,
            out: PathBuf::from(cfg_str(&raw, "out")?),
            index: PathBuf::from(cfg_str(&raw, "prior_index")?),
            pools: cfg_paths(&raw, "pool_documents")?,
            gutenberg_reserved: PathBuf::from(cfg_str(&raw, "gutenberg_reserved")?),
            gutenberg_yield_works: raw["gutenberg_train_selected_works"].as_f64().context("config: gutenberg_train_selected_works")?,
            wikipedia_reserved: PathBuf::from(cfg_str(&raw, "wikipedia_reserved")?),
            wikipedia_yield_denominator: raw["wikipedia_train_range_article_like_ids"].as_f64().context("config: wikipedia_train_range_article_like_ids")?,
            wikinews_scan: PathBuf::from(cfg_str(&raw, "wikinews_scan")?),
            plos_scan: PathBuf::from(cfg_str(&raw, "plos_scan")?),
            fr_scans: cfg_paths(&raw, "federal_register_scans")?,
            se_scans: if raw["stackexchange_scans"].is_null() { Vec::new() } else { cfg_paths(&raw, "stackexchange_scans")? },
            se_aliases: raw["stackexchange_account_aliases"].as_str().map(PathBuf::from).unwrap_or_default(),
            targets,
            caps: per_domain(&raw, "max_docs_per_group")?,
            oversample: per_domain(&raw, "oversample")?,
            train_quota: quota.map(|q| q as u64),
            fr_max_train_docs: raw["federal_register_max_train_docs_per_evaluation_unit"].as_u64().context("config: federal_register_max_train_docs_per_evaluation_unit")?,
            reserve_lo: raw["reserved_buckets"][0].as_u64().context("config: reserved_buckets")? as u32,
            excluded: {
                let mut e = [false; 6];
                for d in raw["excluded_domains"].as_array().into_iter().flatten() {
                    let d = d.as_str().context("config: excluded_domains")?;
                    let i = DOMAINS.iter().position(|x| *x == d).with_context(|| format!("config: unknown excluded domain {d}"))?;
                    e[i] = true;
                }
                e
            },
            minimum: {
                let mut m = [0.0; 4];
                for (j, sname) in EVAL_SPLITS.iter().enumerate() {
                    m[j] = raw["protocol_minimum_human_docs"][sname].as_f64().unwrap_or(0.0);
                }
                m
            },
            backfill_domain: match raw["backfill_domain"].as_str() {
                Some(d) => Some(DOMAINS.iter().position(|x| *x == d).with_context(|| format!("config: unknown backfill domain {d}"))?),
                None => None,
            },
            train_cap: {
                let mut t = [None; 6];
                for (i, d) in DOMAINS.iter().enumerate() {
                    t[i] = raw["train_max_docs_per_contributor"][d].as_u64();
                }
                t
            },
            raw,
        })
    }
}

// ---------------------------------------------------------------- inputs

fn scan_rows(dir: &Path) -> Result<impl Iterator<Item = Result<Value>>> {
    let rep: Value = serde_json::from_slice(&std::fs::read(dir.join("report.json")).with_context(|| format!("{}", dir.display()))?)?;
    ensure!(rep["complete"] == true && rep["metadata_only"].is_object(), "{} is not a complete metadata-only scan", dir.display());
    ensure!(rep["candidates"] == 0, "{} rendered candidates", dir.display());
    let (sha, _) = io::file_sha(&dir.join("reserved-metadata.jsonl"))?;
    ensure!(rep["metadata_only"]["sha256"] == sha.as_str(), "{} metadata changed since its report", dir.display());
    let r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(dir.join("reserved-metadata.jsonl"))?);
    Ok(r.lines().filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty())).map(|l| Ok(serde_json::from_str::<Value>(&l?)?)))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn se_user_key(site: &str, id: &str) -> Option<String> {
    (!id.is_empty() && id != "-1").then(|| format!("stackexchange:{site}:user:{id}"))
}

/// PLOS author identity: folded name tokens (over-merges namesakes, which can
/// only exclude a group, never admit one).
pub fn author_key(name: &str) -> Option<String> {
    let k = text::underscore_key(name);
    (!k.is_empty()).then(|| format!("plos_author:{k}"))
}

/// Federal Register contributor units: department with each sub-agency, or
/// the department alone.
pub fn fr_units(writer_key: &str, subagencies: &[String]) -> Vec<String> {
    let subs: BTreeSet<String> = subagencies.iter().map(|s| text::underscore_key(s)).filter(|s| !s.is_empty()).collect();
    if subs.is_empty() {
        vec![format!("fr_unit:{writer_key}|-")]
    } else {
        subs.into_iter().map(|s| format!("fr_unit:{writer_key}|{s}")).collect()
    }
}

struct TrainDoc {
    id: String,
    domain: usize,
    family: String,
    component: String,
    writer: Option<String>,
    words: u64,
    hashes: [String; 3],
    rank: u64,
    held: Vec<String>,
}

#[derive(Default)]
struct Yield {
    eligible_train_range_units: u64,
    admitted_docs: u64,
}
impl Yield {
    fn rate(&self) -> f64 {
        if self.eligible_train_range_units == 0 { 0.0 } else { self.admitted_docs as f64 / self.eligible_train_range_units as f64 }
    }
}

fn plos_eligible(m: &Value) -> bool {
    m["plos_doi"] == true && m["license"].is_string() && m["published"].as_str().is_some_and(crate::record::before_cutoff)
}
fn fr_eligible(m: &Value) -> bool {
    !s(m, "agency").is_empty()
        && crate::record::before_cutoff(s(m, "issue_date"))
        && m["fr_doc_number_count_in_input"].as_u64() == Some(1)
        && m["third_party_subject"] != true
}
fn se_post_prescreen(m: &Value) -> bool {
    m["community_owned"] != true
        && se_user_key("x", s(m, "owner_user_id")).is_some()
        && crate::record::before_cutoff(s(m, "creation_date"))
        && m["last_edit_date"].as_str().is_none_or(crate::record::before_cutoff)
}
fn wikinews_eligible(m: &Value) -> bool {
    s(m, "ns") == "0" && m["redirect"] != true && crate::record::before_cutoff(s(m, "timestamp"))
}

// ---------------------------------------------------------------- run

pub fn run(config: &Path) -> Result<Value> {
    let c = Config::load(config)?;
    for d in 0..6 {
        if c.excluded[d] {
            ensure!(c.targets[d].iter().all(|t| *t == 0.0) && c.train_quota[d] == 0, "excluded domain {} must have zero targets and quota", DOMAINS[d]);
        }
    }
    ensure!(!c.excluded[5] || c.se_scans.is_empty(), "Stack Exchange is excluded but scans are configured");
    io::fresh_dir(&c.out)?;
    let started = io::now_utc();
    let lo = c.reserve_lo;
    let index = Index::load(&c.index)?;
    let mut receipts = JsonlWriter::create(&c.out.join("overlap-receipts.jsonl"))?;
    let mut notes: Vec<String> = Vec::new();
    let mut yields: [Yield; 6] = Default::default();

    // Stack Exchange account aliases: per-site user -> network account.
    let mut alias: HashMap<u64, u64> = HashMap::new();
    for r in (!c.se_scans.is_empty()).then(|| io::jsonl_rows(&c.se_aliases)).transpose()?.into_iter().flatten() {
        let r = r?;
        alias.insert(text::h64(s(&r, "writer_key").as_bytes()), text::h64(s(&r, "canonical").as_bytes()));
    }
    let canon = |k: &str| -> u64 {
        let h = text::h64(k.as_bytes());
        alias.get(&h).copied().unwrap_or(h)
    };

    // Stack Exchange scans: last editors of Train-range posts; reserved posts by thread.
    let mut se_editor: HashMap<u64, u64> = HashMap::new();
    let mut se_threads: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut se_seen_sites = 0;
    for dir in &c.se_scans {
        se_seen_sites += 1;
        for r in scan_rows(dir)? {
            let r = r?;
            let site = s(&r, "site").to_owned();
            if r["bucket"].as_u64().unwrap_or(0) < lo as u64 {
                if se_post_prescreen(&r) {
                    yields[5].eligible_train_range_units += 1;
                }
                if let Some(e) = se_user_key(&site, s(&r, "last_editor_user_id")) {
                    se_editor.insert(text::h64(format!("{site}\0{}", s(&r, "post_id")).as_bytes()), canon(&e));
                }
            } else {
                se_threads.entry(s(&r, "family_key").to_owned()).or_default().push(r);
            }
        }
    }

    // Federal Register scans: contributor units of every document.
    let mut fr_meta: HashMap<String, Vec<String>> = HashMap::new();
    let mut fr_reserved: Vec<Value> = Vec::new();
    for dir in &c.fr_scans {
        for r in scan_rows(dir)? {
            let r = r?;
            let subs: Vec<String> = r["subagencies"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            let units = r["writer_key"].as_str().map(|w| fr_units(w, &subs)).unwrap_or_default();
            if r["bucket"].as_u64().unwrap_or(0) < lo as u64 {
                if fr_eligible(&r) {
                    yields[4].eligible_train_range_units += 1;
                }
                fr_meta.insert(s(&r, "family_key").to_owned(), units);
            } else if fr_eligible(&r) {
                fr_reserved.push(r);
            }
        }
    }

    // ------------------------------------------------------------ Train side
    let mut docs: Vec<TrainDoc> = Vec::new();
    let mut se_block: HashSet<u64> = HashSet::new();
    let mut plos_block: HashSet<String> = HashSet::new();
    let mut pg_writers: HashSet<String> = HashSet::new();
    let mut pg_titles: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut fr_dsu = Dsu::default();
    let mut train_families: HashSet<String> = HashSet::new();
    let mut shingles: Vec<(u64, u32)> = Vec::new();
    let mut prior_recheck: BTreeMap<String, u64> = BTreeMap::new();
    let mut fr_unmatched = 0u64;
    for pool in &c.pools {
        for r in io::jsonl_rows(pool)? {
            let r = r?;
            let Some(domain) = domain_of_collection(s(&r, "collection")) else {
                bail!("{}: unknown collection {}", pool.display(), s(&r, "collection"));
            };
            ensure!(!c.excluded[domain], "{}: row of excluded domain {}", pool.display(), DOMAINS[domain]);
            ensure!(r["pool_status"] == "admitted", "{}: row not admitted", pool.display());
            let t = s(&r, "text");
            let id = s(&r, "id").to_owned();
            let family = s(&r, "source_group").to_owned();
            let writer = r["writer_key"].as_str().map(str::to_owned);
            let mut held = Vec::new();
            let mut conflicts = index.text_conflicts(t);
            conflicts.extend(index.family_conflicts(&r));
            for k in &conflicts {
                *prior_recheck.entry((*k).to_owned()).or_default() += 1;
            }
            if !conflicts.is_empty() {
                held.push(format!("prior_overlap_recheck:{}", conflicts.join("+")));
                receipts.write(&json!({"check": "prior_recheck_train_side", "id": id, "family_key": family, "reasons": conflicts, "action": "held_out"}))?;
            }
            yields[domain].admitted_docs += 1;
            match domain {
                0 => {
                    if let Some(w) = &writer {
                        pg_writers.insert(w.clone());
                    }
                    let surname = s(&r["provenance"], "author_key").rsplit('_').next().unwrap_or("").to_owned();
                    for a in r["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        if a.starts_with("title:") {
                            pg_titles.entry(a.to_owned()).or_default().insert(surname.clone());
                        }
                    }
                }
                3 => {
                    for a in r["provenance"]["authors"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        if let Some(k) = author_key(a) {
                            plos_block.insert(k);
                        }
                    }
                }
                4 => match fr_meta.get(&family) {
                    Some(units) => {
                        for u in units {
                            fr_dsu.union(u, u);
                            fr_dsu.union(&units[0], u);
                        }
                    }
                    None => fr_unmatched += 1,
                },
                5 => {
                    if let Some(w) = &writer {
                        se_block.insert(canon(w));
                    }
                    let site = s(&r["provenance"], "site");
                    if let Some(e) = se_editor.get(&text::h64(format!("{site}\0{}", s(&r["provenance"], "post_id")).as_bytes())) {
                        se_block.insert(*e);
                    }
                }
                _ => {}
            }
            let di = docs.len() as u32;
            for k in text::text_keys(t) {
                shingles.push((k, di));
            }
            let mut sh = text::shingles_folded(t);
            sh.sort_unstable();
            sh.dedup();
            shingles.extend(sh.into_iter().map(|k| (k, di)));
            train_families.insert(family.clone());
            docs.push(TrainDoc {
                rank: rank(&c.seed, "train_document", &id),
                id,
                domain,
                component: r["pool_admission"]["split_component"].as_str().unwrap_or(&family).to_owned(),
                family,
                writer,
                words: r["word_count"].as_u64().unwrap_or(0),
                hashes: [s(&r, "text_sha256").to_owned(), s(&r, "normalized_text_sha256").to_owned(), s(&r, "folded_text_sha256").to_owned()],
                held,
            });
        }
    }
    ensure!(docs.len() < u32::MAX as usize, "too many documents");
    if fr_unmatched > 0 {
        notes.push(format!("{fr_unmatched} admitted Federal Register documents had no scan row; their contributor unit is unknown and they never join an evaluation unit"));
    }

    // Cross-source duplicates among Train-side documents: keys shared by
    // documents of different domains (within-source pairs were handled by
    // each pool's dedup). The first document in rank order is kept.
    shingles.sort_unstable();
    let mut links: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut cross_keys = 0u64;
    let mut i = 0;
    while i < shingles.len() {
        let mut j = i + 1;
        while j < shingles.len() && shingles[j].0 == shingles[i].0 {
            j += 1;
        }
        if j - i > 1 {
            let grp = &shingles[i..j];
            let d0 = docs[grp[0].1 as usize].domain;
            if grp.iter().any(|x| docs[x.1 as usize].domain != d0) {
                cross_keys += 1;
                for a in grp {
                    for b in grp {
                        if docs[a.1 as usize].domain != docs[b.1 as usize].domain {
                            links.entry(a.1).or_default().push(b.1);
                        }
                    }
                }
            }
        }
        i = j;
    }
    drop(shingles);
    let mut linked: Vec<u32> = links.keys().copied().collect();
    linked.sort_unstable_by_key(|d| (docs[*d as usize].rank, *d));
    let mut kept: HashSet<u32> = HashSet::new();
    let mut cross_excluded = 0u64;
    for d in linked {
        let ks = links.get(&d).cloned().unwrap_or_default();
        if let Some(k) = ks.iter().find(|k| kept.contains(k)) {
            let (kid, kdom) = (docs[*k as usize].id.clone(), DOMAINS[docs[*k as usize].domain]);
            let doc = &mut docs[d as usize];
            doc.held.push("cross_source_duplicate".to_owned());
            cross_excluded += 1;
            receipts.write(&json!({"check": "cross_source_train_side", "id": doc.id, "domain": DOMAINS[doc.domain], "kept_id": kid,
                                   "kept_domain": kdom, "rule": "shares an exact, normalized or folded text hash or a folded 50-word sequence with an earlier-ranked document of another source", "action": "held_out"}))?;
        } else {
            kept.insert(d);
        }
    }

    // ------------------------------------------------------------ evaluation universe
    let mut groups: [Vec<Group>; 6] = Default::default();
    let mut blocked: [BTreeMap<String, u64>; 6] = Default::default();
    let mut universe_units = [0u64; 6];
    let fam_conflict = |key: &str| index.families.contains(key);
    let fam_reasons = |key: &str| -> Vec<&'static str> {
        let mut r = Vec::new();
        if index.families.contains(key) {
            r.push("family_key_in_prior_snapshot");
        }
        if train_families.contains(key) {
            r.push("family_key_in_train_side_pool");
        }
        r
    };

    // Gutenberg: writer groups from the metadata-only plan.
    yields[0].eligible_train_range_units = c.gutenberg_yield_works as u64;
    {
        let rate = yields[0].rate();
        let mut dsu = Dsu::default();
        let mut rows = Vec::new();
        for r in io::jsonl_rows(&c.gutenberg_reserved)? {
            let r = r?;
            if s(&r, "role") != "reserved_evaluation" {
                continue;
            }
            ensure!(r["prose_downloaded"] == false, "reserved Gutenberg work {} was downloaded", r["pg_id"]);
            let w = s(&r, "writer_key").to_owned();
            dsu.union(&w, &w);
            for a in r["creator_agent_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                dsu.union(&w, &format!("pg_agent:{a}"));
            }
            rows.push(r);
        }
        let mut by: BTreeMap<String, Group> = BTreeMap::new();
        let mut bad: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for r in rows {
            universe_units[0] += 1;
            let w = s(&r, "writer_key").to_owned();
            let g = dsu.root_key(&w);
            let fam = s(&r, "source_group").to_owned();
            let pg = r["pg_id"].as_u64().unwrap_or(0);
            let aliases = text::title_aliases(s(&r, "title"));
            let surname = w.rsplit('_').next().unwrap_or("").to_owned();
            let mut why = Vec::new();
            if index.pg_ids.contains(&pg) {
                why.push("pg_id_in_prior_snapshot".to_owned());
            }
            if fam_conflict(&fam) {
                why.push("family_key_in_prior_snapshot".to_owned());
            }
            if aliases.iter().any(|a| index.titles.contains(a)) {
                why.push("title_alias_in_prior_snapshot".to_owned());
            }
            if pg_writers.contains(&w) {
                why.push("writer_in_train_side_pool".to_owned());
            }
            if train_families.contains(&fam) {
                why.push("family_in_train_side_pool".to_owned());
            }
            if aliases.iter().any(|a| pg_titles.get(a).is_some_and(|sn| sn.contains(&surname))) {
                why.push("title_alias_and_surname_match_train_side_work".to_owned());
            }
            if !why.is_empty() {
                bad.entry(g.clone()).or_default().extend(why);
            }
            let e = by.entry(g.clone()).or_insert_with(|| Group { key: g.clone(), rank: rank(&c.seed, DOMAINS[0], &g), units: vec![], contributors: BTreeSet::new(), train_side_docs: 0 });
            e.contributors.insert(w.clone());
            e.units.push(Unit {
                family_key: fam,
                expected_docs: rate,
                meta: json!({"pg_id": pg, "writer_key": w, "writer_bucket": r["writer_bucket"], "family_bucket": r["family_bucket"],
                             "stratum": r["stratum"], "rdf_sha256": r["rdf_sha256"], "pg_ebook_release_date": r["pg_ebook_release_date"],
                             "rights_statement": r["rights_statement"], "acquisition": "download pgN.txt and run the gutenberg adapter on a manifest of this split's works"}),
            });
        }
        for (g, why) in &bad {
            *blocked[0].entry("overlap_conflict_whole_group_excluded".into()).or_default() += 1;
            receipts.write(&json!({"check": "reservation_overlap", "domain": DOMAINS[0], "group_key": g, "reasons": why.iter().collect::<BTreeSet<_>>(), "action": "group_excluded"}))?;
            by.remove(g);
        }
        groups[0] = by.into_values().collect();
    }

    // Wikipedia: one group per article (page id is stable across moves).
    yields[1].eligible_train_range_units = c.wikipedia_yield_denominator as u64;
    {
        let rate = yields[1].rate();
        for r in io::jsonl_rows(&c.wikipedia_reserved.join("reserved-metadata.jsonl"))? {
            let r = r?;
            let fam = s(&r, "family_key").to_owned();
            if r["bucket"].as_u64().unwrap_or(0) < lo as u64 {
                continue;
            }
            universe_units[1] += 1;
            if fam_conflict(&fam) || train_families.contains(&fam) {
                *blocked[1].entry("overlap_conflict_whole_group_excluded".into()).or_default() += 1;
                receipts.write(&json!({"check": "reservation_overlap", "domain": DOMAINS[1], "group_key": fam, "reasons": fam_reasons(&fam), "action": "group_excluded"}))?;
                continue;
            }
            groups[1].push(Group {
                key: fam.clone(),
                rank: rank(&c.seed, DOMAINS[1], &fam),
                units: vec![Unit { family_key: fam.clone(), expected_docs: rate,
                    meta: json!({"page_id": r["page_id"], "bucket": r["bucket"], "index_offset": r["index_offset"], "title_sha256": r["title_sha256"],
                                 "acquisition": "wikipedia adapter on the enwiki-20220301 multistream dump with --families"}) }],
                contributors: BTreeSet::new(),
                train_side_docs: 0,
            });
        }
    }

    // Wikinews: one group per article.
    {
        let mut rows = Vec::new();
        for r in scan_rows(&c.wikinews_scan)? {
            let r = r?;
            if !wikinews_eligible(&r) {
                continue;
            }
            if r["bucket"].as_u64().unwrap_or(0) < lo as u64 {
                yields[2].eligible_train_range_units += 1;
            } else {
                rows.push(r);
            }
        }
        let rate = yields[2].rate();
        for r in rows {
            universe_units[2] += 1;
            let fam = s(&r, "family_key").to_owned();
            if fam_conflict(&fam) || train_families.contains(&fam) {
                *blocked[2].entry("overlap_conflict_whole_group_excluded".into()).or_default() += 1;
                receipts.write(&json!({"check": "reservation_overlap", "domain": DOMAINS[2], "group_key": fam, "reasons": fam_reasons(&fam), "action": "group_excluded"}))?;
                continue;
            }
            groups[2].push(Group {
                key: fam.clone(),
                rank: rank(&c.seed, DOMAINS[2], &fam),
                units: vec![Unit { family_key: fam.clone(), expected_docs: rate,
                    meta: json!({"page_id": r["page_id"], "revision_id": r["revision_id"], "revision_text_sha256": r["revision_text_sha256"],
                                 "revision_sha1_base36": r["revision_sha1_base36"], "timestamp": r["timestamp"], "title_sha256": r["title_sha256"],
                                 "acquisition": "wikinews adapter on enwikinews-20220501 with --families"}) }],
                contributors: BTreeSet::new(),
                train_side_docs: 0,
            });
        }
    }

    // PLOS: author-name closure; any author with Train-side text blocks the article.
    {
        let mut rows = Vec::new();
        for r in scan_rows(&c.plos_scan)? {
            let r = r?;
            if !plos_eligible(&r) {
                continue;
            }
            if r["bucket"].as_u64().unwrap_or(0) < lo as u64 {
                yields[3].eligible_train_range_units += 1;
            } else {
                rows.push(r);
            }
        }
        let rate = yields[3].rate();
        let mut dsu = Dsu::default();
        let mut keep = Vec::new();
        for r in rows {
            universe_units[3] += 1;
            let fam = s(&r, "family_key").to_owned();
            let authors: Vec<String> = r["authors"].as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(author_key).collect();
            if fam_conflict(&fam) || train_families.contains(&fam) {
                *blocked[3].entry("overlap_conflict_whole_group_excluded".into()).or_default() += 1;
                receipts.write(&json!({"check": "reservation_overlap", "domain": DOMAINS[3], "group_key": fam, "reasons": fam_reasons(&fam), "action": "unit_excluded"}))?;
                continue;
            }
            if authors.iter().any(|a| plos_block.contains(a)) {
                *blocked[3].entry("author_has_train_side_text".into()).or_default() += 1;
                continue;
            }
            dsu.union(&fam, &fam);
            for a in &authors {
                dsu.union(&fam, a);
            }
            keep.push((r, authors));
        }
        let mut by: BTreeMap<String, Group> = BTreeMap::new();
        for (r, authors) in keep {
            let fam = s(&r, "family_key").to_owned();
            let g = dsu.root_key(&fam);
            let e = by.entry(g.clone()).or_insert_with(|| Group { key: g.clone(), rank: rank(&c.seed, DOMAINS[3], &g), units: vec![], contributors: BTreeSet::new(), train_side_docs: 0 });
            e.contributors.extend(authors.iter().cloned());
            e.units.push(Unit { family_key: fam, expected_docs: rate,
                meta: json!({"doi": r["doi"], "file_name": r["file_name"], "file_sha256": r["file_sha256"], "journal": r["journal"],
                             "published": r["published"], "license": r["license"], "author_count": authors.len(), "title_sha256": r["title_sha256"],
                             "acquisition": "plos adapter on allofplos.zip (--include-introduction) with --families"}) });
        }
        groups[3] = by.into_values().collect();
    }

    // Federal Register: department/sub-agency units; closure over Train-side and reserved documents.
    {
        let rate = yields[4].rate();
        for r in &fr_reserved {
            let subs: Vec<String> = r["subagencies"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            let units = fr_units(s(r, "writer_key"), &subs);
            for u in &units {
                fr_dsu.union(&units[0], u);
            }
        }
        let mut train_by: HashMap<String, u64> = HashMap::new();
        for d in docs.iter().filter(|d| d.domain == 4) {
            if let Some(units) = fr_meta.get(&d.family) {
                *train_by.entry(fr_dsu.root_key(&units[0])).or_default() += 1;
            }
        }
        let mut by: BTreeMap<String, Group> = BTreeMap::new();
        for r in &fr_reserved {
            universe_units[4] += 1;
            let fam = s(r, "family_key").to_owned();
            if fam_conflict(&fam) || train_families.contains(&fam) {
                *blocked[4].entry("overlap_conflict_whole_group_excluded".into()).or_default() += 1;
                receipts.write(&json!({"check": "reservation_overlap", "domain": DOMAINS[4], "group_key": fam, "reasons": fam_reasons(&fam), "action": "unit_excluded"}))?;
                continue;
            }
            let subs: Vec<String> = r["subagencies"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
            let units = fr_units(s(r, "writer_key"), &subs);
            let g = fr_dsu.root_key(&units[0]);
            let tsd = train_by.get(&g).copied().unwrap_or(0);
            let e = by.entry(g.clone()).or_insert_with(|| Group { key: g.clone(), rank: rank(&c.seed, DOMAINS[4], &g), units: vec![], contributors: BTreeSet::new(), train_side_docs: tsd });
            e.contributors.extend(units.iter().cloned());
            e.units.push(Unit { family_key: fam, expected_docs: rate,
                meta: json!({"fr_doc": r["fr_doc"], "agency": r["agency"], "subagencies": r["subagencies"], "document_type": r["document_type"],
                             "issue_date": r["issue_date"], "file_name": r["file_name"], "file_sha256": r["file_sha256"], "subject_sha256": r["subject_sha256"],
                             "corpus": r["corpus"], "acquisition": "federal-register adapter on the govinfo yearly zip with --families"}) });
        }
        let (mut ok, mut too_big) = (Vec::new(), 0u64);
        for g in by.into_values() {
            if g.train_side_docs > c.fr_max_train_docs {
                too_big += 1;
                *blocked[4].entry("unit_has_too_many_train_side_documents".into()).or_default() += g.units.len() as u64;
            } else {
                ok.push(g);
            }
        }
        notes.push(format!("Federal Register: {too_big} contributor components exceed {} Train-side documents and are not evaluation candidates", c.fr_max_train_docs));
        groups[4] = ok;
    }

    // Stack Exchange: threads whose every owner and last editor is absent from Train-side text.
    {
        let rate = yields[5].rate();
        let mut dsu = Dsu::default();
        let mut keep = Vec::new();
        for (thread, posts) in &se_threads {
            universe_units[5] += 1;
            if fam_conflict(thread) || train_families.contains(thread) {
                *blocked[5].entry("overlap_conflict_whole_group_excluded".into()).or_default() += 1;
                receipts.write(&json!({"check": "reservation_overlap", "domain": DOMAINS[5], "group_key": thread, "reasons": fam_reasons(thread), "action": "thread_excluded"}))?;
                continue;
            }
            let mut contributors = BTreeSet::new();
            let mut exp = 0.0;
            for p in posts {
                let site = s(p, "site");
                for k in ["owner_user_id", "last_editor_user_id"] {
                    if let Some(u) = se_user_key(site, s(p, k)) {
                        contributors.insert(canon(&u));
                    }
                }
                if se_post_prescreen(p) {
                    exp += rate;
                }
            }
            if contributors.iter().any(|a| se_block.contains(a)) {
                *blocked[5].entry("contributor_has_train_side_text".into()).or_default() += 1;
                continue;
            }
            if exp == 0.0 {
                *blocked[5].entry("no_post_passes_prescreen".into()).or_default() += 1;
                continue;
            }
            dsu.union(thread, thread);
            for a in &contributors {
                dsu.union(thread, &format!("se_account:{a:016x}"));
            }
            keep.push((thread.clone(), posts, contributors, exp));
        }
        let mut by: BTreeMap<String, Group> = BTreeMap::new();
        for (thread, posts, contributors, exp) in keep {
            let g = dsu.root_key(&thread);
            let e = by.entry(g.clone()).or_insert_with(|| Group { key: g.clone(), rank: rank(&c.seed, DOMAINS[5], &g), units: vec![], contributors: BTreeSet::new(), train_side_docs: 0 });
            e.contributors.extend(contributors.iter().map(|a| format!("se_account:{a:016x}")));
            let ids: Vec<Value> = posts.iter().map(|p| json!({"post_id": p["post_id"], "post_type": p["post_type"], "body_html_sha256": p["body_html_sha256"], "prescreen_pass": se_post_prescreen(p)})).collect();
            e.units.push(Unit { family_key: thread.clone(), expected_docs: exp,
                meta: json!({"site": posts[0]["site"], "thread_id": posts[0]["thread_id"], "dump_sha256": posts[0]["dump_sha256"], "posts": ids,
                             "acquisition": "stackexchange adapter on the 2022-06-06 site dump with --families"}) });
        }
        groups[5] = by.into_values().collect();
    }

    // ------------------------------------------------------------ assignment
    let mut assigned: [[Vec<Group>; 4]; 6] = Default::default();
    let mut spare: [Vec<Group>; 6] = Default::default();
    for d in 0..6 {
        let mut gs = std::mem::take(&mut groups[d]);
        gs.sort_by(|a, b| (a.rank, &a.key).cmp(&(b.rank, &b.key)));
        let exp: Vec<f64> = gs.iter().map(|g| g.expected(c.caps[d])).collect();
        let alloc = allocate(&exp, &c.targets[d], c.oversample[d]);
        for (g, a) in gs.into_iter().zip(alloc) {
            match a {
                Some(sx) => assigned[d][sx].push(g),
                None => spare[d].push(g),
            }
        }
    }

    // Federal Register units chosen for evaluation hold out their Train-side documents.
    let mut fr_eval_roots: HashSet<String> = HashSet::new();
    for sx in 0..4 {
        for g in &assigned[4][sx] {
            fr_eval_roots.insert(g.key.clone());
        }
    }
    let mut fr_held = 0u64;
    for d in docs.iter_mut().filter(|d| d.domain == 4) {
        if let Some(units) = fr_meta.get(&d.family) {
            if fr_eval_roots.contains(&fr_dsu.root_key(&units[0])) {
                d.held.push("federal_register_unit_reserved_for_evaluation".to_owned());
                fr_held += 1;
            }
        }
    }

    // Cross-split contributor and family disjointness (by construction; verified).
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut cross_split_conflicts = 0u64;
    for d in 0..6 {
        for sx in 0..4 {
            for g in &assigned[d][sx] {
                for k in g.contributors.iter().cloned().chain(g.units.iter().map(|u| u.family_key.clone())) {
                    if let Some(prev) = owner.insert(k.clone(), sx) {
                        if prev != sx {
                            cross_split_conflicts += 1;
                        }
                    }
                }
            }
        }
    }
    ensure!(cross_split_conflicts == 0, "evaluation splits share {cross_split_conflicts} contributor or family keys");
    let fr_contrib_in_train: u64 = {
        let mut n = 0;
        for d in docs.iter().filter(|d| d.domain == 4 && d.held.is_empty()) {
            if let Some(units) = fr_meta.get(&d.family) {
                if units.iter().any(|u| owner.contains_key(u)) {
                    n += 1;
                }
            }
        }
        n
    };
    ensure!(fr_contrib_in_train == 0, "{fr_contrib_in_train} Train-side Federal Register documents share an evaluation unit");

    // ------------------------------------------------------------ Train / Train-mining
    let mut comp: BTreeMap<(usize, String), Vec<u32>> = BTreeMap::new();
    for (i, d) in docs.iter().enumerate() {
        if d.held.is_empty() {
            comp.entry((d.domain, d.component.clone())).or_default().push(i as u32);
        }
    }
    let mut comps: Vec<((usize, String), Vec<u32>)> = comp.into_iter().collect();
    comps.sort_by_key(|((d, k), _)| (*d, rank(&c.seed, "train_component", k), k.clone()));
    // Contributor key for the Train caps: PG writer, Federal Register
    // primary office unit (department and first sub-agency).
    let cap_key = |d: &TrainDoc| -> Option<String> {
        match d.domain {
            4 => fr_meta.get(&d.family).and_then(|u| u.first().cloned()),
            _ => d.writer.clone(),
        }
    };
    let mut in_train: HashSet<u32> = HashSet::new();
    let mut train_n = [0u64; 6];
    let mut per_contrib: HashMap<String, u64> = HashMap::new();
    let mut cap_skipped = [0u64; 6];
    for ((d, _), members) in &comps {
        if train_n[*d] + members.len() as u64 > c.train_quota[*d] {
            continue;
        }
        if let Some(cap) = c.train_cap[*d] {
            let mut need: HashMap<String, u64> = HashMap::new();
            for m in members {
                if let Some(k) = cap_key(&docs[*m as usize]) {
                    *need.entry(k).or_default() += 1;
                }
            }
            if need.iter().any(|(k, n)| per_contrib.get(k).copied().unwrap_or(0) + n > cap) {
                cap_skipped[*d] += members.len() as u64;
                continue;
            }
            for (k, n) in need {
                *per_contrib.entry(k).or_default() += n;
            }
        }
        train_n[*d] += members.len() as u64;
        in_train.extend(members.iter().copied());
    }
    // Contributor concentration in Train: distinct keys, the largest share
    // and the effective number (inverse Simpson index).
    let mut concentration = serde_json::Map::new();
    for d in 0..6 {
        let mut by: HashMap<String, u64> = HashMap::new();
        for i in in_train.iter() {
            let doc = &docs[*i as usize];
            if doc.domain == d {
                if let Some(k) = cap_key(doc) {
                    *by.entry(k).or_default() += 1;
                }
            }
        }
        let n: u64 = by.values().sum();
        if n == 0 {
            continue;
        }
        let simpson: f64 = by.values().map(|&x| (x as f64 / n as f64).powi(2)).sum();
        concentration.insert(DOMAINS[d].to_owned(), json!({"key": if d == 4 { "federal_register_primary_office_unit" } else { "writer_key" },
            "documents_with_key": n, "distinct_keys": by.len(), "max_docs_per_key": by.values().max(),
            "effective_number_inverse_simpson": (1.0 / simpson * 10.0).round() / 10.0, "cap": c.train_cap[d],
            "documents_skipped_by_cap": cap_skipped[d]}));
    }

    // ------------------------------------------------------------ outputs
    let out = &c.out;
    let mut summary_eval = serde_json::Map::new();
    let mut shortfalls: Vec<Value> = Vec::new();
    for (sx, sname) in EVAL_SPLITS.iter().enumerate() {
        let dir = out.join(sname.replace('_', "-"));
        io::fresh_dir(&dir)?;
        let mut units_w = JsonlWriter::create(&dir.join("manifest.jsonl"))?;
        let mut groups_w = JsonlWriter::create(&dir.join("groups.jsonl"))?;
        let mut fams = String::new();
        let mut per = serde_json::Map::new();
        let (mut tot_groups, mut tot_units, mut tot_exp, mut tot_target, mut tot_capacity) = (0u64, 0u64, 0.0f64, 0.0f64, 0.0f64);
        let status = if *sname == "dev_iter" { "not_acquired_acquirable_now_exposed_by_design" } else if *sname == "test" { "sealed_acquire_only_after_protocol_seal" } else { "sealed_not_acquired_until_protocol_step" };
        for d in 0..6 {
            let gs = &assigned[d][sx];
            let cap = c.caps[d];
            let target = c.targets[d][sx];
            let mut exp_sum = 0.0;
            let mut nunits = 0u64;
            let mut groups_at_target = 0.0f64;
            for (order, g) in gs.iter().enumerate() {
                let e = g.expected(cap);
                if exp_sum < target {
                    groups_at_target += e.min(1.0);
                }
                exp_sum += e;
                nunits += g.units.len() as u64;
                groups_w.write(&json!({"schema": GROUP_SCHEMA, "split": sname, "domain": DOMAINS[d], "group_key": g.key, "group_rank": format!("{:016x}", g.rank),
                    "order_in_split_domain": order, "units": g.units.len(), "expected_docs_capped": e, "max_docs_per_group": cap,
                    "contributor_key_sha256": g.contributors.iter().map(|k| text::sha_hex(k.as_bytes())).collect::<Vec<_>>(),
                    "federal_register_train_side_docs_held": if d == 4 { json!(g.train_side_docs) } else { json!(null) }}))?;
                let mut us: Vec<&Unit> = g.units.iter().collect();
                us.sort_by_key(|u| (rank(&c.seed, "unit", &u.family_key), u.family_key.clone()));
                for (uo, u) in us.into_iter().enumerate() {
                    units_w.write(&json!({"schema": RESERVATION_SCHEMA, "split": sname, "domain": DOMAINS[d], "group_key": g.key,
                        "group_order": order, "unit_order_in_group": uo, "family_key": u.family_key, "bucket": crate::adapter::bucket(&u.family_key),
                        "expected_docs": u.expected_docs, "source_unit": u.meta, "prose_status": status, "text_sha256": null,
                        "text_hashes_note": "excerpt text hashes are recorded at acquisition; source-unit hashes above bind the reserved inputs"}))?;
                    fams.push_str(&u.family_key);
                    fams.push('\n');
                }
            }
            let reached = exp_sum.min(target);
            let issue = if c.excluded[d] {
                None
            } else if exp_sum < target {
                Some("expected capacity below target")
            } else if *sname == "test" && (groups_at_target < 15.0 || exp_sum < 150.0) {
                Some("below Test slice gate (15 groups, 150 human documents)")
            } else if exp_sum < 1.15 * target {
                Some("expected capacity within 15% of target; acquisition can fall short (binomial yield)")
            } else {
                None
            };
            if let Some(issue) = issue {
                shortfalls.push(json!({"split": sname, "domain": DOMAINS[d], "target_human_docs": target, "expected_capped_capacity": exp_sum, "groups": gs.len(),
                    "expected_groups_with_documents_at_target_approx": groups_at_target, "issue": issue}));
            }
            per.insert(DOMAINS[d].to_owned(), json!({"groups": gs.len(), "source_units": nunits, "target_human_docs": target,
                "expected_groups_with_documents_at_target_approx": groups_at_target,
                "expected_capped_capacity": exp_sum, "expected_docs_at_target": reached, "max_docs_per_group": cap,
                "reserve_oversample": c.oversample[d]}));
            tot_groups += gs.len() as u64;
            tot_units += nunits;
            tot_exp += reached;
            tot_target += target;
            tot_capacity += exp_sum;
        }
        let (usha, urows) = units_w.finish()?;
        let (gsha, grows) = groups_w.finish()?;
        std::fs::write(dir.join("families.txt"), &fams)?;
        let seal = json!({"split": sname, "prose_status": status, "prose_opened": false, "text_read_or_printed": false,
            "rule": match *sname {
                "test" => "Test prose is acquired only after the protocol seal (protocol-v1 splits.rules); manifest is metadata only",
                "dev_iter" => "Dev-iter is exposed by design and may be acquired for iterative decisions; nothing is acquired by this reservation",
                "dev_select" => "Dev-select prose stays sealed until finalists are bound; opened once to choose among at most four finalists",
                _ => "CAL prose stays sealed until the protocol calibration step",
            },
            "acquisition_rule": match c.backfill_domain {
                Some(b) => json!(format!("process groups in group_order; within a group take units in unit_order_in_group; keep at most max_docs_per_group passing documents per group; stop a domain at its target; record every skip. Backfill (declared before any prose is opened): if a domain ends below its target after its last reserved group, the shortfall is taken from this split's {} groups that follow the {} target, in group_order, until the split total reaches the sum of its domain targets. Unreached groups stay reserved and unused (never Train, never mined, never mirror topics)", DOMAINS[b], DOMAINS[b])),
                None => json!("process groups in group_order; within a group take units in unit_order_in_group; keep at most max_docs_per_group passing documents per group; stop a domain at its target; record every skip; unreached groups stay reserved and unused (never Train, never mined, never mirror topics)"),
            },
            "manifest_sha256": usha, "manifest_rows": urows, "groups_sha256": gsha, "groups_rows": grows});
        io::write_json(&dir.join("SEALED.json"), &seal)?;
        let minimum = c.minimum[sx];
        if minimum > 0.0 && tot_capacity < 1.3 * minimum {
            shortfalls.push(json!({"split": sname, "issue": "expected capacity below 1.3x the protocol minimum", "expected_capped_capacity": tot_capacity, "protocol_minimum_human_docs": minimum}));
        }
        if minimum > 0.0 && tot_target < minimum {
            shortfalls.push(json!({"split": sname, "issue": "sum of domain targets below the protocol minimum", "target_human_docs": tot_target, "protocol_minimum_human_docs": minimum}));
        }
        summary_eval.insert((*sname).to_owned(), json!({"groups": tot_groups, "source_units": tot_units, "target_human_docs": tot_target,
            "expected_docs_at_target": tot_exp, "expected_capped_capacity": tot_capacity, "protocol_minimum_human_docs": minimum,
            "capacity_over_minimum": if minimum > 0.0 { json!((tot_capacity / minimum * 1000.0).round() / 1000.0) } else { json!(null) },
            "capacity_over_target": (tot_capacity / tot_target.max(1.0) * 1000.0).round() / 1000.0, "domains_covered": per.values().filter(|v| v["groups"].as_u64().unwrap_or(0) > 0).count(),
            "by_domain": per, "manifest_sha256": usha, "prose_status": status}));
    }
    // Spare reservations: never Train, never mined.
    {
        let dir = out.join("reserved-unassigned");
        io::fresh_dir(&dir)?;
        let mut w = JsonlWriter::create(&dir.join("groups.jsonl"))?;
        let mut per = serde_json::Map::new();
        for d in 0..6 {
            let n: u64 = spare[d].iter().map(|g| g.units.len() as u64).sum();
            if d == 1 {
                per.insert(DOMAINS[d].to_owned(), json!({"groups": spare[d].len(), "source_units": n,
                    "rows_written": false, "rule": "every other article-like page id in buckets 900..1000 of the reservation index; sealed and unused"}));
                continue;
            }
            for g in &spare[d] {
                w.write(&json!({"schema": GROUP_SCHEMA, "split": "reserved_unassigned", "domain": DOMAINS[d], "group_key": g.key,
                    "group_rank": format!("{:016x}", g.rank), "units": g.units.len(), "family_keys": g.units.iter().map(|u| &u.family_key).collect::<Vec<_>>()}))?;
            }
            per.insert(DOMAINS[d].to_owned(), json!({"groups": spare[d].len(), "source_units": n}));
        }
        let (sha, rows) = w.finish()?;
        summary_eval.insert("reserved_unassigned".into(), json!({"by_domain": per, "groups_sha256": sha, "rows": rows,
            "rule": "reserved-bucket families not needed for targets; sealed and unused; never Train or mining"}));
    }

    // Train-side manifests (no text; join pool documents by id).
    let mut train_summary = serde_json::Map::new();
    for name in ["train", "train_mining", "held"] {
        let dir = out.join(name.replace('_', "-"));
        io::fresh_dir(&dir)?;
        let mut w = JsonlWriter::create(&dir.join("manifest.jsonl"))?;
        let mut per: BTreeMap<&str, (u64, HashSet<&str>, HashSet<&str>, HashSet<&str>, u64)> = BTreeMap::new();
        let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
        let mut order: Vec<u32> = (0..docs.len() as u32)
            .filter(|i| {
                let d = &docs[*i as usize];
                match name {
                    "held" => !d.held.is_empty(),
                    "train" => d.held.is_empty() && in_train.contains(i),
                    _ => d.held.is_empty() && !in_train.contains(i),
                }
            })
            .collect();
        order.sort_by_key(|i| (docs[*i as usize].domain, docs[*i as usize].rank, *i));
        for i in order {
            let d = &docs[i as usize];
            let e = per.entry(DOMAINS[d.domain]).or_default();
            e.0 += 1;
            e.1.insert(&d.family);
            e.2.insert(&d.component);
            if let Some(w) = &d.writer {
                e.3.insert(w);
            }
            e.4 += d.words;
            for r in &d.held {
                *reasons.entry(r.split(':').next().unwrap_or(r).to_owned()).or_default() += 1;
            }
            w.write(&json!({"schema": DOCUMENT_SCHEMA, "split": name, "id": d.id, "domain": DOMAINS[d.domain], "source_group": d.family,
                "split_component": d.component, "writer_key": d.writer, "word_count": d.words, "text_sha256": d.hashes[0],
                "normalized_text_sha256": d.hashes[1], "folded_text_sha256": d.hashes[2], "rank": format!("{:016x}", d.rank),
                "held_reasons": if d.held.is_empty() { json!(null) } else { json!(d.held) }}))?;
        }
        let (sha, rows) = w.finish()?;
        let by: serde_json::Map<String, Value> = per
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!({"documents": v.0, "source_families": v.1.len(), "split_components": v.2.len(), "writer_keys": v.3.len(), "mean_words": v.4 as f64 / v.0.max(1) as f64})))
            .collect();
        train_summary.insert(name.to_owned(), json!({"documents": rows, "manifest_sha256": sha, "by_domain": by,
            "held_reasons": if name == "held" { json!(reasons) } else { json!(null) }}));
    }
    // Text-bearing Train and Train-mining documents: the only files trainers
    // and assembly read. Held documents never appear here.
    {
        let mut split_of: HashMap<&str, &'static str> = HashMap::new();
        for (i, d) in docs.iter().enumerate() {
            if d.held.is_empty() {
                split_of.insert(d.id.as_str(), if in_train.contains(&(i as u32)) { "train" } else { "train_mining" });
            }
        }
        let mut wt = JsonlWriter::create(&out.join("train").join("documents.jsonl"))?;
        let mut wm = JsonlWriter::create(&out.join("train-mining").join("documents.jsonl"))?;
        for pool in &c.pools {
            for r in io::jsonl_rows(pool)? {
                let mut r = r?;
                let Some(&sp) = split_of.get(s(&r, "id")) else { continue };
                r["frozen_split"] = json!(sp);
                r["split_assigned"] = json!(true);
                r["split_assignment"] = json!({"schema": DOCUMENT_SCHEMA, "split": sp, "seed": c.seed,
                    "rule": "read only splits train/documents.jsonl or train-mining/documents.jsonl; pool files include held documents and are not training inputs"});
                if sp == "train" { wt.write(&r)? } else { wm.write(&r)? };
            }
        }
        let (tsha, trows) = wt.finish()?;
        let (msha, mrows) = wm.finish()?;
        ensure!(trows == train_summary["train"]["documents"].as_u64().unwrap_or(u64::MAX), "train documents.jsonl row count differs from its manifest");
        ensure!(mrows == train_summary["train_mining"]["documents"].as_u64().unwrap_or(u64::MAX), "train-mining documents.jsonl row count differs from its manifest");
        train_summary["train"]["documents_jsonl_sha256"] = json!(tsha);
        train_summary["train_mining"]["documents_jsonl_sha256"] = json!(msha);
        train_summary.insert("contributor_concentration_train".into(), json!(concentration));
        train_summary.insert("reading_rule".into(), json!("Trainers, generators and assembly read only train/documents.jsonl and train-mining/documents.jsonl (text rows carry frozen_split) or join the manifests by id; never the pool files, which still contain held documents."));
    }
    let (rsha, rrows) = receipts.finish()?;

    let yields_v: serde_json::Map<String, Value> = (0..6)
        .map(|d| (DOMAINS[d].to_owned(), json!({"eligible_train_range_units": yields[d].eligible_train_range_units, "admitted_train_side_docs": yields[d].admitted_docs, "docs_per_unit": yields[d].rate()})))
        .collect();
    let universe: serde_json::Map<String, Value> = (0..6)
        .map(|d| (DOMAINS[d].to_owned(), json!({"reserved_units_seen": universe_units[d], "blocked": blocked[d],
             "eligible_groups": assigned[d].iter().map(Vec::len).sum::<usize>() + spare[d].len()})))
        .collect();
    let summary = json!({
        "schema": SUMMARY_SCHEMA,
        "started_utc": started,
        "finished_utc": io::now_utc(),
        "config": c.raw,
        "seed": c.seed,
        "excluded_domains": (0..6).filter(|d| c.excluded[*d]).map(|d| DOMAINS[d]).collect::<Vec<_>>(),
        "rank_rule": format!("first 8 bytes of SHA-256(\"{}\\0\" + seed + \"\\0\" + scope + \"\\0\" + key), big-endian, ascending", RANK_DOMAIN.trim_end_matches('\0')),
        "prior_index_sha256": index.sha256,
        "evaluation": summary_eval,
        "train_side": train_summary,
        "yields": yields_v,
        "evaluation_universe": universe,
        "checks": {
            "prior_recheck_train_side_conflicts": prior_recheck,
            "cross_source_keys_shared": cross_keys,
            "cross_source_documents_held": cross_excluded,
            "federal_register_train_side_documents_held_for_evaluation_units": fr_held,
            "evaluation_cross_split_contributor_or_family_conflicts": cross_split_conflicts,
            "train_side_federal_register_documents_sharing_an_evaluation_unit": fr_contrib_in_train,
            "stack_exchange_sites_scanned": se_seen_sites,
            "stack_exchange_train_side_blocked_accounts": se_block.len(),
            "plos_train_side_blocked_author_names": plos_block.len(),
            "overlap_receipts": rrows,
            "overlap_receipts_sha256": rsha,
            "evaluation_text_checks": "not run: evaluation prose is not acquired; exact, normalized, folded and 50-word checks against prior snapshots, Train-side text and other splits run at acquisition and again before the seal"
        },
        "shortfalls": shortfalls,
        "notes": notes,
        "detector_calls": 0,
        "model_calls": 0,
        "evaluation_prose_opened": false,
    });
    io::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_follows_targets_and_leaves_spares() {
        let exp = vec![1.0; 100];
        let a = allocate(&exp, &[20.0, 10.0, 10.0, 5.0], 2.0);
        let count = |s: usize| a.iter().filter(|x| **x == Some(s)).count();
        assert_eq!((count(0), count(1), count(2), count(3)), (40, 20, 20, 10));
        assert_eq!(a.iter().filter(|x| x.is_none()).count(), 10);
        // Scarce universe: proportional allocation, nothing spare.
        let a = allocate(&vec![1.0; 30], &[20.0, 10.0, 10.0, 5.0], 2.0);
        let count = |s: usize| a.iter().filter(|x| **x == Some(s)).count();
        assert!(a.iter().all(Option::is_some));
        assert!((12..=14).contains(&count(0)) && (6..=7).contains(&count(1)) && (3..=4).contains(&count(3)), "{a:?}");
    }

    #[test]
    fn dsu_roots_are_smallest_keys_and_closure_is_transitive() {
        let mut d = Dsu::default();
        d.union("t:3", "u:a");
        d.union("t:1", "u:b");
        d.union("t:2", "u:a");
        d.union("t:2", "u:b");
        assert_eq!(d.root_key("t:3"), "t:1");
        assert_eq!(d.root_key("u:a"), "t:1");
        assert_eq!(d.root_key("x"), "x");
    }

    #[test]
    fn wikipedia_index_filters_namespaces_and_lists_by_bucket() {
        assert!(article_like("Star Wars: A New Hope"));
        assert!(!article_like("Talk:Star Wars"));
        assert!(!article_like("List of birds"));
        assert!(!article_like("Mercury (disambiguation)"));
        let lines: String = (1..=3000).map(|i| format!("{}:{i}:Synthetic {i}\n", i * 7)).collect::<String>() + "9:5000:Template:X\n";
        let d = tempfile::tempdir().unwrap();
        let rep = wikipedia_index_reserve(lines.as_bytes(), 900, 1000, &d.path().join("w")).unwrap();
        assert_eq!(rep["counts"]["not_article_like"], 1);
        let rows: Vec<Value> = io::jsonl_rows(&d.path().join("w/reserved-metadata.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows.len() as u64, rep["counts"]["article_like_buckets_900_1000"].as_u64().unwrap());
        assert!(rows.iter().all(|r| r["bucket"].as_u64().unwrap() >= 900 && r.get("title").is_none()));
        assert!((200..400).contains(&rows.len()), "{}", rows.len());
    }

    #[test]
    fn contributor_units_and_rank_are_stable() {
        assert_eq!(fr_units("federal_register_agency:x", &[]), vec!["fr_unit:federal_register_agency:x|-"]);
        assert_eq!(fr_units("a", &["Office B".into(), "Office B".into()]), vec!["fr_unit:a|office_b"]);
        assert_eq!(author_key("Ana  María Pérez").as_deref(), Some("plos_author:ana_maría_pérez"));
        assert_eq!(rank("s", "x", "k"), rank("s", "x", "k"));
        assert_ne!(rank("s", "x", "k"), rank("t", "x", "k"));
    }
}
