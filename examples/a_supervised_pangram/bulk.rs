//! True bulk API; shared rights checks, immutable intents, raw HTTP journals.
//! `bulk prepare INPUT MANIFEST SPLIT FIXTURES NEW_JOB RESTORATION.json`
//! `bulk submit|collect|watch JOB KEYFILE`
use super::*;

pub(super) fn idempotency_key(request: &Value) -> Result<String> {
    Ok(format!(
        "slop-ninja-bulk-v1-{}",
        digest(&serde_json::to_string(request)?)
    ))
}

fn hash_file(path: &Path) -> Result<String> {
    Ok(digest(&read_text(path, INPUT_BYTES)?))
}
fn binding(row: &Value, plan: &Value) -> Value {
    json!({"id":row["id"],"model":plan["model"],"text_sha256":row["text_sha256"],
        "source_group":row["source_group"],"split":plan["split"],"input_sha256":plan["input_sha256"],
        "manifest_sha256":plan["manifest_sha256"],"source":row["source"],"rights":row["rights"]})
}
fn units(rows: &[Value]) -> Result<usize> {
    let mut n = 0usize;
    let mut hashes = BTreeSet::new();
    for r in rows {
        let text = field(r, "text")?;
        let w = text.split_whitespace().count();
        ensure!(
            w >= 50 && hashes.insert(digest(text)),
            "short or duplicate bulk text"
        );
        n += w.div_ceil(1000);
    }
    ensure!(
        !rows.is_empty() && n <= 1000,
        "bulk limit is 1,000 started 1,000-word units"
    );
    Ok(n)
}
fn rejected_response(
    request: &Value,
    response: &Value,
    outcome: &Value,
    body: &[u8],
    row: &Value,
) -> Result<()> {
    ensure!(
        request["method"] == "POST"
            && request["url"] == "https://text.external-api.pangram.com/task"
            && request["payload"]["text"] == row["text"]
            && request["payload"]["model"] == "pangram-4"
            && response["status"] == 402
            && outcome["status"] == 402
            && outcome["complete"] == true
            && outcome["bytes"] == body.len()
            && outcome["body_sha256"] == hex::encode(sha2::Sha256::digest(body)),
        "not a complete bound rejection"
    );
    let b: Value = serde_json::from_slice(body)?;
    ensure!(
        b == json!({"detail":"Insufficient credits"}),
        "not an explicit insufficient-credit rejection"
    );
    Ok(())
}
fn rejected_402(folder: &Path, row: &Value) -> Result<Value> {
    ensure!(
        !folder.join("submission.json").exists() && !folder.join("archive-blocked.json").exists(),
        "accepted or archived obligation exists"
    );
    let mut journals = Vec::new();
    for e in fs::read_dir(folder)? {
        let e = e?;
        if e.file_name().to_string_lossy().starts_with("http-") {
            journals.push(e.path());
        }
    }
    ensure!(
        journals.len() == 1,
        "ambiguous request history; reconcile separately"
    );
    let j = &journals[0];
    let request = read_receipt(&j.join("request.json"))?;
    let response = read_receipt(&j.join("response.json"))?;
    let outcome = read_receipt(&j.join("outcome.json"))?;
    let body = fs::read(j.join("body.bin"))?;
    rejected_response(&request, &response, &outcome, &body, row)?;
    Ok(
        json!({"kind":"complete_HTTP_402_rejected_before_task_creation","journal":j,
        "request_sha256":hash_file(&j.join("request.json"))?,"response_sha256":hash_file(&j.join("response.json"))?,
        "outcome_sha256":hash_file(&j.join("outcome.json"))?,"body_sha256":outcome["body_sha256"],"original_intent_sha256":hash_file(&folder.join("intent.json"))?}),
    )
}
fn prepare(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 6,
        "bulk prepare INPUT MANIFEST SPLIT FIXTURES NEW_JOB RESTORATION.json"
    );
    let split = &a[2];
    ensure!(
        matches!(split.as_str(), "test" | "development" | "calibration"),
        "evaluation split required"
    );
    let manifest_text = read_text(Path::new(&a[1]), MANIFEST_BYTES)?;
    let manifest: Value = serde_json::from_str(&manifest_text)?;
    manifest_ids(&manifest, split)?;
    let input = read_text(Path::new(&a[0]), INPUT_BYTES)?;
    let rows = validate_input(&input, &manifest, split)?;
    let fixtures = output_root(Path::new(&a[3]))?;
    let restore = read_receipt(Path::new(&a[5]))?;
    ensure!(
        restore["user_confirmed_credit_refill"] == true && restore["allow_new_submissions"] == true,
        "explicit replenishment reconciliation required"
    );
    if fixtures.join("quota-stop.json").exists() {
        ensure!(
            restore["prior_quota_stop_sha256"] == hash_file(&fixtures.join("quota-stop.json"))?,
            "old quota stop differs; preserve it"
        );
    }
    ensure!(
        !Path::new(&a[4]).exists(),
        "new bulk job required; resume existing jobs with submit/collect"
    );
    let job = output_root(Path::new(&a[4]))?;
    let mut pending = Vec::new();
    let mut reused = Vec::new();
    let mut rejections = BTreeMap::new();
    let mut blocked = Vec::new();
    for row in &rows {
        let id = field(row, "id")?;
        let folder = fixtures.join(id);
        if folder.join("result.json").exists() {
            let receipt = read_receipt(&folder.join("result.json"))?;
            binding_matches(&receipt, row, "pangram-4", split)?;
            ensure!(
                receipt["response"]["stage"] == "STAGE_SUCCESS",
                "old result not successful"
            );
            reused.push(json!({"id":id,"result_sha256":hash_file(&folder.join("result.json"))?}));
            continue;
        }
        if folder.join("intent.json").exists() {
            binding_matches(
                &read_receipt(&folder.join("intent.json"))?,
                row,
                "pangram-4",
                split,
            )?;
            match rejected_402(&folder, row) {
                Ok(e) => {
                    rejections.insert(id.to_owned(), e);
                }
                Err(e) => {
                    blocked
                        .push(json!({"id":id,"reason":e.to_string(),"automatic_resubmit":false}));
                    continue;
                }
            }
        } else if folder.join("submission.json").exists()
            || folder.join("archive-blocked.json").exists()
        {
            blocked
                .push(json!({"id":id,"reason":"existing obligation","automatic_resubmit":false}));
            continue;
        }
        pending.push(row.clone());
    }
    let count = units(&pending)?;
    let request = json!({"model":"pangram-4","items":pending.iter().map(|r|json!({"id":r["id"],"text":r["text"]})).collect::<Vec<_>>()});
    put(&job.join("request.json"), &request)?;
    let plan = json!({"schema":"slop_ninja_supervised_bulk_plan_v1","model":"pangram-4","split":split,
        "input":Path::new(&a[0]).canonicalize()?,"manifest":Path::new(&a[1]).canonicalize()?,"input_sha256":digest(&input),"manifest_sha256":digest(&manifest_text),
        "fixtures":fixtures,"job":job,"request_sha256":hash_file(&job.join("request.json"))?,"restoration":restore,"restoration_path":Path::new(&a[5]).canonicalize()?,"restoration_sha256":hash_file(Path::new(&a[5]))?,
        "requested_population":rows.len(),"reused":reused,"blocked":blocked,"confirmed_rejections":rejections,"rows":pending,
        "prepared":pending.len(),"estimated_billable_units":count,"billing_unit":"started 1000-word block per valid item; an estimate",
        "retention":"archive immediately; provider bulk results expire 48h after terminal completion","new_model_fitting":false,"test_reselection":false});
    put(&job.join("plan.json"), &plan)?;
    File::options()
        .create_new(true)
        .write(true)
        .open(job.join("lock"))?
        .sync_all()?;
    put(
        &job.join("preflight.json"),
        &json!({"requested":rows.len(),"prepared":pending.len(),"reused":reused.len(),"blocked":blocked.len(),"confirmed_402_rejections":rejections.len(),"estimated_units":count,"network_calls":0}),
    )?;
    println!("{}", job.join("preflight.json").display());
    Ok(())
}
fn checked_job(job: &Path) -> Result<Value> {
    let p = read_receipt(&job.join("plan.json"))?;
    ensure!(
        p["schema"] == "slop_ninja_supervised_bulk_plan_v1"
            && p["job"] == job.canonicalize()?.to_str().unwrap(),
        "job binding differs"
    );
    for (path, hash) in [
        ("input", "input_sha256"),
        ("manifest", "manifest_sha256"),
        ("restoration_path", "restoration_sha256"),
    ] {
        ensure!(
            hash_file(Path::new(field(&p, path)?))? == p[hash],
            "bound {path} changed"
        );
    }
    ensure!(
        hash_file(&job.join("request.json"))? == p["request_sha256"],
        "request changed"
    );
    let request = read_receipt(&job.join("request.json"))?;
    let rows = p["rows"].as_array().context("rows")?;
    ensure!(
        request["model"] == p["model"]
            && request["items"].as_array().context("items")?.len() == rows.len(),
        "request population differs"
    );
    for (r, i) in rows.iter().zip(request["items"].as_array().unwrap()) {
        ensure!(
            r["id"] == i["id"]
                && r["text"] == i["text"]
                && r["text_sha256"] == digest(field(r, "text")?),
            "request row differs"
        );
        public_source(r)?;
    }
    units(rows)?;
    Ok(p)
}
fn roster(receipt: &Value, request: &Value) -> Result<BTreeMap<usize, String>> {
    let items = request["items"].as_array().context("items")?;
    let mut seen = BTreeSet::new();
    let mut tasks = BTreeSet::new();
    let mut accepted = BTreeMap::new();
    ensure!(
        receipt["total_items"] == items.len() && !field(receipt, "bulk_id")?.is_empty(),
        "bulk receipt binding"
    );
    for (key, success) in [("accepted_items", true), ("failed_items", false)] {
        for r in receipt[key].as_array().context("receipt roster")? {
            let n = r["index"].as_u64().context("index")? as usize;
            ensure!(
                n < items.len() && r["id"] == items[n]["id"] && seen.insert(n),
                "duplicate or mismatched acceptance index"
            );
            if success {
                let task = field(r, "task_id")?.to_owned();
                ensure!(tasks.insert(task.clone()), "duplicate task");
                accepted.insert(n, task);
            }
        }
    }
    ensure!(
        seen.len() == items.len(),
        "acceptance roster omitted inputs"
    );
    Ok(accepted)
}
fn submit(job: &Path, keyfile: &Path) -> Result<()> {
    let lock = File::options()
        .read(true)
        .write(true)
        .open(job.join("lock"))?;
    lock.try_lock()?;
    let plan = checked_job(job)?;
    let request = read_receipt(&job.join("request.json"))?;
    if job.join("receipt.json").exists() {
        roster(&read_receipt(&job.join("receipt.json"))?, &request)?;
        println!("already submitted; collect existing job");
        return Ok(());
    }
    ensure!(
        !job.join("submission-intent.json").exists(),
        "uncertain bulk submission; raw HTTP journal requires reconciliation, never repeat POST"
    );
    let fixtures = Path::new(field(&plan, "fixtures")?);
    let rows = plan["rows"].as_array().unwrap();
    for row in rows {
        let id = field(row, "id")?;
        let folder = fixtures.join(id);
        fs::create_dir_all(&folder)?;
        ensure!(
            !folder.join("result.json").exists()
                && !folder.join("submission.json").exists()
                && !folder.join("archive-blocked.json").exists(),
            "new concurrent obligation/result; re-plan without repeating it"
        );
        let rejection = plan["confirmed_rejections"].get(id);
        let claim = if let Some(expected) = rejection {
            ensure!(
                rejected_402(&folder, row)? == *expected,
                "old rejection changed"
            );
            folder.join("bulk-intent.json")
        } else {
            folder.join("intent.json")
        };
        let mut intent = binding(row, &plan);
        intent["bulk_job"] = json!(job);
        intent["bulk_plan_sha256"] = json!(hash_file(&job.join("plan.json"))?);
        intent["reconciled_prior_rejection"] = rejection.cloned().unwrap_or(Value::Null);
        if claim.exists() {
            ensure!(
                read_receipt(&claim)? == intent,
                "concurrent intent; do not submit"
            );
        } else {
            put(&claim, &intent)?;
        }
    }
    let client = JournalClient::new(credential(keyfile)?, job)?;
    let models = client.request("/models", None, job, Duration::from_secs(60))?;
    ensure!(
        contains_selector(&models, "pangram-4"),
        "selector unavailable"
    );
    put(
        &job.join("submission-intent.json"),
        &json!({"request_sha256":plan["request_sha256"],"plan_sha256":hash_file(&job.join("plan.json"))?,"time":chrono::Utc::now().to_rfc3339(),"automatic_retries":false,"idempotency_key":idempotency_key(&request)?}),
    )?;
    let receipt = client.request("/bulk", Some(request.clone()), job, Duration::from_secs(90))?;
    // Persist the full response before validating it so a schema change cannot lose the ID.
    put(&job.join("receipt.json"), &receipt)?;
    roster(&receipt, &request)?;
    println!("{}", job.join("receipt.json").display());
    Ok(())
}
fn successful(item: &Value, row: &Value, task: &str) -> Result<&'static str> {
    ensure!(
        item["id"] == row["id"] && item["task_id"] == task && item["stage"] == "STAGE_SUCCESS",
        "result identity/stage differs"
    );
    let result = &item["result"];
    ensure!(
        result.get("stage").is_none_or(|s| s == "STAGE_SUCCESS"),
        "inner result stage conflicts"
    );
    let submitted = field(row, "text")?;
    let returned = field(result, "text")?;
    let echo = if returned == submitted {
        "exact"
    } else {
        ensure!(
            returned.split_whitespace().eq(submitted.split_whitespace()),
            "provider changed non-whitespace input bytes"
        );
        "whitespace_only"
    };
    field(result, "version")?;
    let mut sum = 0.;
    for k in ["fraction_human", "fraction_ai", "fraction_ai_assisted"] {
        let v = result[k].as_f64().context("fraction")?;
        ensure!(
            v.is_finite() && (0.0..=1.0).contains(&v),
            "invalid fraction"
        );
        sum += v;
    }
    ensure!((sum - 1.).abs() < 1e-5, "fractions do not sum to one");
    Ok(echo)
}
fn collect(job: &Path, keyfile: &Path) -> Result<bool> {
    let lock = File::options()
        .read(true)
        .write(true)
        .open(job.join("lock"))?;
    lock.try_lock()?;
    let plan = checked_job(job)?;
    if job.join("complete.json").exists() {
        println!("{}", job.join("complete.json").display());
        return Ok(true);
    }
    let receipt = read_receipt(&job.join("receipt.json"))?;
    let request = read_receipt(&job.join("request.json"))?;
    let accepted = roster(&receipt, &request)?;
    let bulk = field(&receipt, "bulk_id")?;
    ensure!(safe_id(bulk), "unsafe provider bulk ID");
    let round = job.join(format!("collection-{}", stamp()?));
    fs::create_dir(&round)?;
    let client = JournalClient::new(credential(keyfile)?, job)?;
    // Only existing-bulk GET routes below; quota stops remain authoritative for submit.
    client.stopped.store(false, Ordering::SeqCst);
    let status = client.request(
        &format!("/bulk/{bulk}"),
        None,
        &round,
        Duration::from_secs(60),
    )?;
    put(&round.join("status.json"), &status)?;
    let rows = plan["rows"].as_array().unwrap();
    ensure!(
        status["bulk_id"] == bulk && status["total_items"] == rows.len(),
        "bulk status binding"
    );
    if !matches!(
        status["status"].as_str(),
        Some("succeeded" | "failed" | "partial")
    ) {
        ensure!(
            matches!(status["status"].as_str(), Some("queued" | "running")),
            "unknown bulk state"
        );
        println!(
            "{}",
            serde_json::to_string(
                &json!({"status":status["status"],"succeeded":status["succeeded"],"total":rows.len()})
            )?
        );
        return Ok(false);
    }
    let mut seen = BTreeSet::new();
    let mut success = 0;
    let mut failed = 0;
    let mut versions = BTreeSet::new();
    let mut echoes = BTreeMap::<String, usize>::new();
    let fixtures = Path::new(field(&plan, "fixtures")?);
    for offset in (0..rows.len()).step_by(100) {
        let page = client.request(
            &format!("/bulk/{bulk}/results?offset={offset}&limit=100"),
            None,
            &round,
            Duration::from_secs(60),
        )?;
        put(&round.join(format!("page-{offset}.json")), &page)?;
        ensure!(
            page["bulk_id"] == bulk
                && page["total_items"] == rows.len()
                && page["offset"] == offset,
            "result page binding"
        );
        for key in ["items", "failed_items"] {
            for item in page[key].as_array().context("result array")? {
                let i = item["index"].as_u64().context("result index")? as usize;
                ensure!(
                    (offset..rows.len().min(offset + 100)).contains(&i)
                        && seen.insert(i)
                        && item["id"] == rows[i]["id"],
                    "duplicate or mismatched result index"
                );
                let folder = fixtures.join(field(&rows[i], "id")?);
                if item["stage"] == "STAGE_FAILED" {
                    if let Some(task) = accepted.get(&i) {
                        ensure!(item["task_id"] == *task, "failed task binding");
                    } else {
                        ensure!(item["task_id"].is_null(), "unaccepted failed task");
                    }
                    let path = folder.join(format!("bulk-failure-{}.json", stamp()?));
                    put(
                        &path,
                        &json!({"bulk_id":bulk,"item":item,"plan_sha256":hash_file(&job.join("plan.json"))?}),
                    )?;
                    failed += 1;
                    continue;
                }
                let task = accepted.get(&i).context("unaccepted result")?;
                let echo = successful(item, &rows[i], task)?;
                *echoes.entry(echo.to_owned()).or_default() += 1;
                versions.insert(field(&item["result"], "version")?.to_owned());
                let mut observation = binding(&rows[i], &plan);
                observation["schema"] = json!("slop_ninja_a_pangram_comparator_v1");
                observation["response"] = item["result"].clone();
                observation["response"]["stage"] = item["stage"].clone();
                observation["task_id"] = json!(task);
                observation["bulk_id"] = json!(bulk);
                observation["bulk_index"] = json!(i);
                observation["bulk_plan_sha256"] = json!(hash_file(&job.join("plan.json"))?);
                observation["echo_match"] = json!(echo);
                observation["returned_text_sha256"] =
                    json!(digest(field(&item["result"], "text")?));
                observation["available_cfg"] = rows[i]
                    .get("available_cfg")
                    .or_else(|| rows[i].get("available"))
                    .cloned()
                    .unwrap_or(Value::Null);
                observation["is_training_label"] = json!(false);
                observation["collected_at"] = json!(chrono::Utc::now().to_rfc3339());
                let path = folder.join(format!("bulk-observation-{}.json", stamp()?));
                put(&path, &observation)?;
                match fs::hard_link(&path, folder.join("result.json")) {
                    Ok(()) => File::open(&folder)?.sync_all()?,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let previous = read_receipt(&folder.join("result.json"))?;
                        binding_matches(&previous, &rows[i], "pangram-4", field(&plan, "split")?)?;
                        ensure!(
                            previous["response"] == observation["response"],
                            "concurrent result differs; both retained"
                        );
                    }
                    Err(e) => return Err(e.into()),
                }
                success += 1;
            }
        }
    }
    ensure!(
        seen.len() == rows.len() && status["succeeded"] == success && status["failed"] == failed,
        "terminal result population incomplete"
    );
    let complete = json!({"schema":"slop_ninja_supervised_bulk_collection_v1","bulk_id":bulk,"requested_population":plan["requested_population"],"submitted":rows.len(),"succeeded":success,"failed":failed,"reused":plan["reused"].as_array().unwrap().len(),"blocked":plan["blocked"],"versions":versions,"echoes":echoes,"collection":round,"plan_sha256":hash_file(&job.join("plan.json"))?,"raw_receipts_retained":true,"is_training_label":false});
    put(&job.join("complete.json"), &complete)?;
    println!("{}", job.join("complete.json").display());
    Ok(true)
}
pub fn main(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("prepare") => prepare(&args[1..]),
        Some("submit") if args.len() == 3 => submit(Path::new(&args[1]), Path::new(&args[2])),
        Some("collect") if args.len() == 3 => {
            collect(Path::new(&args[1]), Path::new(&args[2])).map(|_| ())
        }
        Some("watch") if args.len() == 3 => {
            let until = Instant::now() + Duration::from_secs(1200);
            loop {
                if collect(Path::new(&args[1]), Path::new(&args[2]))? {
                    break;
                }
                ensure!(
                    Instant::now() < until,
                    "watch budget ended; resume existing bulk only"
                );
                std::thread::sleep(Duration::from_secs(15));
            }
            Ok(())
        }
        _ => anyhow::bail!(
            "bulk prepare INPUT MANIFEST SPLIT FIXTURES NEW_JOB RESTORATION.json | bulk submit|collect|watch JOB KEYFILE"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bulk_units_are_started_thousand_word_blocks_not_hundred_word_blocks() {
        assert_eq!(
            units(&[
                json!({"text":vec!["a";1000].join(" ")}),
                json!({"text":vec!["b";1001].join(" ")})
            ])
            .unwrap(),
            3
        );
        assert!(units(&[json!({"text":"too short"})]).is_err());
        assert!(
            units(&[
                json!({"text":vec!["a";50].join(" ")}),
                json!({"text":vec!["a";50].join(" ")})
            ])
            .is_err()
        );
    }
    #[test]
    fn only_complete_explicit_402_rejections_can_be_reconciled() {
        let row = json!({"text":"synthetic text"});
        let request = json!({"method":"POST","url":"https://text.external-api.pangram.com/task","payload":{"model":"pangram-4","text":"synthetic text"}});
        let body = br#"{"detail":"Insufficient credits"}"#;
        let response = json!({"status":402});
        let mut outcome = json!({"status":402,"complete":true,"bytes":body.len(),"body_sha256":hex::encode(sha2::Sha256::digest(body))});
        assert!(rejected_response(&request, &response, &outcome, body, &row).is_ok());
        outcome["complete"] = json!(false);
        assert!(rejected_response(&request, &response, &outcome, body, &row).is_err());
        outcome["complete"] = json!(true);
        assert!(rejected_response(&request, &json!({"status":500}), &outcome, body, &row).is_err());
        assert!(
            rejected_response(
                &request,
                &response,
                &outcome,
                body,
                &json!({"text":"different"})
            )
            .is_err()
        );
    }
    #[test]
    fn acceptance_requires_complete_unique_id_index_and_task_bindings() {
        let request = json!({"items":[{"id":"a"},{"id":"b"}]});
        let valid = json!({"bulk_id":"bulk1","total_items":2,"accepted_items":[{"id":"a","index":0,"task_id":"t1"},{"id":"b","index":1,"task_id":"t2"}],"failed_items":[]});
        assert_eq!(roster(&valid, &request).unwrap().len(), 2);
        let mut v = valid.clone();
        v["accepted_items"][1]["task_id"] = json!("t1");
        assert!(roster(&v, &request).is_err());
        v = valid.clone();
        v["accepted_items"][1]["id"] = json!("a");
        assert!(roster(&v, &request).is_err());
        v = valid;
        v["accepted_items"].as_array_mut().unwrap().pop();
        assert!(roster(&v, &request).is_err());
    }
    #[test]
    fn successful_results_bind_exact_tokens_and_valid_fractions() {
        let row = json!({"id":"a","text":"One two.\r\nThree four."});
        let mut v = json!({"id":"a","task_id":"t1","stage":"STAGE_SUCCESS","result":{"text":"One two. Three four.","version":"4.1","fraction_ai":0.2,"fraction_ai_assisted":0.3,"fraction_human":0.5}});
        assert_eq!(successful(&v, &row, "t1").unwrap(), "whitespace_only");
        v["result"]["text"] = json!("One different. Three four.");
        assert!(successful(&v, &row, "t1").is_err());
        v["result"]["text"] = row["text"].clone();
        v["result"]["fraction_human"] = json!(0.9);
        assert!(successful(&v, &row, "t1").is_err());
    }
}
