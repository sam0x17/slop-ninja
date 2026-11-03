//! Score every captured draft from the closed scientific generation pilot.
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

const PLAN_SCHEMA: &str = "slop_ninja_scientific_pilot_diagnostic_v1";
const REGISTER_SCHEMA: &str = "slop_ninja_scientific_profile_register_diagnostic_v1";
const LIMITATIONS: &str = "Three shared Train source families. Human labels are historical proxies. All captured drafts failed strict fidelity; ten were excluded for source copying. One request timed out without captured text and has no detector result. This is a diagnostic of the existing reference, not an independent benchmark, Pangram comparison or basis for threshold/model promotion.";
const REGISTER_LIMITATIONS: &str = "Three shared Train source families and one new informal draft per generator/source pair. All six drafts failed strict fidelity; two were excluded for source copying. The register prompt changed and generation was stochastic. This is a diagnostic of the existing reference, not a style-only causal experiment, independent benchmark, Pangram comparison or basis for threshold/model promotion.";

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
        #[arg(long)]
        profile_register: bool,
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
struct Config {
    study: PathBuf,
    roots: Binding,
    review: Binding,
    protocol: Binding,
    artifact_manifest: Binding,
    #[serde(default)]
    profile_register: bool,
}
fn string(value: &Value) -> Result<&str> {
    value.as_str().context("Expected string")
}
fn array(value: &Value) -> Result<&Vec<Value>> {
    value.as_array().context("Expected array")
}
fn load(path: &Path, bindings: &mut BTreeMap<PathBuf, String>) -> Result<Vec<u8>> {
    let path = path
        .canonicalize()
        .with_context(|| format!("Locating {}", path.display()))?;
    let bytes = fs::read(&path)?;
    let hash = sha256(&bytes);
    if let Some(prior) = bindings.insert(path, hash.clone()) {
        ensure!(prior == hash, "Input changed during preparation");
    }
    Ok(bytes)
}
fn bound(binding: &Binding, bindings: &mut BTreeMap<PathBuf, String>) -> Result<Vec<u8>> {
    let bytes = load(&binding.path, bindings)?;
    ensure!(sha256(&bytes) == binding.sha256, "Changed declared input");
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
    ensure!(!out.exists(), "Use a new output directory");
    let mut bindings = BTreeMap::new();
    let cfg: Config = serde_json::from_slice(&load(config_path, &mut bindings)?)?;
    let (capture_count, failure_count, admission_count, row_count, plan_schema, review_schema) =
        if cfg.profile_register {
            (
                6,
                0,
                4,
                9,
                REGISTER_SCHEMA,
                "slop_ninja_scientific_profile_register_review_v1",
            )
        } else {
            (
                23,
                1,
                13,
                26,
                PLAN_SCHEMA,
                "slop_ninja_scientific_style_pilot_review_v1",
            )
        };
    let study = cfg.study.canonicalize()?;
    let roots_bytes = bound(&cfg.roots, &mut bindings)?;
    let roots = read_records(&cfg.roots.path)?;
    ensure!(
        sha256(fs::read(&cfg.roots.path)?) == sha256(&roots_bytes),
        "Changed roots"
    );
    ensure!(
        roots.len() == 3
            && roots
                .iter()
                .all(|r| r.origin == Origin::HumanOnly && r.split == Some(Split::Train)),
        "Expected three Train human proxies"
    );
    let sources: BTreeMap<_, _> = roots.iter().map(|r| (r.source_group.as_str(), r)).collect();
    ensure!(sources.len() == 3, "Repeated source family");
    let review: Value = serde_json::from_slice(&bound(&cfg.review, &mut bindings)?)?;
    ensure!(
        review["schema"] == review_schema
            && review["pilot_review_complete"] == true
            && review["detector_scores_consulted"] == false
            && review["complete_outputs_read"] == capture_count
            && review["remaining_80_calls_launched"] == false,
        "Expected closed, unscored pilot review"
    );
    bound(&cfg.protocol, &mut bindings)?;
    let artifact: Value = serde_json::from_slice(&bound(&cfg.artifact_manifest, &mut bindings)?)?;
    ensure!(
        artifact["artifact_id"] == ARTIFACT && artifact["max_tokens"] == 2048,
        "Wrong reference artifact"
    );
    let completion: Value =
        serde_json::from_slice(&load(&study.join("completion.json"), &mut bindings)?)?;
    if cfg.profile_register {
        ensure!(
            completion["exit_code"] == 0
                && completion["qwen_exit_code"] == 0
                && completion["mistral_exit_code"] == 0
                && completion["pilot_call_cap"] == 6
                && completion["phase"] == "both_cases_closed_pending_manual_review"
                && completion["automatic_retries"] == false
                && completion["detector_calls"] == 0,
            "Unexpected register-pilot terminal state"
        );
        for (path, hash) in review["bindings"]
            .as_object()
            .context("Missing register review bindings")?
        {
            let bytes = load(Path::new(path), &mut bindings)?;
            ensure!(
                sha256(bytes) == string(hash)?,
                "Changed register review input"
            );
        }
    } else {
        ensure!(
            completion["exit_code"] == 1
                && completion["phase"] == "generating_fix-slop"
                && completion["remaining_launched"] == false,
            "Unexpected pilot terminal state"
        );
    }
    let attempts = array(&review["attempts"])?;
    let failed = array(&review["operational_failures"])?;
    ensure!(
        attempts.len() == capture_count && failed.len() == failure_count,
        "Incomplete attempt accounting"
    );
    let mut expected = BTreeSet::new();
    for generator in ["qwen", "mistral"] {
        let profiles: &[&str] = if cfg.profile_register {
            &["informal"]
        } else {
            &["source-matched", "plain", "informal", "fix-slop"]
        };
        for &profile in profiles {
            for source in &roots {
                expected.insert((
                    generator.to_owned(),
                    profile.to_owned(),
                    source.source_group.clone(),
                ));
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut tasks = BTreeSet::new();
    let mut rows = Vec::new();
    let mut requests = BTreeMap::new();
    let mut source_tokens = BTreeMap::new();
    let mut absent_files = Vec::new();
    for entry in attempts.iter().chain(failed.iter()) {
        let generator = string(&entry["generator"])?;
        let profile = string(&entry["profile"])?;
        let group = string(&entry["source_group"])?;
        let key = (generator.to_owned(), profile.to_owned(), group.to_owned());
        ensure!(
            expected.contains(&key) && seen.insert(key),
            "Unexpected or repeated matrix entry"
        );
        let source = sources.get(group).context("Missing source")?;
        let task = string(&entry["task"])?;
        ensure!(
            task.len() == 64
                && task.bytes().all(|b| b.is_ascii_hexdigit())
                && tasks.insert(task.to_owned()),
            "Invalid task"
        );
        let cell = study
            .join("results")
            .join(if cfg.profile_register {
                generator
            } else {
                profile
            })
            .join(generator);
        let run: Value = serde_json::from_slice(&load(&cell.join("run.json"), &mut bindings)?)?;
        ensure!(
            run["input_sha256"] == sha256(&roots_bytes)
                && run["split"] == "train"
                && run["operations"] == json!(["draft"]),
            "Wrong run ancestry"
        );
        let call = cell.join("calls").join(task);
        let request_bytes = load(&call.join("request.json"), &mut bindings)?;
        ensure!(
            sha256(&request_bytes) == entry["request_sha256"],
            "Changed request"
        );
        let request: Value = serde_json::from_slice(&request_bytes)?;
        if cfg.profile_register {
            ensure!(
                run["prompt_version"] == "source-conditioned-draft-v3-scientific-profile-register"
                    && run["scientific_register_policy"] == "profile-audience-v1"
                    && run["request_timeout_seconds"] == 1800,
                "Wrong register policy"
            );
            let model: Value = serde_json::from_slice(&load(
                &study.join(format!("{generator}.json")),
                &mut bindings,
            )?)?;
            ensure!(run["model"] == model, "Changed model provenance");
            let planned = load(
                &study
                    .join("planned")
                    .join(generator)
                    .join("planned-requests.jsonl"),
                &mut bindings,
            )?;
            let planned = std::str::from_utf8(&planned)?
                .lines()
                .map(serde_json::from_str::<Value>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(planned.len() == 3, "Wrong prospective request count");
            let matched: Vec<_> = planned.iter().filter(|r| r["task"] == task).collect();
            ensure!(
                matched.len() == 1
                    && matched[0]["request"] == request
                    && matched[0]["parent_id"] == source.id
                    && matched[0]["source_group"] == group,
                "Capture differs from prospective request"
            );
        }
        ensure!(
            string(&request["messages"][1]["content"])?
                .contains(&format!("<source>\n{}\n</source>", source.text)),
            "Wrong prompt source"
        );
        let profile_record: Value =
            serde_json::from_slice(&load(&call.join("profile.json"), &mut bindings)?)?;
        ensure!(profile_record["profile"]["id"] == profile, "Wrong profile");
        if entry["kind"] == "request_timeout" {
            ensure!(
                entry["response_captured"] == false && entry["source_sha256"] == source.text_sha256,
                "Wrong failed source"
            );
            let error = load(&call.join("error.txt"), &mut bindings)?;
            ensure!(
                sha256(&error) == entry["error_sha256"]
                    && std::str::from_utf8(&error)?.contains("operation timed out"),
                "Changed timeout"
            );
            for name in ["response.json", "record.json", "http-status.txt"] {
                let p = call.join(name);
                ensure!(!p.exists(), "Previously absent capture appeared");
                absent_files.push(p);
            }
            continue;
        }
        ensure!(
            entry["full_source_and_output_read"] == true && entry["strict_fidelity"] == "fail",
            "Incomplete manual review"
        );
        let status = load(&call.join("http-status.txt"), &mut bindings)?;
        ensure!(
            std::str::from_utf8(&status)?.trim() == "200",
            "Failed HTTP response"
        );
        let response_bytes = load(&call.join("response.json"), &mut bindings)?;
        ensure!(
            sha256(&response_bytes) == entry["raw_response_sha256"],
            "Changed response"
        );
        let response: Value = serde_json::from_slice(&response_bytes)?;
        ensure!(
            array(&response["choices"])?.len() == 1
                && response["choices"][0]["finish_reason"] == "stop",
            "Incomplete draft"
        );
        let text = string(&response["choices"][0]["message"]["content"])?.trim();
        let hash = sha256(text);
        ensure!(
            entry["draft_sha256"] == hash
                && requests.insert(hash.clone(), text.to_owned()).is_none(),
            "Changed or duplicate draft"
        );
        let admitted = call.join("record.json").exists();
        ensure!(
            entry["automatic_origin_admission"] == admitted,
            "Admission changed"
        );
        if admitted {
            ensure!(
                !call.join("error.txt").exists(),
                "Admitted draft has an error"
            );
            absent_files.push(call.join("error.txt"));
            let record: Value =
                serde_json::from_slice(&load(&call.join("record.json"), &mut bindings)?)?;
            ensure!(
                record["text"] == text
                    && record["text_sha256"] == hash
                    && record["parent_id"] == source.id
                    && record["source_group"] == group
                    && record["split"] == "train"
                    && record["origin"] == "model_only"
                    && record["generation"]["model_revision"] == run["model"]["revision"]
                    && record["generation"]["prompt_profile"] == profile_record,
                "Changed admitted provenance"
            );
        } else {
            let error = load(&call.join("error.txt"), &mut bindings)?;
            let error = std::str::from_utf8(&error)?.trim();
            ensure!(
                entry["admission_error"] == error && error.starts_with("draft_source_copy:"),
                "Changed exclusion"
            );
            absent_files.push(call.join("record.json"));
        }
        let n = entry["draft_a_tokens"]
            .as_u64()
            .context("Missing draft token count")?;
        let human_n = entry["source_a_tokens"]
            .as_u64()
            .context("Missing source token count")?;
        ensure!(
            entry["a_token_limit"] == 2048 && entry["whole_draft_supported"] == json!(n <= 2048),
            "Wrong reviewed coverage"
        );
        if let Some(prior) = source_tokens.insert(group.to_owned(), human_n) {
            ensure!(prior == human_n, "Inconsistent source count");
        }
        rows.push(json!({"source_id":source.id,"source_group":group,"text_sha256":hash,"origin":"model_only","generator":generator,"profile":profile,"model_revision":run["model"]["revision"],"admitted":admitted,"expected_tokens":n,"review":entry}));
    }
    ensure!(
        seen == expected
            && rows.iter().filter(|r| r["admitted"] == true).count() == admission_count,
        "Wrong matrix or admission count"
    );
    for source in &roots {
        ensure!(
            requests
                .insert(source.text_sha256.clone(), source.text.clone())
                .is_none(),
            "Repeated human text"
        );
        rows.push(json!({"source_id":source.id,"source_group":source.source_group,"text_sha256":source.text_sha256,"origin":"human_only","generator":"human_proxy","profile":"original","expected_tokens":source_tokens[&source.source_group],"evidence":"historical_proxy"}));
    }
    ensure!(rows.len() == row_count, "Wrong scoring coverage");
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
        &json!({"schema":plan_schema,"artifact_id":ARTIFACT,"thresholds_strictly_greater_than":THRESHOLDS,"requests_sha256":sha256(&bytes),"bindings":bindings,"absent_files":absent_files,"rows":rows,"unscored_attempts":failed,"training_admission":false,"provider_calls":0}),
    )?;
    println!(
        "{}",
        json!({"status":"prepared","requests":requests.len(),"unscored_attempts":failed.len(),"model_calls":0})
    );
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::Prepare { config, out } => prepare(&config, &out),
        Command::Report {
            prepared,
            responses,
            out,
            profile_register,
        } => frozen_pilot::report(
            &prepared,
            &responses,
            &out,
            &frozen_pilot::Policy {
                plan_schema: if profile_register {
                    REGISTER_SCHEMA
                } else {
                    PLAN_SCHEMA
                },
                report_schema: if profile_register {
                    "slop_ninja_scientific_profile_register_diagnostic_report_v1"
                } else {
                    "slop_ninja_scientific_pilot_diagnostic_report_v1"
                },
                expected_rows: if profile_register { 9 } else { 26 },
                limitations: if profile_register {
                    REGISTER_LIMITATIONS
                } else {
                    LIMITATIONS
                },
            },
        ),
    }
}
