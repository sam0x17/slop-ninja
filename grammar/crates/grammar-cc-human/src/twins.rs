//! Membership twins: server-dated pre-cutoff pages whose
//! URL Common Crawl first captured after the 2022-11-30 cutoff, matched by
//! host and URL month to pages of the same host that the pre-cutoff frame
//! did capture.
//!
//! Steps (each resumable, every network read journaled; range reads share
//! one pacing schedule, at most 5 starts a second, with exponential
//! backoff on throttled responses and a two-minute pause on a 403 rate
//! block; run one network step at a time; see `Ranger::read`):
//! 1. `index`: `cluster.idx` of every frame crawl and of the post-cutoff
//!    crawls, downloaded whole with the polite fetcher.
//! 2. `select`: blocks of the first post-cutoff crawl whose first keys are
//!    two date-path URLs (`/YYYY/MM/`, before 2022-11) of one host are
//!    candidate archives. One block per host, hosts in keyed rank order,
//!    URL screens applied; the block is read and a contiguous sub-range of
//!    dated HTML captures is kept.
//! 3. `cdx`: for every sub-range and every crawl, the CDX blocks that can
//!    hold its keys are read by HTTP Range and the lines in the sub-range
//!    kept (compact rows).
//! 4. `build`: per URL key (SURT, and a base key without query, trailing
//!    slash and AMP/print suffixes): a **twin** has a 200 HTML capture in a
//!    post-cutoff crawl and no capture of either key, of any status, in any
//!    crawl up to and including CC-MAIN-2022-49; a **frame** URL has a 200
//!    HTML capture before the cutoff. Strata are host x URL month. Twins are
//!    drawn by keyed rank (capped per host) from strata that hold a frame
//!    URL, and frame URLs 1:1 from the same strata. Frame URLs recaptured
//!    after the cutoff also give a **frame_post** record (same URL, same
//!    capture era as the twins).
//! 5. `records`: WARC response records by HTTP Range.
//! 6. `extract`: one HTML-to-lines extractor for every arm, the page date
//!    (structured metadata) checked against the URL date, the pilot's page
//!    screens, site templates over distinct URLs of the set, the pilot's
//!    keyed passage choice and English rules, then a folded 12-word window
//!    check of every twin against every frame-side excerpt.
//! 7. `report`: joins the released A (seed 17) predictions, measures the
//!    matched gaps and computes the canary statistic and its power
//!    with the behaviour of models without a lookup index taken from Tier C.
//! 8. `tau`: models without a lookup index scored on the twin set give the
//!    canary statistic against R0 per model, the panel's tau and the power
//!    with it.
//!
//! No text from these pages is ever written to a tracked file.
use crate::{
    candidates, domain, fetch, lang::English, measure, screens, shingle, store,
};
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::{fineweb, psl::Psl, splits::rank, text};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

pub const POST_CRAWLS: [&str; 2] = ["CC-MAIN-2023-06", "CC-MAIN-2024-51"];
pub const STRADDLE: &str = "CC-MAIN-2022-49";
pub const CUTOFF_TS: &str = "20221130000000";
pub const CUTOFF_DATE: &str = "2022-11-30";
/// URL months strictly before this are eligible (November 2022 is dropped).
pub const URL_MONTH_LIMIT: &str = "2022-11";
pub const SEED: &str = "cc_twins_audit_v1";
pub const PASSAGE_SEED: &str = "cc_twins_audit_v1";
pub const EXTRACT_METHOD: &str = "slop_ninja_twins_html_lines_v1";

// ------------------------------------------------------------------ keys

/// (host part, path part) of a SURT key such as `com,example)/2015/03/x`.
pub fn split_surt(surt: &str) -> (&str, &str) {
    match surt.find(')') {
        Some(i) => (&surt[..i], &surt[i + 1..]),
        None => (surt, ""),
    }
}

/// `com,example,www` (optionally with `:port`) to `www.example.com`.
pub fn surt_host(h: &str) -> String {
    let h = h.split(':').next().unwrap_or(h);
    let mut v: Vec<&str> = h.split(',').filter(|s| !s.is_empty()).collect();
    v.reverse();
    v.join(".")
}

/// First `/YYYY/MM/` (optionally `/DD/`) in a path, as (`YYYY-MM`, day).
pub fn url_date(path: &str) -> Option<(String, Option<u8>)> {
    let p = path.split(['?', '#']).next().unwrap_or(path);
    let segs: Vec<&str> = p.split('/').collect();
    for i in 0..segs.len().saturating_sub(1) {
        let (y, m) = (segs[i], segs[i + 1]);
        if y.len() != 4 || m.len() != 2 || !y.bytes().all(|b| b.is_ascii_digit()) || !m.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let yy: u32 = y.parse().ok()?;
        let mm: u32 = m.parse().ok()?;
        if !(1995..=2026).contains(&yy) || !(1..=12).contains(&mm) {
            continue;
        }
        let day = segs.get(i + 2).and_then(|d| {
            (d.len() == 2 && d.bytes().all(|b| b.is_ascii_digit())).then(|| d.parse::<u8>().ok()).flatten().filter(|d| (1..=31).contains(d))
        });
        return Some((format!("{y}-{m}"), day));
    }
    None
}

pub fn eligible_month(ym: &str) -> bool {
    ym < URL_MONTH_LIMIT
}

/// Base key: the SURT without query or fragment, trailing slashes, and
/// AMP / print / feed suffixes, so variants of one page share it.
pub fn base_key(surt: &str) -> String {
    let mut s = surt.split(['?', '#']).next().unwrap_or(surt).to_owned();
    loop {
        let before = s.len();
        while s.ends_with('/') {
            s.pop();
        }
        for suf in ["/amp", ".amp", "/amp.html", "/print", "/feed", "/comments", "/embed", "/index.html", "/index.htm", "/index.php"] {
            if s.ends_with(suf) && s.len() > suf.len() + 1 {
                s.truncate(s.len() - suf.len());
            }
        }
        if s.len() == before {
            break;
        }
    }
    s
}

/// Hosts whose twins are likely copies of text the frame holds under another
/// host: mirror hosts with an embedded host name (`x.comwww.site.net`),
/// syndication portals, and `archive.` subdomains (site migrations).
pub const SYNDICATION_DOMAINS: &[&str] = &["aol.com", "yahoo.com", "msn.com", "newsbreak.com", "flipboard.com"];

pub fn copy_risk_host(host: &str, registered_domain: &str) -> Option<&'static str> {
    let embedded = host.split('.').skip(1).any(|l| ["comwww", "netwww", "orgwww"].iter().any(|p| l.ends_with(p)))
        || host.matches("www").count() > 1
        || (!host.starts_with("www.") && host.contains(".www."));
    if embedded {
        return Some("mirror_host_with_embedded_host_name");
    }
    if SYNDICATION_DOMAINS.contains(&registered_domain) {
        return Some("syndication_portal");
    }
    if host.starts_with("archive.") {
        return Some("archive_subdomain");
    }
    None
}

// ------------------------------------------------------------------ cluster.idx

pub fn cluster_rel(crawl: &str) -> String {
    format!("cc-index/collections/{crawl}/indexes/cluster.idx")
}

pub fn cluster_path(root: &Path, crawl: &str) -> PathBuf {
    root.join("index").join(crawl).join("cluster.idx")
}

pub struct Cluster {
    /// First key (`surt timestamp`) of each block.
    pub keys: Vec<String>,
    pub file: Vec<u16>,
    pub offset: Vec<u64>,
    pub length: Vec<u32>,
}

impl Cluster {
    pub fn load(path: &Path) -> Result<Cluster> {
        let r = BufReader::with_capacity(1 << 20, fs::File::open(path).with_context(|| format!("open {}", path.display()))?);
        let mut c = Cluster {
            keys: Vec::new(),
            file: Vec::new(),
            offset: Vec::new(),
            length: Vec::new(),
        };
        for (i, l) in r.lines().enumerate() {
            let l = l?;
            let f: Vec<&str> = l.split('\t').collect();
            ensure!(f.len() >= 4, "{} line {}: {} fields", path.display(), i + 1, f.len());
            let n = f[1].trim_start_matches("cdx-").trim_end_matches(".gz");
            c.keys.push(f[0].to_owned());
            c.file.push(n.parse().with_context(|| format!("{} line {}: file {}", path.display(), i + 1, f[1]))?);
            c.offset.push(f[2].parse()?);
            c.length.push(f[3].parse()?);
        }
        for w in c.keys.windows(2) {
            ensure!(w[0] <= w[1], "{}: keys not sorted", path.display());
        }
        Ok(c)
    }
    /// Blocks that can hold a line whose SURT is in `[lo, hi)`.
    pub fn blocks(&self, lo: &str, hi: &str) -> std::ops::RangeInclusive<usize> {
        let i0 = self.keys.partition_point(|k| k.as_str() <= lo).saturating_sub(1);
        let i1 = self.keys.partition_point(|k| k.as_str() < hi).saturating_sub(1).max(i0);
        i0..=i1
    }
    pub fn surt(&self, i: usize) -> &str {
        self.keys[i].split(' ').next().unwrap_or("")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockRef {
    pub crawl: String,
    pub file: u16,
    pub offset: u64,
    pub length: u32,
}

impl BlockRef {
    pub fn rel(&self) -> String {
        format!("cc-index/collections/{}/indexes/cdx-{:05}.gz", self.crawl, self.file)
    }
}

// ------------------------------------------------------------------ polite range reads

pub struct Ranger {
    pub budget: u64,
    pub min_free: u64,
    pub received: AtomicU64,
    pub stop: AtomicBool,
    pub throttled_in_a_row: AtomicUsize,
    pub blocked_in_a_row: AtomicUsize,
    /// Why every reader stopped: `rate_block_403` (three 403 rate blocks in a row) or
    /// `throttled_25` (25 throttled responses in a row).
    pub stop_reason: Mutex<Option<&'static str>>,
    /// Range requests sent by this process, and how many returned a throttle or a 403.
    pub requests: AtomicU64,
    pub throttled: AtomicU64,
    pub blocked: AtomicU64,
    pub journal: fetch::Journal,
    pub disk: PathBuf,
    /// Global pacing: (earliest next request start, current interval).
    pace: Mutex<(std::time::Instant, Duration)>,
}

/// Base spacing between request starts across all readers of one process (at most 5 a second; run one network step at a time).
pub const BASE_INTERVAL: Duration = Duration::from_millis(200);
pub const MAX_INTERVAL: Duration = Duration::from_millis(1000);

impl Ranger {
    pub fn new(journal: &Path, budget: u64, min_free: u64, disk: &Path) -> Result<Ranger> {
        Ok(Ranger {
            budget,
            min_free,
            received: AtomicU64::new(fetch::received_bytes(journal)?),
            stop: AtomicBool::new(false),
            throttled_in_a_row: AtomicUsize::new(0),
            blocked_in_a_row: AtomicUsize::new(0),
            stop_reason: Mutex::new(None),
            requests: AtomicU64::new(0),
            throttled: AtomicU64::new(0),
            blocked: AtomicU64::new(0),
            journal: fetch::Journal::open(journal)?,
            disk: disk.to_owned(),
            pace: Mutex::new((std::time::Instant::now(), BASE_INTERVAL)),
        })
    }
    /// Wait for this reader's slot in the global pacing schedule.
    fn wait_turn(&self) {
        let wait = {
            let mut g = self.pace.lock().unwrap();
            let now = std::time::Instant::now();
            let start = g.0.max(now);
            g.0 = start + g.1;
            start - now
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
    /// A throttled response pauses every reader and doubles the spacing.
    fn throttle_pause(&self, d: Duration) {
        let mut g = self.pace.lock().unwrap();
        g.0 = g.0.max(std::time::Instant::now() + d);
        g.1 = g.1.mul_f64(1.25).min(MAX_INTERVAL);
    }
    fn relax(&self) {
        let mut g = self.pace.lock().unwrap();
        g.1 = g.1.mul_f64(0.9).max(BASE_INTERVAL);
    }
    /// One Range read. A throttled response (429/503) sleeps this reader
    /// 2, 4, 8, 16, 32, 60, 60 s before retrying the same range, widens the
    /// shared spacing by 25% (at most 1 s; each success narrows it by 10%)
    /// and is retried up to 8 times; 25
    /// throttled responses in a row across readers, with no success between
    /// them, stop every reader. Other failures retry up to four attempts.
    pub fn read(&self, agent: &ureq::Agent, rel: &str, start: u64, len: u64) -> Result<Vec<u8>> {
        ensure!(len > 0 && len <= 64 << 20, "range length {len}");
        let url = format!("{}{rel}", fetch::BASE);
        let (mut errors, mut throttles) = (0u32, 0u32);
        loop {
            ensure!(!self.stop.load(Ordering::SeqCst), "range reader stopped");
            ensure!(self.received.load(Ordering::SeqCst) + len <= self.budget, "byte budget {} reached", self.budget);
            ensure!(fetch::free_bytes(&self.disk)? >= self.min_free + len, "disk headroom below {}", self.min_free);
            self.wait_turn();
            self.requests.fetch_add(1, Ordering::SeqCst);
            let r = agent.get(&url).header("Range", format!("bytes={}-{}", start, start + len - 1)).call();
            let (st, body, err) = match r {
                Ok(resp) => {
                    let st = resp.status().as_u16();
                    let mut b = Vec::new();
                    let e = if st == 206 {
                        resp.into_body().into_with_config().limit(len + 1).reader().read_to_end(&mut b).err().map(|e| e.to_string())
                    } else {
                        None
                    };
                    (st, b, e)
                }
                Err(e) => (0, Vec::new(), Some(format!("{e:#}"))),
            };
            self.received.fetch_add(body.len() as u64, Ordering::SeqCst);
            let ok = st == 206 && err.is_none() && body.len() as u64 == len;
            self.journal.write(&json!({"schema": fetch::JOURNAL_SCHEMA, "kind": "range", "rel": rel, "url": url,
                "range_start": start, "range_len": len, "attempt": errors + throttles + 1, "http_status": st,
                "status": if ok { "ok_range" } else { "failed_range" }, "detail": err,
                "bytes_received": body.len(), "finished_utc": grammar_a_pools::io::now_utc()}))?;
            if ok {
                self.throttled_in_a_row.store(0, Ordering::SeqCst);
                self.blocked_in_a_row.store(0, Ordering::SeqCst);
                self.relax();
                return Ok(body);
            }
            if st == 429 || st == 503 {
                self.throttled.fetch_add(1, Ordering::SeqCst);
                throttles += 1;
                let n = self.throttled_in_a_row.fetch_add(1, Ordering::SeqCst) + 1;
                if n >= 25 {
                    self.stop_reason.lock().unwrap().get_or_insert("throttled_25");
                    self.stop.store(true, Ordering::SeqCst);
                    bail!("25 consecutive throttled responses (last {st} on {rel})");
                }
                ensure!(throttles <= 8, "range read of {rel} throttled 8 times");
                self.throttle_pause(Duration::ZERO);
                std::thread::sleep(Duration::from_secs([2, 4, 8, 16, 32, 60, 60, 60][(throttles - 1) as usize]));
                continue;
            }
            if st == 403 {
                self.blocked.fetch_add(1, Ordering::SeqCst);
                // data.commoncrawl.org objects are public: a 403 is a rate block. Pause every reader
                // for two minutes; three in a row stop the run.
                let n = self.blocked_in_a_row.fetch_add(1, Ordering::SeqCst) + 1;
                if n >= 3 {
                    self.stop_reason.lock().unwrap().get_or_insert("rate_block_403");
                    self.stop.store(true, Ordering::SeqCst);
                    bail!("three consecutive 403 responses (rate block) on {rel}");
                }
                self.throttle_pause(Duration::from_secs(120));
                continue;
            }
            if st == 404 || st == 416 {
                bail!("range read of {rel} returned {st}");
            }
            errors += 1;
            ensure!(errors < 4, "range read of {rel} failed after 4 attempts");
            std::thread::sleep(Duration::from_secs(5 << errors));
        }
    }
}

/// Run `f` over `items` on at most `threads` (<= 4) workers, each with its
/// own agent. Stops at the first error.
pub fn parallel<T: Sync, F>(items: &[T], threads: usize, f: F) -> Result<()>
where
    F: Fn(&ureq::Agent, &T) -> Result<()> + Sync,
{
    ensure!((1..=fetch::MAX_CONNECTIONS).contains(&threads), "threads must be 1..=4");
    let next = AtomicUsize::new(0);
    let failed: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let agent = fetch::agent();
                loop {
                    if failed.lock().unwrap().is_some() {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= items.len() {
                        break;
                    }
                    if let Err(e) = f(&agent, &items[i]) {
                        let mut g = failed.lock().unwrap();
                        if g.is_none() {
                            *g = Some(e);
                        }
                        break;
                    }
                    if i.is_multiple_of(500) {
                        eprintln!("parallel: {i}/{}", items.len());
                    }
                }
            });
        }
    });
    match failed.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ------------------------------------------------------------------ CDX lines

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cap {
    pub range: u32,
    pub crawl: String,
    pub surt: String,
    pub ts: String,
    pub status: String,
    pub mime: String,
    pub url: String,
    pub filename: String,
    pub offset: u64,
    pub length: u64,
    pub digest: String,
    #[serde(default)]
    pub languages: Option<String>,
}

/// Parse one CDXJ line (`surt timestamp {json}`).
pub fn parse_cdx(line: &str, range: u32, crawl: &str) -> Option<Cap> {
    let (surt, rest) = line.split_once(' ')?;
    let (ts, js) = rest.split_once(' ')?;
    let v: Value = serde_json::from_str(js).ok()?;
    let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
    Some(Cap {
        range,
        crawl: crawl.to_owned(),
        surt: surt.to_owned(),
        ts: ts.to_owned(),
        status: g("status"),
        mime: if v.get("mime-detected").is_some() { g("mime-detected") } else { g("mime") },
        url: g("url"),
        filename: g("filename"),
        offset: g("offset").parse().unwrap_or(0),
        length: g("length").parse().unwrap_or(0),
        digest: g("digest"),
        languages: v.get("languages").and_then(|x| x.as_str()).map(str::to_owned),
    })
}

pub fn decode_block(gz: &[u8]) -> Result<String> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(gz).read_to_end(&mut out).context("CDX block gzip")?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

pub fn is_html(c: &Cap) -> bool {
    c.status == "200" && (c.mime.starts_with("text/html") || c.mime.starts_with("application/xhtml"))
}

// ------------------------------------------------------------------ step 1: index

pub fn frame_crawls(per_crawl_tsv: &Path) -> Result<Vec<String>> {
    let s = fs::read_to_string(per_crawl_tsv)?;
    let v: Vec<String> = s.lines().filter_map(|l| l.split('\t').next()).filter(|c| c.starts_with("CC-MAIN-")).map(str::to_owned).collect();
    ensure!(v.len() == 89 && v.last().map(String::as_str) == Some(STRADDLE), "expected 89 frame crawls ending {STRADDLE}");
    Ok(v)
}

pub fn index(root: &Path, crawls: &[String], limits: fetch::Limits) -> Result<Value> {
    let jobs: Vec<fetch::Job> = crawls
        .iter()
        .map(|c| fetch::Job {
            rel: cluster_rel(c),
            dest: cluster_path(root, c),
        })
        .collect();
    let s = fetch::run(jobs, limits, &root.join("journal").join("downloads.jsonl"))?;
    Ok(json!({"done": s.done, "skipped": s.skipped, "failed": s.failed, "received_bytes": s.received_bytes, "stop_reason": s.stop_reason}))
}

// ------------------------------------------------------------------ step 2: select

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Range {
    pub id: u32,
    pub host: String,
    pub registered_domain: String,
    pub host_rank: String,
    /// The post-cutoff block the host was drawn from.
    pub block_lo: String,
    pub block_hi: String,
    pub block: BlockRef,
    /// The kept sub-range `[lo, hi)` of SURT keys.
    pub lo: String,
    pub hi: String,
    pub sub_lines: usize,
    pub sub_dated_html: usize,
    pub months: BTreeMap<String, usize>,
}

pub struct SelectArgs<'a> {
    pub root: &'a Path,
    pub psl: &'a Psl,
    pub fineweb_excluded: &'a HashSet<String>,
    pub hosts: usize,
    pub sub_lines: usize,
    pub threads: usize,
    pub budget: u64,
    pub min_free: u64,
}

pub fn select(a: &SelectArgs) -> Result<Value> {
    let out = a.root.join("select").join("ranges.json");
    ensure!(!out.exists(), "{} exists", out.display());
    let post = POST_CRAWLS[0];
    let c = Cluster::load(&cluster_path(a.root, post))?;
    // Candidate archive blocks: first key and next first key are dated paths of one host.
    let mut per_host: HashMap<String, (u64, usize)> = HashMap::new();
    let mut n_dated_blocks = 0usize;
    let mut excluded: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut seen_host: HashMap<String, Option<&'static str>> = HashMap::new();
    for i in 0..c.keys.len().saturating_sub(1) {
        let (s1, s2) = (c.surt(i), c.surt(i + 1));
        let ((h1, p1), (h2, p2)) = (split_surt(s1), split_surt(s2));
        if h1 != h2 {
            continue;
        }
        let (Some((m1, _)), Some((m2, _))) = (url_date(p1), url_date(p2)) else { continue };
        if !eligible_month(&m1) || !eligible_month(&m2) {
            continue;
        }
        n_dated_blocks += 1;
        let host = surt_host(h1);
        let verdict = seen_host.entry(host.clone()).or_insert_with(|| {
            let Some(reg) = a.psl.registered_domain(&host) else { return Some("no_registered_domain") };
            screens::url_exclusion(&host, &reg, a.fineweb_excluded).or_else(|| screens::path_exclusion(&host, p1))
        });
        if let Some(r) = verdict {
            *excluded.entry(r).or_default() += 1;
            continue;
        }
        let rk = rank(SEED, "cc_twins_block_v1", s1);
        let e = per_host.entry(host).or_insert((u64::MAX, 0));
        if rk < e.0 {
            *e = (rk, i);
        }
    }
    let mut hosts: Vec<(u64, String, usize)> = per_host.into_iter().map(|(h, (_, i))| (rank(SEED, "cc_twins_host_v1", &h), h, i)).collect();
    hosts.sort();
    let n_hosts_available = hosts.len();
    hosts.truncate(a.hosts);
    // Read each chosen post-cutoff block.
    let ranger = Ranger::new(&a.root.join("journal").join("ranges.jsonl"), a.budget, a.min_free, a.root)?;
    let picked: Vec<(u64, String, usize, BlockRef)> = hosts
        .into_iter()
        .map(|(r, h, i)| {
            let b = BlockRef {
                crawl: post.to_owned(),
                file: c.file[i],
                offset: c.offset[i],
                length: c.length[i],
            };
            (r, h, i, b)
        })
        .collect();
    let lines_dir = a.root.join("select").join("post-blocks");
    fs::create_dir_all(&lines_dir)?;
    parallel(&picked, a.threads, |agent, (_, _, i, b)| {
        let p = lines_dir.join(format!("{i}.txt.zst"));
        if p.exists() {
            return Ok(());
        }
        let gz = ranger.read(agent, &b.rel(), b.offset, b.length as u64)?;
        let txt = decode_block(&gz)?;
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, zstd::encode_all(txt.as_bytes(), 6)?)?;
        fs::rename(&tmp, &p)?;
        Ok(())
    })?;
    let mut ranges = Vec::new();
    let mut skipped: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (r, h, i, b) in &picked {
        let txt = String::from_utf8(zstd::decode_all(&fs::read(lines_dir.join(format!("{i}.txt.zst")))?[..])?)?;
        let (lo_b, hi_b) = (c.surt(*i).to_owned(), c.surt(i + 1).to_owned());
        // Distinct SURTs of the host inside the block, in order, with a dated-HTML flag.
        let mut surts: Vec<(String, bool, Option<String>)> = Vec::new();
        for l in txt.lines() {
            let Some(cap) = parse_cdx(l, 0, post) else { continue };
            if cap.surt.as_str() < lo_b.as_str() || cap.surt.as_str() >= hi_b.as_str() {
                continue;
            }
            let m = url_date(split_surt(&cap.surt).1).map(|x| x.0).filter(|m| eligible_month(m));
            let dated = is_html(&cap) && m.is_some();
            match surts.last_mut() {
                Some(last) if last.0 == cap.surt => last.1 |= dated,
                _ => surts.push((cap.surt.clone(), dated, m)),
            }
        }
        let dated: Vec<usize> = (0..surts.len()).filter(|&k| surts[k].1).collect();
        if dated.len() < 20 {
            *skipped.entry("fewer_than_20_dated_html_urls_in_block").or_default() += 1;
            continue;
        }
        // Contiguous sub-range of `sub_lines` dated URLs starting at a keyed position.
        let take = a.sub_lines.min(dated.len());
        let start = (rank(SEED, "cc_twins_subrange_v1", h) % (dated.len() - take + 1) as u64) as usize;
        let (k0, k1) = (dated[start], dated[start + take - 1]);
        let lo = surts[k0].0.clone();
        let hi = if k1 + 1 < surts.len() { surts[k1 + 1].0.clone() } else { hi_b.clone() };
        let mut months: BTreeMap<String, usize> = BTreeMap::new();
        for s in &surts[k0..=k1] {
            if s.1 {
                *months.entry(s.2.clone().unwrap_or_default()).or_default() += 1;
            }
        }
        ranges.push(Range {
            id: ranges.len() as u32,
            host: h.clone(),
            registered_domain: a.psl.registered_domain(h).unwrap_or_default(),
            host_rank: format!("{r:016x}"),
            block_lo: lo_b,
            block_hi: hi_b,
            block: b.clone(),
            lo,
            hi,
            sub_lines: k1 + 1 - k0,
            sub_dated_html: take,
            months,
        });
    }
    let summary = json!({"schema": "slop_ninja_cc_twins_select_v1", "post_crawl": post, "cluster_lines": c.keys.len(),
        "dated_single_host_blocks": n_dated_blocks, "excluded_blocks_by_reason": excluded, "hosts_available": n_hosts_available,
        "hosts_read": picked.len(), "skipped": skipped, "ranges": ranges.len(), "sub_lines_target": a.sub_lines,
        "rule": "blocks of the first post-cutoff crawl whose first key and next first key are /YYYY/MM/ paths (month before 2022-11) of one host; one block per host by min rank(seed, cc_twins_block_v1, key); hosts by rank(seed, cc_twins_host_v1, host); URL and path exclusion screens; a contiguous run of sub_lines dated 200 HTML URLs at a keyed start",
        "range_bytes_received": ranger.received.load(Ordering::SeqCst)});
    store::write_json(&a.root.join("select").join("SELECT.json"), &summary)?;
    store::write_json(&out, &ranges)?;
    Ok(summary)
}

// ------------------------------------------------------------------ step 3: cdx

/// Range ids of a CDX batch: `select/batches/<batch>.json` (a JSON list).
/// Batches named `prescreen*` read only a few crawls and are not built.
pub fn batch_ranges(root: &Path, batch: &str) -> Result<Vec<Range>> {
    let all: Vec<Range> = store::read_json(&root.join("select").join("ranges.json"))?;
    let ids: Vec<u32> = store::read_json(&root.join("select").join("batches").join(format!("{batch}.json")))?;
    let set: HashSet<u32> = ids.iter().copied().collect();
    let v: Vec<Range> = all.into_iter().filter(|r| set.contains(&r.id)).collect();
    ensure!(v.len() == ids.len(), "batch {batch}: unknown range ids");
    Ok(v)
}

pub fn batches(root: &Path) -> Result<Vec<String>> {
    let mut v: Vec<String> = fs::read_dir(root.join("select").join("batches"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_owned))
        .filter(|n| !n.starts_with("prescreen"))
        .collect();
    v.sort();
    Ok(v)
}

pub fn cdx(root: &Path, batch: &str, crawls: &[String], threads: usize, budget: u64, min_free: u64, max_blocks: usize) -> Result<Value> {
    let ranges = batch_ranges(root, batch)?;
    let dir = root.join("cdx").join(batch);
    fs::create_dir_all(&dir)?;
    let ranger = Ranger::new(&root.join("journal").join("ranges.jsonl"), budget, min_free, root)?;
    let mut plan_rows = Vec::new();
    let mut total_blocks = 0usize;
    let mut total_bytes = 0u64;
    for crawl in crawls {
        let done = dir.join(format!("{crawl}.caps.jsonl.zst"));
        let c = Cluster::load(&cluster_path(root, crawl))?;
        // block -> ranges it serves
        let mut need: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for r in &ranges {
            for i in c.blocks(&r.lo, &r.hi) {
                need.entry(i).or_default().push(r.id);
            }
        }
        let bytes: u64 = need.keys().map(|&i| c.length[i] as u64).sum();
        total_blocks += need.len();
        total_bytes += bytes;
        plan_rows.push(json!({"crawl": crawl, "blocks": need.len(), "bytes": bytes}));
        if done.exists() {
            continue;
        }
        ensure!(need.len() <= max_blocks, "{crawl}: {} blocks exceed the per-crawl cap {max_blocks}", need.len());
        // Adjacent blocks of one CDX file are contiguous gzip members: read runs of up to 8 in one request.
        let mut items: Vec<(Vec<usize>, Vec<u32>)> = Vec::new();
        for (i, rids) in need {
            if let Some((run, rs)) = items.last_mut()
                && let Some(&j) = run.last()
                && j + 1 == i
                && c.file[j] == c.file[i]
                && c.offset[j] + c.length[j] as u64 == c.offset[i]
                && run.len() < 8
            {
                run.push(i);
                for r in rids {
                    if !rs.contains(&r) {
                        rs.push(r);
                    }
                }
                continue;
            }
            items.push((vec![i], rids));
        }
        let caps: Mutex<Vec<Cap>> = Mutex::new(Vec::new());
        let rmap: HashMap<u32, &Range> = ranges.iter().map(|r| (r.id, r)).collect();
        parallel(&items, threads, |agent, (run, rids)| {
            let (i0, i1) = (run[0], *run.last().unwrap());
            let b = BlockRef {
                crawl: crawl.clone(),
                file: c.file[i0],
                offset: c.offset[i0],
                length: (c.offset[i1] + c.length[i1] as u64 - c.offset[i0]) as u32,
            };
            let gz = ranger.read(agent, &b.rel(), b.offset, b.length as u64)?;
            let txt = decode_block(&gz)?;
            let mut v = Vec::new();
            for l in txt.lines() {
                let Some((surt, _)) = l.split_once(' ') else { continue };
                for rid in rids {
                    let r = rmap[rid];
                    if surt >= r.lo.as_str() && surt < r.hi.as_str()
                        && let Some(cap) = parse_cdx(l, *rid, crawl)
                    {
                        v.push(cap);
                    }
                }
            }
            caps.lock().unwrap().extend(v);
            Ok(())
        })?;
        let mut caps = caps.into_inner().unwrap();
        caps.sort_by(|a, b| (a.range, &a.surt, &a.ts).cmp(&(b.range, &b.surt, &b.ts)));
        let mut w = store::ZWriter::create(&done)?;
        for cap in &caps {
            w.write(cap)?;
        }
        let (sha, rows) = w.finish()?;
        eprintln!("cdx: {crawl}: {} requests, {rows} captures in range, sha {sha}", items.len());
    }
    let summary = json!({"schema": "slop_ninja_cc_twins_cdx_v1", "batch": batch, "crawls": crawls.len(), "ranges": ranges.len(),
        "blocks": total_blocks, "block_bytes": total_bytes, "per_crawl": plan_rows,
        "range_bytes_received_total": ranger.received.load(Ordering::SeqCst)});
    store::write_json(&dir.join("CDX.json"), &summary)?;
    Ok(summary)
}

/// Bytes and blocks the `cdx` step would read, without reading them.
/// (blocks, bytes, per-range bytes).
pub type CdxPlan = (usize, u64, Vec<(u32, u64)>);

pub fn cdx_plan(root: &Path, crawls: &[String], ranges: &[Range]) -> Result<CdxPlan> {
    let mut per_range: HashMap<u32, u64> = HashMap::new();
    let (mut blocks, mut bytes) = (0usize, 0u64);
    for crawl in crawls {
        let c = Cluster::load(&cluster_path(root, crawl))?;
        let mut need: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for r in ranges {
            for i in c.blocks(&r.lo, &r.hi) {
                need.entry(i).or_default().push(r.id);
            }
        }
        for (i, rids) in &need {
            blocks += 1;
            bytes += c.length[*i] as u64;
            for rid in rids {
                *per_range.entry(*rid).or_default() += c.length[*i] as u64 / rids.len() as u64;
            }
        }
    }
    let mut v: Vec<(u32, u64)> = per_range.into_iter().collect();
    v.sort();
    Ok((blocks, bytes, v))
}

// ------------------------------------------------------------------ step 4: build

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pick {
    pub id: String,
    pub arm: String,
    pub range: u32,
    pub host: String,
    pub registered_domain: String,
    pub stratum: String,
    pub url_month: String,
    pub surt: String,
    pub url: String,
    pub crawl: String,
    pub ts: String,
    pub filename: String,
    pub offset: u64,
    pub length: u64,
    pub digest: String,
    pub languages: Option<String>,
    /// Frame crawls (2013-20..2022-49) holding any capture of the key or base key.
    pub frame_crawls_seen: usize,
    pub first_frame_ts: Option<String>,
    pub post_crawls_seen: Vec<String>,
    pub rank: String,
    /// For frame_post: the id of the frame pick of the same URL.
    pub pair: Option<String>,
}

/// (rank, SURT) pairs of one arm in one stratum.
type Ranked = Vec<(u64, String)>;

#[derive(Default)]
pub struct KeyInfo {
    frame_any: bool,
    straddle_post_any: bool,
    frame_html: Vec<Cap>,
    post_html: Vec<Cap>,
    frame_crawls: BTreeSet<String>,
    post_crawls: BTreeSet<String>,
    first_frame_ts: Option<String>,
}

/// Every capture of one range, classified per URL key.
#[derive(Default)]
pub struct RangeKeys {
    pub keys: BTreeMap<String, KeyInfo>,
    /// Base keys with a capture before the cutoff (or in CC-MAIN-2022-49 at any time).
    pub base_frame: HashSet<String>,
}

impl RangeKeys {
    /// Add one capture. Feed a range's captures crawl by crawl in the frame-then-post order,
    /// each crawl's captures sorted by (SURT, timestamp).
    pub fn absorb(&mut self, cap: Cap, is_frame: bool) {
        let pre = is_frame && (cap.crawl != STRADDLE || cap.ts.as_str() < CUTOFF_TS);
        let straddle_post = cap.crawl == STRADDLE && cap.ts.as_str() >= CUTOFF_TS;
        if pre || straddle_post {
            self.base_frame.insert(base_key(&cap.surt));
        }
        let k = self.keys.entry(cap.surt.clone()).or_default();
        if pre {
            k.frame_any = true;
            k.frame_crawls.insert(cap.crawl.clone());
            if k.first_frame_ts.as_ref().is_none_or(|t| cap.ts < *t) {
                k.first_frame_ts = Some(cap.ts.clone());
            }
            if is_html(&cap) {
                k.frame_html.push(cap);
            }
        } else if straddle_post {
            k.straddle_post_any = true;
        } else if !is_frame {
            k.post_crawls.insert(cap.crawl.clone());
            if is_html(&cap) {
                k.post_html.push(cap);
            }
        }
    }
}

/// Pick rules of one build: the per-host twin cap and frame URLs drawn per picked twin
/// in each stratum (`frame_ratio` 1.0 is the twins-v1 1:1 draw).
#[derive(Clone, Copy, Debug)]
pub struct PickRule {
    pub per_host: usize,
    pub frame_ratio: f64,
}

impl PickRule {
    /// Frame URLs wanted in a stratum where `n` twins were picked.
    pub fn frames_for(&self, n: usize) -> usize {
        (n as f64 * self.frame_ratio - 1e-9).ceil().max(0.0) as usize
    }
}

/// Twins, frame and frame_post picks of one range, its funnel counts and its summary row.
pub fn build_range(r: &Range, rk: &RangeKeys, rule: PickRule) -> (Vec<Pick>, BTreeMap<String, u64>, Value) {
    let rid = &r.id;
    let keys = &rk.keys;
    let mut picks: Vec<Pick> = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    // stratum -> (twins, frames), each (rank, surt)
    let mut strata: BTreeMap<String, (Ranked, Ranked)> = BTreeMap::new();
    let (mut n_twin, mut n_frame, mut n_dated_post, mut n_base_member) = (0, 0, 0, 0);
    for (surt, k) in keys {
        let Some((ym, _)) = url_date(split_surt(surt).1) else { continue };
        if !eligible_month(&ym) {
            continue;
        }
        let base_member = rk.base_frame.contains(&base_key(surt));
        if !k.post_html.is_empty() {
            n_dated_post += 1;
        }
        let twin = !k.post_html.is_empty() && !k.frame_any && !k.straddle_post_any;
        if twin && base_member {
            n_base_member += 1;
        }
        let rank_ = rank(SEED, "cc_twins_pick_v1", surt);
        let e = strata.entry(ym).or_default();
        if twin && !base_member {
            n_twin += 1;
            e.0.push((rank_, surt.clone()));
        } else if !k.frame_html.is_empty() {
            n_frame += 1;
            e.1.push((rank_, surt.clone()));
        }
    }
    counts.insert("dated_keys_with_post_html".into(), n_dated_post);
    counts.insert("twin_keys".into(), n_twin);
    counts.insert("frame_keys".into(), n_frame);
    counts.insert("twin_keys_dropped_base_key_in_frame".into(), n_base_member);
    // Twins from strata that also hold a frame URL, in rank order, capped per host.
    let mut cand: Vec<(u64, String, String)> = Vec::new();
    for (ym, (t, f)) in &strata {
        if f.is_empty() {
            continue;
        }
        for (rk, s) in t {
            cand.push((*rk, s.clone(), ym.clone()));
        }
    }
    let matched_twins = cand.len();
    cand.sort();
    cand.truncate(rule.per_host);
    let mut need: BTreeMap<String, usize> = BTreeMap::new();
    for (_, _, ym) in &cand {
        *need.entry(ym.clone()).or_default() += 1;
    }
    let mk = |arm: &str, surt: &str, cap: &Cap, ym: &str, rk: u64, k: &KeyInfo, pair: Option<String>| Pick {
        id: format!("tw-{}-{}", arm, &text::sha_hex(format!("{}\0{}\0{}", arm, surt, cap.ts).as_bytes())[..20]),
        arm: arm.to_owned(),
        range: *rid,
        host: r.host.clone(),
        registered_domain: r.registered_domain.clone(),
        stratum: format!("{}|{ym}", r.host),
        url_month: ym.to_owned(),
        surt: surt.to_owned(),
        url: cap.url.clone(),
        crawl: cap.crawl.clone(),
        ts: cap.ts.clone(),
        filename: cap.filename.clone(),
        offset: cap.offset,
        length: cap.length,
        digest: cap.digest.clone(),
        languages: cap.languages.clone(),
        frame_crawls_seen: k.frame_crawls.len(),
        first_frame_ts: k.first_frame_ts.clone(),
        post_crawls_seen: k.post_crawls.iter().cloned().collect(),
        rank: format!("{rk:016x}"),
        pair,
    };
    let (mut kt, mut kf, mut kp) = (0, 0, 0);
    for (rk, s, ym) in &cand {
        let k = &keys[s];
        // earliest post-cutoff capture (first post crawl preferred)
        let cap = k.post_html.iter().min_by(|a, b| (&a.crawl, &a.ts).cmp(&(&b.crawl, &b.ts))).unwrap();
        picks.push(mk("twin", s, cap, ym, *rk, k, None));
        kt += 1;
    }
    for (ym, n) in &need {
        let mut f = strata[ym].1.clone();
        f.sort();
        for (rk, s) in f.into_iter().take(rule.frames_for(*n)) {
            let k = &keys[&s];
            // latest pre-cutoff capture
            let cap = k.frame_html.iter().max_by(|a, b| a.ts.cmp(&b.ts)).unwrap();
            let fp = mk("frame", &s, cap, ym, rk, k, None);
            let fid = fp.id.clone();
            picks.push(fp);
            kf += 1;
            if let Some(pc) = k.post_html.iter().min_by(|a, b| (&a.crawl, &a.ts).cmp(&(&b.crawl, &b.ts))) {
                picks.push(mk("frame_post", &s, pc, ym, rk, k, Some(fid)));
                kp += 1;
            }
        }
    }
    let row = json!({"range": rid, "host": r.host, "keys": keys.len(), "dated_keys_with_post_html": n_dated_post,
        "twin_keys": n_twin, "frame_keys": n_frame, "twin_keys_in_matched_strata": matched_twins,
        "picked_twin": kt, "picked_frame": kf, "picked_frame_post": kp});
    (picks, counts, row)
}

pub const BUILD_RULES_TEXT: &str = "a 200 HTML capture in a post-cutoff crawl; no capture of any status of the SURT in any crawl 2013-20..2022-49 (2022-49 at any time), and no pre-cutoff capture of its base key; URL month before 2022-11";

pub fn build(root: &Path, frame: &[String], per_host: usize, batch_names: &[String]) -> Result<Value> {
    let frame_set: HashSet<&str> = frame.iter().map(String::as_str).collect();
    let mut by_range: BTreeMap<u32, RangeKeys> = BTreeMap::new();
    let mut caps_total = 0u64;
    let mut crawls_read = 0usize;
    let mut ranges: Vec<Range> = Vec::new();
    for b in batch_names {
        ranges.extend(batch_ranges(root, b)?);
    }
    for crawl in frame.iter().chain(POST_CRAWLS.iter().map(|s| s.to_string()).collect::<Vec<_>>().iter()) {
        for b in batch_names {
            let p = root.join("cdx").join(b).join(format!("{crawl}.caps.jsonl.zst"));
            ensure!(p.exists(), "missing {}", p.display());
            crawls_read += 1;
            let caps: Vec<Cap> = store::rows(&p)?;
            caps_total += caps.len() as u64;
            let is_frame = frame_set.contains(crawl.as_str());
            for cap in caps {
                by_range.entry(cap.range).or_default().absorb(cap, is_frame);
            }
        }
    }
    let rmap: HashMap<u32, &Range> = ranges.iter().map(|r| (r.id, r)).collect();
    let rule = PickRule { per_host, frame_ratio: 1.0 };
    let mut picks: Vec<Pick> = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut per_range_rows = Vec::new();
    for (rid, rk) in &by_range {
        let (p, c, row) = build_range(rmap[rid], rk, rule);
        picks.extend(p);
        for (k, v) in c {
            *counts.entry(k).or_default() += v;
        }
        per_range_rows.push(row);
    }
    for p in &picks {
        *counts.entry(format!("picked_{}", p.arm)).or_default() += 1;
    }
    let out = root.join("build");
    let mut w = store::ZWriter::create(&out.join("picks.jsonl.zst"))?;
    for p in &picks {
        w.write(p)?;
    }
    let (sha, rows) = w.finish()?;
    let summary = json!({"schema": "slop_ninja_cc_twins_build_v1", "batches": batch_names, "crawl_batch_files_read": crawls_read, "captures": caps_total,
        "ranges": by_range.len(), "per_host_cap": per_host, "counts": counts, "picks": rows, "picks_sha256": sha,
        "rules": {"twin": BUILD_RULES_TEXT,
                  "frame": "a 200 HTML capture before 2022-11-30 in a frame crawl; URL month before 2022-11; the latest such capture is read",
                  "frame_post": "a frame URL's earliest 200 HTML capture in a post-cutoff crawl",
                  "strata": "host x URL month; twins only from strata holding a frame URL; frame URLs drawn 1:1 per stratum by rank"},
        "per_range": per_range_rows});
    store::write_json(&out.join("BUILD.json"), &summary)?;
    Ok(summary)
}

// ------------------------------------------------------------------ step 5: records

/// Rows of a records index (`records.jsonl`). A torn last line (a crash while appending)
/// is ignored; the record is read again.
pub fn record_index(done_path: &Path) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    if !done_path.exists() {
        return Ok(out);
    }
    let s = fs::read_to_string(done_path)?;
    let lines: Vec<&str> = s.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        match serde_json::from_str::<Value>(l) {
            Ok(v) => out.push(v),
            Err(_) if i + 1 == lines.len() && !s.ends_with('\n') => {}
            Err(e) => return Err(e).with_context(|| format!("{} line {}", done_path.display(), i + 1)),
        }
    }
    Ok(out)
}

/// Read the WARC records of `picks` into `dir/records.warc.gz`, indexed in
/// `dir/records.jsonl` (one fsynced line per record; records already indexed are kept).
/// Returns (already indexed, fetched now, failed: 404/416 or a bad length).
pub fn fetch_records(ranger: &Ranger, threads: usize, picks: &[Pick], dir: &Path) -> Result<(usize, usize, Vec<Value>)> {
    let done_path = dir.join("records.jsonl");
    let done: HashSet<String> = record_index(&done_path)?.iter().map(|v| v["id"].as_str().unwrap_or("").to_owned()).collect();
    let todo: Vec<&Pick> = picks.iter().filter(|p| !done.contains(&p.id)).collect();
    fs::create_dir_all(dir)?;
    if todo.is_empty() {
        return Ok((done.len(), 0, Vec::new()));
    }
    let mut idx = fs::OpenOptions::new().create(true).append(true).open(&done_path)?;
    // Terminate a torn last line so the next record starts on its own line.
    if done_path.metadata()?.len() > 0 && !fs::read(&done_path)?.ends_with(b"\n") {
        writeln!(idx)?;
    }
    let sink = Mutex::new((fs::OpenOptions::new().create(true).append(true).open(dir.join("records.warc.gz"))?, idx));
    let failed = Mutex::new(Vec::new());
    parallel(&todo, threads, |agent, p| {
        if p.length == 0 || p.length > 16 << 20 {
            failed.lock().unwrap().push(json!({"id": p.id, "reason": "bad_length"}));
            return Ok(());
        }
        let b = match ranger.read(agent, &p.filename, p.offset, p.length) {
            Ok(b) => b,
            Err(e) if !ranger.stop.load(Ordering::SeqCst) && (e.to_string().contains("returned 404") || e.to_string().contains("returned 416")) => {
                failed.lock().unwrap().push(json!({"id": p.id, "reason": format!("{e:#}")}));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut g = sink.lock().unwrap();
        let off = g.0.metadata()?.len();
        g.0.write_all(&b)?;
        g.0.sync_data()?;
        let line = serde_json::to_string(&json!({"id": p.id, "local_offset": off, "local_length": b.len(), "sha256": text::sha_hex(&b)}))?;
        writeln!(g.1, "{line}")?;
        g.1.sync_data()?;
        Ok(())
    })?;
    let failed = failed.into_inner().unwrap();
    Ok((done.len(), todo.len() - failed.len(), failed))
}

pub fn records(root: &Path, threads: usize, budget: u64, min_free: u64) -> Result<Value> {
    let picks: Vec<Pick> = store::rows(&root.join("build").join("picks.jsonl.zst"))?;
    let dir = root.join("records");
    let ranger = Ranger::new(&root.join("journal").join("ranges.jsonl"), budget, min_free, root)?;
    let (already, fetched, failed) = fetch_records(&ranger, threads, &picks, &dir)?;
    let summary = json!({"schema": "slop_ninja_cc_twins_records_v1", "picks": picks.len(), "already": already, "fetched_now": fetched, "failed": failed});
    store::write_json(&dir.join("RECORDS.json"), &summary)?;
    Ok(summary)
}

// ------------------------------------------------------------------ HTML

const SKIP: &[&str] = &["script", "style", "noscript", "template", "svg", "math", "iframe", "object", "select", "textarea", "button"];
const BLOCK: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "h1", "h2", "h3", "h4", "h5", "h6", "tr", "td", "th", "table", "thead", "tbody", "tfoot", "section", "article",
    "header", "footer", "nav", "aside", "blockquote", "pre", "form", "hr", "dd", "dt", "dl", "main", "figure", "figcaption", "address", "center",
    "option", "title", "caption", "fieldset", "legend", "details", "summary", "body", "html", "head", "label",
];

fn tag_name(t: &str) -> (String, bool) {
    let t = t.trim_start_matches('<');
    let close = t.starts_with('/');
    let t = t.trim_start_matches('/');
    let n: String = t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    (n, close)
}

/// HTML to non-empty text lines: block tags and `<br>` break lines,
/// inline tags join, whitespace collapses (line breaks kept inside
/// `<pre>`), entities decode, comments and the contents of scripts,
/// styles, embedded objects and form controls drop.
pub fn html_lines(html: &str) -> String {
    let low = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 3);
    let mut cur = String::new();
    let mut pre = 0usize;
    let b = html.as_bytes();
    let mut i = 0;
    let flush = |cur: &mut String, out: &mut String| {
        let t = grammar_a_pools::wikitext::decode_entities(cur);
        for l in t.split('\n') {
            let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
            if !l.is_empty() {
                out.push_str(&l);
                out.push('\n');
            }
        }
        cur.clear();
    };
    while i < b.len() {
        if b[i] == b'<' {
            if low[i..].starts_with("<!--") {
                i = low[i..].find("-->").map(|k| i + k + 3).unwrap_or(b.len());
                continue;
            }
            let end = match low[i..].find('>') {
                Some(k) => i + k + 1,
                None => b.len(),
            };
            let (name, close) = tag_name(&html[i..end]);
            let self_closing = html[i..end].ends_with("/>");
            i = end;
            if name.is_empty() {
                continue;
            }
            if !close && !self_closing && SKIP.contains(&name.as_str()) {
                let pat = format!("</{name}");
                i = match low[i..].find(&pat) {
                    Some(k) => low[i + k..].find('>').map(|e| i + k + e + 1).unwrap_or(b.len()),
                    None => b.len(),
                };
                continue;
            }
            if name == "pre" {
                pre = if close { pre.saturating_sub(1) } else { pre + 1 };
            }
            if BLOCK.contains(&name.as_str()) {
                flush(&mut cur, &mut out);
            }
            continue;
        }
        let next = low[i..].find('<').map(|k| i + k).unwrap_or(b.len());
        let seg = &html[i..next];
        if pre > 0 {
            for (n, part) in seg.split('\n').enumerate() {
                if n > 0 {
                    flush(&mut cur, &mut out);
                }
                cur.push_str(part);
            }
        } else {
            cur.push_str(seg);
        }
        i = next;
    }
    flush(&mut cur, &mut out);
    out
}

fn cp1252(b: &[u8]) -> String {
    const HI: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜',
        '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    b.iter().map(|&x| if (0x80..0xa0).contains(&x) { HI[(x - 0x80) as usize] } else { x as char }).collect()
}

/// Decode an HTTP body by declared charset: UTF-8 (lossy) unless the header
/// or a `<meta>` charset names ISO-8859-1 / windows-1252.
pub fn decode_body(body: &[u8], content_type: &str) -> (String, &'static str) {
    let head: Vec<u8> = body[..body.len().min(4096)].to_ascii_lowercase();
    let ct = content_type.to_ascii_lowercase();
    let latin = |s: &str| s.contains("iso-8859-1") || s.contains("windows-1252") || s.contains("latin1") || s.contains("latin-1");
    let meta = head.windows(7).position(|w| w == b"charset").map(|k| String::from_utf8_lossy(&head[k..(k + 30).min(head.len())]).into_owned());
    let declared_latin = latin(&ct) || (!ct.contains("charset") && meta.as_deref().is_some_and(latin));
    if declared_latin && std::str::from_utf8(body).is_err() {
        (cp1252(body), "cp1252")
    } else {
        (String::from_utf8_lossy(body).into_owned(), "utf8")
    }
}

/// `YYYY-MM-DD` from the start of an attribute or JSON value.
fn ymd(s: &str) -> Option<String> {
    let s = s.trim().trim_matches(|c| c == '"' || c == '\'');
    let d = s.get(..10)?;
    grammar_a_pools::record::valid_date(d).then(|| d.to_owned())
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let low = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(k) = low[from..].find(name) {
        let at = from + k;
        let rest = &tag[at + name.len()..];
        let rest_t = rest.trim_start();
        if let Some(r) = rest_t.strip_prefix('=') {
            let r = r.trim_start();
            let q = r.chars().next()?;
            if q == '"' || q == '\'' {
                let r = &r[1..];
                return r.find(q).map(|e| &r[..e]);
            }
            return Some(r.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or(""));
        }
        from = at + name.len();
    }
    None
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageDates {
    pub published: Option<String>,
    pub published_source: Option<String>,
    pub modified: Option<String>,
}

/// Publication and modification dates from structured page metadata:
/// `article:published_time` / `og:published_time` / `datePublished` /
/// `pubdate` / `date` meta tags, JSON-LD `datePublished`, then the first
/// `<time datetime>`.
/// `&s[a..b]` with `b` moved down to a character boundary.
fn cut(s: &str, a: usize, b: usize) -> &str {
    let mut b = b.min(s.len());
    while b > a && !s.is_char_boundary(b) {
        b -= 1;
    }
    &s[a..b]
}

pub fn page_dates(html: &str) -> PageDates {
    let h = cut(html, 0, 600_000);
    let low = h.to_ascii_lowercase();
    let mut d = PageDates::default();
    let set = |d: &mut PageDates, v: Option<String>, src: &str| {
        if d.published.is_none() && v.is_some() {
            d.published = v;
            d.published_source = Some(src.to_owned());
        }
    };
    for (k, _) in low.match_indices("<meta") {
        let end = low[k..].find('>').map(|e| k + e).unwrap_or(low.len());
        let tag = &h[k..end];
        let key = attr(tag, "property").or_else(|| attr(tag, "name")).or_else(|| attr(tag, "itemprop")).unwrap_or("").to_ascii_lowercase();
        let val = attr(tag, "content").and_then(ymd);
        match key.as_str() {
            "article:published_time" | "og:published_time" | "og:article:published_time" | "datepublished" | "pubdate" | "publishdate"
            | "publish-date" | "publish_date" | "sailthru.date" | "dc.date.issued" | "dcterms.date" | "dc.date" | "date" | "parsely-pub-date" => {
                set(&mut d, val, &format!("meta:{key}"))
            }
            "article:modified_time" | "og:updated_time" | "datemodified" | "dcterms.modified" | "last-modified" if d.modified.is_none() => {
                d.modified = val;
            }
            _ => {}
        }
    }
    if d.published.is_none()
        && let Some(k) = low.find("\"datepublished\"")
    {
        let rest = cut(h, k + 15, k + 80);
        let v = rest.split_once(':').map(|(_, r)| r.trim_start()).and_then(ymd);
        set(&mut d, v, "jsonld:datePublished");
    }
    if d.modified.is_none()
        && let Some(k) = low.find("\"datemodified\"")
    {
        let rest = cut(h, k + 14, k + 80);
        d.modified = rest.split_once(':').map(|(_, r)| r.trim_start()).and_then(ymd);
    }
    if d.published.is_none()
        && let Some(k) = low.find("<time")
    {
        let end = low[k..].find('>').map(|e| k + e).unwrap_or(low.len());
        set(&mut d, attr(&h[k..end], "datetime").and_then(ymd), "time:datetime");
    }
    d
}

pub fn days(a: &str, b: &str) -> Option<i64> {
    let f = |s: &str| -> Option<i64> {
        let y: i64 = s.get(..4)?.parse().ok()?;
        let m: i64 = s.get(5..7)?.parse().ok()?;
        let d: i64 = s.get(8..10).and_then(|x| x.parse().ok()).unwrap_or(15);
        // days from civil (Howard Hinnant)
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(era * 146097 + doe)
    };
    Some(f(a)? - f(b)?)
}

// ------------------------------------------------------------------ step 6: extract

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Doc {
    pub id: String,
    pub arm: String,
    pub range: u32,
    pub host: String,
    pub registered_domain: String,
    pub group: String,
    pub stratum: String,
    pub url_month: String,
    pub url: String,
    pub crawl: String,
    pub warc_date: String,
    pub record_id: String,
    pub payload_digest: Option<String>,
    pub filename: String,
    pub offset: u64,
    pub length: u64,
    pub page_published: Option<String>,
    pub page_published_source: Option<String>,
    pub page_modified: Option<String>,
    pub charset: String,
    pub page_sha256: String,
    pub page_words: usize,
    pub domain: String,
    pub domain_rule: String,
    pub page_domain: String,
    pub line_start: usize,
    pub line_end: usize,
    pub valid_runs: usize,
    pub words: usize,
    pub text: String,
    pub text_sha256: String,
    pub folded_sha256: String,
    pub text_redacted: Option<String>,
    pub pair: Option<String>,
    pub frame_crawls_seen: usize,
    pub first_frame_ts: Option<String>,
    /// Exclusion from the matched analysis (set by the twin text check).
    pub excluded: Option<String>,
}

impl Doc {
    pub fn scored_text(&self) -> &str {
        self.text_redacted.as_deref().unwrap_or(&self.text)
    }
}

struct Page {
    pick: Pick,
    record_id: String,
    warc_date: String,
    payload_digest: Option<String>,
    charset: String,
    dates: PageDates,
    text: String,
}

/// Parse a WARC response member into (WARC record, HTTP status, content type, body).
fn response(member: &[u8]) -> Result<(crate::warc::Record, u16, String, Vec<u8>)> {
    let (raw, _) = crate::warc::decode_member(member)?;
    let rec = crate::warc::parse_record(&raw, 0, member.len() as u64)?;
    ensure!(rec.kind() == "response", "WARC-Type {}", rec.kind());
    let b = &rec.block;
    let end = b.windows(4).position(|w| w == b"\r\n\r\n").context("HTTP header end")?;
    let head = String::from_utf8_lossy(&b[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap_or("").split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
    let (mut ct, mut ce) = (String::new(), String::new());
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-type" => ct = v.trim().to_owned(),
                "content-encoding" => ce = v.trim().to_ascii_lowercase(),
                _ => {}
            }
        }
    }
    let mut body = b[end + 4..].to_vec();
    if ce.contains("gzip") {
        let mut o = Vec::new();
        if flate2::read::MultiGzDecoder::new(&body[..]).read_to_end(&mut o).is_ok() {
            body = o;
        }
    }
    Ok((rec, status, ct, body))
}

/// Funnel counts by arm and stage.
pub type Funnel = BTreeMap<String, BTreeMap<String, u64>>;

fn bump(f: &mut Funnel, arm: &str, stage: &str) {
    *f.entry(arm.to_owned()).or_default().entry(stage.to_owned()).or_default() += 1;
}

/// Domain rules of every twin bank: pilot rules v2 for all arms. twins-v1 was extracted with
/// them; a change is a new bank version, since both arms must use one rule set.
pub const TWIN_RULES_ID: &str = "cc_human_domain_rules_pilot_v2";

pub fn twin_rules() -> domain::Rules {
    domain::Rules::parse(domain::PILOT_RULES_V2).expect("pilot rules v2")
}

/// Parse the indexed WARC records of `dir` (`records.jsonl`, `records.warc.gz`) into pages.
/// Records whose id is not in `pmap` are skipped.
fn load_pages(dir: &Path, pmap: &HashMap<&str, &Pick>, f: &mut Funnel) -> Result<Vec<Page>> {
    let index = record_index(&dir.join("records.jsonl"))?;
    let mut pages: Vec<Page> = Vec::new();
    if index.is_empty() {
        return Ok(pages);
    }
    let warc = fs::read(dir.join("records.warc.gz"))?;
    for v in &index {
        let id = v["id"].as_str().unwrap_or("");
        let Some(p) = pmap.get(id) else { continue };
        let (o, n) = (v["local_offset"].as_u64().unwrap_or(0) as usize, v["local_length"].as_u64().unwrap_or(0) as usize);
        ensure!(o + n <= warc.len(), "{id}: local record out of bounds");
        let member = &warc[o..o + n];
        ensure!(text::sha_hex(member) == v["sha256"].as_str().unwrap_or(""), "{id}: local record digest");
        bump(f, &p.arm, "a_records");
        let (rec, status, ct, body) = match response(member) {
            Ok(x) => x,
            Err(_) => {
                bump(f, &p.arm, "x_unparsed_record");
                continue;
            }
        };
        if status != 200 {
            bump(f, &p.arm, "x_http_status_not_200");
            continue;
        }
        let (html, charset) = decode_body(&body, &ct);
        let dates = page_dates(&html);
        pages.push(Page {
            pick: (*p).clone(),
            record_id: rec.header("WARC-Record-ID").unwrap_or("").to_owned(),
            warc_date: rec.header("WARC-Date").unwrap_or("").to_owned(),
            payload_digest: rec.header("WARC-Payload-Digest").map(str::to_owned),
            charset: charset.to_owned(),
            dates,
            text: html_lines(&html),
        });
    }
    Ok(pages)
}

/// Site templates: line keys on two or more distinct URLs of a registered domain.
fn site_templates(pages: &[Page]) -> HashSet<u64> {
    let mut per_url: HashMap<(String, String), HashSet<u64>> = HashMap::new();
    for pg in pages {
        let e = per_url.entry((pg.pick.registered_domain.clone(), pg.pick.surt.clone())).or_default();
        for l in candidates::lines(&pg.text) {
            if let Some(k) = candidates::site_line_key(&pg.pick.registered_domain, l) {
                e.insert(k);
            }
        }
    }
    let mut kcount: HashMap<u64, usize> = HashMap::new();
    for ks in per_url.values() {
        for k in ks {
            *kcount.entry(*k).or_default() += 1;
        }
    }
    kcount.into_iter().filter(|(_, n)| *n >= 2).map(|(k, _)| k).collect()
}

/// Inputs of the page-to-excerpt path that every arm shares.
pub struct ExtractCtx<'a> {
    pub psl: &'a Psl,
    pub fineweb_excluded: &'a HashSet<String>,
    pub rules: &'a domain::Rules,
}

/// The extraction path of every arm and every bank: records of `dir` to pages, site templates
/// over the distinct URLs of each registered domain among them, then dates, page screens,
/// the keyed passage choice, English and excerpt screens. Returns (documents, funnel, pages,
/// templates); the twin text check is separate (`twin_text_check`).
pub fn extract_dir(dir: &Path, picks: &[Pick], cx: &ExtractCtx) -> Result<(Vec<Doc>, Funnel, usize, usize)> {
    let pmap: HashMap<&str, &Pick> = picks.iter().map(|p| (p.id.as_str(), p)).collect();
    let (psl, fineweb_excluded, rules) = (cx.psl, cx.fineweb_excluded, cx.rules);
    let mut funnel = Funnel::new();
    let f = &mut funnel;
    let pages = load_pages(dir, &pmap, f)?;
    let templates = site_templates(&pages);
    let mut docs: Vec<Doc> = Vec::new();
    let debug = std::env::var_os("TWINS_DEBUG_DUMP").map(PathBuf::from);
    let mut dumped = 0usize;
    for pg in &pages {
        let p = &pg.pick;
        let arm = p.arm.as_str();
        // Dates: the URL month is structural; page metadata must agree within 45 days.
        let url_day = format!("{}-15", p.url_month);
        if let Some(pd) = &pg.dates.published {
            match days(pd, &url_day) {
                Some(dd) if dd.abs() <= 45 => {}
                _ => {
                    bump(f, arm, "x_page_date_disagrees_with_url_month");
                    continue;
                }
            }
            if pd.as_str() >= CUTOFF_DATE {
                bump(f, arm, "x_page_date_after_cutoff");
                continue;
            }
        }
        if arm != "frame" && pg.dates.modified.as_deref().is_some_and(|m| m >= CUTOFF_DATE) {
            bump(f, arm, "x_page_modified_after_cutoff");
            continue;
        }
        bump(f, arm, "b_dates_ok");
        let page = &pg.text;
        let path = domain::url_path(&p.url);
        let reg = psl.registered_domain(&p.host).unwrap_or_default();
        if let Some(_r) = screens::url_exclusion(&p.host, &reg, fineweb_excluded).or_else(|| screens::path_exclusion(&p.host, path)) {
            bump(f, arm, "x_url_gate");
            continue;
        }
        let chars = page.chars().count();
        if !(screens::MIN_PAGE_CHARS..=screens::MAX_PAGE_CHARS).contains(&chars) {
            bump(f, arm, "x_length_gate");
            continue;
        }
        let a = rules.assign(&p.host, path, page);
        if screens::gopher_quality(page).is_some() {
            bump(f, arm, "x_gopher_quality");
            continue;
        }
        if screens::gopher_repetition(page).is_some() {
            bump(f, arm, "x_gopher_repetition");
            continue;
        }
        if !screens::machine_text(page, a.domain == "technical").is_empty() {
            bump(f, arm, "x_machine_text");
            continue;
        }
        if screens::nc_licence(page) {
            bump(f, arm, "x_rights_text");
            continue;
        }
        bump(f, arm, "c_page_screens_ok");
        let paras: Vec<text::Para> = candidates::lines(page)
            .map(|l| {
                let rep = candidates::site_line_key(&p.registered_domain, l).is_some_and(|k| templates.contains(&k));
                fineweb::classify(l, rep)
            })
            .collect();
        let runs = candidates::valid_runs(&paras);
        let Some(&(s, e, _)) = runs.iter().min_by_key(|(s, _, _)| rank(PASSAGE_SEED, candidates::PASSAGE_SCOPE, &format!("{}:{s}", p.surt))) else {
            bump(f, arm, "x_no_valid_run");
            // Diagnostic only (ignored storage): line classes of the first pages without a valid run.
            if let Some(dp) = &debug
                && dumped < 40
            {
                dumped += 1;
                let v: Vec<Value> = paras.iter().take(150).map(|x| json!([x.prose, x.reason, x.text.chars().take(120).collect::<String>()])).collect();
                let mut dumpf = fs::OpenOptions::new().create(true).append(true).open(dp)?;
                writeln!(dumpf, "{}", json!({"id": p.id, "arm": arm, "url": p.url, "lines": v}))?;
            }
            continue;
        };
        let ex = paras[s..e].iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join("\n\n");
        let en = English::check(&ex, None);
        if en.reason().is_some() {
            bump(f, arm, "x_not_english");
            continue;
        }
        let fa = rules.assign_with_excerpt(&p.host, path, page, &ex);
        if screens::excerpt_exclusion(&ex).is_some() {
            bump(f, arm, "x_excerpt_screens");
            continue;
        }
        bump(f, arm, "d_excerpt_ok");
        let (red, _, _) = screens::redact(&ex);
        let (group, _) = rules.group(&p.host, &reg, path);
        docs.push(Doc {
            id: p.id.clone(),
            arm: p.arm.clone(),
            range: p.range,
            host: p.host.clone(),
            registered_domain: p.registered_domain.clone(),
            group,
            stratum: p.stratum.clone(),
            url_month: p.url_month.clone(),
            url: p.url.clone(),
            crawl: p.crawl.clone(),
            warc_date: pg.warc_date.clone(),
            record_id: pg.record_id.clone(),
            payload_digest: pg.payload_digest.clone(),
            filename: p.filename.clone(),
            offset: p.offset,
            length: p.length,
            page_published: pg.dates.published.clone(),
            page_published_source: pg.dates.published_source.clone(),
            page_modified: pg.dates.modified.clone(),
            charset: pg.charset.clone(),
            page_sha256: text::sha_hex(page.as_bytes()),
            page_words: text::words(page),
            domain: fa.domain,
            domain_rule: fa.rule,
            page_domain: a.domain,
            line_start: s,
            line_end: e,
            valid_runs: runs.len(),
            words: text::words(&ex),
            text_sha256: text::sha_hex(ex.as_bytes()),
            folded_sha256: text::folded_sha(&ex),
            text_redacted: (red != ex).then_some(red),
            text: ex,
            pair: p.pair.clone(),
            frame_crawls_seen: p.frame_crawls_seen,
            first_frame_ts: p.first_frame_ts.clone(),
            excluded: None,
        });
    }
    let (np, nt) = (pages.len(), templates.len());
    Ok((docs, funnel, np, nt))
}

/// Twin text check: a twin sharing any folded 12-word window with a frame-side (frame or
/// frame_post) excerpt, or repeating another twin, leaves the matched analysis. Earlier
/// exclusions are kept.
pub fn twin_text_check(docs: &mut [Doc]) -> BTreeMap<String, u64> {
    let mut frame_w: HashSet<u64> = HashSet::new();
    for d in docs.iter().filter(|d| d.arm != "twin") {
        frame_w.extend(shingle::windows(&shingle::token_hashes(&d.text), shingle::W12));
    }
    let mut twin_seen: HashSet<String> = HashSet::new();
    let mut ex_counts: BTreeMap<String, u64> = BTreeMap::new();
    for d in docs.iter_mut() {
        if d.arm != "twin" || d.excluded.is_some() {
            continue;
        }
        if shingle::windows(&shingle::token_hashes(&d.text), shingle::W12).iter().any(|w| frame_w.contains(w)) {
            d.excluded = Some("twin_shares_12_word_window_with_frame_side_excerpt".into());
        } else if !twin_seen.insert(d.folded_sha256.clone()) {
            d.excluded = Some("twin_duplicates_another_twin".into());
        }
        if let Some(r) = &d.excluded {
            *ex_counts.entry(r.clone()).or_default() += 1;
        }
    }
    ex_counts
}

/// A request shards for every document (excluded or not), in keyed order, round robin.
pub fn write_requests(dir: &Path, docs: &[Doc], shards: usize) -> Result<BTreeMap<String, Value>> {
    ensure!(shards > 0, "shards must be positive");
    let mut order: Vec<usize> = (0..docs.len()).collect();
    order.sort_by(|&x, &y| rank(SEED, "cc_twins_shard_v1", &docs[x].id).cmp(&rank(SEED, "cc_twins_shard_v1", &docs[y].id)));
    let mut sh: Vec<Vec<Value>> = vec![Vec::new(); shards];
    for (n, &i) in order.iter().enumerate() {
        sh[n % shards].push(json!({"id": docs[i].id, "text": docs[i].scored_text()}));
    }
    let mut shard_sha = BTreeMap::new();
    for (s, v) in sh.iter().enumerate() {
        let p = dir.join(format!("shard-{s:02}.jsonl"));
        shard_sha.insert(format!("shard-{s:02}.jsonl"), json!({"rows": v.len(), "sha256": measure::write_jsonl(&p, v)?}));
    }
    Ok(shard_sha)
}

pub const EXTRACT_LANG_TEXT: &str = "english_screen + whatlang reliable; the CLD2 rule is not applied (WARC response records carry no WET language header, and the CDX field is missing before 2018-39, so both arms skip it)";
pub const EXTRACT_DATE_TEXT: &str = "page metadata publication date (when present) within 45 days of the URL month's 15th and before 2022-11-30; twins and frame_post also need any metadata modification date before 2022-11-30";

pub fn extract(root: &Path, psl: &Psl, fineweb_excluded: &HashSet<String>, shards: usize) -> Result<Value> {
    let picks: Vec<Pick> = store::rows(&root.join("build").join("picks.jsonl.zst"))?;
    let rules = twin_rules();
    let cx = ExtractCtx { psl, fineweb_excluded, rules: &rules };
    let (mut docs, f, pages, templates) = extract_dir(&root.join("records"), &picks, &cx)?;
    let ex_counts = twin_text_check(&mut docs);
    let out = root.join("extract");
    let mut w = store::ZWriter::create(&out.join("docs.jsonl.zst"))?;
    for d in &docs {
        w.write(d)?;
    }
    let (sha, rows) = w.finish()?;
    let shard_sha = write_requests(&out.join("requests"), &docs, shards)?;
    let mut arms: BTreeMap<String, u64> = BTreeMap::new();
    for d in docs.iter().filter(|d| d.excluded.is_none()) {
        *arms.entry(d.arm.clone()).or_default() += 1;
    }
    let summary = json!({"schema": "slop_ninja_cc_twins_extract_v1", "pages": pages, "templates": templates,
        "funnel_by_arm": f, "docs": rows, "docs_sha256": sha, "twin_text_check": ex_counts, "kept_by_arm": arms,
        "requests": shard_sha, "extract_method": EXTRACT_METHOD, "excerpt_method": candidates::EXCERPT_METHOD,
        "passage_seed": PASSAGE_SEED, "lang": EXTRACT_LANG_TEXT, "date_rule": EXTRACT_DATE_TEXT});
    store::write_json(&out.join("EXTRACT.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surt_parts_and_dates() {
        assert_eq!(split_surt("com,example,www)/2015/03/12/a"), ("com,example,www", "/2015/03/12/a"));
        assert_eq!(surt_host("com,example,www:8080"), "www.example.com");
        assert_eq!(url_date("/news/2015/03/12/slug"), Some(("2015-03".into(), Some(12))));
        assert_eq!(url_date("/2015/03/slug.html?x=2020/01/"), Some(("2015-03".into(), None)));
        assert_eq!(url_date("/p/1234/56/x"), None);
        assert!(eligible_month("2022-10") && !eligible_month("2022-11"));
        assert_eq!(base_key("com,a)/2015/03/x/amp/"), "com,a)/2015/03/x");
        assert_eq!(base_key("com,a)/2015/03/x?utm=1"), "com,a)/2015/03/x");
        assert_eq!(base_key("com,a)/2015/03/x/print"), "com,a)/2015/03/x");
    }

    #[test]
    fn copy_risk_hosts() {
        assert_eq!(copy_risk_host("www.jmar-bi.comwww.example.net", "example.net"), Some("mirror_host_with_embedded_host_name"));
        assert_eq!(copy_risk_host("news.aol.com", "aol.com"), Some("syndication_portal"));
        assert_eq!(copy_risk_host("archive.example.org", "example.org"), Some("archive_subdomain"));
        assert_eq!(copy_risk_host("www.example.com", "example.com"), None);
        assert_eq!(copy_risk_host("en.example.ch", "example.ch"), None);
    }

    #[test]
    fn cluster_blocks_cover_the_range() {
        let c = Cluster {
            keys: vec!["a)/ 1".into(), "c)/x 1".into(), "c)/y 1".into(), "e)/ 1".into()],
            file: vec![0; 4],
            offset: vec![0; 4],
            length: vec![1; 4],
        };
        assert_eq!(c.blocks("b)/", "b)/z"), 0..=0);
        assert_eq!(c.blocks("c)/x", "c)/xz"), 0..=1);
        assert_eq!(c.blocks("c)/x0", "d)/"), 1..=2);
        assert_eq!(c.blocks("f)/", "g)/"), 3..=3);
    }

    #[test]
    fn cdx_lines_parse() {
        let l = r#"com,example)/2015/03/a 20230201010101 {"url": "https://example.com/2015/03/a", "mime": "text/html", "mime-detected": "text/html", "status": "200", "digest": "ABC", "length": "1234", "offset": "567", "filename": "crawl-data/x.warc.gz", "languages": "eng"}"#;
        let c = parse_cdx(l, 3, "CC-MAIN-2023-06").unwrap();
        assert_eq!((c.range, c.offset, c.length, c.status.as_str()), (3, 567, 1234, "200"));
        assert!(is_html(&c));
        assert_eq!(c.languages.as_deref(), Some("eng"));
    }

    #[test]
    fn html_lines_breaks_blocks_and_drops_scripts() {
        let h = "<html><head><title>T &amp; U</title><script>var x='<p>';</script></head><body><p>One <b>two</b>.</p><div>Three<br>Four</div><pre>a\nb</pre><!-- c --></body></html>";
        assert_eq!(html_lines(h), "T & U\nOne two.\nThree\nFour\na\nb\n");
    }

    #[test]
    fn page_dates_read_metadata() {
        let h = r#"<meta property="article:published_time" content="2015-03-12T10:00:00Z"><meta name="x" content="y"><script>{"dateModified": "2016-01-02"}</script>"#;
        let d = page_dates(h);
        assert_eq!(d.published.as_deref(), Some("2015-03-12"));
        assert_eq!(d.modified.as_deref(), Some("2016-01-02"));
        let d = page_dates(r#"<time class="x" datetime="2014-07-01">July</time>"#);
        assert_eq!((d.published.as_deref(), d.published_source.as_deref()), (Some("2014-07-01"), Some("time:datetime")));
        assert_eq!(days("2015-03-12", "2015-03-15"), Some(-3));
        assert_eq!(days("2016-03-01", "2016-02-28"), Some(2));
    }

    #[test]
    fn page_dates_survive_multibyte_cuts() {
        let mut h = String::from("<script>{\"datePublished\": \"2015-03-12");
        h.push_str(&"\u{2019}".repeat(40));
        assert_eq!(page_dates(&h).published.as_deref(), Some("2015-03-12"));
        assert_eq!(cut("a\u{2019}b", 0, 2), "a");
    }

    #[test]
    fn cp1252_decodes_quotes() {
        let (s, cs) = decode_body(b"<meta charset=\"windows-1252\">\x93hi\x94", "text/html");
        assert_eq!(cs, "cp1252");
        assert!(s.ends_with("\u{201c}hi\u{201d}"));
    }
}
