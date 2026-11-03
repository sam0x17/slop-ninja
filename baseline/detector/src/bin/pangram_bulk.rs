//! Budgeted Pangram bulk submission and resumable, exact-input collection.
#[path = "support/annotation_captures.rs"]
mod annotation_captures;
#[path = "support/b_context.rs"]
mod b_context;
#[path = "support/b_research.rs"]
mod b_research;
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{Evidence, read_records, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

const BASE: &str = "https://text.external-api.pangram.com";

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Freeze explicitly authorized document views for private B context diagnostics.
    PlanBContext {
        #[arg(long)]
        document_plan: PathBuf,
        #[arg(long, required = true)]
        view: Vec<String>,
        /// Record the user's existing authorization for these exact views.
        #[arg(long)]
        authorization_note: String,
        #[arg(long)]
        output_dir: PathBuf,
    },
    /// Freeze reviewed B research fixtures and their sources for external evaluation only.
    PlanB {
        #[arg(long, required = true)]
        selection: Vec<PathBuf>,
        #[arg(long)]
        output_dir: PathBuf,
    },
    /// Validate a prepared batch offline without reading credentials or spending.
    Validate {
        #[arg(long)]
        plan_dir: PathBuf,
        #[arg(long, default_value_t = 0)]
        batch: usize,
    },
    /// Create a cumulative spending ledger. Does not contact Pangram.
    Init {
        #[arg(long)]
        budget_dir: PathBuf,
        #[arg(long)]
        cap_cents: u64,
    },
    /// Reserve a bounded remote ledger allocation without copying private text locally.
    Allocate {
        #[arg(long)]
        budget_dir: PathBuf,
        #[arg(long)]
        reserve_cents: u64,
        /// Unique host and absolute path of the child ledger; never reuse elsewhere.
        #[arg(long)]
        destination: String,
    },
    /// A reservation is durable before POST; uncertain submissions never retry.
    Submit {
        #[arg(long)]
        plan_dir: PathBuf,
        #[arg(long, default_value_t = 0)]
        batch: usize,
        #[arg(long)]
        budget_dir: PathBuf,
        #[arg(long)]
        reserve_cents: u64,
    },
    /// Poll an existing job once, then archive and validate all terminal results.
    Collect {
        #[arg(long)]
        job_dir: PathBuf,
        /// Record whitespace-only echo changes separately; all other changes fail.
        #[arg(long)]
        allow_whitespace_echo: bool,
        /// Separately record removal of source soft hyphens (U+00AD) in the echo.
        #[arg(long)]
        allow_soft_hyphen_echo: bool,
    },
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn save(path: &Path, value: &Value) -> Result<()> {
    write_new(path, &serde_json::to_vec_pretty(value)?)
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().context(format!("Missing {key}"))
}

struct Api {
    client: Client,
    key: String,
}
impl Api {
    fn new() -> Result<Self> {
        let key = std::env::var("PANGRAM_API_KEY").context("PANGRAM_API_KEY is required")?;
        ensure!(!key.is_empty(), "Empty API key");
        Ok(Self {
            key,
            client: Client::builder()
                .timeout(Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
        })
    }
    fn request(&self, route: &str, payload: Option<&[u8]>, archive: &Path) -> Result<Value> {
        let url = format!("{BASE}{route}");
        let request = match payload {
            Some(bytes) => self.client.post(&url).body(bytes.to_vec()),
            None => self.client.get(&url),
        };
        let response = request
            .header("x-api-key", &self.key)
            .header("Content-Type", "application/json")
            .send()
            .context("Pangram transport failure; do not resubmit an uncertain POST")?;
        let status = response.status();
        let bytes = response
            .bytes()
            .context("Pangram response read failed; do not resubmit an uncertain POST")?;
        write_new(&archive.with_extension("body"), &bytes)?;
        save(
            &archive.with_extension("http.json"),
            &json!({"url":url,"method":if payload.is_some(){"POST"}else{"GET"},
            "received_at":chrono::Utc::now().to_rfc3339(),"status":status.as_u16(),"body_sha256":sha256(&bytes)}),
        )?;
        ensure!(
            status.is_success(),
            "Pangram HTTP {}; response retained",
            status.as_u16()
        );
        Ok(serde_json::from_slice(&bytes)?)
    }
}

fn budget_used(directory: &Path) -> Result<u64> {
    let mut used = 0u64;
    for entry in fs::read_dir(directory.join("jobs"))? {
        let reservation = entry?.path().join("reservation.json");
        ensure!(
            reservation.exists(),
            "Incomplete budget job; inspect it before another submission"
        );
        used = used
            .checked_add(
                read(&reservation)?["reserved_usd_cents"]
                    .as_u64()
                    .context("Invalid reservation")?,
            )
            .context("Budget overflow")?;
    }
    Ok(used)
}

fn allocate(budget_dir: &Path, reserve_cents: u64, destination: &str) -> Result<Value> {
    ensure!(
        reserve_cents > 0 && !destination.trim().is_empty(),
        "Empty allocation"
    );
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(budget_dir.join("lock"))?;
    lock.try_lock()
        .context("Another budget operation is active")?;
    let authorization_bytes = fs::read(budget_dir.join("authorization.json"))?;
    let authorization: Value = serde_json::from_slice(&authorization_bytes)?;
    let cap = authorization["cap_usd_cents"]
        .as_u64()
        .context("Missing cap")?;
    let used = budget_used(budget_dir)?;
    ensure!(
        used.checked_add(reserve_cents).is_some_and(|v| v <= cap),
        "Cumulative budget exhausted"
    );
    let job_id = format!("allocation-{}", sha256(destination));
    let job = budget_dir.join("jobs").join(&job_id);
    ensure!(
        !job.exists(),
        "Destination already reserved; use its existing child ledger"
    );
    fs::create_dir(&job)?;
    let reservation = json!({"schema":"slop_ninja_pangram_budget_reservation_v1",
        "kind":"remote_ledger_allocation", "reserved_usd_cents":reserve_cents,
        "destination":destination, "child_cap_usd_cents":reserve_cents,
        "job_id":job_id, "reserved_at":chrono::Utc::now().to_rfc3339(),
        "authorization_sha256":sha256(&authorization_bytes),
        "instruction":"Initialize this destination once with the child cap. Retain the full parent reservation until billing is reconciled. This is an allocation, not a provider charge."});
    save(&job.join("reservation.json"), &reservation)?;
    File::open(&job)?.sync_all()?;
    File::open(budget_dir.join("jobs"))?.sync_all()?;
    Ok(
        json!({"status":"allocated", "job_dir":job, "reservation":reservation,
        "cumulative_reserved_usd_cents":used+reserve_cents, "cap_usd_cents":cap,
        "network_calls":0}),
    )
}

fn validate_payload(plan_dir: &Path, batch: usize) -> Result<(Value, Vec<u8>, u64)> {
    let plan = read(&plan_dir.join("plan.json"))?;
    if plan["schema"] == b_context::SCHEMA {
        return b_context::validate(plan_dir, batch);
    }
    if plan["schema"] == b_research::SCHEMA {
        return b_research::validate(plan_dir, batch);
    }
    ensure!(
        (plan["schema"] == "slop_ninja_pangram_annotation_plan_v1"
            || plan["schema"] == annotation_captures::PLAN_SCHEMA)
            && plan["model_selector"] == "pangram-4",
        "Unsupported plan"
    );
    let hosted = if plan["schema"] == annotation_captures::PLAN_SCHEMA {
        ensure!(
            plan["commercial_training_admitted"] == false
                && plan["source_kind"] == "hosted_cli_capture",
            "Hosted evaluation cannot imply training admission"
        );
        annotation_captures::load(
            &plan_dir.join("capture-review"),
            field(&plan, "capture_audit_sha256")?,
        )?
    } else {
        Vec::new()
    };
    let hosted: BTreeMap<_, _> = hosted
        .iter()
        .map(|r| (r.reference["record_id"].as_str().unwrap(), r))
        .collect();
    if plan["schema"] == annotation_captures::PLAN_SCHEMA {
        ensure!(
            plan["repeats"] == 1
                && plan["repeat_start"] == 0
                && plan["record_count"] == hosted.len(),
            "Hosted plan must retain one observation per audited capture"
        );
        let mut covered = BTreeSet::new();
        let mut text_hashes = BTreeSet::new();
        let all_members = plan["members"]
            .as_array()
            .context("Missing hosted members")?;
        for member in all_members {
            let hash = field(member, "text_sha256")?;
            ensure!(
                text_hashes.insert(hash)
                    && member["repeat_index"] == 0
                    && member["request_id"] == format!("{hash}:0"),
                "Duplicate or repeated hosted text"
            );
            let references = member["records"]
                .as_array()
                .context("Missing hosted provenance")?;
            ensure!(!references.is_empty(), "Empty hosted member");
            for reference in references {
                let id = field(reference, "record_id")?;
                let record = hosted.get(id).context("Unknown hosted member")?;
                ensure!(
                    covered.insert(id)
                        && record.reference == *reference
                        && sha256(&record.text) == hash,
                    "Hosted coverage or provenance differs"
                );
            }
        }
        ensure!(
            covered.len() == hosted.len()
                && plan["unique_texts"] == text_hashes.len()
                && plan["request_items"] == all_members.len(),
            "Hosted plan omitted captures"
        );
    }
    let records = if plan["schema"] == "slop_ninja_pangram_annotation_plan_v1" {
        let source = plan_dir.join("source-records.jsonl");
        ensure!(
            sha256(fs::read(&source)?) == field(&plan, "input_sha256")?,
            "Corpus hash mismatch"
        );
        read_records(&source)?
    } else {
        Vec::new()
    };
    let records: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let path = format!("requests/batch-{batch:04}.json");
    let payload = fs::read(plan_dir.join(&path))?;
    ensure!(
        plan["request_files"][batch]["path"] == path
            && plan["request_files"][batch]["sha256"] == sha256(&payload),
        "Request binding mismatch"
    );
    let request: Value = serde_json::from_slice(&payload)?;
    ensure!(request["model"] == "pangram-4", "Wrong selector");
    let items = request["items"].as_array().context("Missing items")?;
    let members: Vec<_> = plan["members"]
        .as_array()
        .context("Missing members")?
        .iter()
        .filter(|m| m["batch_index"].as_u64() == Some(batch as u64))
        .collect();
    ensure!(
        members.len() == items.len() && !items.is_empty(),
        "Coverage mismatch"
    );
    let mut ids = BTreeSet::new();
    let mut units = 0u64;
    for (index, (item, member)) in items.iter().zip(&members).enumerate() {
        let id = field(item, "id")?;
        let text = field(item, "text")?;
        ensure!(ids.insert(id), "Duplicate request ID");
        ensure!(
            member["item_index"] == index
                && member["request_id"] == id
                && member["text_sha256"] == sha256(text),
            "Item mismatch"
        );
        let provenance = member["records"].as_array().context("Missing provenance")?;
        ensure!(!provenance.is_empty(), "Empty provenance");
        for reference in provenance {
            if plan["schema"] == annotation_captures::PLAN_SCHEMA {
                let record = hosted
                    .get(field(reference, "record_id")?)
                    .context("Unknown hosted capture")?;
                ensure!(
                    record.text == text && record.reference == *reference,
                    "Hosted text or provenance differs"
                );
                continue;
            }
            let record = records
                .get(field(reference, "record_id")?)
                .context("Unknown origin record")?;
            ensure!(
                record.text == text
                    && record.rights.external_evaluation
                    && record.evidence != Evidence::SyntheticFixture,
                "Text or external rights mismatch"
            );
            ensure!(
                reference["origin"] == serde_json::to_value(record.origin)?
                    && reference["split"] == serde_json::to_value(record.split)?
                    && reference["evidence"] == serde_json::to_value(record.evidence)?
                    && reference["parent_id"] == serde_json::to_value(&record.parent_id)?
                    && reference["source_group"] == record.source_group,
                "Origin metadata mismatch"
            );
            if let Some(attribution) = reference.get("generator_attribution") {
                ensure!(
                    *attribution == slop_ninja_detector::attribution::describe(record, &records)?,
                    "Generator attribution mismatch"
                );
            }
        }
        let words = text.split_whitespace().count();
        ensure!(words >= 50, "Input too short");
        units += words.div_ceil(100) as u64;
    }
    ensure!(units <= 1_000, "Batch exceeds provider unit limit");
    Ok((plan, payload, units))
}

fn submit(plan_dir: &Path, batch: usize, budget_dir: &Path, reserve_cents: u64) -> Result<Value> {
    let (plan, payload, units) = validate_payload(plan_dir, batch)?;
    // Reserve at least realtime pricing, leaving headroom over the bulk estimate.
    ensure!(
        reserve_cents >= units * 5,
        "Reservation must cover realtime estimate as headroom"
    );
    let api = Api::new()?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(budget_dir.join("lock"))?;
    lock.try_lock()
        .context("Another budget operation is active")?;
    let authorization = read(&budget_dir.join("authorization.json"))?;
    let cap = authorization["cap_usd_cents"]
        .as_u64()
        .context("Missing cap")?;
    let used = budget_used(budget_dir)?;
    for entry in fs::read_dir(budget_dir.join("jobs"))? {
        let previous = entry?.path().join("request.json");
        ensure!(
            !previous.exists() || fs::read(previous)? != payload,
            "This exact request already has a reservation; collect that job instead"
        );
    }
    ensure!(
        used.checked_add(reserve_cents).is_some_and(|v| v <= cap),
        "Cumulative budget exhausted"
    );
    let job_id = format!("{}-{batch}", sha256(fs::read(plan_dir.join("plan.json"))?));
    let job = budget_dir.join("jobs").join(&job_id);
    ensure!(
        !job.exists(),
        "Existing or uncertain submission; collect its receipt, never resubmit"
    );
    fs::create_dir(&job)?;
    save(
        &job.join("reservation.json"),
        &json!({"schema":"slop_ninja_pangram_budget_reservation_v1",
        "reserved_usd_cents":reserve_cents,"estimated_bulk_usd_cents":units*4,"estimated_units":units,
        "job_id":job_id,"reserved_at":chrono::Utc::now().to_rfc3339(),"authorization_sha256":sha256(fs::read(budget_dir.join("authorization.json"))?)}),
    )?;
    write_new(&job.join("request.json"), &payload)?;
    save(&job.join("plan.json"), &plan)?;
    save(
        &job.join("binding.json"),
        &json!({"batch_index":batch,"payload_sha256":sha256(&payload),
        "plan_sha256":sha256(fs::read(job.join("plan.json"))?),"runner_sha256":sha256(fs::read(std::env::current_exe()?)?)}),
    )?;
    File::open(&job)?.sync_all()?;
    File::open(budget_dir.join("jobs"))?.sync_all()?;
    let models = api.request("/models", None, &job.join("models"))?;
    ensure!(
        models["models"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x == "pangram-4")),
        "No Pangram 4 entitlement"
    );
    save(
        &job.join("submission-intent.json"),
        &json!({"at":chrono::Utc::now().to_rfc3339(),"payload_sha256":sha256(&payload)}),
    )?;
    File::open(&job)?.sync_all()?;
    let receipt = api.request("/bulk", Some(&payload), &job.join("submission"))?;
    let bulk_id = field(&receipt, "bulk_id")?;
    ensure!(
        !bulk_id.is_empty(),
        "Receipt omitted bulk ID; do not resubmit"
    );
    save(&job.join("receipt.json"), &receipt)?;
    Ok(json!({"status":"submitted","job_dir":job,"bulk_id":bulk_id,
        "estimated_bulk_usd_cents":units*4,"cumulative_reserved_usd_cents":used+reserve_cents,"cap_usd_cents":cap}))
}

fn validate_result(
    item: &Value,
    expected: &Value,
    task_id: &str,
    allow_whitespace: bool,
    allow_soft_hyphen: bool,
) -> Result<&'static str> {
    ensure!(
        item["id"] == expected["id"] && item["task_id"] == task_id,
        "Result identity mismatch"
    );
    ensure!(
        item["stage"] == "STAGE_SUCCESS",
        "Unexpected non-success result"
    );
    let result = &item["result"];
    let returned = field(result, "text")?;
    let submitted = field(expected, "text")?;
    let echo_match = if returned == submitted {
        "exact"
    } else if allow_whitespace && returned.split_whitespace().eq(submitted.split_whitespace()) {
        "whitespace_only"
    } else {
        let without_soft_hyphens = submitted.replace('\u{00ad}', "");
        ensure!(
            allow_soft_hyphen
                && submitted.contains('\u{00ad}')
                && (returned == without_soft_hyphens
                    || (allow_whitespace
                        && returned
                            .split_whitespace()
                            .eq(without_soft_hyphens.split_whitespace()))),
            "Provider returned different input text"
        );
        "source_soft_hyphens_removed"
    };
    ensure!(
        !field(result, "version")?.is_empty(),
        "Missing provider version"
    );
    let mut sum = 0.0;
    for name in ["fraction_human", "fraction_ai", "fraction_ai_assisted"] {
        let value = result[name].as_f64().context("Missing fraction")?;
        ensure!(
            value.is_finite() && (0.0..=1.0).contains(&value),
            "Invalid content fraction"
        );
        sum += value;
    }
    ensure!(
        (sum - 1.0).abs() < 1e-5,
        "Content fractions do not sum to one"
    );
    Ok(echo_match)
}

fn collect(job: &Path, allow_whitespace: bool, allow_soft_hyphen: bool) -> Result<Value> {
    let receipt = read(&job.join("receipt.json"))?;
    let bulk_id = field(&receipt, "bulk_id")?;
    ensure!(
        !bulk_id.is_empty()
            && bulk_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
        "Invalid bulk ID"
    );
    let binding = read(&job.join("binding.json"))?;
    ensure!(
        binding["payload_sha256"] == sha256(fs::read(job.join("request.json"))?)
            && binding["plan_sha256"] == sha256(fs::read(job.join("plan.json"))?),
        "Job input changed"
    );
    let request = read(&job.join("request.json"))?;
    let items = request["items"].as_array().context("Missing items")?;
    let api = Api::new()?;
    let attempt = job.join(format!(
        "collection-{}",
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .context("Invalid timestamp")?
    ));
    fs::create_dir(&attempt)?;
    let status = api.request(&format!("/bulk/{bulk_id}"), None, &attempt.join("status"))?;
    ensure!(
        status["bulk_id"] == bulk_id && status["total_items"] == items.len(),
        "Bulk status identity mismatch"
    );
    if !["succeeded", "failed", "partial"].contains(&field(&status, "status")?) {
        return Ok(
            json!({"status":status["status"],"bulk_id":bulk_id,"provider":status,"resume_job_dir":job}),
        );
    }
    let mut accepted = BTreeMap::new();
    let mut seen_tasks = BTreeSet::new();
    let mut initial_failed = BTreeMap::new();
    for slot in receipt["accepted_items"]
        .as_array()
        .context("Missing acceptance roster")?
    {
        let index = slot["index"].as_u64().context("Missing accepted index")? as usize;
        ensure!(
            index < items.len() && slot["id"] == items[index]["id"],
            "Acceptance mismatch"
        );
        let task = field(slot, "task_id")?;
        ensure!(
            seen_tasks.insert(task) && accepted.insert(index, task).is_none(),
            "Duplicate accepted task"
        );
    }
    for slot in receipt["failed_items"]
        .as_array()
        .context("Missing failed roster")?
    {
        let index = slot["index"].as_u64().context("Missing failed index")? as usize;
        ensure!(
            index < items.len()
                && slot["id"] == items[index]["id"]
                && !accepted.contains_key(&index)
                && initial_failed.insert(index, slot).is_none(),
            "Failure roster mismatch"
        );
    }
    ensure!(
        accepted.len() + initial_failed.len() == items.len(),
        "Receipt omitted inputs"
    );
    let mut observations = BTreeMap::<usize, Value>::new();
    for offset in (0..items.len()).step_by(100) {
        let page = api.request(
            &format!("/bulk/{bulk_id}/results?offset={offset}&limit=100"),
            None,
            &attempt.join(format!("page-{offset}")),
        )?;
        ensure!(
            page["bulk_id"] == bulk_id
                && page["total_items"] == items.len()
                && page["offset"] == offset,
            "Page identity mismatch"
        );
        for key in ["items", "failed_items"] {
            let empty = Vec::new();
            let rows = match page.get(key) {
                Some(v) => v.as_array().context("Invalid results array")?,
                None => &empty,
            };
            for item in rows {
                let index = item["index"].as_u64().context("Missing result index")? as usize;
                ensure!(
                    (offset..items.len().min(offset + 100)).contains(&index)
                        && item["id"] == items[index]["id"],
                    "Page item mismatch"
                );
                ensure!(!observations.contains_key(&index), "Duplicate result index");
                if key == "items" {
                    validate_result(
                        item,
                        &items[index],
                        accepted.get(&index).context("Unaccepted success")?,
                        allow_whitespace,
                        allow_soft_hyphen,
                    )?;
                } else {
                    ensure!(item["stage"] == "STAGE_FAILED", "Unresolved failed item");
                    if let Some(task) = accepted.get(&index) {
                        ensure!(item["task_id"] == *task, "Failed task identity mismatch");
                    } else {
                        ensure!(
                            initial_failed.contains_key(&index) && item["task_id"].is_null(),
                            "Unknown failed input"
                        );
                    }
                }
                observations.insert(index, item.clone());
            }
        }
    }
    ensure!(
        observations.len() == items.len(),
        "Terminal results do not cover every input"
    );
    let plan = read(&job.join("plan.json"))?;
    let batch = binding["batch_index"]
        .as_u64()
        .context("Missing batch binding")?;
    let members: BTreeMap<_, _> = plan["members"]
        .as_array()
        .context("Missing members")?
        .iter()
        .filter(|m| m["batch_index"].as_u64() == Some(batch))
        .map(|m| (m["item_index"].as_u64().unwrap() as usize, m))
        .collect();
    let mut annotations = Vec::new();
    let mut versions = BTreeSet::new();
    let mut echo_counts = BTreeMap::<&str, usize>::new();
    let mut succeeded = 0;
    for (index, item) in &observations {
        let mut echo_match = "failed";
        if item["stage"] == "STAGE_SUCCESS" {
            succeeded += 1;
            versions.insert(field(&item["result"], "version")?);
            echo_match = validate_result(
                item,
                &items[*index],
                accepted[index],
                allow_whitespace,
                allow_soft_hyphen,
            )?;
        }
        *echo_counts.entry(echo_match).or_default() += 1;
        let annotation_schema = if plan["schema"] == b_context::SCHEMA {
            "slop_ninja_b_private_context_pangram_annotation_v1"
        } else if plan["schema"] == b_research::SCHEMA {
            "slop_ninja_b_pangram_annotation_v1"
        } else {
            "slop_ninja_pangram_corpus_annotation_v1"
        };
        let annotation = json!({"schema":annotation_schema,"bulk_id":bulk_id,
            "member":members.get(index).context("Missing source member")?,"model_selector":"pangram-4",
            "echo_match":echo_match,"submitted_text_sha256":sha256(field(&items[*index],"text")?),
            "returned_text_sha256":item["result"]["text"].as_str().map(sha256),
            "collected_at":chrono::Utc::now().to_rfc3339(),"observation":item});
        serde_json::to_writer(&mut annotations, &annotation)?;
        annotations.push(b'\n');
    }
    ensure!(
        status["succeeded"] == succeeded && status["failed"] == items.len() - succeeded,
        "Final counters disagree with coverage"
    );
    write_new(&attempt.join("annotations.jsonl"), &annotations)?;
    let summary = json!({"schema":"slop_ninja_pangram_corpus_collection_v1","status":"complete",
        "bulk_id":bulk_id,"items":items.len(),"succeeded":succeeded,"failed":items.len()-succeeded,
        "returned_versions":versions,"annotations_sha256":sha256(&annotations),"collection_dir":attempt,
        "echo_counts":echo_counts,"whitespace_echo_allowed":allow_whitespace,
        "source_soft_hyphen_removal_allowed":allow_soft_hyphen,
        "billing":"unverified; retain the full reservation until account usage is reconciled"});
    save(&attempt.join("summary.json"), &summary)?;
    Ok(summary)
}

fn main() -> Result<()> {
    let result = match Args::parse().command {
        Command::PlanBContext {
            document_plan,
            view,
            authorization_note,
            output_dir,
        } => b_context::prepare(&document_plan, &view, &authorization_note, &output_dir)?,
        Command::PlanB {
            selection,
            output_dir,
        } => b_research::prepare(&selection, &output_dir)?,
        Command::Validate { plan_dir, batch } => {
            let (plan, payload, units) = validate_payload(&plan_dir, batch)?;
            json!({"status":"validated_offline","schema":plan["schema"],"batch":batch,
                "payload_sha256":sha256(&payload),"estimated_units":units,
                "estimated_bulk_usd_cents":units*4,"reserve_at_least_usd_cents":units*5})
        }
        Command::Init {
            budget_dir,
            cap_cents,
        } => {
            ensure!(
                cap_cents > 0 && !budget_dir.exists(),
                "Use a new budget directory and a positive cap"
            );
            fs::create_dir_all(budget_dir.join("jobs"))?;
            write_new(&budget_dir.join("lock"), b"")?;
            let value = json!({"schema":"slop_ninja_pangram_budget_v1","cap_usd_cents":cap_cents,
                "authorization":"Caller-supplied cumulative experimental spending cap; not per run",
                "reserved_not_released_without_billing_evidence":true});
            save(&budget_dir.join("authorization.json"), &value)?;
            value
        }
        Command::Submit {
            plan_dir,
            batch,
            budget_dir,
            reserve_cents,
        } => submit(&plan_dir, batch, &budget_dir, reserve_cents)?,
        Command::Allocate {
            budget_dir,
            reserve_cents,
            destination,
        } => allocate(&budget_dir, reserve_cents, &destination)?,
        Command::Collect {
            job_dir,
            allow_whitespace_echo,
            allow_soft_hyphen_echo,
        } => collect(&job_dir, allow_whitespace_echo, allow_soft_hyphen_echo)?,
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_allocations_share_the_cap_and_cannot_be_reserved_twice() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("jobs")).unwrap();
        write_new(&temp.path().join("lock"), b"").unwrap();
        save(
            &temp.path().join("authorization.json"),
            &json!({"cap_usd_cents":150}),
        )
        .unwrap();
        allocate(temp.path(), 100, "host:/private/ledger-one").unwrap();
        assert_eq!(budget_used(temp.path()).unwrap(), 100);
        assert!(allocate(temp.path(), 25, "host:/private/ledger-one").is_err());
        assert!(allocate(temp.path(), 51, "host:/private/ledger-two").is_err());
        assert!(allocate(temp.path(), 0, "host:/private/ledger-two").is_err());
        allocate(temp.path(), 50, "host:/private/ledger-two").unwrap();
        assert_eq!(budget_used(temp.path()).unwrap(), 150);
    }
    #[test]
    fn results_require_exact_text_task_and_valid_fractions() {
        let expected = json!({"id":"row","text":"An invented fixture."});
        let good = json!({"id":"row","task_id":"task","stage":"STAGE_SUCCESS","result":{
            "text":"An invented fixture.","version":"test","fraction_human":0.2,"fraction_ai":0.5,"fraction_ai_assisted":0.3}});
        validate_result(&good, &expected, "task", false, false).unwrap();
        let mut whitespace = good.clone();
        whitespace["result"]["text"] = json!("An\n invented fixture.");
        assert!(validate_result(&whitespace, &expected, "task", false, false).is_err());
        assert_eq!(
            validate_result(&whitespace, &expected, "task", true, false).unwrap(),
            "whitespace_only"
        );
        let mut changed = good.clone();
        changed["result"]["text"] = json!("Different text");
        assert!(validate_result(&changed, &expected, "task", true, false).is_err());
        assert!(validate_result(&good, &expected, "other-task", false, false).is_err());
        let mut changed = good;
        changed["result"]["fraction_ai"] = json!(0.7);
        assert!(validate_result(&changed, &expected, "task", false, false).is_err());
    }

    #[test]
    fn soft_hyphen_opt_in_does_not_allow_other_text_changes() {
        let expected = json!({"id":"row","text":"An in\u{00ad}vented fixture."});
        let mut item = json!({"id":"row","task_id":"task","stage":"STAGE_SUCCESS","result":{
            "text":"An invented fixture.","version":"test","fraction_human":0.2,"fraction_ai":0.5,"fraction_ai_assisted":0.3}});
        assert!(validate_result(&item, &expected, "task", true, false).is_err());
        assert_eq!(
            validate_result(&item, &expected, "task", false, true).unwrap(),
            "source_soft_hyphens_removed"
        );
        item["result"]["text"] = json!("An\n invented fixture.");
        assert!(validate_result(&item, &expected, "task", false, true).is_err());
        validate_result(&item, &expected, "task", true, true).unwrap();
        item["result"]["text"] = json!("An invented example.");
        assert!(validate_result(&item, &expected, "task", true, true).is_err());
        item["result"]["text"] = json!("An in\u{200b}vented fixture.");
        assert!(validate_result(&item, &expected, "task", true, true).is_err());
    }
    #[test]
    fn uncertain_jobs_still_consume_budget() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("jobs")).unwrap();
        let job = temp.path().join("jobs/uncertain");
        fs::create_dir(&job).unwrap();
        assert!(budget_used(temp.path()).is_err());
        save(
            &job.join("reservation.json"),
            &json!({"reserved_usd_cents":1000}),
        )
        .unwrap();
        assert_eq!(budget_used(temp.path()).unwrap(), 1000);
    }
}
