//! Freeze manually reviewed interventions, then report exact paired CPU scores.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::edits::{self, Edit};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const ARTIFACT: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";
const THRESHOLDS: [f64; 2] = [0.8436493155285434, 0.4287369114260753];
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Freeze {
        #[arg(long)]
        study: PathBuf,
        #[arg(long)]
        review: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Report {
        #[arg(long)]
        prepared: PathBuf,
        #[arg(long)]
        responses: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("Missing array")
}
fn string(v: &Value) -> Result<&str> {
    v.as_str().context("Missing string")
}
fn probability(v: &Value) -> Result<f64> {
    let p = v.as_f64().context("Missing probability")?;
    ensure!(
        p.is_finite() && (0.0..=1.0).contains(&p),
        "Invalid probability"
    );
    Ok(p)
}
fn jsonl(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
fn freeze(study: &Path, review_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a new output directory");
    let review_bytes = fs::read(review_path)?;
    let review: Value = serde_json::from_slice(&review_bytes)?;
    ensure!(
        review["schema"] == "slop_ninja_a_edit_stress_review_v1"
            && review["detector_scores_seen"] == false
            && review["artifact_id"] == ARTIFACT
            && review["runtime"] == "cpu_float32_eager"
            && review["thresholds_strictly_greater_than"] == json!(THRESHOLDS),
        "Changed review policy"
    );
    let protocol_bytes = fs::read(study.join("protocol.json"))?;
    let protocol: Value = serde_json::from_slice(&protocol_bytes)?;
    ensure!(
        protocol["schema"] == "slop_ninja_a_edit_stress_protocol_v1",
        "Wrong study"
    );
    let summary: Value = serde_json::from_slice(&fs::read(study.join("summary.json"))?)?;
    let declared: BTreeSet<_> = array(&summary["documents"])?
        .iter()
        .map(|r| string(&r["path"]))
        .collect::<Result<_>>()?;
    let mut reviewed_paths = BTreeSet::new();
    let mut rows = vec![];
    let mut requests = BTreeMap::new();
    let mut documents = BTreeMap::new();
    for entry in array(&review["documents"])? {
        let path = string(&entry["path"])?;
        ensure!(
            declared.contains(path)
                && path.starts_with("documents/")
                && !path.contains("..")
                && reviewed_paths.insert(path),
            "Invalid or duplicate reviewed document"
        );
        let bytes = fs::read(study.join(path))?;
        let doc: Value = serde_json::from_slice(&bytes)?;
        let source = string(&doc["source"]["text"])?;
        let source_hash = sha256(source);
        ensure!(
            entry["source_sha256"] == source_hash
                && doc["source"]["text_sha256"] == source_hash
                && doc["source"]["split"] == "development",
            "Changed reviewed source or split"
        );
        documents.insert(path.to_owned(), sha256(&bytes));
        let mut judgments = BTreeMap::from([(0_u64, "control")]);
        for (key, status) in [
            ("admissible_masks", "admissible_stress"),
            ("quality_concern_masks", "quality_concern"),
            ("register_shift_masks", "register_shift"),
        ] {
            for mask in array(&entry[key])? {
                let mask = mask.as_u64().context("Invalid review mask")?;
                ensure!(
                    mask > 0 && judgments.insert(mask, status).is_none(),
                    "Duplicate judgment"
                );
            }
        }
        let mut seen = BTreeSet::new();
        for state in array(&doc["states"])? {
            let mask = state["mask"].as_u64().context("Missing state mask")?;
            ensure!(seen.insert(mask), "Duplicate state");
            let judgment = judgments.get(&mask).context("Unreviewed state")?;
            let text = string(&state["text"])?;
            let id = string(&state["id"])?;
            let patches: Vec<Edit> = serde_json::from_value(state["edits"].clone())?;
            ensure!(
                sha256(text) == id
                    && edits::apply(source, &patches)? == text
                    && state["syntax"]["text"] == text
                    && state["syntax"]["error"].is_null(),
                "Changed or unparsed reviewed candidate"
            );
            if mask == 0 {
                ensure!(
                    id == source_hash && patches.is_empty(),
                    "Changed original control"
                );
            }
            let nominations = array(&doc["selected_nominations"])?;
            ensure!(
                nominations.len() <= 3 && mask < (1 << nominations.len()),
                "Invalid nomination mask"
            );
            let picked: Vec<_> = nominations
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, n)| n)
                .collect();
            ensure!(
                state["choices"] == json!(picked.iter().map(|n| &n["id"]).collect::<Vec<_>>()),
                "Changed selected choices"
            );
            let declared_patches: Vec<Edit> = picked
                .iter()
                .map(|n| serde_json::from_value::<Vec<Edit>>(n["edits"].clone()))
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            ensure!(
                serde_json::to_value(&patches)? == serde_json::to_value(declared_patches)?,
                "Changed nomination patches"
            );
            if let Some(old) = requests.insert(id.to_owned(), text.to_owned()) {
                ensure!(old == text, "Hash collision");
            }
            rows.push(
                json!({"document":path,"stage":doc["stage"],"origin":doc["source"]["origin"],
                "source_group":doc["family"]["group"],"collection":doc["family"]["collection"],
                "source_sha256":source_hash,"text_sha256":id,"mask":mask,"judgment":judgment,
                "rules":picked.iter().map(|n| &n["rule"]).collect::<Vec<_>>() }),
            );
        }
        ensure!(
            seen.len() == judgments.len(),
            "Review contains unknown states"
        );
    }
    fs::create_dir_all(out)?;
    let mut request_bytes = vec![];
    for (id, text) in &requests {
        serde_json::to_writer(&mut request_bytes, &json!({"id":id,"text":text}))?;
        request_bytes.push(b'\n');
    }
    let mut file = fs::File::create_new(out.join("requests.jsonl"))?;
    file.write_all(&request_bytes)?;
    file.sync_all()?;
    save(
        &out.join("plan.json"),
        &json!({"schema":"slop_ninja_a_edit_stress_scoring_v1",
        "review_sha256":sha256(&review_bytes),"protocol_sha256":sha256(protocol_bytes),
        "documents_sha256":documents,"requests_sha256":sha256(&request_bytes),"unique_texts":requests.len(),
        "artifact_id":ARTIFACT,"runtime":"cpu_float32_eager","thresholds":THRESHOLDS,
        "review":review,"rows":rows,"provider_calls":0,"training_admitted":false}),
    )?;
    println!(
        "{}",
        json!({"prepared_documents":reviewed_paths.len(),"unique_texts":requests.len()})
    );
    Ok(())
}
fn report(prepared: &Path, responses: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a new output directory");
    let plan_bytes = fs::read(prepared.join("plan.json"))?;
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan["schema"] == "slop_ninja_a_edit_stress_scoring_v1"
            && plan["artifact_id"] == ARTIFACT
            && plan["thresholds"] == json!(THRESHOLDS)
            && plan["runtime"] == "cpu_float32_eager",
        "Changed score policy"
    );
    let request_bytes = fs::read(prepared.join("requests.jsonl"))?;
    ensure!(
        plan["requests_sha256"] == sha256(&request_bytes),
        "Changed requests"
    );
    let requests = jsonl(&request_bytes)?;
    let mut requested = BTreeSet::new();
    for row in &requests {
        let id = string(&row["id"])?;
        ensure!(
            sha256(string(&row["text"])?) == id && requested.insert(id),
            "Changed or duplicate request"
        );
    }
    let response_bytes = fs::read(responses)?;
    let mut scores = BTreeMap::new();
    for row in jsonl(&response_bytes)? {
        let id = string(&row["id"])?;
        ensure!(
            requested.contains(id)
                && row["status"] == "ok"
                && row["artifact_id"] == ARTIFACT
                && row["runtime"] == "cpu_float32_eager"
                && row["labels"] == json!(["human_only", "model_only", "mixed"])
                && row["token_count"]
                    .as_u64()
                    .is_some_and(|n| (1..=2048).contains(&n)),
            "Unexpected, failed, unsupported or noncanonical response; retain evidence"
        );
        let p = probability(&row["model_or_assistance_probability"])?;
        let probs = array(&row["probabilities"])?
            .iter()
            .map(probability)
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            probs.len() == 3
                && (probs.iter().sum::<f64>() - 1.0).abs() < 1e-6
                && (p - probs[1] - probs[2]).abs() < 1e-6
                && scores.insert(id.to_owned(), p).is_none(),
            "Invalid or duplicate score"
        );
    }
    ensure!(
        scores.len() == requested.len() && plan["unique_texts"] == requested.len(),
        "Incomplete response coverage"
    );
    let mut rows = array(&plan["rows"])?.clone();
    for row in &mut rows {
        let p = *scores
            .get(string(&row["text_sha256"])?)
            .context("Missing candidate score")?;
        let original = *scores
            .get(string(&row["source_sha256"])?)
            .context("Missing original score")?;
        row["probability"] = json!(p);
        row["source_probability"] = json!(original);
        row["delta_percentage_points"] = json!(100.0 * (p - original));
        row["strict_flag"] = json!(p > THRESHOLDS[0]);
        row["loose_flag"] = json!(p > THRESHOLDS[1]);
        row["strict_changed"] = json!((p > THRESHOLDS[0]) != (original > THRESHOLDS[0]));
        row["loose_changed"] = json!((p > THRESHOLDS[1]) != (original > THRESHOLDS[1]));
    }
    let mut summaries = vec![];
    let paths: BTreeSet<_> = rows
        .iter()
        .map(|r| string(&r["document"]))
        .collect::<Result<_>>()?;
    for path in paths {
        let states: Vec<_> = rows.iter().filter(|r| r["document"] == path).collect();
        let control = states
            .iter()
            .find(|r| r["mask"] == 0)
            .context("Missing control")?;
        let edited: Vec<_> = states.iter().filter(|r| r["mask"] != 0).collect();
        let minimum = states
            .iter()
            .map(|r| r["probability"].as_f64().unwrap())
            .fold(f64::INFINITY, f64::min);
        let maximum = states
            .iter()
            .map(|r| r["probability"].as_f64().unwrap())
            .fold(f64::NEG_INFINITY, f64::max);
        summaries.push(json!({"document":path,"stage":control["stage"],"collection":control["collection"],
            "source_probability":control["probability"],"edited_states":edited.len(),"minimum":minimum,"maximum":maximum,
            "largest_absolute_delta_pp":edited.iter().map(|r| r["delta_percentage_points"].as_f64().unwrap().abs()).fold(0.0,f64::max),
            "strict_crossings":edited.iter().filter(|r| r["strict_changed"] == true).count(),
            "loose_crossings":edited.iter().filter(|r| r["loose_changed"] == true).count()}));
    }
    let result = json!({"schema":"slop_ninja_a_edit_stress_diagnostic_v1","artifact_id":ARTIFACT,
        "plan_sha256":sha256(plan_bytes),"responses_sha256":sha256(response_bytes),"review_sha256":plan["review_sha256"],
        "thresholds":THRESHOLDS,"documents":summaries,"rows":rows,"provider_calls":0,"new_training":false,
        "limits":["Three familiar Development source families; this is neither fresh evaluation nor a population accuracy estimate.",
            "All 26 states were manually reviewed before A inference, including density and register concerns. Other prepared states remain pending.",
            "This measures sensitivity to fixed edits, not agreement with Pangram. No new provider calls.",
            "Human versus model labels describe the source workflow. Original model generations are not necessarily faithful to their human ancestors.",
            "No author approval, training admission or full-parse correctness certification."]});
    save(out, &result)?;
    println!("{}", result["documents"]);
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::Freeze { study, review, out } => freeze(&study, &review, &out),
        Command::Report {
            prepared,
            responses,
            out,
        } => report(&prepared, &responses, &out),
    }
}
