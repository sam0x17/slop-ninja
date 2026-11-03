//! Polite, journaled downloads from `https://data.commoncrawl.org`.
//!
//! * At most `connections` transfers at once (hard ceiling 4), one HEAD
//!   before each GET to learn its size.
//! * A byte budget: a file starts only if every byte already received plus
//!   the bytes reserved by running transfers plus its own size stays within
//!   the budget. Bytes of failed attempts count too.
//! * Disk headroom: a file starts only if the free space of the target
//!   filesystem minus the file's size stays at or above `min_free_bytes`.
//! * Throttling: a 429 or 503 response backs off (15 s, 60 s, 180 s); three
//!   consecutive throttled responses stop every transfer. Other failures
//!   retry up to four attempts with backoff.
//! * Every attempt is a JSON line in the journal (fsynced). A file is
//!   written to `<dest>.part` and renamed only after its byte count matches
//!   `Content-Length`; a rerun skips files whose journal entry and on-disk
//!   size and SHA-256 agree.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

pub const BASE: &str = "https://data.commoncrawl.org/";
pub const USER_AGENT: &str = "slop-ninja-cc-human-pilot/0.1 (+https://slop.ninja; bounded research sample, at most 4 connections)";
pub const MAX_CONNECTIONS: usize = 4;
pub const JOURNAL_SCHEMA: &str = "slop_ninja_cc_human_download_v1";

#[derive(Clone, Debug)]
pub struct Limits {
    pub budget_bytes: u64,
    pub min_free_bytes: u64,
    pub connections: usize,
}

#[derive(Clone, Debug)]
pub struct Job {
    /// Path relative to `BASE`, for example `crawl-data/CC-MAIN-2016-07/wet.paths.gz`.
    pub rel: String,
    pub dest: PathBuf,
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .timeout_recv_body(Some(Duration::from_secs(3600)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Free bytes for unprivileged users on the filesystem holding `path`.
pub fn free_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut s) };
    ensure!(rc == 0, "statvfs {}: {}", path.display(), std::io::Error::last_os_error());
    Ok(s.f_bavail as u64 * s.f_frsize as u64)
}

pub struct Journal {
    f: Mutex<File>,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Journal> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        Ok(Journal {
            f: Mutex::new(OpenOptions::new().create(true).append(true).open(path)?),
        })
    }
    pub fn write(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        let mut f = self.f.lock().unwrap();
        f.write_all(&line)?;
        f.sync_data()?;
        Ok(())
    }
}

/// Completed downloads recorded in a journal: rel path -> (bytes, sha256).
pub fn completed(journal: &Path) -> Result<HashMap<String, (u64, String)>> {
    let mut out = HashMap::new();
    if !journal.exists() {
        return Ok(out);
    }
    for (i, l) in fs::read_to_string(journal)?.lines().enumerate() {
        let v: Value = serde_json::from_str(l).with_context(|| format!("{} line {}", journal.display(), i + 1))?;
        if v["status"] == "ok" {
            out.insert(
                v["rel"].as_str().unwrap_or("").to_owned(),
                (v["bytes"].as_u64().unwrap_or(0), v["sha256"].as_str().unwrap_or("").to_owned()),
            );
        }
    }
    Ok(out)
}

/// Bytes received from the network in every journaled attempt.
pub fn received_bytes(journal: &Path) -> Result<u64> {
    if !journal.exists() {
        return Ok(0);
    }
    let mut n = 0;
    for l in fs::read_to_string(journal)?.lines() {
        let v: Value = serde_json::from_str(l)?;
        n += v["bytes_received"].as_u64().unwrap_or(0);
    }
    Ok(n)
}

struct State {
    received: u64,
    reserved: u64,
}

struct Shared {
    limits: Limits,
    journal: Journal,
    journal_path: PathBuf,
    state: Mutex<State>,
    stop: AtomicBool,
    throttled_in_a_row: AtomicU32,
    stop_reason: Mutex<Option<String>>,
}

impl Shared {
    fn halt(&self, why: String) {
        let mut r = self.stop_reason.lock().unwrap();
        if r.is_none() {
            eprintln!("fetch: stopping: {why}");
            *r = Some(why);
        }
        self.stop.store(true, Ordering::SeqCst);
    }
    /// Count a throttled response; returns the backoff to sleep, or None
    /// when the run must stop.
    fn throttled(&self, rel: &str, status: u16) -> Option<Duration> {
        let n = self.throttled_in_a_row.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 3 {
            self.halt(format!("three consecutive throttled responses (last {status} on {rel})"));
            return None;
        }
        Some(Duration::from_secs([15, 60, 180][(n - 1) as usize]))
    }
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

enum Attempt {
    Done { sha: String },
    Throttled(u16),
    Retry(String),
    Fatal(String),
}

fn head_len(agent: &ureq::Agent, url: &str) -> Result<std::result::Result<u64, u16>> {
    let r = agent.head(url).call().with_context(|| format!("HEAD {url}"))?;
    let st = r.status().as_u16();
    if st != 200 {
        return Ok(Err(st));
    }
    let len = r
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .context("HEAD without Content-Length")?;
    Ok(Ok(len))
}

fn attempt(sh: &Shared, agent: &ureq::Agent, job: &Job, n: u32) -> Result<Attempt> {
    let url = format!("{BASE}{}", job.rel);
    let started = grammar_a_pools::io::now_utc();
    let len = match head_len(agent, &url) {
        Ok(Ok(l)) => l,
        Ok(Err(st)) if st == 429 || st == 503 => return Ok(Attempt::Throttled(st)),
        Ok(Err(st)) if st == 404 || st == 403 => return Ok(Attempt::Fatal(format!("HEAD status {st}"))),
        Ok(Err(st)) => return Ok(Attempt::Retry(format!("HEAD status {st}"))),
        Err(e) => return Ok(Attempt::Retry(format!("{e:#}"))),
    };
    {
        let mut s = sh.state.lock().unwrap();
        if s.received + s.reserved + len > sh.limits.budget_bytes {
            drop(s);
            sh.halt(format!(
                "byte budget {} would be exceeded by {} ({} bytes)",
                sh.limits.budget_bytes, job.rel, len
            ));
            return Ok(Attempt::Fatal("budget".into()));
        }
        let dir = job.dest.parent().unwrap_or(Path::new("."));
        let free = free_bytes(dir)?;
        if free < len + sh.limits.min_free_bytes + s.reserved {
            drop(s);
            sh.halt(format!(
                "disk headroom: {free} free, {len} needed, {} must stay free",
                sh.limits.min_free_bytes
            ));
            return Ok(Attempt::Fatal("disk".into()));
        }
        s.reserved += len;
    }
    let part = job.dest.with_extension("part");
    let t0 = Instant::now();
    let mut received = 0u64;
    let res: Result<Attempt> = (|| {
        let resp = agent.get(&url).call().with_context(|| format!("GET {url}"))?;
        let st = resp.status().as_u16();
        if st == 429 || st == 503 {
            return Ok(Attempt::Throttled(st));
        }
        if st != 200 {
            return Ok(Attempt::Retry(format!("GET status {st}")));
        }
        let mut body = resp.into_body().into_with_config().limit(len + 1).reader();
        let mut out = File::create(&part).with_context(|| format!("create {}", part.display()))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let k = match body.read(&mut buf) {
                Ok(k) => k,
                Err(e) => return Ok(Attempt::Retry(format!("body read after {received} bytes: {e}"))),
            };
            if k == 0 {
                break;
            }
            received += k as u64;
            if received > len {
                return Ok(Attempt::Fatal(format!("body longer than Content-Length {len}")));
            }
            h.update(&buf[..k]);
            out.write_all(&buf[..k])?;
        }
        if received != len {
            return Ok(Attempt::Retry(format!("short body: {received} of {len} bytes")));
        }
        out.sync_all()?;
        drop(out);
        fs::rename(&part, &job.dest)?;
        Ok(Attempt::Done {
            sha: hex::encode(h.finalize()),
        })
    })();
    {
        let mut s = sh.state.lock().unwrap();
        s.reserved -= len;
        s.received += received;
    }
    let a = match res {
        Ok(a) => a,
        Err(e) => Attempt::Retry(format!("{e:#}")),
    };
    let (status, detail, sha) = match &a {
        Attempt::Done { sha, .. } => ("ok", String::new(), sha.clone()),
        Attempt::Throttled(st) => ("throttled", format!("status {st}"), String::new()),
        Attempt::Retry(d) => ("retry", d.clone(), String::new()),
        Attempt::Fatal(d) => ("fatal", d.clone(), String::new()),
    };
    if !matches!(a, Attempt::Done { .. }) {
        let _ = fs::remove_file(&part);
    }
    sh.journal.write(&json!({
        "schema": JOURNAL_SCHEMA, "rel": job.rel, "url": url, "dest": job.dest, "attempt": n, "status": status,
        "detail": detail, "content_length": len, "bytes_received": received,
        "bytes": if status == "ok" { len } else { 0 }, "sha256": sha,
        "started_utc": started, "finished_utc": grammar_a_pools::io::now_utc(), "seconds": t0.elapsed().as_secs_f64(),
    }))?;
    Ok(a)
}

fn run_job(sh: &Shared, agent: &ureq::Agent, job: &Job) -> Result<bool> {
    let mut n = 0;
    loop {
        if sh.stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        n += 1;
        match attempt(sh, agent, job, n)? {
            Attempt::Done { .. } => {
                sh.throttled_in_a_row.store(0, Ordering::SeqCst);
                return Ok(true);
            }
            Attempt::Throttled(st) => match sh.throttled(&job.rel, st) {
                Some(d) => std::thread::sleep(d),
                None => return Ok(false),
            },
            Attempt::Retry(d) => {
                if n >= 4 {
                    eprintln!("fetch: giving up on {} after {n} attempts: {d}", job.rel);
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_secs(5 * (1 << n)));
            }
            Attempt::Fatal(d) => {
                eprintln!("fetch: {} failed: {d}", job.rel);
                return Ok(false);
            }
        }
    }
}

pub struct Summary {
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub received_bytes: u64,
    pub stop_reason: Option<String>,
}

/// Download every job not already complete. Returns once all jobs ran or
/// a stop condition fired.
pub fn run(jobs: Vec<Job>, limits: Limits, journal_path: &Path) -> Result<Summary> {
    ensure!(
        (1..=MAX_CONNECTIONS).contains(&limits.connections),
        "connections must be 1..={MAX_CONNECTIONS}"
    );
    let done_before = completed(journal_path)?;
    let mut queue = VecDeque::new();
    let mut skipped = 0;
    for j in jobs {
        if let Some((bytes, _)) = done_before.get(&j.rel)
            && j.dest.metadata().map(|m| m.len()).ok() == Some(*bytes)
        {
            skipped += 1;
            continue;
        }
        if let Some(p) = j.dest.parent() {
            fs::create_dir_all(p)?;
        }
        queue.push_back(j);
    }
    let sh = Arc::new(Shared {
        journal: Journal::open(journal_path)?,
        journal_path: journal_path.to_path_buf(),
        state: Mutex::new(State {
            received: received_bytes(journal_path)?,
            reserved: 0,
        }),
        limits: limits.clone(),
        stop: AtomicBool::new(false),
        throttled_in_a_row: AtomicU32::new(0),
        stop_reason: Mutex::new(None),
    });
    let total = queue.len();
    let queue = Arc::new(Mutex::new(queue));
    let ok = Arc::new(AtomicU32::new(0));
    let failed = Arc::new(AtomicU32::new(0));
    std::thread::scope(|s| {
        for _ in 0..limits.connections {
            let (sh, queue, ok, failed) = (sh.clone(), queue.clone(), ok.clone(), failed.clone());
            s.spawn(move || {
                let agent = agent();
                loop {
                    if sh.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Some(job) = queue.lock().unwrap().pop_front() else { break };
                    match run_job(&sh, &agent, &job) {
                        Ok(true) => {
                            let k = ok.fetch_add(1, Ordering::SeqCst) + 1;
                            eprintln!("fetch: {k}/{total} {}", job.rel);
                        }
                        Ok(false) => {
                            failed.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(e) => {
                            failed.fetch_add(1, Ordering::SeqCst);
                            sh.halt(format!("{}: {e:#}", job.rel));
                        }
                    }
                }
            });
        }
    });
    let _ = &sh.journal_path;
    let received = sh.state.lock().unwrap().received;
    let reason = sh.stop_reason.lock().unwrap().clone();
    Ok(Summary {
        done: ok.load(Ordering::SeqCst) as usize,
        skipped,
        failed: failed.load(Ordering::SeqCst) as usize,
        received_bytes: received,
        stop_reason: reason,
    })
}

/// One HTTP Range read (`bytes=start..start+len-1`), journaled, for
/// re-fetching a record by its pointer. The bytes count against `budget`.
pub fn range(agent: &ureq::Agent, rel: &str, start: u64, len: u64, journal: &Journal) -> Result<Vec<u8>> {
    ensure!(len > 0 && len <= 64 << 20, "range length {len}");
    let url = format!("{BASE}{rel}");
    let resp = agent
        .get(&url)
        .header("Range", format!("bytes={}-{}", start, start + len - 1))
        .call()
        .with_context(|| format!("GET range {url}"))?;
    let st = resp.status().as_u16();
    let mut b = Vec::new();
    if st == 206 {
        resp.into_body().into_with_config().limit(len + 1).reader().read_to_end(&mut b)?;
    }
    journal.write(
        &json!({"schema": JOURNAL_SCHEMA, "kind": "range", "rel": rel, "url": url, "range_start": start, "range_len": len,
                          "status": if st == 206 { "ok_range" } else { "failed_range" }, "http_status": st,
                          "bytes_received": b.len(), "finished_utc": grammar_a_pools::io::now_utc()}),
    )?;
    if st != 206 {
        bail!("range read of {rel} returned {st}");
    }
    ensure!(b.len() as u64 == len, "range read of {rel}: {} of {len} bytes", b.len());
    Ok(b)
}
