//! Permitted, exact-document Pangram comparator with durable no-resubmit receipts.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use unslop::{evaluation::PangramClient, util::digest};
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(&serde_json::to_vec_pretty(v)?)?;
    f.sync_all()?;
    Ok(())
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn credential(path: &Path) -> Result<String> {
    if let Ok(key) = std::env::var("PANGRAM_API_KEY") {
        ensure!(!key.is_empty(), "empty key");
        return Ok(key);
    }
    let text = fs::read_to_string(path)?;
    let mut found = None;
    for line in text.lines() {
        let line = line
            .trim()
            .strip_prefix("export ")
            .unwrap_or(line.trim())
            .trim();
        if let Some(value) = line.strip_prefix("PANGRAM_API_KEY=") {
            let value = value.trim().trim_end_matches(';');
            let value = if value.starts_with('"') && value.ends_with('"')
                || value.starts_with('\'') && value.ends_with('\'')
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            ensure!(
                !value.is_empty() && !value.contains(['$', '`', '\\', '\n', ' ']),
                "key must be a literal assignment; no shell code is executed"
            );
            found = Some(value.to_owned());
        }
    }
    found.context("PANGRAM_API_KEY literal assignment absent")
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 6,
        "a_pangram_compare REPO POPULATION.json SPLIT OUTPUT KEY_ASSIGNMENT_FILE MODEL"
    );
    ensure!(
        matches!(a[2].as_str(), "development" | "calibration" | "test"),
        "explicit evaluation split required"
    );
    let repo = Path::new(&a[0]);
    let population = read(Path::new(&a[1]))?;
    let out = Path::new(&a[3]);
    fs::create_dir_all(out)?;
    let client = PangramClient::with_api_key(credential(Path::new(&a[4]))?)?;
    if !out.join("models.json").exists() {
        put(&out.join("models.json"), &client.models()?)?;
    }
    let targets = population["rows"]
        .as_array()
        .context("population rows")?
        .iter()
        .filter(|r| r["split"] == a[2])
        .collect::<Vec<_>>();
    let files = targets
        .iter()
        .map(|r| {
            r["metadata"]["source"]["path"]
                .as_str()
                .context("source path")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let mut sources = BTreeMap::new();
    for path in files {
        ensure!(
            path.starts_with("datasets/")
                && !Path::new(path)
                    .components()
                    .any(|c| c == std::path::Component::ParentDir),
            "only tracked public corpus allowed"
        );
        for line in fs::read_to_string(repo.join(path))?.lines() {
            let value: Value = serde_json::from_str(line)?;
            let id = value["id"].as_str().context("record id")?.to_owned();
            sources.insert((path.to_owned(), id), (digest(line), value));
        }
    }
    let mut results = Vec::new();
    for row in targets {
        let id = row["id"].as_str().context("row id")?;
        ensure!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
            "unsafe row id"
        );
        let source = &row["metadata"]["source"];
        let key = (
            source["path"].as_str().context("source path")?.to_owned(),
            source["record_id"]
                .as_str()
                .context("source id")?
                .to_owned(),
        );
        let (raw_hash, record) = sources.get(&key).context("source missing")?;
        ensure!(
            *raw_hash == source["record_sha256"],
            "source record changed"
        );
        ensure!(
            record["rights"]["external_evaluation"] == true,
            "source lacks external evaluation permission"
        );
        let text = record["text"].as_str().context("text")?;
        ensure!(digest(text) == source["text_sha256"], "source text changed");
        let folder = out.join(id);
        fs::create_dir_all(&folder)?;
        let final_path = folder.join("result.json");
        if final_path.exists() {
            let prior = read(&final_path)?;
            ensure!(
                prior["text_sha256"] == digest(text) && prior["model"] == a[5],
                "existing comparator binding differs"
            );
            results.push(json!({"id":id,"result":final_path,"reused":true}));
            continue;
        }
        let started = Instant::now();
        let submit = folder.join("submission.json");
        let submission = if submit.exists() {
            read(&submit)?
        } else {
            if folder.join("intent.json").exists() {
                results.push(json!({"id":id,"status":"uncertain_submission_do_not_repeat"}));
                continue;
            }
            put(
                &folder.join("intent.json"),
                &json!({"id":id,"model":a[5],"text_sha256":digest(text),"source_group":row["group"],"split":a[2],"input_population_sha256":digest(&fs::read_to_string(&a[1])?),"status":"submission_intent"}),
            )?;
            match client.submit(text, &a[5]) {
                Ok(s) => {
                    put(&submit, &s)?;
                    s
                }
                Err(e) => {
                    put(
                        &folder.join("failure.json"),
                        &json!({"stage":"submit","error":e.to_string(),"automatic_resubmit":false}),
                    )?;
                    results.push(json!({"id":id,"status":"submission_failed_or_uncertain"}));
                    continue;
                }
            }
        };
        let task = submission["task_id"].as_str().context("task id")?;
        match client.poll(task, 600.) {
            Ok(response) => {
                put(
                    &final_path,
                    &json!({"schema":"slop_ninja_a_pangram_comparator_v1","id":id,"model":a[5],"text_sha256":digest(text),"source_group":row["group"],"split":a[2],"response":response,"elapsed_ms":started.elapsed().as_millis(),"available_cfg":row["available"],"is_training_label":false}),
                )?;
                results.push(json!({"id":id,"result":final_path,"reused":false}));
            }
            Err(e) => {
                let failure = folder.join(format!(
                    "poll-failure-{}.json",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs()
                ));
                put(
                    &failure,
                    &json!({"stage":"poll","task_id":task,"error":e.to_string(),"resume_existing_task":true}),
                )?;
                results.push(json!({"id":id,"status":"poll_failed"}));
            }
        }
        eprintln!(
            "completed comparator item {} of current split",
            results.len()
        );
    }
    let summary = PathBuf::from(&a[3]).join(format!(
        "summary-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
    ));
    put(
        &summary,
        &json!({"split":a[2],"results":results,"training_labels_from_detector":false}),
    )?;
    println!("{}", summary.display());
    Ok(())
}
