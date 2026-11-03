//! Pangram true-bulk collector for the competitive A protocol v1.2 Tier C
//! subsamples, adapted from the stage-3 bulk collector
//! (`a_supervised_pangram/bulk.rs`).
//!
//! ```text
//! a_comp_pangram_bulk prepare SPLIT SUBSAMPLE_JSONL ROWS_JSONL COLLECTIONS_JSON STORE [--tail-units N]
//! a_comp_pangram_bulk ledger-init LEDGER BUDGET_UNITS
//! a_comp_pangram_bulk run STORE LEDGER KEYFILE [WATCH_SECONDS]
//! a_comp_pangram_bulk collect STORE JOB KEYFILE
//! a_comp_pangram_bulk summary STORE
//! ```
//!
//! - SPLIT is `cal` or `test` (Tier C), or `cc_pilot` (the Common Crawl
//!   human-slice pilot's contamination diagnostic, comparator evidence
//!   only); nothing else is accepted.
//! - SUBSAMPLE_JSONL lists the pre-registered subsample (`id`, `text_sha256`);
//!   ROWS_JSONL holds the exact texts. Each selected row must carry a public
//!   evaluation `collection` listed in COLLECTIONS_JSON (`{"collections": [...]}`).
//! - Billed units are one started 100-word block per item, at least one
//!   (`max(1, ceil(words / 100))`, Unicode whitespace words). Jobs hold at most
//!   1,000 such units. Items under the provider's 50-word minimum are not sent
//!   and are recorded as missing. Items are dispatched in a label-blind hash
//!   order; with `--tail-units N` the last N units go into jobs of at most 25
//!   units, so that a budget overshoot leaves the fewest documents unsent.
//! - The LEDGER holds the spendable units (computed privately after the
//!   margin). A job is sent only if the units already committed plus its own
//!   stay within them; otherwise it is never sent and its items are missing.
//!   The commitment is written durably before the POST.
//! - Every HTTP exchange is journalled. A submission intent without a receipt
//!   stops the run for manual reconciliation; a POST is never repeated
//!   automatically. HTTP 402 or 429 latches a stop file.
//! - Results are fetched as soon as a job is terminal, because the provider
//!   keeps them for a limited time. Echoed text must match exactly or differ
//!   only in whitespace. Versions are recorded per observation; the evaluator
//!   applies the protocol's version pin.
//! - No observation from an earlier campaign is read or reused. All output must
//!   live under the checkout's ignored `data/` directory.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const BASE: &str = "https://text.external-api.pangram.com";
const MODEL: &str = "pangram-4";
const JOB_UNITS: u64 = 1000;
const MIN_WORDS: usize = 50;
const BODY_BYTES: u64 = 256 * 1024 * 1024;
const PAGE: usize = 100;
const TAIL_JOB_UNITS: u64 = 25;
const ORDER_KEY_DOMAIN: &str = "slopninja-a-comp-pangram-dispatch-order-v1";

fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}
fn words(text: &str) -> usize {
    text.split_whitespace().count()
}
fn billed_units(text: &str) -> u64 {
    (words(text) as u64).div_ceil(100).max(1)
}
fn stamp() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos())
}
fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing/non-string/empty {k}"))
}
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 240
        && !matches!(id, "." | "..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

fn ignored(path: &Path) -> Result<()> {
    ensure!(
        Command::new("git")
            .arg("-C")
            .arg(path.parent().context("parent")?)
            .args(["check-ignore", "--quiet", "--"])
            .arg(path)
            .status()?
            .success(),
        "receipts and raw responses must be Git-ignored: {}",
        path.display()
    );
    Ok(())
}
fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    File::open(path)
        .with_context(|| path.display().to_string())?
        .take(BODY_BYTES + 1)
        .read_to_end(&mut b)?;
    ensure!(b.len() as u64 <= BODY_BYTES, "file exceeds byte limit");
    Ok(b)
}
fn read_json(path: &Path) -> Result<Value> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "receipt must be a regular file: {}",
        path.display()
    );
    serde_json::from_slice(&read_bytes(path)?).with_context(|| path.display().to_string())
}
fn hash_file(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(read_bytes(path)?)))
}
/// Atomic no-clobber publication plus directory fsync.
fn put(path: &Path, value: &Value) -> Result<()> {
    ignored(path)?;
    let parent = path.parent().context("parent")?;
    let mut t = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut t, value)?;
    t.write_all(b"\n")?;
    t.as_file().sync_all()?;
    t.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
/// Atomic replacement (ledger only).
fn replace(path: &Path, value: &Value) -> Result<()> {
    ignored(path)?;
    let parent = path.parent().context("parent")?;
    let mut t = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut t, value)?;
    t.write_all(b"\n")?;
    t.as_file().sync_all()?;
    t.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn output_root(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)?;
    let path = path.canonicalize()?;
    let git = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    ensure!(git.status.success(), "STORE must belong to a Git checkout");
    let repo = PathBuf::from(String::from_utf8(git.stdout)?.trim()).canonicalize()?;
    ensure!(
        path.starts_with(repo.join("data")),
        "STORE must be inside the checkout's ignored data/ directory"
    );
    ignored(&path.join("probe.json"))?;
    Ok(path)
}
fn credential(path: &Path) -> Result<String> {
    if let Ok(key) = std::env::var("PANGRAM_API_KEY") {
        ensure!(!key.is_empty(), "empty key");
        return Ok(key);
    }
    let text = String::from_utf8(read_bytes(path)?)?;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        if let Some(v) = line.strip_prefix("PANGRAM_API_KEY=") {
            let v = v.trim().trim_end_matches(';');
            let v = if v.len() >= 2
                && ((v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\'')))
            {
                &v[1..v.len() - 1]
            } else {
                v
            };
            ensure!(
                !v.is_empty()
                    && !v.contains(['$', '`', '\\', '\n', ' ', '\t', '\r', '"', '\'']),
                "key must be a literal assignment; no shell code is executed"
            );
            found = Some(v.to_owned());
        }
    }
    found.context("PANGRAM_API_KEY literal assignment absent")
}

fn idempotency_key(request: &Value) -> Result<String> {
    Ok(format!(
        "slop-ninja-a-comp-bulk-v1-{}",
        digest(&serde_json::to_string(request)?)
    ))
}

/// Journalled HTTP client: no automatic retries, no redirects, no persisted
/// authorization header; HTTP 402 or 429 latches `STORE/quota-stop.json`.
struct Client {
    http: reqwest::blocking::Client,
    key: String,
    store: PathBuf,
    stopped: AtomicBool,
}
impl Client {
    fn new(key: String, store: &Path, gets_only_after_stop: bool) -> Result<Self> {
        ensure!(!key.is_empty(), "empty key");
        let latched = store.join("quota-stop.json").exists();
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(120))
                .build()?,
            key,
            store: store.to_owned(),
            stopped: AtomicBool::new(latched && !gets_only_after_stop),
        })
    }
    fn request(&self, route: &str, payload: Option<&Value>, folder: &Path) -> Result<Value> {
        ensure!(
            !self.stopped.load(Ordering::SeqCst),
            "HTTP 402/429 stop is latched; no further API work"
        );
        let journal = folder.join(format!("http-{}-{}", std::process::id(), stamp()?));
        fs::create_dir(&journal)?;
        File::open(folder)?.sync_all()?;
        let url = format!("{BASE}{route}");
        let idem = match (route, payload) {
            ("/bulk", Some(p)) => Some(idempotency_key(p)?),
            _ => None,
        };
        put(
            &journal.join("request.json"),
            &json!({"method": if payload.is_some() {"POST"} else {"GET"}, "url": url,
                "payload_sha256": payload.map(|p| digest(&p.to_string())),
                "created_utc": chrono::Utc::now().to_rfc3339(), "automatic_retries": false,
                "authorization_header_retained": false, "idempotency_key": idem}),
        )?;
        let req = match payload {
            Some(p) => self.http.post(&url).json(p),
            None => self.http.get(&url),
        };
        let req = match &idem {
            Some(k) => req.header("Idempotency-Key", k),
            None => req,
        };
        let mut resp = match req.header("x-api-key", &self.key).send() {
            Ok(r) => r,
            Err(e) => {
                put(
                    &journal.join("transport-error.json"),
                    &json!({"kind": "transport", "uncertain_submission": payload.is_some(),
                        "timeout": e.is_timeout(), "connect": e.is_connect(), "automatic_resubmit": false}),
                )?;
                bail!("Pangram transport failure; journal retained; no retry");
            }
        };
        let status = resp.status().as_u16();
        let headers: Vec<Value> = resp
            .headers()
            .iter()
            .map(|(k, v)| json!({"name": k.as_str(), "value_hex": hex::encode(v.as_bytes())}))
            .collect();
        put(
            &journal.join("response.json"),
            &json!({"status": status, "headers": headers, "received_utc": chrono::Utc::now().to_rfc3339()}),
        )?;
        if status == 402 || status == 429 {
            self.stopped.store(true, Ordering::SeqCst);
            let stop = self.store.join("quota-stop.json");
            if !stop.exists() {
                put(
                    &stop,
                    &json!({"status": status, "journal": journal,
                        "action": "stop new API work; keep in-flight obligations; no automatic resubmit"}),
                )?;
            }
        }
        let body_path = journal.join("body.bin");
        ignored(&body_path)?;
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&body_path)?;
        let copied = std::io::copy(&mut resp.by_ref().take(BODY_BYTES + 1), &mut f);
        f.sync_all()?;
        File::open(&journal)?.sync_all()?;
        let body = fs::read(&body_path)?;
        let complete = copied.is_ok() && body.len() as u64 <= BODY_BYTES;
        put(
            &journal.join("outcome.json"),
            &json!({"status": status, "bytes": body.len(), "body_sha256": hex::encode(Sha256::digest(&body)),
                "complete": complete, "finished_utc": chrono::Utc::now().to_rfc3339()}),
        )?;
        ensure!(complete, "Pangram response incomplete; partial body retained");
        ensure!(
            (200..300).contains(&status),
            "Pangram HTTP {status}; raw response retained; no retry"
        );
        let v: Value = serde_json::from_slice(&body).context("invalid JSON; raw body retained")?;
        ensure!(v.is_object(), "non-object response; raw body retained");
        Ok(v)
    }
}

fn contains_selector(v: &Value, s: &str) -> bool {
    match v {
        Value::String(x) => x == s,
        Value::Array(a) => a.iter().any(|v| contains_selector(v, s)),
        Value::Object(o) => o.contains_key(s) || o.values().any(|v| contains_selector(v, s)),
        _ => false,
    }
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = String::from_utf8(read_bytes(path)?)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l).with_context(|| format!("{} line {}", path.display(), i + 1))
        })
        .collect()
}

/// Job packing over items in their given order. Items before the last
/// `tail_units` units go into jobs of at most `JOB_UNITS`; the tail goes into
/// jobs of at most `TAIL_JOB_UNITS`, so that a budget overshoot leaves the
/// fewest documents unsent.
fn pack(items: &[(String, u64)], tail_units: u64) -> Result<Vec<Vec<usize>>> {
    let total: u64 = items.iter().map(|x| x.1).sum();
    let tail_start = total.saturating_sub(tail_units);
    let mut jobs: Vec<Vec<usize>> = Vec::new();
    let mut cur = Vec::new();
    let (mut units, mut cum, mut in_tail) = (0u64, 0u64, false);
    for (i, (_, u)) in items.iter().enumerate() {
        ensure!(*u <= JOB_UNITS, "single item above the job cap");
        let tail = tail_units > 0 && cum >= tail_start;
        if tail && !in_tail && !cur.is_empty() {
            jobs.push(std::mem::take(&mut cur));
            units = 0;
        }
        in_tail = tail;
        let cap = if tail { TAIL_JOB_UNITS.max(*u) } else { JOB_UNITS };
        if units + u > cap {
            jobs.push(std::mem::take(&mut cur));
            units = 0;
        }
        cur.push(i);
        units += u;
        cum += u;
    }
    if !cur.is_empty() {
        jobs.push(cur);
    }
    Ok(jobs)
}

/// Label-blind dispatch order: `SHA-256(domain + ":" + split + ":" + id)`.
fn order_key(split: &str, id: &str) -> String {
    digest(&format!("{ORDER_KEY_DOMAIN}:{split}:{id}"))
}

fn prepare(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 5 || (a.len() == 7 && a[5] == "--tail-units"),
        "prepare SPLIT SUBSAMPLE_JSONL ROWS_JSONL COLLECTIONS_JSON STORE [--tail-units N]"
    );
    let tail_units: u64 = if a.len() == 7 { a[6].parse()? } else { 0 };
    let split = a[0].as_str();
    ensure!(matches!(split, "cal" | "test" | "cc_pilot"), "split must be cal, test or cc_pilot");
    let (sub_p, rows_p, coll_p) = (Path::new(&a[1]), Path::new(&a[2]), Path::new(&a[3]));
    ensure!(!Path::new(&a[4]).exists(), "STORE exists; prepare once");
    let allowed: BTreeSet<String> = read_json(coll_p)?["collections"]
        .as_array()
        .context("collections")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).context("collection"))
        .collect::<Result<_>>()?;
    let sub = read_jsonl(sub_p)?;
    let mut want: BTreeMap<String, String> = BTreeMap::new();
    for s in &sub {
        let id = field(s, "id")?.to_owned();
        ensure!(safe_id(&id), "unsafe id {id}");
        ensure!(
            want.insert(id.clone(), field(s, "text_sha256")?.to_owned())
                .is_none(),
            "duplicate subsample id {id}"
        );
    }
    let mut rows: BTreeMap<String, Value> = BTreeMap::new();
    for r in read_jsonl(rows_p)? {
        let id = field(&r, "id")?.to_owned();
        if want.contains_key(&id) {
            ensure!(field(&r, "split")? == split, "row {id} is not in split {split}");
            ensure!(rows.insert(id.clone(), r).is_none(), "duplicate row {id}");
        }
    }
    ensure!(
        rows.len() == want.len(),
        "{} subsample ids missing from ROWS",
        want.len() - rows.len()
    );
    let store = output_root(Path::new(&a[4]))?;
    fs::create_dir(store.join("jobs"))?;
    fs::create_dir(store.join("obs"))?;
    let mut items = Vec::new();
    let mut texts = Vec::new();
    let mut not_sent = Vec::new();
    let mut seen_text = BTreeSet::new();
    let mut ordered: Vec<(&String, &String)> = want.iter().collect();
    ordered.sort_by_key(|(id, _)| order_key(split, id));
    for (id, sha) in ordered {
        let r = &rows[id];
        let text = field(r, "text")?;
        ensure!(digest(text) == *sha && field(r, "text_sha256")? == sha, "{id}: text hash");
        let coll = field(r, "collection")?;
        ensure!(allowed.contains(coll), "{id}: collection {coll} not allowed");
        if words(text) < MIN_WORDS {
            not_sent.push(json!({"id": id, "reason": "below_provider_minimum_words", "words": words(text)}));
            continue;
        }
        if !seen_text.insert(sha.to_owned()) {
            not_sent.push(json!({"id": id, "reason": "duplicate_text_in_subsample"}));
            continue;
        }
        items.push((id.clone(), billed_units(text)));
        texts.push(text.to_owned());
    }
    let jobs = pack(&items, tail_units)?;
    let mut job_list = Vec::new();
    for (j, members) in jobs.iter().enumerate() {
        let dir = store.join("jobs").join(format!("job-{j:03}"));
        fs::create_dir(&dir)?;
        let request = json!({"model": MODEL, "items": members.iter()
            .map(|&i| json!({"id": items[i].0, "text": texts[i]})).collect::<Vec<_>>()});
        put(&dir.join("request.json"), &request)?;
        let units: u64 = members.iter().map(|&i| items[i].1).sum();
        let plan = json!({"schema": "slop_ninja_a_comp_pangram_bulk_job_v1", "split": split,
            "job": format!("job-{j:03}"), "model": MODEL, "items": members.iter()
                .map(|&i| json!({"id": items[i].0, "text_sha256": digest(&texts[i]), "billed_units": items[i].1}))
                .collect::<Vec<_>>(),
            "billed_units": units, "request_sha256": hash_file(&dir.join("request.json"))?,
            "unit_policy": "max(1, ceil(whitespace words / 100)) per item"});
        put(&dir.join("plan.json"), &plan)?;
        File::options()
            .create_new(true)
            .write(true)
            .open(dir.join("lock"))?
            .sync_all()?;
        job_list.push(json!({"job": format!("job-{j:03}"), "items": members.len(), "billed_units": units}));
    }
    let total: u64 = items.iter().map(|x| x.1).sum();
    put(
        &store.join("PLAN.json"),
        &json!({"schema": "slop_ninja_a_comp_pangram_bulk_plan_v1", "split": split,
            "subsample": sub_p.canonicalize()?, "subsample_sha256": hash_file(sub_p)?,
            "rows": rows_p.canonicalize()?, "rows_sha256": hash_file(rows_p)?,
            "collections": coll_p.canonicalize()?, "collections_sha256": hash_file(coll_p)?,
            "selected": want.len(), "to_send": items.len(), "not_sent": not_sent,
            "jobs": job_list, "billed_units": total, "job_unit_cap": JOB_UNITS,
            "tail_units": tail_units, "tail_job_unit_cap": TAIL_JOB_UNITS,
            "dispatch_order": format!("ascending SHA-256(\"{ORDER_KEY_DOMAIN}:{split}:\" + id)"),
            "network_calls": 0}),
    )?;
    println!(
        "{}",
        json!({"selected": want.len(), "to_send": items.len(), "jobs": jobs.len(), "billed_units": total})
    );
    Ok(())
}

fn ledger_init(a: &[String]) -> Result<()> {
    ensure!(a.len() == 2, "ledger-init LEDGER BUDGET_UNITS");
    let p = Path::new(&a[0]);
    let u: f64 = a[1].parse()?;
    ensure!(u.is_finite() && u > 0.0, "bad budget");
    output_root(p.parent().context("parent")?)?;
    put(
        p,
        &json!({"schema": "slop_ninja_a_comp_pangram_ledger_v1", "budget_units": u,
            "committed": [], "rule": "a job is sent only if committed units plus its own stay within budget_units; the commitment is durable before the POST"}),
    )
}

fn checked_job(store: &Path, job: &str) -> Result<(PathBuf, Value, Value)> {
    ensure!(safe_id(job), "bad job");
    let dir = store.join("jobs").join(job);
    let plan = read_json(&dir.join("plan.json"))?;
    ensure!(
        hash_file(&dir.join("request.json"))? == field(&plan, "request_sha256")?,
        "request changed"
    );
    let request = read_json(&dir.join("request.json"))?;
    let items = request["items"].as_array().context("items")?;
    let planned = plan["items"].as_array().context("plan items")?;
    ensure!(items.len() == planned.len(), "request population differs");
    let mut units = 0;
    for (r, p) in items.iter().zip(planned) {
        ensure!(
            r["id"] == p["id"] && digest(field(r, "text")?) == field(p, "text_sha256")?,
            "request row differs"
        );
        units += billed_units(field(r, "text")?);
    }
    ensure!(
        units == plan["billed_units"].as_u64().context("units")? && units <= JOB_UNITS,
        "unit count differs"
    );
    Ok((dir, plan, request))
}

fn roster(receipt: &Value, request: &Value) -> Result<BTreeMap<usize, String>> {
    let items = request["items"].as_array().context("items")?;
    ensure!(
        receipt["total_items"] == items.len() && !field(receipt, "bulk_id")?.is_empty(),
        "bulk receipt binding"
    );
    let mut seen = BTreeSet::new();
    let mut tasks = BTreeSet::new();
    let mut accepted = BTreeMap::new();
    for (key, ok) in [("accepted_items", true), ("failed_items", false)] {
        for r in receipt[key].as_array().context("receipt roster")? {
            let n = r["index"].as_u64().context("index")? as usize;
            ensure!(
                n < items.len() && r["id"] == items[n]["id"] && seen.insert(n),
                "duplicate or mismatched acceptance index"
            );
            if ok {
                let t = field(r, "task_id")?.to_owned();
                ensure!(tasks.insert(t.clone()), "duplicate task");
                accepted.insert(n, t);
            }
        }
    }
    ensure!(seen.len() == items.len(), "acceptance roster omitted inputs");
    Ok(accepted)
}

/// Returns false when the job was not sent because the ledger cannot hold it.
fn submit(store: &Path, job: &str, ledger_p: &Path, key: &str) -> Result<bool> {
    let (dir, plan, request) = checked_job(store, job)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .open(dir.join("lock"))?;
    lock.try_lock()?;
    if dir.join("receipt.json").exists() {
        roster(&read_json(&dir.join("receipt.json"))?, &request)?;
        return Ok(true);
    }
    ensure!(
        !dir.join("submission-intent.json").exists(),
        "uncertain bulk submission for {job}; reconcile from the journal, never repeat the POST"
    );
    ensure!(
        !store.join("quota-stop.json").exists(),
        "quota stop latched; no new submissions"
    );
    let units = plan["billed_units"].as_u64().context("units")?;
    let mut ledger = read_json(ledger_p)?;
    let budget = ledger["budget_units"].as_f64().context("budget")?;
    let committed: u64 = ledger["committed"]
        .as_array()
        .context("committed")?
        .iter()
        .map(|c| c["billed_units"].as_u64().unwrap_or(0))
        .sum();
    let tag = format!("{}:{}", store.display(), job);
    if !ledger["committed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["job"] == tag)
    {
        if (committed + units) as f64 > budget {
            put(
                &dir.join("not-sent.json"),
                &json!({"reason": "budget", "committed_units": committed, "job_units": units,
                    "budget_units": budget, "missing": true}),
            )?;
            return Ok(false);
        }
        ledger["committed"].as_array_mut().unwrap().push(
            json!({"job": tag, "split": plan["split"], "billed_units": units,
                "time": chrono::Utc::now().to_rfc3339()}),
        );
        replace(ledger_p, &ledger)?;
    }
    for it in plan["items"].as_array().unwrap() {
        let id = field(it, "id")?;
        let folder = store.join("obs").join(id);
        fs::create_dir_all(&folder)?;
        let intent = json!({"id": id, "text_sha256": it["text_sha256"], "split": plan["split"],
            "model": MODEL, "job": job, "plan_sha256": hash_file(&dir.join("plan.json"))?});
        let p = folder.join("intent.json");
        if p.exists() {
            ensure!(read_json(&p)? == intent, "{id}: existing intent differs");
        } else {
            put(&p, &intent)?;
        }
    }
    let client = Client::new(key.to_owned(), store, false)?;
    let models = client.request("/models", None, &dir)?;
    ensure!(contains_selector(&models, MODEL), "selector unavailable");
    put(
        &dir.join("submission-intent.json"),
        &json!({"request_sha256": plan["request_sha256"], "time": chrono::Utc::now().to_rfc3339(),
            "automatic_retries": false, "idempotency_key": idempotency_key(&request)?}),
    )?;
    let receipt = client.request("/bulk", Some(&request), &dir)?;
    put(&dir.join("receipt.json"), &receipt)?;
    roster(&receipt, &request)?;
    Ok(true)
}

fn check_success(item: &Value, submitted: &str, task: &str) -> Result<&'static str> {
    ensure!(
        item["task_id"] == task && item["stage"] == "STAGE_SUCCESS",
        "result identity/stage differs"
    );
    let r = &item["result"];
    ensure!(
        r.get("stage").is_none_or(|s| s == "STAGE_SUCCESS"),
        "inner stage conflicts"
    );
    let returned = field(r, "text")?;
    let echo = if returned == submitted {
        "exact"
    } else {
        ensure!(
            returned.split_whitespace().eq(submitted.split_whitespace()),
            "echo mismatch beyond whitespace"
        );
        "whitespace_only"
    };
    field(r, "version")?;
    let mut sum = 0.0;
    for k in ["fraction_human", "fraction_ai", "fraction_ai_assisted"] {
        let v = r[k].as_f64().context("fraction")?;
        ensure!(v.is_finite() && (0.0..=1.0).contains(&v), "invalid fraction");
        sum += v;
    }
    ensure!((sum - 1.0).abs() < 1e-5, "fractions do not sum to one");
    Ok(echo)
}

fn collect(store: &Path, job: &str, key: &str) -> Result<bool> {
    let (dir, plan, request) = checked_job(store, job)?;
    if dir.join("complete.json").exists() {
        return Ok(true);
    }
    let lock = File::options()
        .read(true)
        .write(true)
        .open(dir.join("lock"))?;
    lock.try_lock()?;
    let receipt = read_json(&dir.join("receipt.json"))?;
    let accepted = roster(&receipt, &request)?;
    let bulk = field(&receipt, "bulk_id")?.to_owned();
    ensure!(safe_id(&bulk), "unsafe bulk id");
    let items = request["items"].as_array().unwrap();
    let round = dir.join(format!("collection-{}", stamp()?));
    fs::create_dir(&round)?;
    // Collection of an accepted job is a GET on an existing obligation and
    // stays allowed after a quota stop.
    let client = Client::new(key.to_owned(), store, true)?;
    let status = client.request(&format!("/bulk/{bulk}"), None, &round)?;
    put(&round.join("status.json"), &status)?;
    ensure!(
        status["bulk_id"] == bulk.as_str() && status["total_items"] == items.len(),
        "bulk status binding"
    );
    match status["status"].as_str() {
        Some("succeeded" | "failed" | "partial") => {}
        Some("queued" | "running") => return Ok(false),
        other => bail!("unknown bulk state {other:?}"),
    }
    let mut seen = BTreeSet::new();
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut versions = BTreeSet::new();
    let mut echoes: BTreeMap<String, usize> = BTreeMap::new();
    let plan_sha = hash_file(&dir.join("plan.json"))?;
    for offset in (0..items.len()).step_by(PAGE) {
        let page = client.request(
            &format!("/bulk/{bulk}/results?offset={offset}&limit={PAGE}"),
            None,
            &round,
        )?;
        put(&round.join(format!("page-{offset}.json")), &page)?;
        ensure!(
            page["bulk_id"] == bulk.as_str()
                && page["total_items"] == items.len()
                && page["offset"] == offset,
            "result page binding"
        );
        for k in ["items", "failed_items"] {
            for item in page[k].as_array().context("result array")? {
                let i = item["index"].as_u64().context("index")? as usize;
                ensure!(
                    (offset..items.len().min(offset + PAGE)).contains(&i)
                        && seen.insert(i)
                        && item["id"] == items[i]["id"],
                    "duplicate or mismatched result index"
                );
                let id = field(&items[i], "id")?;
                let folder = store.join("obs").join(id);
                let submitted = field(&items[i], "text")?;
                let base = json!({"schema": "slop_ninja_a_comp_pangram_observation_v1",
                    "id": id, "split": plan["split"], "model": MODEL, "text_sha256": digest(submitted),
                    "bulk_id": bulk, "bulk_index": i, "job": job, "plan_sha256": plan_sha,
                    "collected_utc": chrono::Utc::now().to_rfc3339(), "is_training_label": false});
                let outcome = if item["stage"] == "STAGE_FAILED" {
                    Err(anyhow::anyhow!("provider item failure"))
                } else {
                    accepted
                        .get(&i)
                        .context("unaccepted result")
                        .and_then(|t| check_success(item, submitted, t))
                };
                let mut obs = base;
                match outcome {
                    Ok(echo) => {
                        *echoes.entry(echo.into()).or_default() += 1;
                        let v = field(&item["result"], "version")?.to_owned();
                        versions.insert(v.clone());
                        let r = &item["result"];
                        obs["status"] = json!("success");
                        obs["version"] = json!(v);
                        obs["echo_match"] = json!(echo);
                        obs["fraction_ai"] = r["fraction_ai"].clone();
                        obs["fraction_ai_assisted"] = r["fraction_ai_assisted"].clone();
                        obs["fraction_human"] = r["fraction_human"].clone();
                        obs["task_id"] = item["task_id"].clone();
                        obs["returned_text_sha256"] = json!(digest(field(r, "text")?));
                        obs["response"] = r.clone();
                        ok += 1;
                    }
                    Err(e) => {
                        obs["status"] = json!("failure");
                        obs["failure"] = json!(e.to_string());
                        obs["item"] = item.clone();
                        failed += 1;
                    }
                }
                let path = folder.join(format!("observation-{}.json", stamp()?));
                put(&path, &obs)?;
                match fs::hard_link(&path, folder.join("result.json")) {
                    Ok(()) => File::open(&folder)?.sync_all()?,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let prev = read_json(&folder.join("result.json"))?;
                        ensure!(
                            prev["response"] == obs["response"] && prev["status"] == obs["status"],
                            "{id}: differing result already stored; both retained"
                        );
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    ensure!(seen.len() == items.len(), "terminal result population incomplete");
    put(
        &dir.join("complete.json"),
        &json!({"schema": "slop_ninja_a_comp_pangram_bulk_complete_v1", "bulk_id": bulk,
            "submitted": items.len(), "succeeded": ok, "failed": failed,
            "provider_succeeded": status["succeeded"], "provider_failed": status["failed"],
            "versions": versions, "echoes": echoes, "collection": round, "plan_sha256": plan_sha}),
    )?;
    Ok(true)
}

fn run(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 3 || a.len() == 4,
        "run STORE LEDGER KEYFILE [WATCH_SECONDS]"
    );
    let store = Path::new(&a[0]).canonicalize()?;
    let ledger = Path::new(&a[1]).canonicalize()?;
    let watch = Duration::from_secs(a.get(3).map(|s| s.parse()).transpose()?.unwrap_or(7200));
    let key = credential(Path::new(&a[2]))?;
    let plan = read_json(&store.join("PLAN.json"))?;
    for j in plan["jobs"].as_array().context("jobs")? {
        let job = field(j, "job")?;
        let dir = store.join("jobs").join(job);
        if dir.join("complete.json").exists() || dir.join("not-sent.json").exists() {
            continue;
        }
        if !dir.join("receipt.json").exists() {
            if !submit(&store, job, &ledger, &key)? {
                eprintln!("{job}: not sent (budget); its items are missing");
                continue;
            }
        }
        let until = Instant::now() + watch;
        while !collect(&store, job, &key)? {
            ensure!(
                Instant::now() < until,
                "{job}: watch budget ended; rerun to resume collection"
            );
            std::thread::sleep(Duration::from_secs(20));
        }
        eprintln!("{job}: complete");
    }
    summary(&[a[0].clone()])
}

fn summary(a: &[String]) -> Result<()> {
    ensure!(a.len() == 1, "summary STORE");
    let store = Path::new(&a[0]);
    let plan = read_json(&store.join("PLAN.json"))?;
    let mut rows = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for nsent in plan["not_sent"].as_array().context("not_sent")? {
        *counts.entry("not_sent".into()).or_default() += 1;
        rows.push(json!({"id": nsent["id"], "status": "missing", "reason": nsent["reason"]}));
    }
    let mut units_sent = 0u64;
    for j in plan["jobs"].as_array().context("jobs")? {
        let job = field(j, "job")?;
        let dir = store.join("jobs").join(job);
        let jp = read_json(&dir.join("plan.json"))?;
        let sent = dir.join("receipt.json").exists();
        if sent {
            units_sent += jp["billed_units"].as_u64().unwrap_or(0);
        }
        for it in jp["items"].as_array().unwrap() {
            let id = field(it, "id")?;
            let r = store.join("obs").join(id).join("result.json");
            let row = if r.exists() {
                let o = read_json(&r)?;
                json!({"id": id, "status": o["status"], "version": o["version"],
                    "fraction_ai": o["fraction_ai"], "fraction_ai_assisted": o["fraction_ai_assisted"],
                    "echo_match": o["echo_match"], "billed_units": it["billed_units"]})
            } else if dir.join("not-sent.json").exists() {
                json!({"id": id, "status": "missing", "reason": "budget_not_sent"})
            } else if sent {
                json!({"id": id, "status": "pending_or_uncertain"})
            } else {
                json!({"id": id, "status": "not_submitted"})
            };
            *counts
                .entry(row["status"].as_str().unwrap_or("?").to_owned())
                .or_default() += 1;
            rows.push(row);
        }
    }
    let out = store.join(format!("summary-{}.json", stamp()?));
    put(
        &out,
        &json!({"schema": "slop_ninja_a_comp_pangram_bulk_summary_v1", "counts": counts,
            "billed_units_sent": units_sent, "rows": rows}),
    )?;
    println!("{}", json!({"counts": counts, "billed_units_sent": units_sent, "summary": out}));
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("prepare") => prepare(&args[1..]),
        Some("ledger-init") => ledger_init(&args[1..]),
        Some("run") => run(&args[1..]),
        Some("collect") if args.len() == 4 => {
            let key = credential(Path::new(&args[3]))?;
            let store = Path::new(&args[1]).canonicalize()?;
            println!("{}", collect(&store, &args[2], &key)?);
            Ok(())
        }
        Some("summary") => summary(&args[1..]),
        _ => bail!(
            "prepare SPLIT SUBSAMPLE ROWS COLLECTIONS STORE | ledger-init LEDGER UNITS | run STORE LEDGER KEYFILE [SECS] | collect STORE JOB KEYFILE | summary STORE"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn billed_units_are_started_hundred_word_blocks_at_least_one() {
        assert_eq!(billed_units(""), 1);
        assert_eq!(billed_units(&vec!["a"; 100].join(" ")), 1);
        assert_eq!(billed_units(&vec!["a"; 101].join(" ")), 2);
        assert_eq!(billed_units(&vec!["a"; 450].join("\n ")), 5);
    }
    #[test]
    fn jobs_never_exceed_the_unit_cap() {
        let items: Vec<(String, u64)> = (0..700).map(|i| (format!("d{i}"), 3)).collect();
        let jobs = pack(&items, 0).unwrap();
        assert_eq!(jobs.iter().map(Vec::len).sum::<usize>(), 700);
        for j in &jobs {
            assert!(j.iter().map(|&i| items[i].1).sum::<u64>() <= JOB_UNITS);
        }
        assert_eq!(jobs.len(), 3);
        assert!(pack(&[("x".into(), 1001)], 0).is_err());
        // Tail: the last 300 units go into jobs of at most 25 units, in order.
        let jobs = pack(&items, 300).unwrap();
        let flat: Vec<usize> = jobs.iter().flatten().copied().collect();
        assert_eq!(flat, (0..700).collect::<Vec<_>>());
        let tail: Vec<&Vec<usize>> = jobs.iter().filter(|j| j.iter().map(|&i| items[i].1).sum::<u64>() <= 25).collect();
        assert_eq!(tail.iter().map(|j| j.len()).sum::<usize>(), 100);
        for j in &jobs {
            assert!(j.iter().map(|&i| items[i].1).sum::<u64>() <= JOB_UNITS);
        }
    }
    #[test]
    fn acceptance_requires_complete_unique_bindings() {
        let request = json!({"items":[{"id":"a"},{"id":"b"}]});
        let valid = json!({"bulk_id":"b1","total_items":2,"accepted_items":[{"id":"a","index":0,"task_id":"t1"},{"id":"b","index":1,"task_id":"t2"}],"failed_items":[]});
        assert_eq!(roster(&valid, &request).unwrap().len(), 2);
        let mut v = valid.clone();
        v["accepted_items"][1]["task_id"] = json!("t1");
        assert!(roster(&v, &request).is_err());
        let mut v = valid.clone();
        v["accepted_items"].as_array_mut().unwrap().pop();
        assert!(roster(&v, &request).is_err());
    }
    #[test]
    fn echo_and_fraction_checks() {
        let sub = "One two.\r\nThree four.";
        let mut v = json!({"id":"a","task_id":"t1","stage":"STAGE_SUCCESS","result":{"text":"One two. Three four.","version":"4.1","fraction_ai":0.2,"fraction_ai_assisted":0.3,"fraction_human":0.5}});
        assert_eq!(check_success(&v, sub, "t1").unwrap(), "whitespace_only");
        v["result"]["text"] = json!(sub);
        assert_eq!(check_success(&v, sub, "t1").unwrap(), "exact");
        v["result"]["text"] = json!("One changed. Three four.");
        assert!(check_success(&v, sub, "t1").is_err());
        v["result"]["text"] = json!(sub);
        v["result"]["fraction_human"] = json!(0.9);
        assert!(check_success(&v, sub, "t1").is_err());
        v["result"]["fraction_human"] = json!(0.5);
        assert!(check_success(&v, sub, "t2").is_err());
    }
}
