//! Run a bounded local MLX campaign, retaining each server and generation log.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Split, sha256};
use slop_ninja_detector::generation::{self, RevisionStyle};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    input: PathBuf,
    input_sha256: String,
    generator: PathBuf,
    generator_sha256: String,
    python: PathBuf,
    runtime_files: BTreeMap<PathBuf, String>,
    /// Exact Jinja template text, bound in runtime_files; omitted uses model defaults.
    chat_template: Option<PathBuf>,
    protocol: PathBuf,
    protocol_sha256: String,
    profile_ids: Vec<String>,
    #[serde(default)]
    drafts_only: bool,
    #[serde(default)]
    source_relative_length: bool,
    #[serde(default)]
    scientific_profile_register: bool,
    #[serde(default)]
    omit_word_count_target: bool,
    #[serde(default)]
    source_fidelity_check: bool,
    request_timeout_seconds: Option<u64>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    model_dir: PathBuf,
    model_spec: PathBuf,
    model_spec_sha256: String,
    server_script: Option<PathBuf>,
    port: u16,
    split: Option<Split>,
    max_calls: usize,
    /// Absent for initial draft/edit pairs; present for one call per model-only leaf.
    revision_style: Option<RevisionStyle>,
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn check_hash(path: &Path, expected: &str) -> Result<()> {
    ensure!(
        sha256(fs::read(path)?) == expected,
        "Input changed: {}",
        path.display()
    );
    Ok(())
}

fn check_inputs(plan: &Plan) -> Result<()> {
    ensure!(
        !(plan.omit_word_count_target || plan.source_fidelity_check)
            || (plan.drafts_only && plan.cases.iter().all(|c| c.revision_style.is_none())),
        "Draft prompt policies require draft-only cases"
    );
    ensure!(
        plan.request_timeout_seconds
            .is_none_or(|s| (1..=86400).contains(&s)),
        "Request deadline must be 1..86400 seconds"
    );
    ensure!(
        !plan.scientific_profile_register
            || (plan.drafts_only
                && !plan.profile_ids.is_empty()
                && plan.cases.iter().all(|c| c.revision_style.is_none())),
        "Profile-register mode requires profiled draft-only cases"
    );
    check_hash(&plan.input, &plan.input_sha256)?;
    check_hash(&plan.generator, &plan.generator_sha256)?;
    check_hash(&plan.protocol, &plan.protocol_sha256)?;
    for (path, hash) in &plan.runtime_files {
        check_hash(path, hash)?;
    }
    if let Some(path) = &plan.chat_template {
        ensure!(
            plan.runtime_files.contains_key(path),
            "Unpinned chat template"
        );
    }
    for case in &plan.cases {
        check_hash(&case.model_spec, &case.model_spec_sha256)?;
    }
    Ok(())
}

fn add_chat_template(command: &mut Command, plan: &Plan) -> Result<()> {
    if let Some(path) = &plan.chat_template {
        let expected = plan
            .runtime_files
            .get(path)
            .context("Unpinned chat template")?;
        let template = fs::read_to_string(path)?;
        ensure!(sha256(&template) == *expected, "Chat template changed");
        ensure!(!template.trim().is_empty(), "Empty chat template");
        command.arg("--chat-template").arg(template);
    }
    Ok(())
}

fn invocation(command: &Command) -> Value {
    json!({"program": command.get_program().to_string_lossy(),
        "args": command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        "environment_overrides": command.get_envs().map(|(key,value)|
            (key.to_string_lossy().into_owned(),value.map(|v|v.to_string_lossy().into_owned())))
            .collect::<BTreeMap<_,_>>()})
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn generation_command(plan: &Plan, case: &Case, cohort: &Path, base_url: &str) -> Result<Command> {
    ensure!(
        !(plan.omit_word_count_target || plan.source_fidelity_check)
            || (plan.drafts_only && case.revision_style.is_none()),
        "Draft prompt policies require draft-only cases"
    );
    let mut generate = Command::new(&plan.generator);
    generate
        .arg("--input")
        .arg(&plan.input)
        .arg("--model-spec")
        .arg(&case.model_spec)
        .arg("--output-dir")
        .arg(cohort)
        .arg("--base-url")
        .arg(base_url)
        .args(["--concurrency", "1", "--max-calls"])
        .arg(case.max_calls.to_string());
    if let Some(style) = case.revision_style {
        ensure!(
            !plan.drafts_only,
            "Draft-only campaigns cannot revise model leaves"
        );
        ensure!(
            plan.profile_ids.is_empty(),
            "Revision campaigns require empty profile_ids"
        );
        generate
            .args(["--model-revisions", "--revision-style"])
            .arg(serde_json::to_value(style)?.as_str().unwrap());
    } else {
        generate
            .args([
                "--prompt-profile-set",
                "style-mix-v1",
                "--prompt-profile-ids",
            ])
            .arg(plan.profile_ids.join(","));
        if plan.drafts_only {
            generate.arg("--drafts-only");
        }
    }
    if plan.source_relative_length {
        generate.arg("--source-relative-length");
    }
    if plan.scientific_profile_register {
        generate.arg("--scientific-profile-register");
    }
    if plan.omit_word_count_target {
        generate.arg("--omit-word-count-target");
    }
    if plan.source_fidelity_check {
        generate.arg("--source-fidelity-check");
    }
    if let Some(seconds) = plan.request_timeout_seconds {
        generate
            .arg("--request-timeout-seconds")
            .arg(seconds.to_string());
    }
    generate.arg("--complete-families-only");
    if let Some(split) = case.split {
        generate
            .arg("--split")
            .arg(serde_json::to_value(split)?.as_str().unwrap());
    }
    Ok(generate)
}

fn request_count(plan: &Plan, case: &Case, records: &[dataset::OriginRecord]) -> Result<usize> {
    let count = if case.revision_style.is_some() {
        ensure!(
            !plan.drafts_only,
            "Draft-only campaigns cannot revise model leaves"
        );
        ensure!(
            plan.profile_ids.is_empty(),
            "Revision campaigns require empty profile_ids"
        );
        generation::revision_parents(records)?
            .iter()
            .filter(|r| case.split.is_none() || r.split == case.split)
            .count()
    } else {
        ensure!(
            records
                .iter()
                .all(|r| r.parent_id.is_none() && r.split.is_some()),
            "Initial generation requires frozen source roots"
        );
        (if plan.drafts_only { 1 } else { 2 })
            * records
                .iter()
                .filter(|r| case.split.is_none() || r.split == case.split)
                .count()
    };
    ensure!(
        count > 0 && case.max_calls == count,
        "Call cap must equal the selected request count ({count})"
    );
    Ok(count)
}

fn run_case(plan: &Plan, case: &Case, output: &Path) -> Result<Value> {
    check_inputs(plan)?;
    // Never adopt an unrelated listener or send a request to its default model.
    let port_guard = TcpListener::bind(("127.0.0.1", case.port))
        .context("Campaign port already occupied; inspect its owner")?;
    let spec: Value = serde_json::from_slice(&fs::read(&case.model_spec)?)?;
    let max_tokens = spec["max_tokens"]
        .as_u64()
        .context("Missing generation token cap")?;
    // The MLX model-list endpoint scans this directory even for local weights.
    let hf_home = output.join(format!("{}-hf-cache", case.name));
    let hf_hub = hf_home.join("hub");
    fs::create_dir_all(&hf_hub)?;
    let mut server_command = Command::new(&plan.python);
    if let Some(script) = &case.server_script {
        ensure!(
            plan.runtime_files.contains_key(script),
            "Unpinned server adapter"
        );
        server_command.arg(script);
    } else {
        server_command.args(["-m", "mlx_lm.server"]);
    }
    server_command
        .arg("--model")
        .arg(&case.model_dir)
        .args(["--host", "127.0.0.1", "--port"])
        .arg(case.port.to_string())
        .arg("--allowed-origins")
        .arg(format!("http://127.0.0.1:{}", case.port))
        .args([
            "--decode-concurrency",
            "1",
            "--prompt-concurrency",
            "1",
            "--prompt-cache-size",
            "0",
            "--max-tokens",
        ])
        .arg(max_tokens.to_string())
        .env("HF_HUB_OFFLINE", "1")
        .env("HF_HOME", &hf_home)
        .env("HF_HUB_CACHE", &hf_hub)
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONUNBUFFERED", "1");
    add_chat_template(&mut server_command, plan)?;
    write_json(
        &output.join(format!("{}-server-invocation.json", case.name)),
        &invocation(&server_command),
    )?;
    let server_log = fs::File::create_new(output.join(format!("{}-server.log", case.name)))?;
    drop(port_guard);
    let mut server = Server(
        server_command
            .stdin(Stdio::null())
            .stdout(server_log.try_clone()?)
            .stderr(server_log)
            .spawn()?,
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let base_url = format!("http://127.0.0.1:{}/v1", case.port);
    let started = Instant::now();
    loop {
        ensure!(
            server.0.try_wait()?.is_none(),
            "Model server exited before readiness"
        );
        if let Ok(response) = client.get(format!("{base_url}/models")).send()
            && response.status().is_success()
            && let Ok(catalog) = response.json::<Value>()
            && catalog["data"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        {
            write_json(
                &output.join(format!("{}-readiness.json", case.name)),
                &json!({"pid":server.0.id(),"seconds":started.elapsed().as_secs_f64(),"catalog":catalog}),
            )?;
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(120),
            "Model server readiness timed out"
        );
        thread::sleep(Duration::from_secs(1));
    }
    let cohort = output.join(&case.name);
    let mut generate = generation_command(plan, case, &cohort, &base_url)?;
    write_json(
        &output.join(format!("{}-generation-invocation.json", case.name)),
        &invocation(&generate),
    )?;
    let generation_log =
        fs::File::create_new(output.join(format!("{}-generation.log", case.name)))?;
    println!(
        "Generating {}: at most {} local calls",
        case.name, case.max_calls
    );
    let status = generate
        .stdin(Stdio::null())
        .stdout(generation_log.try_clone()?)
        .stderr(generation_log)
        .status()?;
    ensure!(
        status.success(),
        "Generation failed for {}; preserve its logs and responses",
        case.name
    );
    let summary_bytes = fs::read(cohort.join("summary.json"))?;
    let summary: Value = serde_json::from_slice(&summary_bytes)?;
    ensure!(
        summary["requested"] == case.max_calls,
        "Generator task count differs from the frozen campaign"
    );
    if plan.drafts_only {
        ensure!(
            summary["operations"] == json!(["draft"]),
            "Expected only draft operations"
        );
    }
    ensure!(
        summary.get("output_length_policy")
            == plan
                .source_relative_length
                .then(|| json!("source-relative-v1"))
                .as_ref(),
        "Generator output-length policy differs from the frozen campaign"
    );
    ensure!(
        summary.get("scientific_register_policy")
            == plan
                .scientific_profile_register
                .then(|| json!("profile-audience-v1"))
                .as_ref(),
        "Generator register policy differs from the frozen campaign"
    );
    ensure!(
        summary.get("request_timeout_seconds")
            == plan.request_timeout_seconds.map(|s| json!(s)).as_ref(),
        "Generator deadline differs from the frozen campaign"
    );
    for (key, expected) in [
        (
            "word_count_prompt_policy",
            plan.omit_word_count_target.then_some("omit-target-v1"),
        ),
        (
            "source_fidelity_prompt_policy",
            plan.source_fidelity_check.then_some("claim-check-v1"),
        ),
    ] {
        ensure!(
            summary.get(key) == expected.map(|s| json!(s)).as_ref(),
            "Generator {key} differs from the frozen campaign"
        );
    }
    ensure!(
        summary["status"] == "paused"
            || (summary["status"] == "complete" && summary["complete_export_ready"] == true),
        "Generation did not produce a terminal or paused cohort"
    );
    check_inputs(plan)?;
    let mut result = json!({"name":case.name,"status":summary["status"],"requested":summary["requested"],
        "completed":summary["completed"],"excluded_root_count":summary["excluded_root_count"],
        "summary_sha256":sha256(summary_bytes),"seconds":started.elapsed().as_secs_f64()});
    if let Some(style) = case.revision_style {
        ensure!(
            summary["cohort_kind"] == "model_only_revision_chains",
            "Expected a model-only revision cohort"
        );
        result["revision_style"] = serde_json::to_value(style)?;
        result["excluded_source_group_count"] = summary["excluded_source_group_count"].clone();
    }
    Ok(result)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.output_dir.exists(),
        "Use a new campaign directory; resume individual cohorts explicitly"
    );
    let plan_bytes = fs::read(&args.plan)?;
    let plan: Plan = serde_json::from_slice(&plan_bytes)?;
    check_inputs(&plan)?;
    let records = dataset::read_records(&plan.input)?;
    ensure!(
        !plan.scientific_profile_register
            || records
                .iter()
                .all(|r| r.source.collection == "plos-introductions-historical-v1"),
        "Profile-register campaigns require scientific introduction roots"
    );
    ensure!(
        (1..=4).contains(&plan.cases.len()),
        "Declare 1..4 local models"
    );
    let mut names = BTreeSet::new();
    for case in &plan.cases {
        ensure!(
            !case.name.is_empty()
                && case
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "Unsafe case name"
        );
        ensure!(names.insert(&case.name), "Duplicate case name");
        request_count(&plan, case, &records)?;
    }
    fs::create_dir(&args.output_dir)?;
    fs::write(args.output_dir.join("plan.json"), &plan_bytes)?;
    let state_path = args.output_dir.join("status.json");
    let mut results = Vec::new();
    for case in &plan.cases {
        if args.output_dir.join("PAUSE").exists() {
            write_json(
                &state_path,
                &json!({"status":"paused","results":results,"pangram_calls":0}),
            )?;
            return Ok(());
        }
        write_json(
            &state_path,
            &json!({"status":"running","case":case.name,"results":results,"pangram_calls":0}),
        )?;
        match run_case(&plan, case, &args.output_dir) {
            Ok(result) => {
                let paused = result["status"] == "paused";
                results.push(result);
                if paused {
                    write_json(
                        &state_path,
                        &json!({"status":"paused","results":results,"pangram_calls":0}),
                    )?;
                    return Ok(());
                }
            }
            Err(error) => {
                write_json(
                    &state_path,
                    &json!({"status":"failed","case":case.name,"error":format!("{error:#}"),"results":results,"pangram_calls":0}),
                )?;
                return Err(error);
            }
        }
    }
    write_json(
        &state_path,
        &json!({"status":"complete","results":results,"pangram_calls":0,"detector_predictions_opened":false}),
    )?;
    println!("Generation campaign complete; no detector was fitted or evaluated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        serde_json::from_value(json!({
            "input":"source.jsonl", "input_sha256":"unused", "generator":"generator",
            "generator_sha256":"unused", "python":"python", "runtime_files":{},
            "protocol":"protocol.md", "protocol_sha256":"unused", "profile_ids":["anti-ai"],
            "cases":[{"name":"test", "model_dir":"weights", "model_spec":"model.json",
                "model_spec_sha256":"unused", "port":18123, "split":"test", "max_calls":234}]
        }))
        .unwrap()
    }

    #[test]
    fn draft_prompt_policies_reach_the_generator_and_reject_other_operations() -> Result<()> {
        let mut plan = plan();
        plan.omit_word_count_target = true;
        plan.source_fidelity_check = true;
        let cohort = Path::new("cohort");
        let base = "http://127.0.0.1:18123/v1";
        assert!(generation_command(&plan, &plan.cases[0], cohort, base).is_err());
        plan.drafts_only = true;
        let args = invocation(&generation_command(&plan, &plan.cases[0], cohort, base)?);
        for flag in ["--omit-word-count-target", "--source-fidelity-check"] {
            assert!(args["args"].as_array().unwrap().contains(&json!(flag)));
        }
        plan.cases[0].revision_style = Some(RevisionStyle::AntiAi);
        assert!(generation_command(&plan, &plan.cases[0], cohort, base).is_err());
        Ok(())
    }

    #[test]
    fn template_override_requires_bound_bytes_and_leaves_defaults_unchanged() -> Result<()> {
        let mut plan = plan();
        let mut command = Command::new("python");
        add_chat_template(&mut command, &plan)?;
        assert!(command.get_args().next().is_none());
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("template.jinja");
        let template = "Current date: 2026-09-15\n{{ messages[0]['content'] }}";
        fs::write(&path, template)?;
        plan.chat_template = Some(path.clone());
        assert!(add_chat_template(&mut command, &plan).is_err());
        plan.runtime_files.insert(path.clone(), sha256(template));
        add_chat_template(&mut command, &plan)?;
        assert_eq!(
            invocation(&command)["args"],
            json!(["--chat-template", template])
        );
        fs::write(&path, "changed")?;
        assert!(add_chat_template(&mut Command::new("python"), &plan).is_err());
        Ok(())
    }

    #[test]
    fn legacy_and_revision_commands_keep_their_distinct_prompt_contracts() -> Result<()> {
        let mut plan = plan();
        let cohort = Path::new("cohort");
        let base = "http://127.0.0.1:18123/v1";
        let expected_prefix = vec![
            "--input",
            "source.jsonl",
            "--model-spec",
            "model.json",
            "--output-dir",
            "cohort",
            "--base-url",
            base,
            "--concurrency",
            "1",
            "--max-calls",
        ];
        let mut legacy = expected_prefix.clone();
        legacy.extend([
            "234",
            "--prompt-profile-set",
            "style-mix-v1",
            "--prompt-profile-ids",
            "anti-ai",
            "--complete-families-only",
            "--split",
            "test",
        ]);
        assert_eq!(
            invocation(&generation_command(&plan, &plan.cases[0], cohort, base)?)["args"],
            json!(legacy)
        );
        plan.drafts_only = true;
        let draft_command = invocation(&generation_command(&plan, &plan.cases[0], cohort, base)?);
        assert!(
            draft_command["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--drafts-only"))
        );
        plan.source_relative_length = true;
        assert!(
            invocation(&generation_command(&plan, &plan.cases[0], cohort, base)?)["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--source-relative-length"))
        );
        plan.source_relative_length = false;
        plan.cases[0].revision_style = Some(RevisionStyle::AntiAi);
        assert!(generation_command(&plan, &plan.cases[0], cohort, base).is_err());
        plan.drafts_only = false;
        plan.cases[0].revision_style = Some(RevisionStyle::AntiAi);
        assert!(generation_command(&plan, &plan.cases[0], cohort, base).is_err());
        plan.profile_ids.clear();
        plan.cases[0].max_calls = 117;
        for (style, name) in [
            (RevisionStyle::AntiAi, "anti-ai"),
            (RevisionStyle::FixSlop, "fix-slop"),
        ] {
            plan.cases[0].revision_style = Some(style);
            let mut expected = expected_prefix.clone();
            expected.extend([
                "117",
                "--model-revisions",
                "--revision-style",
                name,
                "--complete-families-only",
                "--split",
                "test",
            ]);
            assert_eq!(
                invocation(&generation_command(&plan, &plan.cases[0], cohort, base)?)["args"],
                json!(expected)
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a closed local revision archive and its frozen input"]
    fn archived_revision_campaign_counts_only_the_next_stage() -> Result<()> {
        let archive = PathBuf::from(std::env::var("SLOP_NINJA_REVISION_ARCHIVE")?);
        let input = PathBuf::from(std::env::var("SLOP_NINJA_REVISION_INPUT")?);
        ensure!(!archive.join("run.lock").exists(), "Archive is still open");
        let run: Value = serde_json::from_slice(&fs::read(archive.join("run.json"))?)?;
        let summary: Value = serde_json::from_slice(&fs::read(archive.join("summary.json"))?)?;
        ensure!(summary["status"] == "complete", "Archive is incomplete");
        ensure!(
            run["input_sha256"] == sha256(fs::read(&input)?),
            "Wrong input"
        );
        let records = dataset::read_records(&input)?;
        let mut plan = plan();
        plan.profile_ids.clear();
        plan.cases[0].revision_style = Some(serde_json::from_value(run["revision_style"].clone())?);
        plan.cases[0].split = serde_json::from_value(run["split"].clone())?;
        let expected = summary["requested"]
            .as_u64()
            .context("Missing task count")? as usize;
        plan.cases[0].max_calls = expected;
        assert_eq!(request_count(&plan, &plan.cases[0], &records)?, expected);
        plan.cases[0].max_calls += 1;
        assert!(request_count(&plan, &plan.cases[0], &records).is_err());
        plan.cases[0].max_calls = expected;
        plan.cases[0].revision_style = None;
        assert!(request_count(&plan, &plan.cases[0], &records).is_err());
        Ok(())
    }
}
