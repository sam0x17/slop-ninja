//! Bounded, exact-document Pangram comparator for explicitly permitted sources.
//!
//! Usage: a_supervised_pangram INPUT MANIFEST SPLIT OUT KEYFILE MODEL
//! Test requires: a_supervised_pangram test INPUT MANIFEST test OUT KEYFILE MODEL
//! Prefix either route with `prepare` for an offline reuse audit (no credentials
//! or network). --reuse-from accepts only explicit Development/CAL archives;
//! a Test destination does not authorize scanning a historical Test corpus.
//! Ordinary SPLIT is development or calibration; training is never supported.
//!
//! INPUT is plain JSONL, with each record containing id, text,
//! text_sha256, source_group, frozen_split, source (a nonempty provenance object),
//! and rights: {external_evaluation: true, ...}. Preserve dataset
//! source/rights in those objects, including the origin of local generations.
//! Do not manufacture permission for private correspondence or coursework.
//!
//! MANIFEST is an explicitly curated evaluation allow-list, not a corpus scan:
//! {"input_sha256":"<SHA-256 of exact INPUT bytes>",
//!  "frozen_split":"development", "allowed_ids":["example-id", ...]}
//! Also accepts slop_ninja_a_supervised_dataset_v1: records_sha256, non_test_ids,
//! test_ids (disjoint), and split_assignments (source_group -> frozen_split).
//! That input contains all non-Test records; only SPLIT rows are submitted.
//! Explicit Test uses test_records_sha256 and exactly test_ids instead.
//! All and only allowed IDs must occur once. Extra manifest/record fields survive
//! in the input digest; source and rights are also copied into durable receipts.
//! Prepare a bounded single-split input/manifest from authorized public-domain
//! excerpts, local generations, or public datasets. No source paths are opened.
//! Source authorization accepts source.record containing a public origin record
//! (schema slop-ninja-origin-record-v1, source.collection, rights), or
//! source.{dataset_source:{collection:...},dataset_rights:{...}}. Original rights
//! must permit external_evaluation and record a license. PG author rows retain
//! source.{source_manifest_sha256,record:{pg_id,source_url,rights:{
//! external_evaluation:true,catalog_statement:"Public domain in the USA.",...}}}.
//! Locally generated descendants retain that authorized source provenance.
//!
//! OUT must be under a Git-ignored data/ directory in a Git checkout. Keep this
//! same OUT for recovery/reuse: OUT/<id>/result.json retains the existing
//! slop_ninja_a_pangram_comparator_v1 schema used by grammar's compare_pangram.
//! available_cfg is null unless supplied by the input; additional receipt fields
//! bind the input and manifest. Detector fractions are not training labels or
//! native calibrated involvement probabilities. Compare's separate report still
//! has its original cohort-specific scope; this adapter does not update it.
//!
//! Four workers handle at most 256 documents needing submission/polling per run,
//! with 600 seconds of polling per task and a shared 15-minute API-work budget
//! (in-flight submits may add up to
//! 60 seconds). Input/manifest limits are 64/8 MiB and 10,000 records. Rerun the
//! same command to finish a bounded batch. Reused results consume no API budget.
//! Uncertain POSTs are NEVER automatically repeated; an intent without a saved
//! submission requires manual reconciliation. Never delete receipts to retry.
//! Concurrent processes race on an atomic, fsynced intent; only its creator may
//! submit. Changing OUT defeats that protection. No credential shell is run.
//! `poll-only [test] ...` recovers already submitted task IDs after a quota stop.
//! It never permits POST or catalog requests and retains the submission stop file.
//!
//! True bulk support shares the exact input and rights validation:
//! `bulk prepare INPUT MANIFEST SPLIT FIXTURES NEW_JOB RESTORATION.json`
//! `bulk submit JOB KEYFILE`, then `bulk watch JOB KEYFILE` (20-minute bound),
//! or `bulk collect JOB KEYFILE` for one status/collection attempt.
//! RESTORATION records user-confirmed replenishment, permission for new requests,
//! and the SHA-256 of any original quota stop. Completed results are reused.
//! Only a complete hash-bound HTTP402 Insufficient credits response without a
//! task receipt can be reconciled; all other uncertain intents stay excluded.
//! Bulk POSTs have stable recorded idempotency keys AND a no-repeat intent guard.
//! The 1,000-unit cap counts started 1,000-word blocks, not per-document labels
//! or a claimed monetary price. Archive promptly: provider bulk retention is 48h.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use unslop::{evaluation::PangramFailure, util::digest};
#[path = "a_supervised_pangram/bulk.rs"]
mod bulk;

const MAX_ROWS: usize = 10_000;
const MAX_OPERATIONS: usize = 256;
const WORKERS: usize = 4;
const RUN_BUDGET: Duration = Duration::from_secs(900);
const INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

// The shared client intentionally omits HTTP error bodies. This paid runner
// journals every HTTP exchange, including intermediate polls and partial bodies.
// No authorization header is persisted, printed, or passed through a shell.
struct JournalClient {
    http: reqwest::blocking::Client,
    key: String,
    base: String,
    root: PathBuf,
    stopped: AtomicBool,
    existing_tasks_only: bool,
}

impl JournalClient {
    fn new(key: String, root: &Path) -> Result<Self> {
        ensure!(!key.is_empty(), "empty key");
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(60))
                .build()?,
            key,
            base: "https://text.external-api.pangram.com".into(),
            root: root.to_owned(),
            stopped: AtomicBool::new(root.join("quota-stop.json").exists()),
            existing_tasks_only: false,
        })
    }

    fn existing_tasks(key: String, root: &Path) -> Result<Self> {
        let mut client = Self::new(key, root)?;
        client.existing_tasks_only = true;
        // The original stop remains authoritative for submissions. This separate
        // read-only invocation can recover prior tasks; a new 402/429 stops it.
        client.stopped = AtomicBool::new(false);
        Ok(client)
    }

    fn request(
        &self,
        route: &str,
        payload: Option<Value>,
        folder: &Path,
        timeout: Duration,
    ) -> Result<Value> {
        ensure!(
            !self.existing_tasks_only
                || (payload.is_none()
                    && route.starts_with("/task/")
                    && route.len() > "/task/".len()),
            "poll-only client permits only existing-task GET requests"
        );
        ensure!(
            !self.stopped.load(Ordering::SeqCst),
            "HTTP 402 stop is latched; no further API work"
        );
        let journal = folder.join(format!("http-{}-{}", std::process::id(), stamp()?));
        fs::create_dir(&journal)?;
        File::open(folder)?.sync_all()?;
        let url = format!("{}{route}", self.base);
        let idempotency_key = if route == "/bulk" {
            payload.as_ref().map(bulk::idempotency_key).transpose()?
        } else {
            None
        };
        put(
            &journal.join("request.json"),
            &json!({"method":if payload.is_some(){"POST"}else{"GET"},
            "url":url,"payload":payload,"created_utc":chrono::Utc::now().to_rfc3339(),
            "automatic_retries":false,"authorization_header_retained":false,
            "idempotency_key":idempotency_key}),
        )?;
        let request = if let Some(payload) = &payload {
            self.http.post(&url).json(payload)
        } else {
            self.http.get(&url)
        };
        let request = if let Some(key) = &idempotency_key {
            request.header("Idempotency-Key", key)
        } else {
            request
        };
        let response = request
            .header("x-api-key", &self.key)
            .timeout(timeout)
            .send();
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                put(
                    &journal.join("transport-error.json"),
                    &json!({"kind":"transport","uncertain_submission":payload.is_some(),
                    "timeout":error.is_timeout(),"connect":error.is_connect(),"automatic_resubmit":false}),
                )?;
                anyhow::bail!("Pangram transport failure; durable journal retained; no retry");
            }
        };
        let status = response.status().as_u16();
        let headers = response.headers().iter().map(|(key, value)|
            json!({"name":key.as_str(),"value_hex":hex::encode(value.as_bytes())})).collect::<Vec<_>>();
        put(
            &journal.join("response.json"),
            &json!({"status":status,"headers":headers,
            "received_utc":chrono::Utc::now().to_rfc3339()}),
        )?;
        // Latch immediately on headers, before reading a possibly slow body.
        if status == 402 || status == 429 {
            self.stopped.store(true, Ordering::SeqCst);
            let stop = self.root.join("quota-stop.json");
            if !stop.exists() {
                let value = json!({"status":status,"request_journal":journal,"balance":null,
                    "action":"stop_new_API_work; retain in-flight obligations; no automatic resubmit"});
                if let Err(error) = put(&stop, &value) {
                    if !stop.exists() {
                        return Err(error);
                    }
                }
            }
        }
        let body_path = journal.join("body.bin");
        ignored(&body_path)?;
        let mut body_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&body_path)?;
        let copied = std::io::copy(&mut response.by_ref().take(INPUT_BYTES + 1), &mut body_file);
        body_file.sync_all()?;
        File::open(&journal)?.sync_all()?;
        let body = fs::read(&body_path)?;
        put(
            &journal.join("outcome.json"),
            &json!({"status":status,"bytes":body.len(),
            "body_sha256":hex::encode(sha2::Sha256::digest(&body)),"complete":copied.is_ok() && body.len() as u64 <= INPUT_BYTES,
            "body_read_failed":copied.is_err(),"finished_utc":chrono::Utc::now().to_rfc3339()}),
        )?;
        ensure!(
            copied.is_ok() && body.len() as u64 <= INPUT_BYTES,
            "Pangram response incomplete; partial raw body retained"
        );
        ensure!(
            (200..300).contains(&status),
            "Pangram HTTP {status}; raw response retained; no retry"
        );
        let value: Value =
            serde_json::from_slice(&body).context("Pangram invalid JSON; raw body retained")?;
        ensure!(
            value.is_object(),
            "Pangram non-object response; raw body retained"
        );
        Ok(value)
    }

    fn submit(&self, text: &str, model: &str, folder: &Path) -> Result<Value> {
        ensure!(text.split_whitespace().count() >= 50, "minimum 50 words");
        let submitted_at = chrono::Utc::now().to_rfc3339();
        let response = self.request(
            "/task",
            Some(json!({"text":text,"model":model,"public_dashboard_link":false})),
            folder,
            Duration::from_secs(60),
        )?;
        let task = field(&response, "task_id")?.to_owned();
        Ok(
            json!({"detector":"pangram","model":model,"task_id":task,"submitted_text":text,
            "sha256":digest(text),"submitted_at":submitted_at,"submission_response":response}),
        )
    }

    fn poll(&self, task: &str, timeout: Duration, folder: &Path) -> Result<Value> {
        let mut url = reqwest::Url::parse(&format!("{}/task/", self.base))?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("task URL"))?
            .pop_if_empty()
            .push(task);
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "poll budget exhausted; resume existing task only"
            );
            let response = self.request(
                url.path(),
                None,
                folder,
                remaining.min(Duration::from_secs(60)),
            )?;
            ensure!(
                response
                    .get("task_id")
                    .is_none_or(|v| v.is_null() || v == task),
                "Pangram task ID mismatch"
            );
            match response["stage"].as_str() {
                Some("STAGE_SUCCESS") => return Ok(response),
                Some("STAGE_FAILED") => anyhow::bail!("Pangram task failed; raw response retained"),
                Some(stage) if stage.starts_with("STAGE_") => (),
                _ => anyhow::bail!("Pangram invalid task stage; raw response retained"),
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2)),
            );
        }
    }
}

fn read_text(path: &Path, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "file exceeds byte limit");
    Ok(String::from_utf8(bytes)?)
}

fn read_receipt(path: &Path) -> Result<Value> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "receipt must be a regular file, not a symlink"
    );
    ignored(path)?;
    Ok(serde_json::from_str(&read_text(path, INPUT_BYTES)?)?)
}

fn ignored(path: &Path) -> Result<()> {
    ensure!(
        Command::new("git")
            .arg("-C")
            .arg(path.parent().context("receipt parent")?)
            .args(["check-ignore", "--quiet", "--"])
            .arg(path)
            .status()?
            .success(),
        "receipts and raw responses must be Git-ignored (and untracked)"
    );
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
    ensure!(git.status.success(), "OUT must belong to a Git checkout");
    let repo = PathBuf::from(String::from_utf8(git.stdout)?.trim());
    ensure!(
        path.starts_with(repo.canonicalize()?.join("data")),
        "OUT must be inside the checkout's ignored data/ directory"
    );
    ignored(&path.join("receipt-probe.json"))?;
    // Persist newly created output ancestors, as well as individual row folders.
    for ancestor in path.ancestors().take_while(|p| p.starts_with(&repo)) {
        File::open(ancestor)?.sync_all()?;
    }
    Ok(path)
}

// Atomic no-clobber publication plus directory fsync: never POST before the
// complete intent is durable, and never overwrite an observation on recovery.
fn put(path: &Path, value: &Value) -> Result<()> {
    ignored(path)?;
    let parent = path.parent().context("receipt parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn credential(path: &Path) -> Result<String> {
    if let Ok(key) = std::env::var("PANGRAM_API_KEY") {
        ensure!(!key.is_empty(), "empty key");
        return Ok(key);
    }
    let text = read_text(path, 1024 * 1024)?;
    let mut found = None;
    for line in text.lines() {
        let line = line
            .trim()
            .strip_prefix("export ")
            .unwrap_or(line.trim())
            .trim();
        if let Some(value) = line.strip_prefix("PANGRAM_API_KEY=") {
            let value = value.trim().trim_end_matches(';');
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            ensure!(
                !value.is_empty()
                    && !value.contains(['$', '`', '\\', '\n', ' ', '\t', '\r', '"', '\'']),
                "key must be a literal assignment; no shell code is executed"
            );
            found = Some(value.to_owned());
        }
    }
    found.context("PANGRAM_API_KEY literal assignment absent")
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing/non-string/empty {key}"))
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 240
        && !matches!(id, "." | "..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

fn ids_at(manifest: &Value, key: &str) -> Result<BTreeSet<String>> {
    let ids = manifest[key]
        .as_array()
        .with_context(|| format!("manifest {key}"))?;
    ensure!(ids.len() <= MAX_ROWS, "manifest row limit");
    let mut allowed = BTreeSet::new();
    for id in ids {
        let id = id.as_str().context("allow-listed ID must be a string")?;
        ensure!(
            safe_id(id) && allowed.insert(id.to_owned()),
            "unsafe/duplicate allowed ID"
        );
    }
    Ok(allowed)
}

fn dataset_manifest(manifest: &Value) -> bool {
    manifest["schema"] == "slop_ninja_a_supervised_dataset_v1"
}

fn manifest_ids(manifest: &Value, split: &str) -> Result<BTreeSet<String>> {
    let allowed = if dataset_manifest(manifest) {
        let non_test = ids_at(manifest, "non_test_ids")?;
        let test = ids_at(manifest, "test_ids")?;
        ensure!(
            non_test.is_disjoint(&test),
            "Test/non-Test allow-lists overlap"
        );
        ensure!(
            manifest["split_assignments"].is_object(),
            "missing family split assignments"
        );
        if split == "test" { test } else { non_test }
    } else {
        ensure!(manifest["frozen_split"] == split, "manifest split mismatch");
        ids_at(manifest, "allowed_ids")?
    };
    ensure!(!allowed.is_empty(), "empty manifest allow-list");
    Ok(allowed)
}

fn public_source(row: &Value) -> Result<()> {
    ensure!(
        row["rights"]["external_evaluation"] == true,
        "source lacks external evaluation permission"
    );
    let source = &row["source"];
    let record = &source["record"];
    if let Some(source_split) = record.get("split").and_then(Value::as_str) {
        let normalized = if source_split == "train_reference" {
            "train"
        } else {
            source_split
        };
        ensure!(
            row["frozen_split"] == normalized,
            "source provenance split mismatch"
        );
    }
    let (origin_source, origin_rights) = if record["schema"] == "slop-ninja-origin-record-v1" {
        (&record["source"], &record["rights"])
    } else {
        (
            source.get("dataset_source").unwrap_or(&source["source"]),
            source.get("dataset_rights").unwrap_or(&source["rights"]),
        )
    };
    let origin = field(origin_source, "collection").is_ok()
        && origin_rights["external_evaluation"] == true
        && field(origin_rights, "license").is_ok()
        && row["rights"] == *origin_rights;
    let manifest_hash = source["source_manifest_sha256"].as_str().unwrap_or("");
    let pg_id = record["pg_id"]
        .as_u64()
        .or_else(|| record["pg_id"].as_str().and_then(|s| s.parse().ok()));
    let pg = pg_id.is_some_and(|id| id > 0)
        && manifest_hash.len() == 64
        && manifest_hash.bytes().all(|b| b.is_ascii_hexdigit())
        && record["source_url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://www.gutenberg.org/"))
        && record["rights"]["external_evaluation"] == true
        && record["rights"]["catalog_statement"] == "Public domain in the USA."
        && row["rights"] == record["rights"];
    ensure!(
        origin || pg,
        "explicit public origin rights or pinned PG author provenance required"
    );
    Ok(())
}

fn validate_input(input: &str, manifest: &Value, split: &str) -> Result<Vec<Value>> {
    let allowed = manifest_ids(manifest, split)?;
    let hash_key = if dataset_manifest(manifest) {
        if split == "test" {
            "test_records_sha256"
        } else {
            "records_sha256"
        }
    } else {
        "input_sha256"
    };
    ensure!(
        manifest[hash_key] == digest(input),
        "input digest does not match manifest"
    );
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for line in input.lines() {
        ensure!(seen.len() < MAX_ROWS, "input row limit");
        let row: Value = serde_json::from_str(line).context("invalid JSONL record")?;
        let id = field(&row, "id")?;
        ensure!(
            allowed.contains(id) && seen.insert(id.to_owned()),
            "unlisted/duplicate input ID"
        );
        let group = field(&row, "source_group")?;
        let row_split = field(&row, "frozen_split")?;
        if dataset_manifest(manifest) {
            ensure!(
                manifest["split_assignments"][group] == row_split,
                "family split mismatch"
            );
            ensure!(
                if split == "test" {
                    row_split == "test"
                } else {
                    matches!(row_split, "train" | "development" | "calibration")
                },
                "unauthorized input split"
            );
        } else {
            ensure!(row_split == split, "input split mismatch");
        }
        let text = field(&row, "text")?;
        ensure!(
            row["text_sha256"] == digest(text),
            "exact text digest mismatch"
        );
        public_source(&row)?;
        if row_split == split {
            ensure!(
                text.split_whitespace().count() >= 50,
                "Pangram requires at least 50 words"
            );
            rows.push(row);
        }
    }
    ensure!(
        seen == allowed,
        "input IDs must exactly match manifest allow-list"
    );
    ensure!(!rows.is_empty(), "requested split has no rows");
    Ok(rows)
}

fn binding_matches(receipt: &Value, row: &Value, model: &str, split: &str) -> Result<()> {
    ensure!(
        receipt["id"] == row["id"]
            && receipt["text_sha256"] == row["text_sha256"]
            && receipt["model"] == model
            && receipt["source_group"] == row["source_group"]
            && receipt["split"] == split,
        "existing receipt binding differs; refusing reuse or resubmission"
    );
    Ok(())
}

fn submission_matches(submission: &Value, row: &Value, model: &str) -> Result<()> {
    ensure!(
        submission["model"] == model
            && submission["sha256"] == row["text_sha256"]
            && submission["submitted_text"] == row["text"],
        "saved submission differs from exact input/model"
    );
    field(submission, "task_id")?;
    Ok(())
}

fn failure(stage: &str, error: &anyhow::Error) -> Value {
    json!({"stage":stage,"automatic_resubmit":false,
        "error":error.to_string(),
        "details":error.downcast_ref::<PangramFailure>().map(PangramFailure::metadata)})
}

fn stamp() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos())
}

struct Run<'a> {
    client: Option<&'a JournalClient>,
    out: &'a Path,
    model: &'a str,
    split: &'a str,
    input_hash: &'a str,
    manifest_hash: &'a str,
    started: Instant,
    operations: AtomicUsize,
    poll_only: bool,
}

fn compare(row: &Value, state: &BTreeMap<&str, Value>, run: &Run<'_>) -> Result<Value> {
    let id = field(row, "id")?;
    let folder = run.out.join(id);
    let final_path = folder.join("result.json");
    if state.contains_key("result.json") {
        return Ok(json!({"id":id,"result":final_path,"reused":true}));
    }
    if state.contains_key("archive-blocked.json") {
        return Ok(
            json!({"id":id,"status":"archived_submission_requires_reconciliation_do_not_repeat"}),
        );
    }
    if let Some(canonical) = state.get("duplicate_of") {
        return Ok(json!({"id":id,"status":"deferred_exact_hash_alias","canonical_id":canonical}));
    }
    if state.contains_key("intent.json") && !state.contains_key("submission.json") {
        return Ok(json!({"id":id,"status":"uncertain_submission_do_not_repeat"}));
    }
    if run.poll_only && !state.contains_key("submission.json") {
        return Ok(json!({"id":id,"status":"not_submitted_poll_only"}));
    }
    if run.client.is_some_and(|c| c.stopped.load(Ordering::SeqCst)) {
        return Ok(json!({"id":id,"status":"deferred_http_402_stop"}));
    }
    if run.started.elapsed() >= RUN_BUDGET
        || run
            .operations
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < MAX_OPERATIONS).then_some(n + 1)
            })
            .is_err()
    {
        return Ok(json!({"id":id,"status":"deferred_run_budget"}));
    }
    let client = run.client.context("client")?;
    let item_started = Instant::now();
    let binding = json!({"id":id,"model":run.model,"text_sha256":row["text_sha256"],
        "source_group":row["source_group"],"split":run.split,
        "input_sha256":run.input_hash,"manifest_sha256":run.manifest_hash,
        "source":row["source"],"rights":row["rights"]});
    let submission = if let Some(submission) = state.get("submission.json") {
        submission.clone()
    } else {
        ensure!(
            !run.poll_only,
            "poll-only cannot create a submission intent"
        );
        let mut intent = binding.clone();
        intent["status"] = json!("submission_intent");
        // Atomic publication: a competing creator cannot reach POST.
        put(&folder.join("intent.json"), &intent)?;
        match client.submit(field(row, "text")?, run.model, &folder) {
            Ok(submission) => {
                put(&folder.join("submission.json"), &submission)?;
                submission
            }
            Err(error) => {
                put(&folder.join("failure.json"), &failure("submit", &error))?;
                return Ok(json!({"id":id,"status":"submission_failed_or_uncertain"}));
            }
        }
    };
    let remaining = RUN_BUDGET
        .saturating_sub(run.started.elapsed())
        .min(Duration::from_secs(600));
    if remaining.is_zero() {
        return Ok(json!({"id":id,"status":"submitted_poll_deferred"}));
    }
    let task = field(&submission, "task_id")?;
    match client.poll(task, remaining, &folder) {
        Ok(response) => {
            let mut result = binding;
            result["schema"] = json!("slop_ninja_a_pangram_comparator_v1");
            result["response"] = response;
            result["task_id"] = json!(task);
            result["elapsed_ms"] = json!(item_started.elapsed().as_millis());
            result["available_cfg"] = row
                .get("available_cfg")
                .or_else(|| row.get("available"))
                .cloned()
                .unwrap_or(Value::Null);
            result["is_training_label"] = json!(false);
            // Save every observation first; atomic linking chooses the canonical
            // result without discarding a concurrent poll's distinct observation.
            let observation = folder.join(format!(
                "observation-{}-{}.json",
                std::process::id(),
                stamp()?
            ));
            put(&observation, &result)?;
            ignored(&final_path)?;
            match fs::hard_link(&observation, &final_path) {
                Ok(()) => File::open(&folder)?.sync_all()?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let prior = read_receipt(&final_path)?;
                    binding_matches(&prior, row, run.model, run.split)?;
                    ensure!(
                        prior["response"]["stage"] == "STAGE_SUCCESS",
                        "concurrent result failed"
                    );
                }
                Err(error) => return Err(error.into()),
            }
            Ok(json!({"id":id,"result":final_path,"reused":false}))
        }
        Err(error) => {
            let mut value = failure("poll", &error);
            value["task_id"] = json!(task);
            value["resume_existing_task"] = json!(true);
            put(
                &folder.join(format!(
                    "poll-failure-{}-{}.json",
                    std::process::id(),
                    stamp()?
                )),
                &value,
            )?;
            Ok(json!({"id":id,"status":"poll_failed"}))
        }
    }
}

fn reuse_observation(
    original: &Value,
    row: &Value,
    model: &str,
    split: &str,
    archive: &Path,
    input_hash: &str,
    manifest_hash: &str,
) -> Result<Value> {
    ensure!(
        original["model"] == model
            && original["text_sha256"] == row["text_sha256"]
            && original["response"]["stage"] == "STAGE_SUCCESS",
        "archive exact hash/model/status mismatch"
    );
    let submission_path = archive
        .parent()
        .context("archive parent")?
        .join("submission.json");
    let submission = read_receipt(&submission_path)?;
    submission_matches(&submission, row, model)?;
    ensure!(
        submission["task_id"] == original["task_id"],
        "archive task mismatch"
    );
    let mut value = original.clone();
    value["observation_reuse"] = json!({"kind":"exact_bytes_existing_paid_observation","new_submission":false,
        "original_id":original["id"],"original_source_group":original["source_group"],"original_split":original["split"],
        "result_path":archive,"result_sha256":hex::encode(sha2::Sha256::digest(fs::read(archive)?)),
        "submission_path":submission_path,"submission_sha256":hex::encode(sha2::Sha256::digest(fs::read(&submission_path)?))});
    for (key, replacement) in [
        ("id", row["id"].clone()),
        ("source_group", row["source_group"].clone()),
        ("split", json!(split)),
        ("source", row["source"].clone()),
        ("rights", row["rights"].clone()),
        ("input_sha256", json!(input_hash)),
        ("manifest_sha256", json!(manifest_hash)),
    ] {
        value[key] = replacement;
    }
    Ok(value)
}

fn prepare_reuse(
    rows: &[Value],
    archives: &[PathBuf],
    out: &Path,
    model: &str,
    split: &str,
    input_hash: &str,
    manifest_hash: &str,
) -> Result<Value> {
    let wanted = rows
        .iter()
        .map(|r| field(r, "text_sha256").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut matches = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut pending = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut inspected = 0;
    for root in archives
        .iter()
        .map(PathBuf::as_path)
        .chain(std::iter::once(out))
    {
        if root != out {
            ensure!(
                matches!(
                    root.file_name().and_then(|s| s.to_str()),
                    Some("development" | "calibration")
                ),
                "archive root must explicitly be development or calibration; no Test scan"
            );
        }
        let mut children = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            if !child.file_type()?.is_dir() {
                continue;
            }
            let result = child.path().join("result.json");
            let intent = child.path().join("intent.json");
            let path = if result.exists() {
                &result
            } else if intent.exists() {
                &intent
            } else {
                continue;
            };
            let value = read_receipt(path)?;
            inspected += 1;
            if value["model"] != model {
                continue;
            }
            let hash = field(&value, "text_sha256")?;
            if !wanted.contains(hash) {
                continue;
            }
            if path == &result && value["response"]["stage"] == "STAGE_SUCCESS" {
                // Reused aliases point back to the immutable original observation.
                let original = value["observation_reuse"]["result_path"]
                    .as_str()
                    .map(PathBuf::from)
                    .unwrap_or(result);
                matches.entry(hash.into()).or_default().push(original);
            } else {
                pending.entry(hash.into()).or_default().push(path.clone());
            }
        }
    }
    let mut reused = 0;
    let mut blocked = 0;
    let mut versions = BTreeSet::new();
    for row in rows {
        let folder = out.join(field(row, "id")?);
        let result = folder.join("result.json");
        if result.exists() {
            continue;
        }
        let hash = field(row, "text_sha256")?;
        if let Some(paths) = matches.get(hash) {
            let original = read_receipt(&paths[0])?;
            let value = reuse_observation(
                &original,
                row,
                model,
                split,
                &paths[0],
                input_hash,
                manifest_hash,
            )?;
            fs::create_dir_all(&folder)?;
            File::open(out)?.sync_all()?;
            put(&result, &value)?;
            versions.insert(field(&original["response"], "version")?.to_owned());
            reused += 1;
        } else if let Some(paths) = pending.get(hash) {
            let external = paths
                .iter()
                .filter(|p| p.parent() != Some(folder.as_path()))
                .collect::<Vec<_>>();
            if !external.is_empty() && !folder.join("intent.json").exists() {
                fs::create_dir_all(&folder)?;
                File::open(out)?.sync_all()?;
                let blocked_path = folder.join("archive-blocked.json");
                if !blocked_path.exists() {
                    put(
                        &blocked_path,
                        &json!({"id":row["id"],"text_sha256":row["text_sha256"],"model":model,
                        "source_group":row["source_group"],"split":split,"archive_intents":external,"automatic_resubmit":false}),
                    )?;
                }
                blocked += 1;
            }
        }
    }
    Ok(
        json!({"schema":"slop_ninja_pangram_offline_preflight_v1","split":split,"model":model,
        "requested":rows.len(),"unique_text_hashes":wanted.len(),"archived_receipts_inspected":inspected,
        "new_exact_hash_reuses":reused,"archived_pending_blocks":blocked,"reused_versions":versions,
        "archive_roots":archives,"input_sha256":input_hash,"manifest_sha256":manifest_hash,
        "permissions":"all rows passed exact input, split and public source rights checks",
        "quota_balance":null,"quota_status":"unavailable_no_documented_balance_endpoint; stop_on_HTTP_402_or_429",
        "test_opened":split == "test","network_calls":0}),
    )
}

fn contains_selector(value: &Value, selector: &str) -> bool {
    match value {
        Value::String(s) => s == selector,
        Value::Array(a) => a.iter().any(|v| contains_selector(v, selector)),
        Value::Object(o) => {
            o.contains_key(selector) || o.values().any(|v| contains_selector(v, selector))
        }
        _ => false,
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|s| s == "bulk") {
        return bulk::main(&args[1..]);
    }
    let prepare_only = args.first().is_some_and(|s| s == "prepare");
    if prepare_only {
        args.remove(0);
    }
    let poll_only = args.first().is_some_and(|s| s == "poll-only");
    if poll_only {
        args.remove(0);
    }
    ensure!(
        !(prepare_only && poll_only),
        "prepare and poll-only are separate modes"
    );
    let test = args.first().is_some_and(|s| s == "test");
    if test {
        args.remove(0);
    }
    let mut archives = Vec::new();
    while args.len() > 6 {
        ensure!(
            args.len() >= 8 && args[6] == "--reuse-from",
            "expected --reuse-from NON_TEST_ARCHIVE_ROOT"
        );
        args.remove(6);
        archives.push(PathBuf::from(args.remove(6)));
    }
    ensure!(
        args.len() == 6,
        "a_supervised_pangram [test] INPUT MANIFEST SPLIT OUT KEYFILE MODEL"
    );
    let split = &args[2];
    ensure!(
        if test {
            split == "test"
        } else {
            matches!(split.as_str(), "development" | "calibration")
        },
        "only development/calibration allowed; Test requires explicit test command"
    );
    let model = &args[5];
    ensure!(!model.trim().is_empty(), "explicit model required");
    let manifest_text = read_text(Path::new(&args[1]), MANIFEST_BYTES)?;
    let manifest: Value = serde_json::from_str(&manifest_text)?;
    // Reject an unauthorized split/allow-list before opening INPUT at all.
    manifest_ids(&manifest, split)?;
    let input = read_text(Path::new(&args[0]), INPUT_BYTES)?;
    let rows = validate_input(&input, &manifest, split)?;
    let input_hash = digest(&input);
    let manifest_hash = digest(&manifest_text);
    let out = output_root(Path::new(&args[3]))?;
    if !archives.is_empty() || prepare_only {
        let preflight = prepare_reuse(
            &rows,
            &archives,
            &out,
            model,
            split,
            &input_hash,
            &manifest_hash,
        )?;
        let path = out.join(format!(
            "preflight-{}-{}.json",
            std::process::id(),
            stamp()?
        ));
        put(&path, &preflight)?;
        println!("{}", serde_json::to_string(&preflight)?);
        if prepare_only {
            return Ok(());
        }
    }
    ensure!(
        poll_only || !out.join("quota-stop.json").exists(),
        "HTTP 402 stop retained; explicit quota reconciliation required"
    );

    // Inspect every prior binding before creating a client or submitting anything.
    let mut states = BTreeMap::new();
    let mut canonical = BTreeMap::<String, String>::new();
    for row in &rows {
        let id = field(row, "id")?;
        let folder = out.join(id);
        if fs::symlink_metadata(&folder).is_ok() {
            ensure!(
                fs::symlink_metadata(&folder)?.file_type().is_dir(),
                "row directory must not be a symlink"
            );
        } else {
            fs::create_dir(&folder)?;
            File::open(&out)?.sync_all()?;
        }
        let mut state = BTreeMap::new();
        for name in [
            "intent.json",
            "submission.json",
            "result.json",
            "archive-blocked.json",
        ] {
            let path = folder.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                let receipt = read_receipt(&path)?;
                if name == "submission.json" {
                    submission_matches(&receipt, row, model)?;
                } else {
                    binding_matches(&receipt, row, model, split)?;
                }
                if name == "result.json" {
                    ensure!(
                        receipt["response"]["stage"] == "STAGE_SUCCESS",
                        "saved result is not successful"
                    );
                }
                // Retain only task IDs/flags after validating full receipts, so
                // raw response sizes do not accumulate across the population.
                state.insert(
                    name,
                    if name == "submission.json" {
                        json!({"task_id":receipt["task_id"]})
                    } else {
                        Value::Null
                    },
                );
            }
        }
        ensure!(
            !state.contains_key("submission.json") || state.contains_key("intent.json"),
            "submission without bound intent; manual reconciliation required"
        );
        let first = canonical
            .entry(field(row, "text_sha256")?.to_owned())
            .or_insert_with(|| id.to_owned());
        if first != id && !state.contains_key("intent.json") && !state.contains_key("result.json") {
            state.insert("duplicate_of", json!(first));
        }
        states.insert(id.to_owned(), state);
    }

    let needs_client = states.values().any(|state| {
        !state.contains_key("result.json")
            && !state.contains_key("archive-blocked.json")
            && !state.contains_key("duplicate_of")
            && (!state.contains_key("intent.json") || state.contains_key("submission.json"))
            && (!poll_only || state.contains_key("submission.json"))
    });
    let client = if needs_client {
        let client = if poll_only {
            JournalClient::existing_tasks(credential(Path::new(&args[4]))?, &out)?
        } else {
            JournalClient::new(credential(Path::new(&args[4]))?, &out)?
        };
        if !poll_only {
            let catalog = client.request("/models", None, &out, Duration::from_secs(60))?;
            ensure!(
                contains_selector(&catalog, model),
                "model selector not in account catalog"
            );
        }
        Some(client)
    } else {
        None
    };
    let run = Run {
        client: client.as_ref(),
        out: &out,
        model,
        split,
        input_hash: &input_hash,
        manifest_hash: &manifest_hash,
        started: Instant::now(),
        operations: AtomicUsize::new(0),
        poll_only,
    };
    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let mut results =
        std::thread::scope(|scope| -> Result<Vec<(usize, Value)>> {
            let handles = (0..WORKERS.min(rows.len()))
                .map(|_| {
                    scope.spawn(|| -> Result<Vec<(usize, Value)>> {
                        let mut results = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(row) = rows.get(index) else {
                                break;
                            };
                            let result = compare(row, &states[field(row, "id")?], &run)?;
                            results.push((index, result));
                            eprintln!(
                                "completed comparator item {} of {}",
                                completed.fetch_add(1, Ordering::Relaxed) + 1,
                                rows.len()
                            );
                        }
                        Ok(results)
                    })
                })
                .collect::<Vec<_>>();
            let mut results = Vec::new();
            for handle in handles {
                results.extend(handle.join().map_err(|_| {
                    anyhow::anyhow!("comparator worker panicked; receipts retained")
                })??);
            }
            Ok(results)
        })?;
    results.sort_by_key(|(index, _)| *index);
    let results = results
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    let operations = run.operations.load(Ordering::Relaxed);
    let summary = out.join(format!("summary-{}-{}.json", std::process::id(), stamp()?));
    put(
        &summary,
        &json!({"split":split,"model":model,"input_sha256":input_hash,
        "manifest_sha256":manifest_hash,"results":results,"operations":operations,
        "max_operations":MAX_OPERATIONS,"workers":WORKERS,"run_budget_seconds":RUN_BUDGET.as_secs(),
        "training_labels_from_detector":false,"test_opened":test,"poll_only":poll_only,
        "new_submissions_allowed":!poll_only,"submission_quota_stop_retained":out.join("quota-stop.json").exists(),
        "http_402_stop":run.client.is_some_and(|c|c.stopped.load(Ordering::SeqCst)),
        "quota_balance":null,"quota_status":"no_documented_balance_endpoint; bounded_batches_stop_HTTP_402_or_429"}),
    )?;
    println!("{}", summary.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_only_cannot_submit_or_open_unsubmitted_work() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("quota-stop.json"), b"{}").unwrap();
        let client = JournalClient::existing_tasks("synthetic-key".into(), temp.path()).unwrap();
        for (route, payload) in [
            ("/task", Some(json!({"text":"synthetic"}))),
            ("/models", None),
            ("/task/known", Some(json!({}))),
        ] {
            assert!(
                client
                    .request(route, payload, temp.path(), Duration::from_millis(1))
                    .is_err()
            );
        }
        assert!(temp.path().join("quota-stop.json").exists());
        let run = Run {
            client: None,
            out: temp.path(),
            model: "synthetic",
            split: "test",
            input_hash: "input",
            manifest_hash: "manifest",
            started: Instant::now(),
            operations: AtomicUsize::new(0),
            poll_only: true,
        };
        let row = json!({"id":"synthetic"});
        let result = compare(&row, &BTreeMap::new(), &run).unwrap();
        assert_eq!(result["status"], "not_submitted_poll_only");
        let state = BTreeMap::from([("intent.json", Value::Null)]);
        assert_eq!(
            compare(&row, &state, &run).unwrap()["status"],
            "uncertain_submission_do_not_repeat"
        );
        assert_eq!(run.operations.load(Ordering::Relaxed), 0);
        assert!(!temp.path().join("synthetic").exists());
    }

    #[test]
    fn offline_http_retains_raw_errors_and_latches_quota_stop() {
        use std::net::TcpListener;
        for (status, body) in [
            (200, "{\"stage\":\"STAGE_SUCCESS\"}"),
            (200, "not JSON"),
            (402, "balance exhausted"),
            (429, "rate limited"),
            (503, "temporarily unavailable"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            assert!(
                Command::new("git")
                    .args(["init", "--quiet"])
                    .arg(temp.path())
                    .status()
                    .unwrap()
                    .success()
            );
            fs::write(temp.path().join(".gitignore"), "data/\n").unwrap();
            let root = temp.path().join("data");
            fs::create_dir(&root).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let mut client = JournalClient::new("synthetic-key".into(), &root).unwrap();
            client.base = format!("http://{address}");
            client.http = reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap();
            let result = client.request("/fixture", None, &root, Duration::from_secs(5));
            server.join().unwrap();
            assert_eq!(result.is_ok(), status == 200 && body.starts_with('{'));
            let journals = fs::read_dir(&root)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.is_dir())
                .collect::<Vec<_>>();
            assert_eq!(journals.len(), 1);
            assert_eq!(
                fs::read(journals[0].join("body.bin")).unwrap(),
                body.as_bytes()
            );
            let outcome = read_receipt(&journals[0].join("outcome.json")).unwrap();
            assert_eq!(outcome["status"], status);
            assert_eq!(outcome["complete"], true);
            assert_eq!(outcome["body_sha256"], digest(body));
            let request = fs::read_to_string(journals[0].join("request.json")).unwrap();
            assert!(!request.contains("synthetic-key"));
            if status == 402 || status == 429 {
                assert!(
                    client
                        .request("/must-not-send", None, &root, Duration::from_secs(1))
                        .is_err()
                );
                assert_eq!(
                    fs::read_dir(&root)
                        .unwrap()
                        .filter(|e| e.as_ref().unwrap().path().is_dir())
                        .count(),
                    1
                );
                let resumed = JournalClient::new("synthetic-key".into(), &root).unwrap();
                assert!(resumed.stopped.load(Ordering::SeqCst));
            } else {
                assert!(!client.stopped.load(Ordering::SeqCst));
            }
        }
    }

    fn fixture() -> (Value, Value) {
        let text = "Synthetic example word. ".repeat(20);
        let row = json!({"id":"synthetic-1","text":text,"text_sha256":digest(&text),
            "source_group":"synthetic","frozen_split":"development",
            "source":{"dataset_source":{"collection":"synthetic-adapter-test"},
                "dataset_rights":{"external_evaluation":true,"license":"CC0-1.0"}},
            "rights":{"external_evaluation":true,"license":"CC0-1.0"}});
        let manifest = json!({"input_sha256":digest(&format!("{row}\n")),
            "frozen_split":"development","allowed_ids":["synthetic-1"]});
        (row, manifest)
    }

    #[test]
    fn exact_manifest_and_rights_required_before_requests() {
        let (mut row, mut manifest) = fixture();
        let input = format!("{row}\n");
        assert!(validate_input(&input, &manifest, "development").is_ok());
        assert!(validate_input(&(input.clone() + "\n"), &manifest, "development").is_err());
        assert!(validate_input(&input, &manifest, "test").is_err());
        row["rights"]["external_evaluation"] = json!(false);
        let denied = format!("{row}\n");
        manifest["input_sha256"] = json!(digest(&denied));
        assert!(validate_input(&denied, &manifest, "development").is_err());
        manifest["allowed_ids"] = json!(["different-id"]);
        assert!(validate_input(&denied, &manifest, "development").is_err());
    }

    #[test]
    fn reuse_requires_exact_identity_hash_and_model() {
        let (row, _) = fixture();
        let mut receipt = json!({"id":row["id"],"text_sha256":row["text_sha256"],
            "model":"model-a","source_group":row["source_group"],"split":"development"});
        assert!(binding_matches(&receipt, &row, "model-a", "development").is_ok());
        assert!(binding_matches(&receipt, &row, "model-b", "development").is_err());
        receipt["id"] = json!("other-id");
        assert!(binding_matches(&receipt, &row, "model-a", "development").is_err());
        let submission = json!({"model":"model-a","sha256":row["text_sha256"],
            "submitted_text":"different text","task_id":"task-1"});
        assert!(submission_matches(&submission, &row, "model-a").is_err());
    }

    #[test]
    fn final_test_reuse_is_offline_and_keeps_original_observation() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .arg(temp.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(temp.path().join(".gitignore"), "data/\n").unwrap();
        let archive = temp.path().join("data/development");
        let original_dir = archive.join("original");
        let output = temp.path().join("data/final-test");
        fs::create_dir_all(&original_dir).unwrap();
        fs::create_dir(&output).unwrap();
        let (mut row, _) = fixture();
        row["frozen_split"] = json!("test");
        row["id"] = json!("fresh-routing-id");
        let original = json!({"id":"original","source_group":"old-group","split":"development",
            "model":"model-a","text_sha256":row["text_sha256"],"task_id":"task-1",
            "response":{"stage":"STAGE_SUCCESS","version":"synthetic-version"}});
        let original_path = original_dir.join("result.json");
        put(&original_path, &original).unwrap();
        let before = fs::read(&original_path).unwrap();
        put(
            &original_dir.join("submission.json"),
            &json!({"model":"model-a",
            "sha256":row["text_sha256"],"submitted_text":row["text"],"task_id":"task-1"}),
        )
        .unwrap();
        let report = prepare_reuse(
            &[row],
            &[archive],
            &output,
            "model-a",
            "test",
            "input",
            "manifest",
        )
        .unwrap();
        assert_eq!(report["test_opened"], true);
        assert_eq!(report["network_calls"], 0);
        assert_eq!(report["new_exact_hash_reuses"], 1);
        assert_eq!(fs::read(original_path).unwrap(), before);
        let reused = read_receipt(&output.join("fresh-routing-id/result.json")).unwrap();
        assert_eq!(reused["split"], "test");
        assert_eq!(reused["observation_reuse"]["original_split"], "development");
        let old_test = temp.path().join("data/test");
        fs::create_dir(&old_test).unwrap();
        assert!(
            prepare_reuse(
                &[],
                &[old_test],
                &output,
                "model-a",
                "test",
                "input",
                "manifest"
            )
            .is_err()
        );
    }

    #[test]
    fn dataset_selects_only_requested_split_and_rejects_reserved_ids() {
        let (row, _) = fixture();
        let mut train = row.clone();
        train["id"] = json!("synthetic-train");
        train["source_group"] = json!("train-family");
        train["frozen_split"] = json!("train");
        let input = format!("{row}\n{train}\n");
        let mut manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1",
            "records_sha256":digest(&input),"non_test_ids":["synthetic-1","synthetic-train"],
            "test_ids":["reserved-test"],
            "split_assignments":{"synthetic":"development","train-family":"train"}});
        let selected = validate_input(&input, &manifest, "development").unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["id"], "synthetic-1");
        manifest["test_ids"] = json!(["synthetic-1"]);
        assert!(validate_input(&input, &manifest, "development").is_err());
    }

    #[test]
    fn rights_flag_alone_does_not_authorize_private_source() {
        let (mut row, _) = fixture();
        row["source"] = json!({"path":"private-correspondence"});
        assert!(public_source(&row).is_err());
        let rights =
            json!({"external_evaluation":true,"catalog_statement":"Public domain in the USA."});
        row["rights"] = rights.clone();
        row["source"] = json!({"source_manifest_sha256":"a".repeat(64),
            "record":{"pg_id":1,"source_url":"https://www.gutenberg.org/ebooks/1.txt.utf-8","rights":rights}});
        assert!(public_source(&row).is_ok());
        row["source"]["record"]["rights"]["external_evaluation"] = json!(false);
        assert!(public_source(&row).is_err());
    }

    #[test]
    fn assembled_author_and_controlled_generation_formats_are_accepted() {
        let (mut author, _) = fixture();
        let rights = json!({"commercial_training":true,"external_evaluation":true,
            "catalog_statement":"Public domain in the USA.","jurisdiction":"US"});
        author["id"] = json!("author-pg1-p20");
        author["rights"] = rights.clone();
        author["source"] = json!({"source_manifest_sha256":"a".repeat(64),
            "byte_span":[0,100],"record":{"pg_id":1,"split":"development",
                "source_url":"https://www.gutenberg.org/ebooks/1.txt.utf-8","rights":rights}});
        // a_controlled_generate clones the author's provenance and rights.
        let mut generated = author.clone();
        generated["id"] = json!("local-0123456789abcdef01234567");
        generated["parent_id"] = author["id"].clone();
        generated["text"] = json!("Different synthetic generated words. ".repeat(20));
        generated["text_sha256"] = json!(digest(generated["text"].as_str().unwrap()));
        let (mut public, _) = fixture();
        public["id"] = json!("generation:synthetic-source:0123456789abcdef");
        let input = format!("{author}\n{generated}\n{public}\n");
        let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1",
            "records_sha256":digest(&input),"input_sha256":digest(&input),
            "non_test_ids":[author["id"],generated["id"],public["id"]],"test_ids":[],
            "split_assignments":{"synthetic":"development"},
            "sources":[{"path":"records.jsonl","sha256":digest(&input)}]});
        assert_eq!(
            validate_input(&input, &manifest, "development")
                .unwrap()
                .len(),
            3
        );
        for id in ["../escape", "path/file", "path\\file", ".", ".."] {
            assert!(!safe_id(id));
        }
    }
}
