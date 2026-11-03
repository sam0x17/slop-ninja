//! Freeze a full source/style panel before running the existing generation CLI.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Origin, OriginRecord, Split, sha256},
    generation::ModelSpec,
};
use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

const TRAIN_SHA: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const PACKET_SHA: &str = "6d6542888c3a7eb3146e5e0de10518b144043a0efe9cf2b9f36c2230f4b6e15e";
const PROFILES: [&str; 6] = [
    "source-matched",
    "plain",
    "informal",
    "direct",
    "anti-ai",
    "fix-slop",
];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    train: PathBuf,
    #[arg(long)]
    packet: PathBuf,
    #[arg(long)]
    probe: PathBuf,
    #[arg(long)]
    generator: PathBuf,
    #[arg(long)]
    campaign: PathBuf,
    #[arg(long)]
    python: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.root.exists(), "Use a fresh preparation directory");
    let train_bytes = fs::read(&args.train)?;
    let packet_bytes = fs::read(&args.packet)?;
    ensure!(
        sha256(&train_bytes) == TRAIN_SHA && sha256(&packet_bytes) == PACKET_SHA,
        "Changed frozen sources"
    );
    let records = dataset::read_records(&args.train)?;
    ensure!(records.len() == 3833, "Changed Train size");
    let mut groups = BTreeMap::<String, Vec<(String, OriginRecord)>>::new();
    for r in records {
        ensure!(r.split == Some(Split::Train), "Non-Train source");
        if r.origin == Origin::HumanOnly {
            ensure!(
                r.parent_id.is_none() && r.generation.is_none(),
                "Not a human root"
            );
            let rank = sha256(serde_json::to_vec(&(
                "slop-ninja-human-train-register-v1",
                &r.source.collection,
                &r.source_group,
                &r.id,
            ))?);
            groups
                .entry(r.source.collection.clone())
                .or_default()
                .push((rank, r));
        }
    }
    ensure!(
        groups.len() == 4 && groups.values().map(Vec::len).sum::<usize>() == 471,
        "Changed human population"
    );
    let packet: Value = serde_json::from_slice(&packet_bytes)?;
    let mut roots = Vec::new();
    let mut selected = Vec::new();
    for rows in groups.values_mut() {
        rows.sort_by(|a, b| (&a.0, &a.1.id).cmp(&(&b.0, &b.1.id)));
        let (rank, root) = &rows[0];
        ensure!(
            packet["selected"]
                .as_array()
                .context("Missing selections")?
                .iter()
                .any(|p| p["selection_sha256"] == *rank
                    && p["record"] == serde_json::to_value(root).unwrap()),
            "Root differs from frozen packet"
        );
        selected.push(json!({"id":root.id,"source_group":root.source_group,"collection":root.source.collection,"text_sha256":root.text_sha256,"selection_sha256":rank}));
        roots.push(root.clone());
    }
    let model = args.probe.join("model");
    let model_files: Value =
        serde_json::from_slice(&fs::read(args.probe.join("probe/model-files.json"))?)?;
    let old_template = fs::read_to_string(model.join("chat_template.jinja"))?;
    ensure!(
        sha256(&old_template) == model_files["chat_template.jinja"],
        "Changed upstream template"
    );
    let date_expr = "strftime_now(\"%Y-%m-%d\")";
    ensure!(
        old_template.matches(date_expr).count() == 1,
        "Unexpected date expression"
    );
    let template = old_template.replace(date_expr, "\"2026-09-15\"");
    ensure!(
        !template.contains("strftime_now"),
        "Unfrozen template clock"
    );
    let mut spec: ModelSpec =
        serde_json::from_slice(&fs::read(args.probe.join("probe/model-spec.json"))?)?;
    ensure!(
        spec.revision
            == "mlx-community/gpt-oss-20b-mxfp4-bf16@f8c03dc936dbf66db17d3411a177f498fcdd9a87"
            && spec.temperature == 0.7
            && spec.max_tokens == 4096
            && serde_json::to_value(spec.response_format)? == "harmony-text-v1",
        "Changed generator settings"
    );
    spec.runtime.push_str(&format!(
        "; frozen_template_sha256={}; template_date=2026-09-15; reasoning=medium",
        sha256(&template)
    ));
    let report: Value = serde_json::from_slice(&fs::read(args.probe.join("probe/report.json"))?)?;
    let package = args
        .python
        .parent()
        .context("Python bin")?
        .parent()
        .context("Environment")?
        .join("lib/python3.12/site-packages/mlx_lm");
    let mut runtime_files = BTreeMap::new();
    for (name, hash) in report["runtime_files"]
        .as_object()
        .context("Missing runtime identities")?
    {
        let path = package.join(name);
        ensure!(
            sha256(fs::read(&path)?) == hash.as_str().unwrap(),
            "Changed MLX runtime file"
        );
        runtime_files.insert(path, hash.clone());
    }
    // Weights were verified in the preceding probe; bind small runtime assets again.
    for (name, hash) in model_files.as_object().context("Missing model manifest")? {
        if !name.ends_with(".safetensors") {
            let path = model.join(name);
            ensure!(
                sha256(fs::read(&path)?) == hash.as_str().unwrap(),
                "Changed model asset"
            );
            runtime_files.insert(path, hash.clone());
        }
    }
    fs::create_dir(&args.root)?;
    let template_path = args.root.join("chat_template.jinja");
    fs::write(&template_path, &template)?;
    runtime_files.insert(template_path.clone(), json!(sha256(&template)));
    let input = args.root.join("input.jsonl");
    dataset::write_records(&input, &roots)?;
    let input_hash = sha256(fs::read(&input)?);
    let spec_path = args.root.join("model-spec.json");
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;
    let protocol = args.root.join("PROTOCOL.md");
    fs::write(&protocol, include_bytes!("../GPT_OSS_TRAIN_PILOT_V1.md"))?;
    let generator = args.root.join("generate_samples");
    let campaign = args.root.join("generation_campaign");
    fs::copy(&args.generator, &generator)?;
    fs::copy(&args.campaign, &campaign)?;
    let mut plans = Vec::new();
    for profile in PROFILES {
        let prospective = args.root.join(format!("prospective-{profile}"));
        let output = Command::new(&generator)
            .arg("--input")
            .arg(&input)
            .arg("--model-spec")
            .arg(&spec_path)
            .arg("--output-dir")
            .arg(&prospective)
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
            ])
            .output()?;
        fs::write(
            args.root.join(format!("prepare-{profile}.stdout")),
            output.stdout,
        )?;
        fs::write(
            args.root.join(format!("prepare-{profile}.stderr")),
            output.stderr,
        )?;
        ensure!(
            output.status.success(),
            "Failed prospective {profile} requests"
        );
        let plan = json!({"input":input,"input_sha256":input_hash,
            "generator":generator,"generator_sha256":sha256(fs::read(&generator)?),
            "python":args.python,"runtime_files":runtime_files,"chat_template":template_path,
            "protocol":protocol,"protocol_sha256":sha256(fs::read(&protocol)?),
            "profile_ids":[profile],"drafts_only":true,"request_timeout_seconds":1800,
            "cases":[{"name":"gpt-oss","model_dir":model,"model_spec":spec_path,
                "model_spec_sha256":sha256(fs::read(&spec_path)?),"port":18139,"split":"train","max_calls":4}]});
        let plan_path = args.root.join(format!("plan-{profile}.json"));
        fs::write(&plan_path, serde_json::to_vec_pretty(&plan)?)?;
        plans.push(json!({"profile":profile,"path":plan_path,"sha256":sha256(fs::read(&plan_path)?),"prospective":prospective}));
    }
    ensure!(
        sha256(fs::read(&args.train)?) == TRAIN_SHA,
        "Train changed during preparation"
    );
    let prepared = json!({"schema":"slop_ninja_gpt_oss_train_pilot_preparation_v1",
        "at":chrono::Utc::now().to_rfc3339(),"train_sha256":TRAIN_SHA,"packet_sha256":PACKET_SHA,
        "input_sha256":input_hash,"selected":selected,"profiles":PROFILES,"planned_requests":24,
        "template_sha256":sha256(template),"template_date":"2026-09-15","plans":plans,
        "source_sha256":sha256(include_bytes!("gpt_oss_train_pilot.rs")),
        "protocol_sha256":sha256(fs::read(&protocol)?),"campaign_sha256":sha256(fs::read(&campaign)?),
        "generator_sha256":sha256(fs::read(&generator)?),"model_spec_sha256":sha256(fs::read(&spec_path)?),
        "model_calls":0,"detector_calls":0,"training_archive_changed":false,
        "rendered_prompt_review":"pending","generation_status":"not_started"});
    fs::write(
        args.root.join("preparation.json"),
        serde_json::to_vec_pretty(&prepared)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&prepared)?);
    Ok(())
}
