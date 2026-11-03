use crate::{
    evaluation::{self, PangramClient, PangramFailure},
    provider_ids::ProviderIds,
    util::{digest, read, write_json},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub struct StudyOptions<'a> {
    pub baseline: &'a Path,
    pub candidate: &'a Path,
    pub out: &'a Path,
    pub model: &'a str,
    pub repeats: usize,
    pub max_requests: usize,
    pub timeout: f64,
    pub threshold: f64,
    pub full_document: bool,
    pub audit: Option<&'a Path>,
}

pub fn study(o: StudyOptions<'_>) -> Result<Value> {
    ensure!(
        o.repeats > 0
            && o.timeout.is_finite()
            && o.timeout > 0.0
            && o.threshold.is_finite()
            && o.threshold > 0.0
            && o.threshold <= 1.0
            && !o.model.trim().is_empty(),
        "Invalid study settings"
    );
    let a = read(o.baseline)?;
    let b = read(o.candidate)?;
    ensure!(a != b, "Candidate equals baseline");
    ensure!(
        a.split_whitespace().count() >= 50 && b.split_whitespace().count() >= 50,
        "Both inputs must contain at least 50 words"
    );
    let audit = if let Some(path) = o.audit {
        serde_json::from_str::<Value>(&read(path)?)?
    } else {
        json!({})
    };
    ensure!(audit.is_object(), "Quality audit must be an object");
    if !audit.as_object().unwrap().is_empty() {
        ensure!(
            audit["source_sha256"] == digest(&a) && audit["candidate_sha256"] == digest(&b),
            "Quality audit does not match exact source/candidate hashes"
        );
    }
    // Before any paid request: the summary writes salted task digests.
    let ids = ProviderIds::load()?;
    fs::create_dir_all(o.out)?;
    let manifest = json!({"baseline_sha256":digest(&a),"candidate_sha256":digest(&b),"model":o.model,"repeats":o.repeats,"full_document":o.full_document});
    let mp = o.out.join("manifest.json");
    if mp.exists() {
        ensure!(
            serde_json::from_str::<Value>(&read(&mp)?)? == manifest,
            "Study directory already has different inputs"
        );
    } else {
        write_json(&mp, &manifest)?;
    }
    let mut order = Vec::new();
    for i in 0..o.repeats {
        let roles = if i % 2 == 0 {
            ["baseline", "candidate"]
        } else {
            ["candidate", "baseline"]
        };
        for role in roles {
            order.push((role, i, o.out.join(format!("{:02}-{role}.json", i + 1))));
        }
    }
    let missing = order.iter().filter(|(_, _, p)| !p.exists()).count();
    ensure!(
        missing <= o.max_requests,
        "Study needs {missing} new paid requests; budget is {}",
        o.max_requests
    );
    let mut complete = Vec::new();
    let mut task_ids = std::collections::BTreeSet::new();
    for (role, _, p) in &order {
        if p.exists() {
            let r: Value = serde_json::from_str(&read(p)?)?;
            let expected = if *role == "baseline" { &a } else { &b };
            ensure!(
                r["sha256"] == digest(expected)
                    && r["submitted_text"] == *expected
                    && r["model"] == o.model
                    && r["full_document"] == o.full_document,
                "Cached study record mismatch"
            );
            ensure!(
                r["task_id"].as_str().is_some_and(|s| !s.is_empty()),
                "Uncertain prior submission in {}; recover its task ID before resuming",
                p.display()
            );
            ensure!(
                r["detector"].as_str().unwrap_or("pangram") == "pangram",
                "Cached detector must be pangram"
            );
            ensure!(
                task_ids.insert(r["task_id"].as_str().unwrap().to_string()),
                "Cached study slots reuse a task ID"
            );
            ensure!(
                r["result"]["stage"] != "STAGE_FAILED",
                "Previously failed detector task requires inspection"
            );
            if r["result"]["stage"] == "STAGE_SUCCESS" {
                complete.push(r);
            }
        }
    }
    if !complete.is_empty() {
        let s = evaluation::summarize_runs(&complete, o.threshold, 3, &ids)?;
        ensure!(
            s["duplicate_records_ignored"] == 0,
            "Cached study slots reuse a task ID"
        );
    }
    for (role, i, p) in &order {
        let text = if *role == "baseline" { &a } else { &b };
        let mut r = if p.exists() {
            serde_json::from_str::<Value>(&read(p)?)?
        } else {
            let client = PangramClient::new()?;
            let intent = json!({"state":"submission_uncertain","detector":"pangram","model":o.model,"submitted_text":text,"sha256":digest(text),"submitted_at":chrono::Utc::now().to_rfc3339(),"full_document":o.full_document,"role":role,"block":i});
            write_json(p, &intent)?;
            let mut r = client.submit(text, o.model)?;
            r["full_document"] = json!(o.full_document);
            r["role"] = json!(role);
            r["block"] = json!(i);
            write_json(p, &r)?;
            r
        };
        if r["result"]["stage"] != "STAGE_SUCCESS" {
            let client = PangramClient::new()?;
            match client.poll(r["task_id"].as_str().unwrap(), o.timeout) {
                Ok(result) => {
                    r["result"] = result;
                    write_json(p, &r)?;
                }
                Err(e) => {
                    if let Some(f) = e.downcast_ref::<PangramFailure>() {
                        r["failure"] = f.metadata();
                        if let Some(response) = &f.response {
                            if response["stage"] == "STAGE_FAILED" {
                                r["result"] = response.clone();
                            } else {
                                r["last_poll_response"] = response.clone();
                            }
                        }
                        write_json(p, &r)?;
                    }
                    return Err(e);
                }
            }
        }
        evaluation::summarize_runs(&[r], o.threshold, 3, &ids)?;
        eprintln!("Recorded {role} repeat {}/{}", i + 1, o.repeats);
    }
    let mut baseline = Vec::new();
    let mut candidate = Vec::new();
    for (role, _, path) in &order {
        let r = serde_json::from_str::<Value>(&read(path)?)?;
        if *role == "baseline" {
            baseline.push(r);
        } else {
            candidate.push(r);
        }
    }
    let report = if o.full_document {
        evaluation::paired_study(
            &baseline,
            &candidate,
            &audit,
            o.threshold,
            o.repeats.max(3),
            &ids,
        )?
    } else {
        json!({"scope":"section","baseline":evaluation::summarize_runs(&baseline,o.threshold,3,&ids)?,"candidate":evaluation::summarize_runs(&candidate,o.threshold,3,&ids)?,"reliability_established":false})
    };
    let summary = o.out.join("summary.json");
    write_json(&summary, &report)?;
    ids.record(&summary.display().to_string())?;
    Ok(report)
}

/// Score one file. The raw record at `out` keeps the task ID for recovery and
/// belongs in private storage; the returned summary lists its salted digest.
pub fn score_file(text_path: &Path, out: &Path, model: &str, max_requests: usize) -> Result<Value> {
    let ids = ProviderIds::load()?;
    let summary = score_file_with(text_path, out, model, max_requests, &ids)?;
    ids.record("unslop score (standard output)")?;
    Ok(summary)
}

fn score_file_with(
    text_path: &Path,
    out: &Path,
    model: &str,
    max_requests: usize,
    ids: &ProviderIds,
) -> Result<Value> {
    let text = read(text_path)?;
    ensure!(
        text.split_whitespace().count() >= 50 && !model.trim().is_empty(),
        "Pangram requires >=50 words and explicit model"
    );
    let mut record = if out.exists() {
        let r: Value = serde_json::from_str(&read(out)?)?;
        ensure!(
            r["submitted_text"] == text && r["sha256"] == digest(&text) && r["model"] == model,
            "Score record input mismatch"
        );
        ensure!(
            r["task_id"].as_str().is_some_and(|s| !s.is_empty()),
            "Uncertain prior submission; recover task ID"
        );
        ensure!(
            r["detector"].as_str().unwrap_or("pangram") == "pangram",
            "Cached detector must be pangram"
        );
        ensure!(
            r["result"]["stage"] != "STAGE_FAILED",
            "Previously failed detector task requires inspection"
        );
        r
    } else {
        ensure!(max_requests >= 1, "One new request needed");
        let c = PangramClient::new()?;
        write_json(
            out,
            &json!({"state":"submission_uncertain","submitted_text":text,"sha256":digest(&text),"model":model}),
        )?;
        let r = c.submit(&text, model)?;
        write_json(out, &r)?;
        r
    };
    if record["result"]["stage"] != "STAGE_SUCCESS" {
        let c = PangramClient::new()?;
        match c.poll(record["task_id"].as_str().unwrap(), 600.0) {
            Ok(r) => record["result"] = r,
            Err(e) => {
                if let Some(f) = e.downcast_ref::<PangramFailure>() {
                    record["failure"] = f.metadata();
                    write_json(out, &record)?;
                }
                return Err(e);
            }
        }
        write_json(out, &record)?;
    }
    evaluation::summarize_runs(&[record], 0.1, 3, ids)
}

pub fn score_corpus(
    jsonl: &Path,
    split: &str,
    out: &Path,
    model: &str,
    max_requests: usize,
) -> Result<Value> {
    ensure!(!model.trim().is_empty(), "Explicit detector model required");
    let ids = ProviderIds::load()?;
    let docs: Vec<_> = crate::util::read_jsonl(jsonl)?
        .into_iter()
        .filter(|d| d["split"] == split)
        .collect();
    ensure!(!docs.is_empty(), "No documents in requested split");
    let mut slots = Vec::new();
    let mut hashes = std::collections::BTreeSet::new();
    let mut task_ids = std::collections::BTreeSet::new();
    for d in &docs {
        let text = d["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing corpus text"))?;
        ensure!(
            text.split_whitespace().count() >= 50,
            "Every detector input requires at least 50 words"
        );
        let hash = digest(text);
        ensure!(
            hashes.insert(hash.clone()),
            "Duplicate corpus text; deduplicate before scoring"
        );
        let path = out.join(format!("{hash}.json"));
        if path.exists() {
            let r: Value = serde_json::from_str(&read(&path)?)?;
            ensure!(
                r["submitted_text"] == text && r["model"] == model && r["sha256"] == hash,
                "Cached corpus score mismatch"
            );
            ensure!(
                r["task_id"].as_str().is_some_and(|s| !s.is_empty()),
                "Uncertain prior corpus submission"
            );
            ensure!(
                r["detector"].as_str().unwrap_or("pangram") == "pangram",
                "Cached detector must be pangram"
            );
            ensure!(
                task_ids.insert(r["task_id"].as_str().unwrap().to_string()),
                "Cached corpus slots reuse a task ID"
            );
            ensure!(
                r["result"]["stage"] != "STAGE_FAILED",
                "Prior corpus task failed"
            );
            if r["result"]["stage"] == "STAGE_SUCCESS" {
                evaluation::summarize_runs(&[r], 0.1, 3, &ids)?;
            }
        }
        slots.push(path);
    }
    let missing = slots.iter().filter(|p| !p.exists()).count();
    ensure!(
        missing <= max_requests,
        "Needs {missing} new requests; budget {max_requests}"
    );
    fs::create_dir_all(out)?;
    let mut observations = Vec::new();
    for (d, path) in docs.iter().zip(&slots) {
        let source = path.with_extension("txt");
        fs::write(&source, d["text"].as_str().unwrap())?;
        let summary = score_file_with(&source, path, model, 1, &ids)?;
        observations.push(json!({"corpus":d["corpus"],"group_id":d["group_id"],"split":d["split"],"domain":d["domain"],"source_kind":d["source_kind"],"model":d["model"],"sha256":digest(d["text"].as_str().unwrap()),"result_file":path,"summary":summary}));
        eprintln!(
            "Scored {} / {}",
            d["corpus"].as_str().unwrap_or("corpus"),
            d["group_id"].as_str().unwrap_or("group")
        );
    }
    let report = json!({"scope":"exploratory_single_observation_per_input","split":split,"new_requests":missing,"documents":docs.len(),"observations":observations,"reliability_established":false});
    let summary = out.join("summary.json");
    write_json(&summary, &report)?;
    ids.record(&summary.display().to_string())?;
    Ok(report)
}
