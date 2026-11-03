//! Freeze and score the already reviewed six-source long-style Development pilot.
#[path = "../src/bin/support/frozen_pilot.rs"]
mod frozen_pilot;
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use frozen_pilot::{ARTIFACT, THRESHOLDS};
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{Origin, Split, read_records, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Prepare {
        #[arg(long)]
        config: PathBuf,
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cell {
    generator: String,
    profile: String,
    directory: PathBuf,
    run: Binding,
    reviews: Vec<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    roots: Binding,
    protocol: Binding,
    artifact_manifest: Binding,
    reviews: Vec<Binding>,
    cells: Vec<Cell>,
}
fn string(v: &Value) -> Result<&str> {
    v.as_str().context("Expected string")
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("Expected array")
}
fn load(path: &Path, bindings: &mut BTreeMap<PathBuf, String>) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Reading {}", path.display()))?;
    let hash = sha256(&bytes);
    if let Some(old) = bindings.insert(path.to_owned(), hash.clone()) {
        ensure!(old == hash, "File changed during preparation");
    }
    Ok(bytes)
}
fn bound(binding: &Binding, bindings: &mut BTreeMap<PathBuf, String>) -> Result<Vec<u8>> {
    let bytes = load(&binding.path, bindings)?;
    ensure!(
        sha256(&bytes) == binding.sha256,
        "Binding mismatch: {}",
        binding.path.display()
    );
    Ok(bytes)
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn prepare(config_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a fresh output directory");
    let mut bindings = BTreeMap::new();
    let config: Config = serde_json::from_slice(&load(config_path, &mut bindings)?)?;
    let root_bytes = bound(&config.roots, &mut bindings)?;
    bound(&config.protocol, &mut bindings)?;
    let artifact: Value =
        serde_json::from_slice(&bound(&config.artifact_manifest, &mut bindings)?)?;
    ensure!(
        artifact["artifact_id"] == ARTIFACT && artifact["max_tokens"] == 2048,
        "Wrong reference artifact"
    );
    let roots = read_records(&config.roots.path)?;
    ensure!(
        roots.len() == 6
            && roots
                .iter()
                .all(|r| r.origin == Origin::HumanOnly && r.split == Some(Split::Development)),
        "Expected six human Development roots"
    );
    ensure!(
        sha256(fs::read(&config.roots.path)?) == sha256(&root_bytes),
        "Roots changed"
    );
    let sources: BTreeMap<_, _> = roots.iter().map(|r| (r.id.as_str(), r)).collect();
    ensure!(sources.len() == 6, "Repeated source id");
    let reviews: Vec<Value> = config
        .reviews
        .iter()
        .map(|b| Ok(serde_json::from_slice(&bound(b, &mut bindings)?)?))
        .collect::<Result<_>>()?;
    for review in &reviews {
        ensure!(
            review["source_split"] == "development" && review["detector_scores_opened"] == false,
            "Review was not frozen before scoring"
        );
    }
    let expected: BTreeSet<_> = ["qwen", "phi4"]
        .into_iter()
        .flat_map(|g| {
            ["source-matched", "formal", "editorial"].map(|p| (g.to_owned(), p.to_owned()))
        })
        .collect();
    let actual: BTreeSet<_> = config
        .cells
        .iter()
        .map(|c| (c.generator.clone(), c.profile.clone()))
        .collect();
    ensure!(
        config.cells.len() == 6 && actual == expected,
        "Incomplete model/profile matrix"
    );
    let mut rows = Vec::new();
    let mut requests = BTreeMap::new();
    let mut source_tokens = BTreeMap::new();
    let mut tasks = BTreeSet::new();
    for cell in &config.cells {
        ensure!(
            cell.run.path == cell.directory.join("run.json"),
            "Wrong generation run path"
        );
        let run: Value = serde_json::from_slice(&bound(&cell.run, &mut bindings)?)?;
        ensure!(
            run["input_sha256"] == sha256(&root_bytes)
                && run["split"] == "development"
                && run["operations"] == json!(["draft"]),
            "Changed generation inputs"
        );
        let mut seen = BTreeSet::new();
        for &index in &cell.reviews {
            let review = reviews.get(index).context("Missing review index")?;
            for entry in array(&review["reviews"])? {
                if entry.get("generator").is_some_and(|v| v != &cell.generator)
                    || entry.get("profile").is_some_and(|v| v != &cell.profile)
                {
                    continue;
                }
                let source_id = string(&entry["source_id"])?;
                let source = sources
                    .get(source_id)
                    .context("Review outside pilot roots")?;
                ensure!(
                    seen.insert(source_id.to_owned())
                        && entry["source_sha256"] == source.text_sha256,
                    "Repeated or changed reviewed source"
                );
                let task = string(&entry["task"])?;
                ensure!(
                    task.len() == 64
                        && task.bytes().all(|b| b.is_ascii_hexdigit())
                        && tasks.insert(task.to_owned()),
                    "Invalid/repeated task"
                );
                let call = cell.directory.join("calls").join(task);
                let response_bytes = load(&call.join("response.json"), &mut bindings)?;
                ensure!(
                    sha256(&response_bytes) == entry["raw_response_sha256"],
                    "Changed reviewed response"
                );
                let response: Value = serde_json::from_slice(&response_bytes)?;
                ensure!(
                    response["choices"][0]["finish_reason"] == "stop",
                    "Incomplete response"
                );
                let text = string(&response["choices"][0]["message"]["content"])?.trim();
                let hash = sha256(text);
                ensure!(entry["draft_sha256"] == hash, "Changed reviewed draft");
                let request: Value =
                    serde_json::from_slice(&load(&call.join("request.json"), &mut bindings)?)?;
                ensure!(
                    array(&request["messages"])?
                        .iter()
                        .any(|m| m["content"].as_str().is_some_and(
                            |s| s.contains(&format!("<source>\n{}\n</source>", source.text))
                        )),
                    "Request source mismatch"
                );
                let profile: Value =
                    serde_json::from_slice(&load(&call.join("profile.json"), &mut bindings)?)?;
                ensure!(
                    profile["profile"]["id"] == cell.profile
                        && profile["assignment"]["split"] == "development"
                        && profile["assignment"]["source_group_sha256"]
                            == sha256(&source.source_group),
                    "Profile/source assignment mismatch"
                );
                let admitted = call.join("record.json").exists();
                let admission = if admitted {
                    let record: Value =
                        serde_json::from_slice(&load(&call.join("record.json"), &mut bindings)?)?;
                    ensure!(
                        record["text"] == text
                            && record["text_sha256"] == hash
                            && record["parent_id"] == source.id
                            && record["split"] == "development"
                            && record["origin"] == "model_only"
                            && record["generation"]["model_revision"] == run["model"]["revision"],
                        "Admitted record mismatch"
                    );
                    json!("admitted")
                } else {
                    json!(String::from_utf8(load(
                        &call.join("error.txt"),
                        &mut bindings
                    )?)?)
                };
                let n = entry["draft_a_tokens"]
                    .as_u64()
                    .context("Missing reviewed token count")?;
                let human_n = entry["source_a_tokens"]
                    .as_u64()
                    .context("Missing source token count")?;
                if let Some(old) = source_tokens.insert(source_id.to_owned(), human_n) {
                    ensure!(old == human_n, "Inconsistent source token count");
                }
                ensure!(
                    entry["a_token_limit"] == 2048
                        && entry["whole_draft_supported"] == json!(n <= 2048),
                    "Reviewed coverage mismatch"
                );
                ensure!(
                    requests.insert(hash.clone(), text.to_owned()).is_none(),
                    "Duplicate exact draft"
                );
                rows.push(json!({"text_sha256":hash,"source_group":source.source_group,"source_id":source.id,"origin":"model_only","generator":cell.generator,"profile":cell.profile,"model_revision":run["model"]["revision"],"expected_tokens":n,"admitted":admitted,"admission":admission,"review":entry}));
            }
        }
        ensure!(seen.len() == 6, "Incomplete cell");
    }
    ensure!(
        rows.len() == 36 && rows.iter().filter(|r| r["admitted"] == true).count() == 33,
        "Unexpected reviewed admission coverage"
    );
    for source in &roots {
        ensure!(
            requests
                .insert(source.text_sha256.clone(), source.text.clone())
                .is_none(),
            "Repeated source text"
        );
        rows.push(json!({"text_sha256":source.text_sha256,"source_group":source.source_group,"source_id":source.id,"origin":"human_only","generator":"human_proxy","profile":"original","expected_tokens":source_tokens[&source.id],"evidence":"historical_proxy"}));
    }
    let mut bytes = Vec::new();
    for (id, text) in &requests {
        serde_json::to_writer(&mut bytes, &json!({"id":id,"text":text}))?;
        bytes.push(b'\n');
    }
    fs::create_dir_all(out)?;
    let mut f = fs::File::create_new(out.join("requests.jsonl"))?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    save(
        &out.join("plan.json"),
        &json!({"schema":"slop_ninja_long_pilot_diagnostic_v1","artifact_id":ARTIFACT,"thresholds_strictly_greater_than":THRESHOLDS,"requests_sha256":sha256(&bytes),"bindings":bindings,"rows":rows,"training_admission":false,"provider_calls":0}),
    )?;
    println!(
        "{}",
        json!({"status":"prepared","requests":requests.len(),"model_calls":0})
    );
    Ok(())
}
fn report(prepared: &Path, responses: &Path, out: &Path) -> Result<()> {
    frozen_pilot::report(
        prepared,
        responses,
        out,
        &frozen_pilot::Policy {
            plan_schema: "slop_ninja_long_pilot_diagnostic_v1",
            report_schema: "slop_ninja_long_pilot_diagnostic_report_v1",
            expected_rows: 42,
            limitations: "Six shared Development source families. Human labels are historical proxies; drafts failed strict fidelity. Support counts remain separate from detections. No fresh confirmation, Pangram comparison, threshold tuning or model promotion.",
        },
    )
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::Prepare { config, out } => prepare(&config, &out),
        Command::Report {
            prepared,
            responses,
            out,
        } => report(&prepared, &responses, &out),
    }
}
