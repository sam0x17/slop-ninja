//! Twin bank (`bank-plan`, `bank-run`, `bank-finish`): the membership-twin
//! build of `twins` scaled to a target number of matched twins and frame
//! documents, as one resumable long-running job.
//!
//! * `bank-plan` (no network): candidate host archives come from the
//!   `cluster.idx` of the post-cutoff crawls (blocks whose first key and next
//!   first key are dated paths of one host, as in `select`). The URL, path,
//!   copy-risk and non-English ccTLD screens run on the host; each
//!   registered domain keeps one block (keyed rank), and the whole block is
//!   the range. Every range's CDX blocks in all 91 crawls are planned from
//!   the `cluster.idx` files; ranges above the CDX byte cap are dropped. The
//!   kept ranges are ordered by a keyed rank of the registered domain.
//! * `bank-run` (network): ranges are processed one at a time in plan
//!   order. Each range runs CDX -> picks -> WARC records -> extraction and
//!   ends with a `DONE.json` checkpoint holding its per-stratum (host x URL
//!   month) counts. A restarted run skips finished ranges, reuses a finished
//!   CDX or pick file and re-reads only the WARC records not yet indexed. The
//!   run stops once the matched twin and frame counts reach the targets
//!   (times 1 + stop margin), then assembles the bank (`bank-finish`).
//! * `bank-finish`: joins the finished ranges, runs the twin text check
//!   across the whole bank and writes `extract/` in the layout `report` and
//!   `tau` read.
//!
//! The pick rule, extraction path and rules are those of `twins` (one path
//! for every arm). All network reads go through `twins::Ranger` (one pacing
//! schedule, at most 5 request starts a second, 403 = rate block).
use crate::{
    candidates, screens, store,
    twins::{self, BlockRef, Cap, Cluster, Doc, ExtractCtx, Pick, PickRule, Range, RangeKeys, Ranger},
};
use anyhow::{Context, Result, ensure};
use grammar_a_pools::{psl::Psl, splits::rank, text};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, atomic::Ordering},
    time::Instant,
};

pub const BANK_SCHEMA: &str = "slop_ninja_cc_twins_bank_v2";
pub const PLAN_SCHEMA: &str = "slop_ninja_cc_twins_bank_plan_v2";
pub const RANGE_RULE: &str = "cc_twins_bank_range_full_post_block_v2";
pub const STOP_RULE: &str = "cc_twins_bank_stop_matched_v2";
pub const PROBE_RULE: &str = "cc_twins_bank_host_probe_v2";

/// Registered domains under non-English country-code TLDs (the twins-v1 range filter).
pub const NON_ENGLISH_CCTLDS: &[&str] = &[
    "al", "am", "ar", "at", "az", "ba", "bg", "bo", "br", "by", "cl", "cn", "co", "cr", "cu", "cv", "cz", "de", "dk", "do", "ec", "ee", "es", "fi", "fr",
    "ge", "gr", "gt", "hn", "hr", "hu", "ir", "it", "jp", "kr", "kz", "lt", "lu", "lv", "md", "me", "mk", "mx", "ni", "nl", "no", "pa", "pe", "pl", "pt",
    "py", "ro", "rs", "ru", "se", "si", "sk", "sv", "th", "tr", "tw", "ua", "uy", "uz", "ve", "vn",
];

/// Every parameter of one bank; frozen in `plan/PLAN.json` when the plan is made.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BankParams {
    pub schema: String,
    /// Seed of the bank's block, domain and order ranks.
    pub seed: String,
    /// Post-cutoff crawls whose `cluster.idx` gives candidate host archives.
    pub host_sources: Vec<String>,
    pub range_rule: String,
    /// Planned CDX bytes over all 91 crawls above which a range is dropped.
    pub max_range_cdx_bytes: u64,
    pub non_english_cctlds: Vec<String>,
    pub exclude_copy_risk_hosts: bool,
    /// Twin picks per host (one host per registered domain).
    pub per_host_twin_cap: usize,
    /// Frame URLs drawn per picked twin in each stratum (twins-v1: 1.0).
    pub frame_ratio: f64,
    pub target_matched_twins: usize,
    pub target_matched_frame: usize,
    /// The run stops at target x (1 + margin) per arm (per-range counts), so that the
    /// bank-wide twin text check still leaves at least the target.
    pub stop_margin: f64,
    pub stop_rule: String,
    pub rules_id: String,
    pub extract_method: String,
    pub excerpt_method: String,
    pub passage_seed: String,
    /// Seed of the pick ranks inside a range (shared with twins-v1).
    pub pick_seed: String,
    /// At most this many request starts a second (one pacing schedule per process).
    pub max_requests_per_second: f64,
    /// Adjacent CDX blocks of one file read in one request, at most.
    pub max_blocks_per_request: usize,
    pub request_shards: usize,
    /// Host probe, before the frame crawls' CDX is read: the earliest post-cutoff 200 HTML
    /// capture of up to `probe_pages` dated URLs of the range (keyed rank, either arm) goes
    /// through the extraction path; a range with fewer than `probe_min_docs` excerpts ends
    /// there (`probe_failed`). Host-level, so it shapes which hosts appear, not the arms.
    pub probe_rule: String,
    pub probe_pages: usize,
    pub probe_min_docs: usize,
}

impl BankParams {
    pub fn v2_default() -> BankParams {
        BankParams {
            schema: BANK_SCHEMA.into(),
            seed: "cc_twins_bank_v2".into(),
            host_sources: twins::POST_CRAWLS.iter().map(|s| s.to_string()).collect(),
            range_rule: RANGE_RULE.into(),
            max_range_cdx_bytes: 40_000_000,
            non_english_cctlds: NON_ENGLISH_CCTLDS.iter().map(|s| s.to_string()).collect(),
            exclude_copy_risk_hosts: true,
            per_host_twin_cap: 120,
            frame_ratio: 2.0,
            target_matched_twins: 10_000,
            target_matched_frame: 10_000,
            stop_margin: 0.02,
            stop_rule: STOP_RULE.into(),
            rules_id: twins::TWIN_RULES_ID.into(),
            extract_method: twins::EXTRACT_METHOD.into(),
            excerpt_method: candidates::EXCERPT_METHOD.into(),
            passage_seed: twins::PASSAGE_SEED.into(),
            pick_seed: twins::SEED.into(),
            max_requests_per_second: 1.0 / twins::BASE_INTERVAL.as_secs_f64(),
            max_blocks_per_request: 8,
            request_shards: 16,
            probe_rule: PROBE_RULE.into(),
            probe_pages: 16,
            probe_min_docs: 1,
        }
    }
    pub fn pick_rule(&self) -> PickRule {
        PickRule {
            per_host: self.per_host_twin_cap,
            frame_ratio: self.frame_ratio,
        }
    }
    /// Per-range matched counts at which the run stops: (twins, frame).
    pub fn stop_at(&self) -> (usize, usize) {
        let f = |t: usize| (t as f64 * (1.0 + self.stop_margin)).ceil() as usize;
        (f(self.target_matched_twins), f(self.target_matched_frame))
    }
}

/// One planned read: `blocks` adjacent CDX blocks of one file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Run {
    pub crawl: String,
    pub file: u16,
    pub offset: u64,
    pub length: u32,
    pub blocks: u8,
}

impl Run {
    pub fn block(&self) -> BlockRef {
        BlockRef {
            crawl: self.crawl.clone(),
            file: self.file,
            offset: self.offset,
            length: self.length,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanRange {
    pub range: Range,
    pub source_crawl: String,
    pub cdx_bytes: u64,
    pub cdx_blocks: usize,
    pub runs: Vec<Run>,
}

/// Merge the blocks `i0..=i1` of a cluster into reads of at most `max` adjacent blocks.
pub fn merge_runs(c: &Cluster, crawl: &str, blocks: std::ops::RangeInclusive<usize>, max: usize) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for i in blocks {
        if let Some(last) = out.last_mut()
            && last.file == c.file[i]
            && last.offset + last.length as u64 == c.offset[i]
            && (last.blocks as usize) < max
        {
            last.length += c.length[i];
            last.blocks += 1;
            continue;
        }
        out.push(Run {
            crawl: crawl.to_owned(),
            file: c.file[i],
            offset: c.offset[i],
            length: c.length[i],
            blocks: 1,
        });
    }
    out
}

/// TLD of a registered domain.
fn tld(reg: &str) -> &str {
    reg.rsplit('.').next().unwrap_or("")
}

pub struct PlanArgs<'a> {
    pub root: &'a Path,
    /// Frame crawls then post-cutoff crawls (the 91 crawls whose CDX is read).
    pub crawls: &'a [String],
    pub psl: &'a Psl,
    pub fineweb_excluded: &'a HashSet<String>,
    pub params: BankParams,
    /// Optional twins-v1 `select/ranges.json`, for the host-overlap count.
    pub v1_ranges: Option<&'a Path>,
}

pub fn plan_dir(root: &Path) -> PathBuf {
    root.join("plan")
}

pub fn plan(a: &PlanArgs) -> Result<Value> {
    let p = &a.params;
    let dir = plan_dir(a.root);
    ensure!(!dir.join("PLAN.json").exists(), "{} exists; a bank's plan is frozen", dir.join("PLAN.json").display());
    // 1. Candidate host archives.
    struct Cand {
        rank: u64,
        crawl: String,
        host: String,
        reg: String,
        lo: String,
        hi: String,
        block: BlockRef,
    }
    let mut best: HashMap<String, Cand> = HashMap::new();
    let mut verdict: HashMap<String, Option<String>> = HashMap::new();
    let mut excluded: BTreeMap<String, usize> = BTreeMap::new();
    let mut dated_blocks: BTreeMap<String, usize> = BTreeMap::new();
    let mut hosts_seen: HashSet<String> = HashSet::new();
    for crawl in &p.host_sources {
        ensure!(twins::POST_CRAWLS.contains(&crawl.as_str()), "host source {crawl} is not a post-cutoff crawl");
        let c = Cluster::load(&twins::cluster_path(a.root, crawl))?;
        for i in 0..c.keys.len().saturating_sub(1) {
            let (s1, s2) = (c.surt(i), c.surt(i + 1));
            let ((h1, p1), (h2, p2)) = (twins::split_surt(s1), twins::split_surt(s2));
            if h1 != h2 {
                continue;
            }
            let (Some((m1, _)), Some((m2, _))) = (twins::url_date(p1), twins::url_date(p2)) else { continue };
            if !twins::eligible_month(&m1) || !twins::eligible_month(&m2) {
                continue;
            }
            *dated_blocks.entry(crawl.clone()).or_default() += 1;
            let host = twins::surt_host(h1);
            hosts_seen.insert(host.clone());
            let v = verdict.entry(host.clone()).or_insert_with(|| {
                let Some(reg) = a.psl.registered_domain(&host) else { return Some("no_registered_domain".into()) };
                if let Some(r) = screens::url_exclusion(&host, &reg, a.fineweb_excluded).or_else(|| screens::path_exclusion(&host, p1)) {
                    return Some(r.into());
                }
                if p.exclude_copy_risk_hosts
                    && let Some(r) = twins::copy_risk_host(&host, &reg)
                {
                    return Some(format!("copy_risk_host:{r}"));
                }
                if p.non_english_cctlds.iter().any(|t| t == tld(&reg)) {
                    return Some("non_english_cctld".into());
                }
                None
            });
            if let Some(r) = v {
                *excluded.entry(r.clone()).or_default() += 1;
                continue;
            }
            let reg = a.psl.registered_domain(&host).unwrap_or_default();
            let rk = rank(&p.seed, "cc_twins_bank_block_v2", &format!("{crawl}\t{s1}"));
            let better = best.get(&reg).is_none_or(|b| rk < b.rank);
            if better {
                best.insert(
                    reg.clone(),
                    Cand {
                        rank: rk,
                        crawl: crawl.clone(),
                        host,
                        reg,
                        lo: s1.to_owned(),
                        hi: s2.to_owned(),
                        block: BlockRef {
                            crawl: crawl.clone(),
                            file: c.file[i],
                            offset: c.offset[i],
                            length: c.length[i],
                        },
                    },
                );
            }
        }
    }
    let mut cands: Vec<(u64, Cand)> = best.into_values().map(|c| (rank(&p.seed, "cc_twins_bank_domain_v2", &c.reg), c)).collect();
    cands.sort_by(|x, y| (x.0, &x.1.reg).cmp(&(y.0, &y.1.reg)));
    let n_domains = cands.len();
    // 2. CDX reads of every candidate in every crawl.
    let mut runs: Vec<Vec<Run>> = vec![Vec::new(); cands.len()];
    let mut bytes: Vec<u64> = vec![0; cands.len()];
    let mut nblocks: Vec<usize> = vec![0; cands.len()];
    for crawl in a.crawls {
        let c = Cluster::load(&twins::cluster_path(a.root, crawl))?;
        for (k, (_, cand)) in cands.iter().enumerate() {
            let b = c.blocks(&cand.lo, &cand.hi);
            nblocks[k] += b.clone().count();
            bytes[k] += b.clone().map(|i| c.length[i] as u64).sum::<u64>();
            runs[k].extend(merge_runs(&c, crawl, b, p.max_blocks_per_request));
        }
        eprintln!("plan: {crawl} done");
    }
    // 3. Cost filter and order.
    let v1_hosts: HashSet<String> = match a.v1_ranges {
        Some(path) => store::read_json::<Vec<Range>>(path)?.into_iter().map(|r| r.host).collect(),
        None => HashSet::new(),
    };
    let mut kept: Vec<PlanRange> = Vec::new();
    let mut over_cap = 0usize;
    let mut v1_overlap = 0usize;
    for (k, (rk, cand)) in cands.into_iter().enumerate() {
        if bytes[k] > p.max_range_cdx_bytes {
            over_cap += 1;
            continue;
        }
        if v1_hosts.contains(&cand.host) {
            v1_overlap += 1;
        }
        let id = kept.len() as u32;
        kept.push(PlanRange {
            range: Range {
                id,
                host: cand.host,
                registered_domain: cand.reg,
                host_rank: format!("{rk:016x}"),
                block_lo: cand.lo.clone(),
                block_hi: cand.hi.clone(),
                block: cand.block,
                lo: cand.lo,
                hi: cand.hi,
                sub_lines: 0,
                sub_dated_html: 0,
                months: BTreeMap::new(),
            },
            source_crawl: cand.crawl,
            cdx_bytes: bytes[k],
            cdx_blocks: nblocks[k],
            runs: std::mem::take(&mut runs[k]),
        });
    }
    let mut w = store::ZWriter::create(&dir.join("ranges.jsonl.zst"))?;
    for r in &kept {
        w.write(r)?;
    }
    let (sha, rows) = w.finish()?;
    let mut costs: Vec<u64> = kept.iter().map(|r| r.cdx_bytes).collect();
    costs.sort();
    let q = |f: f64| costs.get(((costs.len() as f64 - 1.0) * f) as usize).copied().unwrap_or(0);
    let requests: usize = kept.iter().map(|r| r.runs.len()).sum();
    let summary = json!({"schema": PLAN_SCHEMA, "params": p, "crawls": a.crawls.len(), "dated_single_host_blocks": dated_blocks,
        "hosts_seen": hosts_seen.len(), "excluded_blocks_by_reason": excluded, "registered_domains": n_domains,
        "dropped_over_cdx_cap": over_cap, "ranges": rows, "ranges_sha256": sha, "v1_host_overlap": v1_overlap,
        "planned_cdx_bytes_all": costs.iter().sum::<u64>(), "planned_cdx_requests_all": requests,
        "per_range_cdx_bytes_p10_p50_p90_max": [q(0.1), q(0.5), q(0.9), q(1.0)],
        "rule": "candidate blocks: first key and next first key are /YYYY/MM/ paths (month before 2022-11) of one host in a host-source crawl's cluster.idx; host screens: URL and path exclusions, copy-risk hosts, non-English ccTLDs; one block per registered domain by min rank(seed, cc_twins_bank_block_v2, crawl TAB key); the range is the whole block [first key, next first key); ranges ordered by rank(seed, cc_twins_bank_domain_v2, registered domain); CDX blocks of every crawl planned from cluster.idx; ranges above max_range_cdx_bytes dropped",
        "made_utc": grammar_a_pools::io::now_utc()});
    store::write_json(&dir.join("PLAN.json"), &summary)?;
    Ok(summary)
}

/// The frozen plan: parameters and ordered ranges (checked against the plan's digest).
pub fn load_plan(root: &Path) -> Result<(BankParams, Vec<PlanRange>)> {
    let dir = plan_dir(root);
    let pj: Value = store::read_json(&dir.join("PLAN.json"))?;
    let params: BankParams = serde_json::from_value(pj["params"].clone()).context("plan params")?;
    let path = dir.join("ranges.jsonl.zst");
    let raw = zstd::decode_all(&fs::read(&path)?[..])?;
    ensure!(text::sha_hex(&raw) == pj["ranges_sha256"].as_str().unwrap_or(""), "{}: digest differs from PLAN.json", path.display());
    let ranges: Vec<PlanRange> = store::rows(&path)?;
    for (i, r) in ranges.iter().enumerate() {
        ensure!(r.range.id as usize == i, "plan range {i} has id {}", r.range.id);
    }
    Ok((params, ranges))
}

// ------------------------------------------------------------------ per-range steps

pub fn range_dir(root: &Path, id: u32) -> PathBuf {
    root.join("ranges").join(format!("{id:05}"))
}

/// Per-range checkpoint: what the range gave and why it ended.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RangeDone {
    pub range: u32,
    pub host: String,
    /// `done`, `probe_failed`, or `skipped:<reason>` (a CDX file returned 404 or 416).
    pub status: String,
    #[serde(default)]
    pub probe_pages: usize,
    #[serde(default)]
    pub probe_kept: usize,
    pub captures: u64,
    pub build_counts: BTreeMap<String, u64>,
    pub picks: BTreeMap<String, u64>,
    pub records_failed: usize,
    pub funnel: twins::Funnel,
    /// Documents kept per arm after the range's own twin text check.
    pub docs: BTreeMap<String, u64>,
    pub matched_twin: u64,
    pub matched_frame: u64,
    pub matched_frame_post: u64,
    /// URL month -> [twin, frame, frame_post] documents kept.
    pub strata: BTreeMap<String, [u64; 3]>,
    pub matched_strata: u64,
    pub finished_utc: String,
}

/// Documents per arm in strata holding at least one kept twin and one kept frame
/// document: (twin, frame, frame_post, strata), the `report` matched design.
pub fn matched_counts(docs: &[Doc]) -> (u64, u64, u64, u64) {
    let mut by: BTreeMap<&str, [u64; 3]> = BTreeMap::new();
    for d in docs.iter().filter(|d| d.excluded.is_none()) {
        let k = match d.arm.as_str() {
            "twin" => 0,
            "frame" => 1,
            "frame_post" => 2,
            _ => continue,
        };
        by.entry(&d.stratum).or_default()[k] += 1;
    }
    let mut out = (0, 0, 0, 0);
    for c in by.values().filter(|c| c[0] > 0 && c[1] > 0) {
        out.0 += c[0];
        out.1 += c[1];
        out.2 += c[2];
        out.3 += 1;
    }
    out
}

/// Read every planned CDX block of one range; keep its lines, sorted crawl by crawl
/// (plan crawl order) by (SURT, timestamp). `None` if a CDX file returned 404 or 416.
fn range_caps(ranger: &Ranger, threads: usize, r: &Range, runs: &[Run], crawl_order: &HashMap<&str, usize>) -> Result<Option<Vec<Cap>>> {
    let caps: Mutex<Vec<Cap>> = Mutex::new(Vec::new());
    let missing: Mutex<Option<String>> = Mutex::new(None);
    if runs.is_empty() {
        return Ok(Some(Vec::new()));
    }
    twins::parallel(runs, threads, |agent, run| {
        let b = run.block();
        let gz = match ranger.read(agent, &b.rel(), b.offset, b.length as u64) {
            Ok(g) => g,
            Err(e) if !ranger.stop.load(Ordering::SeqCst) && (e.to_string().contains("returned 404") || e.to_string().contains("returned 416")) => {
                missing.lock().unwrap().get_or_insert(format!("{e:#}"));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let txt = twins::decode_block(&gz)?;
        let mut v = Vec::new();
        for l in txt.lines() {
            let Some((surt, _)) = l.split_once(' ') else { continue };
            if surt >= r.lo.as_str()
                && surt < r.hi.as_str()
                && let Some(cap) = twins::parse_cdx(l, r.id, &run.crawl)
            {
                v.push(cap);
            }
        }
        caps.lock().unwrap().extend(v);
        Ok(())
    })?;
    if missing.into_inner().unwrap().is_some() {
        return Ok(None);
    }
    let mut caps = caps.into_inner().unwrap();
    caps.sort_by(|a, b| (crawl_order[a.crawl.as_str()], &a.surt, &a.ts).cmp(&(crawl_order[b.crawl.as_str()], &b.surt, &b.ts)));
    Ok(Some(caps))
}

/// Probe picks of a range: per dated eligible URL with a post-cutoff 200 HTML capture, its
/// earliest such capture; the first `n` URLs by rank(seed, cc_twins_bank_probe_v2, SURT).
pub fn probe_picks(r: &Range, post: &[Cap], seed: &str, n: usize) -> Vec<Pick> {
    let mut first: BTreeMap<&str, &Cap> = BTreeMap::new();
    for c in post.iter().filter(|c| twins::is_html(c)) {
        let Some((ym, _)) = twins::url_date(twins::split_surt(&c.surt).1) else { continue };
        if !twins::eligible_month(&ym) {
            continue;
        }
        let e = first.entry(c.surt.as_str()).or_insert(c);
        if (&c.crawl, &c.ts) < (&e.crawl, &e.ts) {
            *e = c;
        }
    }
    let mut v: Vec<(u64, &Cap)> = first.into_iter().map(|(s, c)| (rank(seed, "cc_twins_bank_probe_v2", s), c)).collect();
    v.sort_by(|a, b| (a.0, &a.1.surt).cmp(&(b.0, &b.1.surt)));
    v.into_iter()
        .take(n)
        .map(|(rk, c)| {
            let ym = twins::url_date(twins::split_surt(&c.surt).1).map(|x| x.0).unwrap_or_default();
            Pick {
                id: format!("tw-probe-{}", &text::sha_hex(format!("probe\0{}\0{}", c.surt, c.ts).as_bytes())[..20]),
                arm: "probe".into(),
                range: r.id,
                host: r.host.clone(),
                registered_domain: r.registered_domain.clone(),
                stratum: format!("{}|{ym}", r.host),
                url_month: ym,
                surt: c.surt.clone(),
                url: c.url.clone(),
                crawl: c.crawl.clone(),
                ts: c.ts.clone(),
                filename: c.filename.clone(),
                offset: c.offset,
                length: c.length,
                digest: c.digest.clone(),
                languages: c.languages.clone(),
                frame_crawls_seen: 0,
                first_frame_ts: None,
                post_crawls_seen: vec![c.crawl.clone()],
                rank: format!("{rk:016x}"),
                pair: None,
            }
        })
        .collect()
}

pub struct RunArgs<'a> {
    pub root: &'a Path,
    pub frame: &'a [String],
    pub psl: &'a Psl,
    pub fineweb_excluded: &'a HashSet<String>,
    pub connections: usize,
    pub budget: u64,
    pub min_free: u64,
    /// Stop after this many ranges in this invocation (a bounded smoke run).
    pub max_ranges: Option<usize>,
}

/// How a `bank-run` invocation ended; `exit_code` is the process exit status.
#[derive(Debug)]
pub enum Outcome {
    /// Targets reached and the bank assembled.
    Complete(Value),
    /// `max_ranges` reached (smoke run).
    Paused(Value),
    /// Every planned range is finished and the targets are not reached (the bank is still
    /// assembled; `extract/EXTRACT.json` has `targets_met: false`).
    PlanExhausted(Value),
    /// Stopped by the reader: `rate_block_403`, `throttled_25`, `byte_budget` or `disk_headroom`.
    Stopped(&'static str, String),
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Complete(_) | Outcome::Paused(_) => 0,
            // Throttling: a restart after backoff is allowed.
            Outcome::Stopped("throttled_25", _) => 74,
            // A rate block (three 403s in a row) stops the bank until a person restarts it.
            Outcome::Stopped("rate_block_403", _) => 75,
            Outcome::Stopped("byte_budget", _) => 76,
            Outcome::Stopped("disk_headroom", _) => 77,
            Outcome::Stopped(_, _) => 1,
            Outcome::PlanExhausted(_) => 78,
        }
    }
}

struct Totals {
    done: usize,
    probe_failed: usize,
    skipped: usize,
    matched_twin: u64,
    matched_frame: u64,
    matched_frame_post: u64,
    matched_strata: u64,
    hosts_matched: usize,
    picks: BTreeMap<String, u64>,
    docs: BTreeMap<String, u64>,
}

fn totals(done: &BTreeMap<u32, RangeDone>) -> Totals {
    let mut t = Totals {
        done: 0,
        probe_failed: 0,
        skipped: 0,
        matched_twin: 0,
        matched_frame: 0,
        matched_frame_post: 0,
        matched_strata: 0,
        hosts_matched: 0,
        picks: BTreeMap::new(),
        docs: BTreeMap::new(),
    };
    for d in done.values() {
        match d.status.as_str() {
            "done" => t.done += 1,
            "probe_failed" => t.probe_failed += 1,
            _ => t.skipped += 1,
        }
        t.matched_twin += d.matched_twin;
        t.matched_frame += d.matched_frame;
        t.matched_frame_post += d.matched_frame_post;
        t.matched_strata += d.matched_strata;
        t.hosts_matched += (d.matched_twin > 0) as usize;
        for (k, v) in &d.picks {
            *t.picks.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &d.docs {
            *t.docs.entry(k.clone()).or_default() += v;
        }
    }
    t
}

fn load_done(root: &Path, n: usize) -> Result<BTreeMap<u32, RangeDone>> {
    let mut out = BTreeMap::new();
    for id in 0..n as u32 {
        let p = range_dir(root, id).join("DONE.json");
        if p.exists() {
            out.insert(id, store::read_json(&p)?);
        }
    }
    Ok(out)
}

fn progress(root: &Path, v: &Value) -> Result<()> {
    let p = root.join("journal").join("progress.jsonl");
    fs::create_dir_all(p.parent().unwrap())?;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{}", serde_json::to_string(v)?)?;
    f.sync_data()?;
    Ok(())
}

/// Process one range up to its `DONE.json`.
fn run_range(a: &RunArgs, params: &BankParams, ranger: &Ranger, pr: &PlanRange, crawl_order: &HashMap<&str, usize>, cx: &ExtractCtx) -> Result<RangeDone> {
    let r = &pr.range;
    let dir = range_dir(a.root, r.id);
    fs::create_dir_all(&dir)?;
    let mut done = RangeDone {
        range: r.id,
        host: r.host.clone(),
        status: "done".into(),
        ..Default::default()
    };
    let skipped = |mut done: RangeDone, why: &str| {
        done.status = format!("skipped:{why}");
        done.finished_utc = grammar_a_pools::io::now_utc();
        done
    };
    // Post-cutoff CDX, then the host probe.
    let caps_path = dir.join("caps.jsonl.zst");
    let post_path = dir.join("caps-post.jsonl.zst");
    let is_post = |c: &str| twins::POST_CRAWLS.contains(&c);
    if !caps_path.exists() && !post_path.exists() {
        let runs: Vec<Run> = pr.runs.iter().filter(|x| is_post(&x.crawl)).cloned().collect();
        let Some(post) = range_caps(ranger, a.connections, r, &runs, crawl_order)? else { return Ok(skipped(done, "cdx_file_404_or_416")) };
        let mut w = store::ZWriter::create(&post_path)?;
        for c in &post {
            w.write(c)?;
        }
        w.finish()?;
    }
    let probe_dir = dir.join("probe");
    let probe_json = probe_dir.join("PROBE.json");
    if !probe_json.exists() {
        let post: Vec<Cap> = store::rows(&post_path)?;
        let probe = probe_picks(r, &post, &params.seed, params.probe_pages);
        let (_, _, pfailed) = twins::fetch_records(ranger, a.connections, &probe, &probe_dir)?;
        let (pdocs, pfunnel, _, _) = twins::extract_dir(&probe_dir, &probe, cx)?;
        store::write_json(&probe_json, &json!({"rule": params.probe_rule, "pages": probe.len(), "records_failed": pfailed.len(),
            "kept": pdocs.len(), "funnel": pfunnel}))?;
    }
    let pj: Value = store::read_json(&probe_json)?;
    done.probe_pages = pj["pages"].as_u64().unwrap_or(0) as usize;
    done.probe_kept = pj["kept"].as_u64().unwrap_or(0) as usize;
    if done.probe_kept < params.probe_min_docs {
        done.status = "probe_failed".into();
        done.finished_utc = grammar_a_pools::io::now_utc();
        return Ok(done);
    }
    // The frame crawls' CDX, joined with the post-cutoff lines.
    if !caps_path.exists() {
        let runs: Vec<Run> = pr.runs.iter().filter(|x| !is_post(&x.crawl)).cloned().collect();
        let Some(mut caps) = range_caps(ranger, a.connections, r, &runs, crawl_order)? else { return Ok(skipped(done, "cdx_file_404_or_416")) };
        caps.extend(store::rows::<Cap>(&post_path)?);
        caps.sort_by(|a, b| (crawl_order[a.crawl.as_str()], &a.surt, &a.ts).cmp(&(crawl_order[b.crawl.as_str()], &b.surt, &b.ts)));
        let mut w = store::ZWriter::create(&caps_path)?;
        for c in &caps {
            w.write(c)?;
        }
        w.finish()?;
    }
    // Picks.
    let picks_path = dir.join("picks.jsonl.zst");
    let build_path = dir.join("BUILD.json");
    if !picks_path.exists() || !build_path.exists() {
        let frame_set: HashSet<&str> = a.frame.iter().map(String::as_str).collect();
        let caps: Vec<Cap> = store::rows(&caps_path)?;
        let n_caps = caps.len() as u64;
        let mut rk = RangeKeys::default();
        for cap in caps {
            let is_frame = frame_set.contains(cap.crawl.as_str());
            rk.absorb(cap, is_frame);
        }
        let (picks, counts, row) = twins::build_range(r, &rk, params.pick_rule());
        let mut w = store::ZWriter::create(&picks_path)?;
        for p in &picks {
            w.write(p)?;
        }
        let (sha, rows) = w.finish()?;
        store::write_json(&build_path, &json!({"captures": n_caps, "counts": counts, "row": row, "picks": rows, "picks_sha256": sha}))?;
    }
    let build: Value = store::read_json(&build_path)?;
    done.captures = build["captures"].as_u64().unwrap_or(0);
    done.build_counts = serde_json::from_value(build["counts"].clone()).unwrap_or_default();
    let picks: Vec<Pick> = store::rows(&picks_path)?;
    for p in &picks {
        *done.picks.entry(p.arm.clone()).or_default() += 1;
    }
    // WARC records (resumable per record).
    let (_, _, failed) = twins::fetch_records(ranger, a.connections, &picks, &dir)?;
    done.records_failed = failed.len();
    if !failed.is_empty() {
        store::write_json(&dir.join("RECORDS-FAILED.json"), &failed)?;
    }
    // Extraction (the shared path) and the range's own twin text check.
    let (mut docs, funnel, _, _) = twins::extract_dir(&dir, &picks, cx)?;
    twins::twin_text_check(&mut docs);
    let mut w = store::ZWriter::create(&dir.join("docs.jsonl.zst"))?;
    for d in &docs {
        w.write(d)?;
    }
    w.finish()?;
    done.funnel = funnel;
    for d in docs.iter().filter(|d| d.excluded.is_none()) {
        *done.docs.entry(d.arm.clone()).or_default() += 1;
        let k = match d.arm.as_str() {
            "twin" => 0,
            "frame" => 1,
            _ => 2,
        };
        done.strata.entry(d.url_month.clone()).or_default()[k] += 1;
    }
    let (mt, mf, mp, ms) = matched_counts(&docs);
    (done.matched_twin, done.matched_frame, done.matched_frame_post, done.matched_strata) = (mt, mf, mp, ms);
    done.finished_utc = grammar_a_pools::io::now_utc();
    Ok(done)
}

#[allow(clippy::too_many_arguments)]
fn status_json(state: &str, params: &BankParams, n_plan: usize, done: &BTreeMap<u32, RangeDone>, ranger: &Ranger, started: &Instant, started_utc: &str, run_matched0: u64, extra: Value) -> Value {
    let t = totals(done);
    let (st_t, st_f) = params.stop_at();
    let hours = started.elapsed().as_secs_f64() / 3600.0;
    let gained = t.matched_twin.saturating_sub(run_matched0) as f64;
    let rate = if hours > 0.0 { gained / hours } else { 0.0 };
    let need = (st_t as f64 - t.matched_twin as f64).max(0.0);
    json!({"schema": "slop_ninja_cc_twins_bank_status_v2", "state": state, "updated_utc": grammar_a_pools::io::now_utc(),
        "ranges_planned": n_plan, "ranges_finished": done.len(), "ranges_done": t.done, "ranges_probe_failed": t.probe_failed, "ranges_skipped": t.skipped,
        "matched_twin": t.matched_twin, "matched_frame": t.matched_frame, "matched_frame_post": t.matched_frame_post,
        "matched_strata": t.matched_strata, "hosts_with_matched_twins": t.hosts_matched,
        "stop_at": {"matched_twin": st_t, "matched_frame": st_f},
        "picks": t.picks, "docs_kept": t.docs,
        "bytes_received_total": ranger.received.load(Ordering::SeqCst), "byte_budget": ranger.budget,
        "this_run": {"started_utc": started_utc, "hours": hours, "requests": ranger.requests.load(Ordering::SeqCst),
            "throttled": ranger.throttled.load(Ordering::SeqCst), "blocked_403": ranger.blocked.load(Ordering::SeqCst),
            "matched_twins_gained": gained, "matched_twins_per_hour": rate,
            "eta_hours_at_this_rate": if rate > 0.0 { Value::from(need / rate) } else { Value::Null }},
        "extra": extra})
}

fn stop_kind(ranger: &Ranger, e: &anyhow::Error) -> &'static str {
    if let Some(r) = *ranger.stop_reason.lock().unwrap() {
        return r;
    }
    let s = format!("{e:#}");
    if s.contains("byte budget") {
        "byte_budget"
    } else if s.contains("disk headroom") {
        "disk_headroom"
    } else {
        "error"
    }
}

pub fn run(a: &RunArgs) -> Result<Outcome> {
    let (params, plan) = load_plan(a.root)?;
    let mut crawls: Vec<String> = a.frame.to_vec();
    crawls.extend(twins::POST_CRAWLS.iter().map(|s| s.to_string()));
    let crawl_order: HashMap<&str, usize> = crawls.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
    let rules = twins::twin_rules();
    let cx = ExtractCtx {
        psl: a.psl,
        fineweb_excluded: a.fineweb_excluded,
        rules: &rules,
    };
    let state_dir = a.root.join("state");
    fs::create_dir_all(&state_dir)?;
    let mut done = load_done(a.root, plan.len())?;
    let ranger = Ranger::new(&a.root.join("journal").join("ranges.jsonl"), a.budget, a.min_free, a.root)?;
    let started = Instant::now();
    let started_utc = grammar_a_pools::io::now_utc();
    let (stop_t, stop_f) = params.stop_at();
    let t0 = totals(&done);
    let run_matched0 = t0.matched_twin;
    progress(a.root, &json!({"event": "start", "utc": started_utc, "ranges_finished": done.len(), "matched_twin": t0.matched_twin, "matched_frame": t0.matched_frame, "bytes_received_total": ranger.received.load(Ordering::SeqCst)}))?;
    let write_status = |state: &str, done: &BTreeMap<u32, RangeDone>, extra: Value| -> Result<()> {
        store::write_json(&state_dir.join("STATUS.json"), &status_json(state, &params, plan.len(), done, &ranger, &started, &started_utc, run_matched0, extra))
    };
    let reached = |done: &BTreeMap<u32, RangeDone>| {
        let t = totals(done);
        t.matched_twin as usize >= stop_t && t.matched_frame as usize >= stop_f
    };
    let mut this_run = 0usize;
    for pr in &plan {
        if reached(&done) {
            break;
        }
        if done.contains_key(&pr.range.id) {
            continue;
        }
        if a.max_ranges.is_some_and(|m| this_run >= m) {
            write_status("paused", &done, json!({"max_ranges": a.max_ranges}))?;
            progress(a.root, &json!({"event": "paused", "utc": grammar_a_pools::io::now_utc(), "ranges_this_run": this_run}))?;
            return Ok(Outcome::Paused(json!({"ranges_this_run": this_run})));
        }
        write_status("running", &done, json!({"current_range": pr.range.id, "current_host": pr.range.host}))?;
        match run_range(a, &params, &ranger, pr, &crawl_order, &cx) {
            Ok(d) => {
                store::write_json(&range_dir(a.root, pr.range.id).join("DONE.json"), &d)?;
                done.insert(pr.range.id, d.clone());
                this_run += 1;
                let t = totals(&done);
                progress(a.root, &json!({"event": "range", "utc": d.finished_utc, "range": d.range, "host": d.host, "status": d.status,
                    "probe": [d.probe_kept, d.probe_pages], "picks": d.picks, "docs": d.docs, "matched_twin": d.matched_twin, "matched_frame": d.matched_frame,
                    "cum_ranges": done.len(), "cum_matched_twin": t.matched_twin, "cum_matched_frame": t.matched_frame,
                    "bytes_received_total": ranger.received.load(Ordering::SeqCst), "requests_this_run": ranger.requests.load(Ordering::SeqCst)}))?;
            }
            Err(e) => {
                let kind = stop_kind(&ranger, &e);
                let msg = format!("{e:#}");
                write_status(&format!("stopped:{kind}"), &done, json!({"range": pr.range.id, "error": msg}))?;
                progress(a.root, &json!({"event": "stopped", "utc": grammar_a_pools::io::now_utc(), "kind": kind, "range": pr.range.id, "error": msg}))?;
                return Ok(Outcome::Stopped(kind, msg));
            }
        }
    }
    if !reached(&done) {
        // Every planned range is finished: assemble what the plan gave, marked as short of the target.
        write_status("finishing", &done, Value::Null)?;
        let fin = finish(a.root, None)?;
        write_status("plan_exhausted", &done, fin.clone())?;
        progress(a.root, &json!({"event": "plan_exhausted", "utc": grammar_a_pools::io::now_utc(), "targets_met": fin["targets_met"]}))?;
        return Ok(Outcome::PlanExhausted(json!({"ranges": done.len(), "matched": fin["matched"]})));
    }
    write_status("finishing", &done, Value::Null)?;
    let fin = finish(a.root, None)?;
    let t = totals(&done);
    store::write_json(&a.root.join("BANK-COMPLETE.json"), &json!({"schema": BANK_SCHEMA, "utc": grammar_a_pools::io::now_utc(),
        "ranges_finished": done.len(), "per_range_matched_twin": t.matched_twin, "per_range_matched_frame": t.matched_frame, "finish": fin}))?;
    write_status("complete", &done, fin.clone())?;
    progress(a.root, &json!({"event": "complete", "utc": grammar_a_pools::io::now_utc()}))?;
    Ok(Outcome::Complete(fin))
}

/// Assemble the bank from every finished range: the bank-wide twin text check, then
/// `extract/docs.jsonl.zst`, the A request shards and `extract/EXTRACT.json`.
pub fn finish(root: &Path, shards: Option<usize>) -> Result<Value> {
    let (params, plan) = load_plan(root)?;
    let done = load_done(root, plan.len())?;
    let mut docs: Vec<Doc> = Vec::new();
    let mut funnel = twins::Funnel::new();
    let mut ranges_used = 0usize;
    for (id, d) in &done {
        if d.status != "done" {
            continue;
        }
        ranges_used += 1;
        let mut v: Vec<Doc> = store::rows(&range_dir(root, *id).join("docs.jsonl.zst"))?;
        for x in v.iter_mut() {
            x.excluded = None;
        }
        docs.extend(v);
        for (arm, st) in &d.funnel {
            for (k, n) in st {
                *funnel.entry(arm.clone()).or_default().entry(k.clone()).or_default() += n;
            }
        }
    }
    let ids: BTreeSet<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    ensure!(ids.len() == docs.len(), "duplicate document ids across ranges");
    let ex_counts = twins::twin_text_check(&mut docs);
    let out = root.join("extract");
    let mut w = store::ZWriter::create(&out.join("docs.jsonl.zst"))?;
    for d in &docs {
        w.write(d)?;
    }
    let (sha, rows) = w.finish()?;
    let req = out.join("requests");
    if req.exists() {
        fs::remove_dir_all(&req)?;
    }
    let shard_sha = twins::write_requests(&req, &docs, shards.unwrap_or(params.request_shards))?;
    let mut arms: BTreeMap<String, u64> = BTreeMap::new();
    for d in docs.iter().filter(|d| d.excluded.is_none()) {
        *arms.entry(d.arm.clone()).or_default() += 1;
    }
    let (mt, mf, mp, ms) = matched_counts(&docs);
    let hosts: BTreeSet<&str> = docs.iter().filter(|d| d.excluded.is_none() && d.arm == "twin").map(|d| d.host.as_str()).collect();
    let summary = json!({"schema": "slop_ninja_cc_twins_extract_v1", "bank_schema": BANK_SCHEMA, "params": params,
        "ranges_finished": done.len(), "ranges_used": ranges_used, "funnel_by_arm": funnel, "docs": rows, "docs_sha256": sha,
        "twin_text_check": ex_counts, "kept_by_arm": arms,
        "matched": {"twin": mt, "frame": mf, "frame_post": mp, "strata": ms, "hosts_with_kept_twins": hosts.len()},
        "targets_met": mt as usize >= params.target_matched_twins && mf as usize >= params.target_matched_frame,
        "requests": shard_sha, "extract_method": twins::EXTRACT_METHOD, "excerpt_method": candidates::EXCERPT_METHOD,
        "passage_seed": twins::PASSAGE_SEED, "rules": twins::TWIN_RULES_ID, "lang": twins::EXTRACT_LANG_TEXT, "date_rule": twins::EXTRACT_DATE_TEXT,
        "templates": "site templates over the distinct URLs of each registered domain; one range per registered domain, so per-range templates equal bank-wide ones",
        "made_utc": grammar_a_pools::io::now_utc()});
    store::write_json(&out.join("EXTRACT.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, arm: &str, stratum: &str, excluded: Option<&str>) -> Doc {
        let v = json!({"id": id, "arm": arm, "range": 0, "host": "h.example", "registered_domain": "example", "group": "g",
            "stratum": stratum, "url_month": "2015-03", "url": "u", "crawl": "c", "warc_date": "", "record_id": "", "payload_digest": null,
            "filename": "", "offset": 0, "length": 0, "page_published": null, "page_published_source": null, "page_modified": null,
            "charset": "utf8", "page_sha256": "", "page_words": 0, "domain": "news", "domain_rule": "", "page_domain": "news",
            "line_start": 0, "line_end": 1, "valid_runs": 1, "words": 3, "text": "a b c", "text_sha256": "", "folded_sha256": id,
            "text_redacted": null, "pair": null, "frame_crawls_seen": 0, "first_frame_ts": null, "excluded": excluded});
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn matched_counts_need_both_arms_in_a_stratum() {
        let docs = vec![
            doc("1", "twin", "h|2015-03", None),
            doc("2", "frame", "h|2015-03", None),
            doc("3", "frame_post", "h|2015-03", None),
            doc("4", "twin", "h|2015-04", None),
            doc("5", "frame", "h|2015-05", None),
            doc("6", "twin", "h|2015-06", None),
            doc("7", "frame", "h|2015-06", Some("x")),
        ];
        assert_eq!(matched_counts(&docs), (1, 1, 1, 1));
    }

    #[test]
    fn stop_targets_include_the_margin() {
        let p = BankParams::v2_default();
        assert_eq!(p.stop_at(), (10_200, 10_200));
        assert_eq!(p.max_requests_per_second, 5.0);
        assert_eq!(p.pick_rule().frames_for(2), 4);
        assert_eq!(PickRule { per_host: 1, frame_ratio: 1.5 }.frames_for(3), 5);
        assert_eq!(PickRule { per_host: 1, frame_ratio: 1.0 }.frames_for(7), 7);
    }

    #[test]
    fn runs_merge_adjacent_blocks_of_one_file() {
        let c = Cluster {
            keys: vec!["a 1".into(), "b 1".into(), "c 1".into(), "d 1".into(), "e 1".into()],
            file: vec![0, 0, 0, 1, 1],
            offset: vec![0, 10, 25, 0, 7],
            length: vec![10, 15, 5, 7, 3],
        };
        let r = merge_runs(&c, "X", 0..=4, 2);
        let got: Vec<(u16, u64, u32, u8)> = r.iter().map(|x| (x.file, x.offset, x.length, x.blocks)).collect();
        assert_eq!(got, vec![(0, 0, 25, 2), (0, 25, 5, 1), (1, 0, 10, 2)]);
    }

    #[test]
    fn probe_takes_the_earliest_post_capture_of_dated_html_urls() {
        let cap = |surt: &str, crawl: &str, ts: &str, status: &str| Cap {
            range: 0,
            crawl: crawl.into(),
            surt: surt.into(),
            ts: ts.into(),
            status: status.into(),
            mime: "text/html".into(),
            url: format!("https://x/{surt}"),
            filename: "f".into(),
            offset: 1,
            length: 2,
            digest: String::new(),
            languages: None,
        };
        let post = vec![
            cap("com,x)/2015/03/a", "CC-MAIN-2024-51", "20241201000000", "200"),
            cap("com,x)/2015/03/a", "CC-MAIN-2023-06", "20230201000000", "200"),
            cap("com,x)/2015/04/b", "CC-MAIN-2023-06", "20230201000000", "404"),
            cap("com,x)/2022/12/c", "CC-MAIN-2023-06", "20230201000000", "200"),
            cap("com,x)/about", "CC-MAIN-2023-06", "20230201000000", "200"),
        ];
        let r: Range = serde_json::from_value(json!({"id": 3, "host": "x", "registered_domain": "x", "host_rank": "0", "block_lo": "a", "block_hi": "b",
            "block": {"crawl": "c", "file": 0, "offset": 0, "length": 1}, "lo": "a", "hi": "b", "sub_lines": 0, "sub_dated_html": 0, "months": {}})).unwrap();
        let p = probe_picks(&r, &post, "s", 16);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].crawl.as_str(), p[0].arm.as_str(), p[0].stratum.as_str()), ("CC-MAIN-2023-06", "probe", "x|2015-03"));
        assert_eq!(probe_picks(&r, &post, "s", 0).len(), 0);
    }

    #[test]
    fn exit_codes_separate_restartable_stops() {
        assert_eq!(Outcome::Stopped("throttled_25", String::new()).exit_code(), 74);
        assert_eq!(Outcome::Stopped("rate_block_403", String::new()).exit_code(), 75);
        assert_eq!(Outcome::Stopped("error", String::new()).exit_code(), 1);
        assert_eq!(Outcome::PlanExhausted(Value::Null).exit_code(), 78);
    }

    #[test]
    fn non_english_cctlds_match_the_v1_filter() {
        assert_eq!(NON_ENGLISH_CCTLDS.len(), 66);
        assert_eq!(tld("example.co.uk"), "uk");
        assert!(NON_ENGLISH_CCTLDS.contains(&tld("example.de")));
    }
}
