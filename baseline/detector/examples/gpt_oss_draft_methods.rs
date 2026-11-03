//! Prepare and run a bounded factorial prompt-method comparison on exposed Train roots.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    generation::{ModelSpec, ResponseFormat},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const INPUT: &str = "461dd77d7d440c2b77091a47e08ef86ffbff6957b80497a0741c7b792c6f484a";
const SPEC: &str = "7f47de78513eabdc6204af7baa9a3323bfeec65f4c55640f178f73b987b442b8";
const CONTRACT: &str = "762c18847a9c8e476343199d47ac782791219198a2f6ad34d02f3f704511152e";
const PROFILES: [&str; 6] = [
    "source-matched",
    "plain",
    "informal",
    "direct",
    "anti-ai",
    "fix-slop",
];
const CONDITIONS: [(&str, bool, bool); 4] = [
    ("control", false, false),
    ("no-target", true, false),
    ("claim-check", false, true),
    ("both", true, true),
];

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    Prepare {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        pilot: PathBuf,
        #[arg(long)]
        generator: PathBuf,
        #[arg(long)]
        campaign: PathBuf,
    },
    Run {
        #[arg(long)]
        root: PathBuf,
    },
}
fn read(path: impl AsRef<Path>) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn hash(path: impl AsRef<Path>) -> Result<String> {
    Ok(sha256(fs::read(path)?))
}
fn save(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn prospective(
    generator: &Path,
    input: &Path,
    spec: &Path,
    output: &Path,
    profile: &str,
    omit: bool,
    check: bool,
) -> Result<()> {
    let mut command = Command::new(generator);
    command
        .arg("--input")
        .arg(input)
        .arg("--model-spec")
        .arg(spec)
        .arg("--output-dir")
        .arg(output)
        .args([
            "--prompt-profile-set",
            "style-mix-v1",
            "--prompt-profile-ids",
            profile,
            "--drafts-only",
            "--complete-families-only",
            "--split",
            "train",
            "--concurrency",
            "1",
            "--max-calls",
            "4",
            "--request-timeout-seconds",
            "1800",
            "--base-url",
            "http://127.0.0.1:18139/v1",
            "--plan-only",
        ]);
    if omit {
        command.arg("--omit-word-count-target");
    }
    if check {
        command.arg("--source-fidelity-check");
    }
    let result = command.output()?;
    ensure!(
        result.status.success(),
        "Planning failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
fn prepare(root: PathBuf, pilot: PathBuf, generator: PathBuf, campaign: PathBuf) -> Result<()> {
    ensure!(!root.exists(), "Use a fresh study directory");
    ensure!(
        hash(pilot.join("input.jsonl"))? == INPUT
            && hash(pilot.join("model-spec.json"))? == SPEC
            && hash(pilot.join("source-review-contract.json"))? == CONTRACT,
        "Changed pilot inputs"
    );
    let old_plan = read(pilot.join("plan-source-matched.json"))?;
    for (path, expected) in old_plan["runtime_files"]
        .as_object()
        .context("Runtime bindings")?
    {
        ensure!(
            hash(path)? == expected.as_str().context("Hash")?,
            "Changed runtime {path}"
        );
    }
    fs::create_dir(&root)?;
    let mut compatibility = Vec::new();
    for profile in PROFILES {
        let out = root.join(format!("legacy-{profile}"));
        prospective(
            &generator,
            &pilot.join("input.jsonl"),
            &pilot.join("model-spec.json"),
            &out,
            profile,
            false,
            false,
        )?;
        for name in ["planned-requests.jsonl", "run.json", "profile-plan.json"] {
            ensure!(
                fs::read(out.join(name))?
                    == fs::read(pilot.join(format!("prospective-{profile}")).join(name))?,
                "Changed default {profile}/{name}"
            );
        }
        compatibility.push(
            json!({"profile":profile,"requests_sha256":hash(out.join("planned-requests.jsonl"))?}),
        );
    }
    for name in [
        "input.jsonl",
        "chat_template.jinja",
        "source-review-contract.json",
    ] {
        fs::copy(pilot.join(name), root.join(name))?;
    }
    fs::copy(&generator, root.join("generate_samples"))?;
    fs::copy(&campaign, root.join("generation_campaign"))?;
    fs::write(
        root.join("PROTOCOL.md"),
        include_bytes!("../GPT_OSS_DRAFT_METHODS_V1.md"),
    )?;
    fs::write(
        root.join("source.rs"),
        include_bytes!("gpt_oss_draft_methods.rs"),
    )?;
    let mut spec: ModelSpec = serde_json::from_value(read(pilot.join("model-spec.json"))?)?;
    ensure!(
        matches!(spec.response_format, Some(ResponseFormat::HarmonyTextV1)),
        "Expected frozen V1 pilot"
    );
    spec.response_format = Some(ResponseFormat::HarmonyTextV2);
    save(root.join("model-spec.json"), &serde_json::to_value(spec)?)?;
    let mut conditions = Vec::new();
    for (name, omit, check) in CONDITIONS {
        let out = root.join(format!("prospective-{name}"));
        prospective(
            &root.join("generate_samples"),
            &root.join("input.jsonl"),
            &root.join("model-spec.json"),
            &out,
            "source-matched",
            omit,
            check,
        )?;
        let mut plan = old_plan.clone();
        plan["input"] = json!(root.join("input.jsonl"));
        plan["generator"] = json!(root.join("generate_samples"));
        plan["generator_sha256"] = json!(hash(root.join("generate_samples"))?);
        plan["protocol"] = json!(root.join("PROTOCOL.md"));
        plan["protocol_sha256"] = json!(hash(root.join("PROTOCOL.md"))?);
        plan["chat_template"] = json!(root.join("chat_template.jinja"));
        plan["runtime_files"][root.join("chat_template.jinja").to_str().unwrap()] =
            json!(hash(root.join("chat_template.jinja"))?);
        plan["omit_word_count_target"] = json!(omit);
        plan["source_fidelity_check"] = json!(check);
        plan["profile_ids"] = json!(["source-matched"]);
        plan["cases"][0]["model_spec"] = json!(root.join("model-spec.json"));
        plan["cases"][0]["model_spec_sha256"] = json!(hash(root.join("model-spec.json"))?);
        let path = root.join(format!("plan-{name}.json"));
        save(&path, &plan)?;
        conditions.push(json!({"name":name,"omit_word_count_target":omit,"source_fidelity_check":check,"plan":path,"plan_sha256":hash(&path)?,"prospective":out,"requests_sha256":hash(out.join("planned-requests.jsonl"))?}));
    }
    let prepared = json!({"schema":"slop_ninja_gpt_oss_draft_methods_preparation_v1","at":chrono::Utc::now(),"input_sha256":INPUT,"source_contract_sha256":CONTRACT,"model_spec_sha256":hash(root.join("model-spec.json"))?,"template_sha256":hash(root.join("chat_template.jinja"))?,"generator_sha256":hash(root.join("generate_samples"))?,"campaign_sha256":hash(root.join("generation_campaign"))?,"binary_sha256":hash(std::env::current_exe()?)?,"source_sha256":hash(root.join("source.rs"))?,"protocol_sha256":hash(root.join("PROTOCOL.md"))?,"legacy_requests_reproduced":24,"legacy_compatibility":compatibility,"conditions":conditions,"planned_requests":16,"sources":4,"model_calls":0,"detector_calls":0,"training_archive_changed":false,"rendered_prompt_review":"pending"});
    save(root.join("preparation.json"), &prepared)?;
    println!("{}", serde_json::to_string_pretty(&prepared)?);
    Ok(())
}
fn run(root: PathBuf) -> Result<()> {
    let prepared = read(root.join("preparation.json"))?;
    let check = read(root.join("preflight.json"))?;
    ensure!(
        check["ready_for_generation"] == true
            && check["preparation_sha256"] == hash(root.join("preparation.json"))?,
        "Missing reviewed preparation"
    );
    ensure!(
        prepared["binary_sha256"] == hash(std::env::current_exe()?)?,
        "Changed runner"
    );
    ensure!(
        prepared["campaign_sha256"] == hash(root.join("generation_campaign"))?,
        "Changed campaign"
    );
    save(
        root.join("started.json"),
        &json!({"at":chrono::Utc::now(),"pid":std::process::id(),"preparation_sha256":hash(root.join("preparation.json"))?}),
    )?;
    let mut results = Vec::new();
    for condition in prepared["conditions"].as_array().context("Conditions")? {
        let name = condition["name"].as_str().context("Condition name")?;
        let path = condition["plan"].as_str().context("Plan path")?;
        ensure!(hash(path)? == condition["plan_sha256"], "Changed plan");
        let log = fs::File::create_new(root.join(format!("{name}.log")))?;
        let status = Command::new(root.join("generation_campaign"))
            .arg("--plan")
            .arg(path)
            .arg("--output-dir")
            .arg(root.join(format!("run-{name}")))
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()?;
        save(
            root.join(format!("{name}-exit.json")),
            &json!({"code":status.code(),"success":status.success(),"at":chrono::Utc::now()}),
        )?;
        ensure!(
            status.success(),
            "Condition {name} failed; inspect preserved attempts before continuing"
        );
        let result = read(root.join(format!("run-{name}/status.json")))?;
        ensure!(result["status"] == "complete", "Condition incomplete");
        results.push(json!({"name":name,"status":result,"status_sha256":hash(root.join(format!("run-{name}/status.json")))?}));
    }
    save(
        root.join("completion.json"),
        &json!({"at":chrono::Utc::now(),"conditions":results,"model_calls_planned":16,"review_pending":true,"detector_calls":0,"training_archive_changed":false}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().task {
        Task::Prepare {
            root,
            pilot,
            generator,
            campaign,
        } => prepare(root, pilot, generator, campaign),
        Task::Run { root } => run(root),
    }
}
