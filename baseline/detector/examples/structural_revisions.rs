//! A-data proposal pilot without detector feedback or private B inputs.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::edits::{self, Edit};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Evidence, Origin, Split, sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

const TRAIN: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const POLICY: &str = include_str!("../STRUCTURAL_REVISION_PILOT_V1.md");
const KINDS: [&str; 3] = ["nominalization", "subject_voice", "clause_packaging"];
const INSTRUCTION: &str = "You are preparing a controlled English revision experiment. Read the complete passage. Propose up to three local edits on DIFFERENT, NONOVERLAPPING spans: one clause-to-nominalization or nominalization-to-clause recast (kind nominalization); one subject-choice or active/passive recast (kind subject_voice); one adjunct-placement or clause-packaging change (kind clause_packaging). Preserve every claim, participant, number, qualification, tense, modality, negation, causal relation and reference. Keep the intended tone, accessibility and readability. Do not add facts, strengthen claims, introduce ambiguity, or make the prose more bureaucratic. Keep all surrounding text unchanged. Do not use synonyms alone, contractions alone or punctuation alone as the proposed structural change. A before span must be copied EXACTLY from the passage and occur only once. Neither before nor after may cross a paragraph boundary. Each kind can occur at most once. If a kind has no suitable edit, explain that in unavailable. Return ONLY a JSON object with edits (an array of objects with kind, before, after, reason) and unavailable (an array of objects with kind, reason). Do not return a rewritten passage or markdown.";

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Prepare {
        #[arg(long)]
        records: PathBuf,
        #[arg(long)]
        model_spec: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Capture {
        #[arg(long)]
        prepared: PathBuf,
        #[arg(long)]
        base_url: String,
        #[arg(long)]
        out: PathBuf,
    },
    Materialize {
        #[arg(long)]
        prepared: PathBuf,
        #[arg(long)]
        captures: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}
fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn bound(path: &Path, hash: &Value) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    ensure!(
        hash.as_str() == Some(sha256(&bytes).as_str()),
        "Changed bound file"
    );
    Ok(bytes)
}
fn plan(prepared: &Path) -> Result<Value> {
    let p = read(&prepared.join("plan.json"))?;
    ensure!(
        p["schema"] == "slop_ninja_a_structural_revision_plan_v1"
            && p["policy_sha256"] == sha256(POLICY),
        "Wrong plan/policy"
    );
    bound(&prepared.join("selected.json"), &p["selected_sha256"])?;
    bound(&prepared.join("model.json"), &p["model_sha256"])?;
    let requests = p["requests"].as_array().context("Missing requests")?;
    ensure!(
        !requests.is_empty() && requests.len() <= 8 && p["call_cap"] == requests.len(),
        "Wrong call cap"
    );
    for (index, item) in requests.iter().enumerate() {
        let id = format!("{index:03}");
        let file = format!("requests/{id}.json");
        ensure!(
            item["id"] == id && item["file"] == file,
            "Wrong request sequence"
        );
        bound(&prepared.join(file), &item["sha256"])?;
    }
    Ok(p)
}
fn prepare(records: &Path, model_spec: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a new directory");
    let bytes = fs::read(records)?;
    ensure!(sha256(&bytes) == TRAIN, "Wrong frozen Train archive");
    let rows = dataset::read_records(records)?;
    ensure!(
        rows.iter().all(|r| r.split == Some(Split::Train)),
        "Non-Train record"
    );
    let by_id: BTreeMap<_, _> = rows.iter().map(|r| (&r.id, r)).collect();
    let mut eligible = BTreeMap::<_, Vec<_>>::new();
    for row in &rows {
        eligible.entry(row.source.collection.clone()).or_default();
        if row.origin != Origin::ModelOnly
            || row.evidence != Evidence::RecordedModelGeneration
            || !(150..=450).contains(&row.text.split_whitespace().count())
        {
            continue;
        }
        let root = by_id
            .get(
                row.parent_id
                    .as_ref()
                    .context("Missing generation parent")?,
            )
            .context("Missing human root")?;
        ensure!(
            root.origin == Origin::HumanOnly
                && root.parent_id.is_none()
                && root.source_group == row.source_group,
            "Wrong generation ancestry"
        );
        let rank = sha256(serde_json::to_vec(&json!([
            "slop-ninja-a-structural-revisions-v1",
            row.source_group
        ]))?);
        eligible
            .entry(row.source.collection.clone())
            .or_default()
            .push((rank, row.id.clone(), row, *root));
    }
    let model_bytes = fs::read(model_spec)?;
    let model: Value = serde_json::from_slice(&model_bytes)?;
    ensure!(
        model["revision"]
            == "mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit@e9675aa3ca5f900ccef55267914466d55ab325fa"
            && model["temperature"] == 0.7
            && model["max_tokens"] == 2048,
        "Wrong pilot generator"
    );
    fs::create_dir_all(out.join("requests"))?;
    fs::write(out.join("model.json"), &model_bytes)?;
    fs::write(out.join("PROTOCOL.md"), POLICY)?;
    let mut selected = Vec::new();
    let mut requests = Vec::new();
    let mut strata = Vec::new();
    for (collection, group) in &mut eligible {
        group.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        strata.push(json!({"collection":collection,"eligible_drafts":group.len(),"selected":!group.is_empty()}));
        let Some((rank, _, parent, root)) = group.first() else {
            continue;
        };
        let id = format!("{:03}", selected.len());
        let request = json!({"model":model["id"],"messages":[{"role":"system","content":INSTRUCTION},{"role":"user","content":parent.text}],"temperature":0.7,"seed":17,"max_tokens":2048,"stream":false});
        let file = format!("requests/{id}.json");
        save(&out.join(&file), &request)?;
        requests.push(json!({"id":id,"file":file,"sha256":sha256(fs::read(out.join(&file))?)}));
        selected.push(json!({"id":id,"rank":rank,"parent":parent,"human_root":root}));
    }
    ensure!(
        !selected.is_empty() && selected.len() <= 8,
        "Unexpected pilot size"
    );
    save(&out.join("selected.json"), &selected)?;
    ensure!(
        fs::read(records)? == bytes && fs::read(model_spec)? == model_bytes,
        "Changed preparation inputs"
    );
    save(
        &out.join("plan.json"),
        &json!({"schema":"slop_ninja_a_structural_revision_plan_v1","policy_sha256":sha256(POLICY),"train_sha256":TRAIN,"model_sha256":sha256(model_bytes),"selected_sha256":sha256(fs::read(out.join("selected.json"))?),"strata":strata,"requests":requests,"call_cap":selected.len(),"model_calls":0,"training_admission":false}),
    )?;
    println!("{}", json!({"prepared":selected.len(),"calls":0}));
    Ok(())
}
fn capture(prepared: &Path, base_url: &str, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a new capture directory");
    let url = reqwest::Url::parse(base_url)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.path() == "/"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use the dedicated loopback worker root URL"
    );
    let original = fs::read(prepared.join("plan.json"))?;
    let p = plan(prepared)?;
    fs::create_dir(out)?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1800))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    for item in p["requests"].as_array().context("Missing requests")? {
        let id = item["id"].as_str().context("Missing request ID")?;
        ensure!(
            id.len() == 3 && id.bytes().all(|b| b.is_ascii_digit()),
            "Invalid request ID"
        );
        let file = format!("requests/{id}.json");
        ensure!(item["file"] == file, "Wrong request path");
        let bytes = bound(&prepared.join(file), &item["sha256"])?;
        save(
            &out.join(format!("{id}-invocation.json")),
            &json!({"request_sha256":item["sha256"],"model_sha256":p["model_sha256"],"plan_sha256":sha256(&original),"url":format!("{base_url}v1/chat/completions"),"at":chrono::Utc::now(),"timeout_seconds":1800}),
        )?;
        let result = client
            .post(format!("{base_url}v1/chat/completions"))
            .header("Content-Type", "application/json")
            .body(bytes)
            .send();
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                save(
                    &out.join(format!("{id}-error.json")),
                    &json!({"error":e.to_string(),"remaining_requests":"not attempted","at":chrono::Utc::now()}),
                )?;
                return Err(e.into());
            }
        };
        let status = response.status();
        let bytes = response.bytes()?;
        fs::write(out.join(format!("{id}-response.json")), &bytes)?;
        save(
            &out.join(format!("{id}-capture.json")),
            &json!({"http_status":status.as_u16(),"response_sha256":sha256(&bytes),"at":chrono::Utc::now()}),
        )?;
        ensure!(status.is_success(), "Generation HTTP failure; no retry");
        println!("Captured {id}");
    }
    ensure!(
        fs::read(prepared.join("plan.json"))? == original,
        "Changed plan"
    );
    plan(prepared)?;
    save(
        &out.join("completion.json"),
        &json!({"status":"captured_pending_review","calls":p["call_cap"],"plan_sha256":sha256(original),"at":chrono::Utc::now()}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    edits: Vec<Alternative>,
    unavailable: Vec<Unavailable>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Alternative {
    kind: String,
    before: String,
    after: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unavailable {
    kind: String,
    reason: String,
}
fn materialize_proposal(text: &str, proposal: Proposal) -> Result<Value> {
    ensure!(
        proposal.edits.len() + proposal.unavailable.len() == 3,
        "Every category must be accounted for"
    );
    let mut kinds = std::collections::BTreeSet::new();
    let mut patches = Vec::new();
    let mut alternatives = Vec::new();
    for alt in proposal.edits {
        ensure!(
            KINDS.contains(&alt.kind.as_str())
                && kinds.insert(alt.kind.clone())
                && !alt.reason.trim().is_empty(),
            "Invalid/repeated category"
        );
        ensure!(
            !alt.before.is_empty()
                && !alt.after.trim().is_empty()
                && alt.before != alt.after
                && !alt.before.contains(['\n', '\r'])
                && !alt.after.contains(['\n', '\r']),
            "Invalid edit span"
        );
        let matches: Vec<_> = text.match_indices(&alt.before).collect();
        ensure!(matches.len() == 1, "Source span must be unique");
        let start = matches[0].0;
        let patch = Edit {
            start_byte: start,
            end_byte: start + alt.before.len(),
            expected: alt.before,
            replacement: alt.after,
        };
        alternatives.push(json!({"kind":alt.kind,"reason":alt.reason,"edit":patch}));
        patches.push(patch);
    }
    let mut unavailable = Vec::new();
    for alt in proposal.unavailable {
        ensure!(
            KINDS.contains(&alt.kind.as_str())
                && kinds.insert(alt.kind.clone())
                && !alt.reason.trim().is_empty(),
            "Invalid/repeated unavailable category"
        );
        unavailable.push(json!({"kind":alt.kind,"reason":alt.reason}));
    }
    edits::apply(text, &patches)?; // Reject overlaps before constructing any subset.
    let mut states = Vec::new();
    for mask in 0..(1usize << patches.len()) {
        let selected: Vec<_> = patches
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, e)| e.clone())
            .collect();
        let candidate = edits::apply(text, &selected)?;
        states.push(json!({"mask":mask,"text_sha256":sha256(&candidate),"text":candidate,"edits":selected,"review":"pending"}));
    }
    Ok(json!({"alternatives":alternatives,"unavailable":unavailable,"states":states}))
}
fn materialize(prepared: &Path, captures: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a new state directory");
    let p = plan(prepared)?;
    let completion = read(&captures.join("completion.json"))?;
    let plan_hash = sha256(fs::read(prepared.join("plan.json"))?);
    ensure!(
        completion["plan_sha256"] == plan_hash && completion["status"] == "captured_pending_review",
        "Incomplete or wrong capture"
    );
    let selected = read(&prepared.join("selected.json"))?;
    fs::create_dir(out)?;
    let mut outcomes = Vec::new();
    for source in selected.as_array().context("Missing sources")? {
        let id = source["id"].as_str().context("Missing ID")?;
        let input = read(&captures.join(format!("{id}-invocation.json")))?;
        let request = p["requests"]
            .as_array()
            .context("Missing requests")?
            .iter()
            .find(|r| r["id"] == id)
            .context("Missing request")?;
        ensure!(
            input["plan_sha256"] == plan_hash && input["request_sha256"] == request["sha256"],
            "Wrong invocation ancestry"
        );
        let observation = read(&captures.join(format!("{id}-capture.json")))?;
        let response: Value = serde_json::from_slice(&bound(
            &captures.join(format!("{id}-response.json")),
            &observation["response_sha256"],
        )?)?;
        let outcome = (|| -> Result<Value> {
            ensure!(
                observation["http_status"] == 200
                    && response["choices"].as_array().is_some_and(|a| a.len() == 1)
                    && response["choices"][0]["finish_reason"] == "stop",
                "Incomplete generator response"
            );
            let content = response["choices"][0]["message"]["content"]
                .as_str()
                .context("Missing content")?;
            let proposal: Proposal = serde_json::from_str(content)?;
            materialize_proposal(
                source["parent"]["text"]
                    .as_str()
                    .context("Missing parent")?,
                proposal,
            )
        })();
        let report = match outcome {
            Ok(states) => {
                json!({"id":id,"status":"materialized_pending_review","source":source,"response_sha256":observation["response_sha256"],"proposal":states})
            }
            Err(error) => {
                json!({"id":id,"status":"rejected","source":source,"response_sha256":observation["response_sha256"],"reason":error.to_string()})
            }
        };
        save(&out.join(format!("{id}.json")), &report)?;
        outcomes.push(json!({"id":id,"status":report["status"],"states":report["proposal"]["states"].as_array().map_or(0,Vec::len)}));
    }
    save(
        &out.join("summary.json"),
        &json!({"schema":"slop_ninja_a_structural_revision_materialization_v1","plan_sha256":plan_hash,"outcomes":outcomes,"detector_calls":0,"training_admission":false}),
    )
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::Prepare {
            records,
            model_spec,
            out,
        } => prepare(&records, &model_spec, &out),
        Command::Capture {
            prepared,
            base_url,
            out,
        } => capture(&prepared, &base_url, &out),
        Command::Materialize {
            prepared,
            captures,
            out,
        } => materialize(&prepared, &captures, &out),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn proposal(before: &str) -> Proposal {
        serde_json::from_value(json!({"edits":[{"kind":"nominalization","before":before,"after":"The result is uncertain.","reason":"Recast"}],"unavailable":[{"kind":"subject_voice","reason":"No suitable change"},{"kind":"clause_packaging","reason":"No suitable change"}]})).unwrap()
    }
    #[test]
    fn duplicate_source_spans_are_rejected_and_partial_menus_keep_control() {
        assert!(
            materialize_proposal("It may work. It may work.", proposal("It may work.")).is_err()
        );
        let value =
            materialize_proposal("It may work. We will check.", proposal("It may work.")).unwrap();
        assert_eq!(value["states"][0]["text"], "It may work. We will check.");
        assert_eq!(
            value["states"][1]["text"],
            "The result is uncertain. We will check."
        );
        assert_eq!(value["states"][1]["review"], "pending");
    }
    #[test]
    fn overlap_rejects_the_response_before_subset_enumeration() {
        let p:Proposal=serde_json::from_value(json!({"edits":[{"kind":"nominalization","before":"The result is uncertain.","after":"There is uncertainty.","reason":"Recast"},{"kind":"subject_voice","before":"result is uncertain","after":"outcome remains uncertain","reason":"Recast"}],"unavailable":[{"kind":"clause_packaging","reason":"No suitable change"}]})).unwrap();
        assert!(materialize_proposal("The result is uncertain.", p).is_err());
    }
}
