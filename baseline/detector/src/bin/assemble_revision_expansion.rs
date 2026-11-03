//! Assemble the frozen v6 revision experiment without fitting or scoring.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Evidence, Origin, OriginRecord, Split, sha256},
    evaluation_view::{self, Selection},
    generation::{self, ModelSpec, RevisionStyle},
    prompt_profiles::Provenance,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const PRIOR: &str = "1b589c754dfb6d9585b9b724105057903aef3fa66a950808730d4aa6595a5cbd";
const SEEDS: &str = "d0cfb18e850615def38c5854697a2e2088428fd4ef1a6dce92147f68cfd1a764";
const SEED_MANIFEST: &str = "5765b9ee37bca85afb0b06f49a0ce7a70b6c513ad7d2a66ea7a8b694bb2459f8";
const PLAN: &str = "f146e8b132272c3233b43cef932d880b2ef1a5962c39c4958cae1fa4c0d0f9d7";
const SEED_DOMAIN: &str = "slop-ninja-v6-revision-train-seed-v1";
const QWEN: &str =
    "mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit@e9675aa3ca5f900ccef55267914466d55ab325fa";
const MISTRAL: &str = "mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit@2a1d5eabfc504747bdc24178394821a1efc0edde";
const OLMO: &str = "mlx-community/Olmo-3-7B-Instruct-4bit@d732c91ae02e90cd5d86e810fbeb9741794b4dd9";
const PHI: &str = "mlx-community/phi-4-4bit@fc0f8f23d369dc29b55cad1d65cb5bf0dcbee910";

#[derive(Parser)]
struct Args {
    /// Exact original v5 assembly; only its unchanged Train partition enters v6.
    #[arg(long)]
    prior_v5: PathBuf,
    /// Frozen seeds-v1 directory containing input.jsonl, manifest.json and binding.json.
    #[arg(long)]
    train_seeds: PathBuf,
    #[arg(long)]
    train_chains: PathBuf,
    #[arg(long)]
    primary_chains: PathBuf,
    #[arg(long)]
    phi4_chains: Option<PathBuf>,
    /// Frozen evaluation-plan-v1 directory.
    #[arg(long)]
    evaluation_plan: PathBuf,
    /// JSON run/input/records paths for both passes of each route; paths are relative to this file.
    #[arg(long)]
    revision_runs: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PassPaths {
    run: PathBuf,
    input: PathBuf,
    records: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutePaths {
    first: PassPaths,
    second: PassPaths,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunPaths {
    schema: String,
    train: RoutePaths,
    primary: RoutePaths,
    phi4: Option<RoutePaths>,
}

type Bindings = BTreeMap<String, Value>;

fn bytes(path: &Path, label: &str, bindings: &mut Bindings) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        bindings
            .insert(label.into(), json!({"path":path,"sha256":sha256(&bytes)}))
            .is_none(),
        "Duplicate input binding"
    );
    Ok(bytes)
}

fn object(path: &Path, label: &str, bindings: &mut Bindings) -> Result<Value> {
    Ok(serde_json::from_slice(&bytes(path, label, bindings)?)?)
}

fn archive(path: &Path, label: &str, bindings: &mut Bindings) -> Result<Vec<OriginRecord>> {
    let (records, hash) = evaluation_view::read_archive(path)?;
    ensure!(
        bindings
            .insert(label.into(), json!({"path":path,"sha256":hash}))
            .is_none(),
        "Duplicate input binding"
    );
    Ok(records)
}

fn record_bytes(records: &[OriginRecord]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn same_record(a: &OriginRecord, b: &OriginRecord) -> Result<()> {
    ensure!(
        serde_json::to_value(a)? == serde_json::to_value(b)?,
        "An existing record changed: {}",
        a.id
    );
    Ok(())
}

fn check_inheritance(
    child: &OriginRecord,
    parent: &OriginRecord,
    operation: &str,
    revision: &str,
) -> Result<()> {
    ensure!(
        serde_json::to_value(&child.source)? == serde_json::to_value(&parent.source)?,
        "Generated record changed inherited source provenance"
    );
    let mut rights = parent.rights.clone();
    if let Some(obligations) = &mut rights.share_alike {
        rights.license = slop_ninja_detector::rights::LICENSE.into();
        obligations.changes.push_str(&format!(
            " Model {operation} operation by {revision}; see generation provenance."
        ));
    }
    ensure!(
        serde_json::to_value(&child.rights)? == serde_json::to_value(rights)?,
        "Generated record changed inherited rights or its required adaptation notice"
    );
    Ok(())
}

#[derive(Clone)]
struct Chain {
    human: OriginRecord,
    draft: OriginRecord,
    mixed: OriginRecord,
    revisions: Vec<OriginRecord>,
}

fn chains(records: &[OriginRecord], depth: usize) -> Result<BTreeMap<String, Chain>> {
    dataset::validate_records(records)?;
    let mut groups = BTreeMap::<&str, Vec<&OriginRecord>>::new();
    for record in records {
        groups.entry(&record.source_group).or_default().push(record);
    }
    ensure!(!groups.is_empty(), "Empty complete-chain archive");
    let mut result = BTreeMap::new();
    for (group, family) in groups {
        ensure!(
            family.len() == depth + 3,
            "Expected exactly a root, draft, paired edit and {depth} revisions per family"
        );
        let one = |origin: Origin, evidence: Option<Evidence>| -> Result<&OriginRecord> {
            let selected: Vec<_> = family
                .iter()
                .copied()
                .filter(|r| r.origin == origin && evidence.is_none_or(|e| r.evidence == e))
                .collect();
            ensure!(
                selected.len() == 1,
                "Expected one declared origin-stage record in family {group}"
            );
            Ok(selected[0])
        };
        let human = one(Origin::HumanOnly, None)?;
        let draft = one(Origin::ModelOnly, Some(Evidence::RecordedModelGeneration))?;
        let mixed = one(Origin::Mixed, Some(Evidence::ModelEditOfHistoricalProxy))?;
        ensure!(
            human.parent_id.is_none()
                && human.generation.is_none()
                && matches!(
                    human.evidence,
                    Evidence::HistoricalProxy | Evidence::DocumentedHuman
                ),
            "Expected a historical/documented human root"
        );
        ensure!(
            draft.parent_id.as_deref() == Some(&human.id)
                && mixed.parent_id.as_deref() == Some(&human.id),
            "Draft/edit pair does not share its human root"
        );
        let dg = draft
            .generation
            .as_ref()
            .context("Draft lacks generation")?;
        let mg = mixed.generation.as_ref().context("Edit lacks generation")?;
        ensure!(
            dg.model_id == mg.model_id
                && dg.model_revision == mg.model_revision
                && dg.prompt_profile == mg.prompt_profile,
            "Draft/edit composer or full profile differs"
        );
        check_inheritance(draft, human, "draft", &dg.model_revision)?;
        check_inheritance(mixed, human, "edit", &mg.model_revision)?;
        let mut parent = draft;
        let mut revisions = Vec::new();
        for _ in 0..depth {
            let children: Vec<_> = family
                .iter()
                .copied()
                .filter(|r| {
                    r.evidence == Evidence::RecordedModelRevision
                        && r.parent_id.as_deref() == Some(&parent.id)
                })
                .collect();
            ensure!(
                children.len() == 1,
                "Missing or branching revision stage in {group}"
            );
            parent = children[0];
            revisions.push(parent.clone());
        }
        result.insert(
            group.into(),
            Chain {
                human: human.clone(),
                draft: draft.clone(),
                mixed: mixed.clone(),
                revisions,
            },
        );
    }
    Ok(result)
}

fn reconstruct_seeds(train: &[OriginRecord]) -> Result<Vec<OriginRecord>> {
    let mut groups = BTreeMap::<&str, Vec<&OriginRecord>>::new();
    for r in train {
        ensure!(
            r.split == Some(Split::Train),
            "Seed reconstruction requires Train only"
        );
        groups.entry(&r.source_group).or_default().push(r);
    }
    let mut selected = Vec::new();
    for (group, family) in groups {
        let humans: Vec<_> = family
            .iter()
            .copied()
            .filter(|r| r.origin == Origin::HumanOnly && r.parent_id.is_none())
            .collect();
        ensure!(
            humans.len() == 1,
            "Expected one old human root per Train family"
        );
        let human = humans[0];
        let mut candidates = Vec::new();
        for draft in family
            .iter()
            .copied()
            .filter(|r| r.origin == Origin::ModelOnly)
        {
            ensure!(
                draft.evidence == Evidence::RecordedModelGeneration
                    && draft.parent_id.as_deref() == Some(&human.id),
                "Old Train contains a non-original model stage"
            );
            let generation = draft
                .generation
                .as_ref()
                .context("Missing original draft provenance")?;
            if generation.prompt_profile.is_none() {
                continue;
            } // Frozen seed rule excludes unprofiled candidates.
            let mixed: Vec<_> = family
                .iter()
                .copied()
                .filter(|r| {
                    r.origin == Origin::Mixed
                        && r.parent_id.as_deref() == Some(&human.id)
                        && r.generation.as_ref().is_some_and(|g| {
                            g.operation == "edit"
                                && g.model_revision == generation.model_revision
                                && g.prompt_profile == generation.prompt_profile
                        })
                })
                .collect();
            if mixed.len() != 1 {
                continue;
            } // Same eligibility rule as the frozen seed selector.
            let rank = sha256(serde_json::to_vec(&(
                SEED_DOMAIN,
                group,
                &generation.model_revision,
                &draft.id,
            ))?);
            candidates.push((rank, draft, mixed[0]));
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        ensure!(
            !candidates.is_empty() && candidates.windows(2).all(|w| w[0].0 != w[1].0),
            "No seed pair or colliding selection ranks"
        );
        selected.extend([
            human.clone(),
            candidates[0].1.clone(),
            candidates[0].2.clone(),
        ]);
    }
    Ok(selected)
}

fn check_revision(
    child: &OriginRecord,
    parent: &OriginRecord,
    spec: &ModelSpec,
    style: RevisionStyle,
) -> Result<()> {
    let (key, request) = generation::revision(parent, spec, style)?;
    let g = child
        .generation
        .as_ref()
        .context("Missing revision provenance")?;
    ensure!(
        child.id == format!("generation:{key}")
            && child.parent_id.as_deref() == Some(&parent.id)
            && child.evidence == Evidence::RecordedModelRevision
            && child.origin == Origin::ModelOnly
            && g.prompt == request["messages"][1]["content"]
            && g.request_sha256 == sha256(serde_json::to_vec(&request)?)
            && g.model_id == spec.id
            && g.model_revision == spec.revision
            && g.model_license == spec.license
            && g.model_license_url == spec.license_url
            && g.model_license_sha256 == spec.license_sha256
            && g.quantization == spec.quantization
            && g.runtime == spec.runtime
            && g.temperature == spec.temperature
            && g.max_tokens == spec.max_tokens
            && g.seed.is_none()
            && g.prompt_profile.is_none()
            && g.operation == "revise"
            && g.finish_reason == "stop",
        "Revision differs from its exact frozen parent/spec/style/request identity"
    );
    check_inheritance(child, parent, "revise", &spec.revision)?;
    Ok(())
}

struct Pass {
    input: Vec<OriginRecord>,
    output: Vec<OriginRecord>,
    input_hash: String,
    output_hash: String,
    binding: Value,
}

fn read_pass(
    base: &Path,
    paths: &PassPaths,
    label: &str,
    revision: &str,
    style: RevisionStyle,
    bindings: &mut Bindings,
) -> Result<Pass> {
    let run_path = base.join(&paths.run);
    ensure!(
        !run_path
            .parent()
            .context("Run lacks parent directory")?
            .join("run.lock")
            .try_exists()?,
        "Revision cohort has run.lock; wait for generation/export to close"
    );
    let run = object(&run_path, &format!("{label}.run"), bindings)?;
    let summary = object(
        &run_path
            .parent()
            .context("Run lacks parent directory")?
            .join("summary.json"),
        &format!("{label}.summary"),
        bindings,
    )?;
    let input = archive(
        &base.join(&paths.input),
        &format!("{label}.input"),
        bindings,
    )?;
    let output = archive(
        &base.join(&paths.records),
        &format!("{label}.records"),
        bindings,
    )?;
    let input_hash = bindings[&format!("{label}.input")]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let output_hash = bindings[&format!("{label}.records")]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    ensure!(
        run["schema"] == "slop-ninja-generation-run-v1"
            && run["prompt_version"] == "slop-ninja-model-revision-v1"
            && run["revision_style"] == serde_json::to_value(style)?
            && run["input_sha256"] == input_hash,
        "Run input, prompt version or revision style differs"
    );
    ensure!(
        summary["status"] == "complete"
            && summary["complete_export_ready"] == true
            && summary["complete_families_only"] == true
            && summary["unattempted"].as_array().is_some_and(Vec::is_empty)
            && summary["summary"]["records"] == output.len(),
        "Revision export is not a closed complete-family cohort"
    );
    let spec: ModelSpec = serde_json::from_value(run["model"].clone())?;
    ensure!(
        spec.revision == revision && spec.temperature == 0.7 && spec.max_tokens == 1800,
        "Undeclared reviser pin or generation settings"
    );
    if !run["split"].is_null() {
        let split: Split = serde_json::from_value(run["split"].clone())?;
        ensure!(
            input.iter().all(|r| r.split == Some(split)),
            "Split-filtered run input must contain only its declared partition"
        );
    }
    let by_id: BTreeMap<_, _> = input.iter().map(|r| (r.id.as_str(), r)).collect();
    let out_ids: BTreeSet<_> = output.iter().map(|r| r.id.as_str()).collect();
    let out_groups: BTreeSet<_> = output.iter().map(|r| r.source_group.as_str()).collect();
    let mut revised_groups = BTreeSet::new();
    for record in &output {
        if let Some(original) = by_id.get(record.id.as_str()) {
            same_record(record, original)?;
        } else {
            let parent = by_id
                .get(
                    record
                        .parent_id
                        .as_deref()
                        .context("New revision lacks parent")?,
                )
                .context("Revision parent is not in frozen input")?;
            ensure!(
                !input
                    .iter()
                    .any(|r| r.parent_id.as_deref() == Some(&parent.id)),
                "Revision advanced an earlier stage instead of an input leaf"
            );
            check_revision(record, parent, &spec, style)?;
            ensure!(
                revised_groups.insert(record.source_group.as_str()),
                "Multiple new revisions per family in one pass"
            );
        }
    }
    ensure!(
        revised_groups == out_groups
            && input
                .iter()
                .filter(|r| out_groups.contains(r.source_group.as_str()))
                .all(|r| out_ids.contains(r.id.as_str())),
        "Export dropped an admitted ancestor or retained an incomplete family"
    );
    Ok(Pass {
        input,
        output,
        input_hash,
        output_hash,
        binding: json!({"run_sha256":bindings[&format!("{label}.run")]["sha256"],"summary_sha256":bindings[&format!("{label}.summary")]["sha256"],
        "model_revision":revision,"revision_style":style,"request_identity_verified":true,"raw_response_bytes_reverified":false,
        "attempt_counts":"Not reconstructed from complete-only exports; consult the separately retained master attempt ledger and bound generator summary."}),
    })
}

fn read_route(
    base: &Path,
    paths: &RoutePaths,
    label: &str,
    pins: [&str; 2],
    final_hash: &str,
    bindings: &mut Bindings,
) -> Result<(Pass, Pass)> {
    let first = read_pass(
        base,
        &paths.first,
        &format!("{label}.first"),
        pins[0],
        RevisionStyle::AntiAi,
        bindings,
    )?;
    let second = read_pass(
        base,
        &paths.second,
        &format!("{label}.second"),
        pins[1],
        RevisionStyle::FixSlop,
        bindings,
    )?;
    ensure!(
        second.input_hash == first.output_hash && second.output_hash == final_hash,
        "Revision route does not bind both consecutive full exports and its final archive"
    );
    chains(&first.input, 0)?;
    chains(&first.output, 1)?;
    chains(&second.output, 2)?;
    Ok((first, second))
}

struct Plan {
    roots: Vec<OriginRecord>,
    cohorts: BTreeMap<String, (String, Provenance)>,
}

fn read_plan(path: &Path, protocol_hash: &str, bindings: &mut Bindings) -> Result<Plan> {
    let plan = object(&path.join("plan.json"), "evaluation_plan", bindings)?;
    ensure!(
        bindings["evaluation_plan"]["sha256"] == PLAN && plan["protocol_sha256"] == protocol_hash,
        "Evaluation plan differs from its frozen protocol binding"
    );
    let roots = archive(
        &path.join("frozen-roots.jsonl"),
        "evaluation_roots",
        bindings,
    )?;
    ensure!(
        bindings["evaluation_roots"]["sha256"] == plan["files_sha256"]["frozen-roots.jsonl"],
        "Frozen evaluation roots changed"
    );
    let mut cohorts = BTreeMap::new();
    for (name, pin) in [
        ("qwen", QWEN),
        ("mistral", MISTRAL),
        ("olmo", OLMO),
        ("phi4", PHI),
    ] {
        let file = format!("{name}-roots.jsonl");
        let records = archive(&path.join(&file), &format!("{name}.roots"), bindings)?;
        ensure!(
            bindings[&format!("{name}.roots")]["sha256"] == plan["files_sha256"][&file],
            "Composer root file changed"
        );
        let file = format!("{name}-profile-plan.json");
        let value = object(&path.join(&file), &format!("{name}.profiles"), bindings)?;
        ensure!(
            bindings[&format!("{name}.profiles")]["sha256"] == plan["files_sha256"][&file],
            "Composer profile plan changed"
        );
        let profiles: BTreeMap<String, Provenance> = serde_json::from_value(value)?;
        ensure!(
            profiles.len() == records.len(),
            "Incomplete composer profile assignment"
        );
        for root in &records {
            let key = if name == "phi4" {
                format!("phi4:{}", root.source_group)
            } else {
                root.source_group.clone()
            };
            ensure!(
                cohorts
                    .insert(
                        key,
                        (
                            pin.into(),
                            profiles
                                .get(&root.source_group)
                                .context("Missing frozen profile")?
                                .clone()
                        )
                    )
                    .is_none(),
                "Duplicate composer assignment"
            );
        }
    }
    Ok(Plan { roots, cohorts })
}

fn check_fresh(records: &[OriginRecord], plan: &Plan, phi: bool) -> Result<()> {
    let roots: BTreeMap<_, _> = plan
        .roots
        .iter()
        .map(|r| (r.source_group.as_str(), r))
        .collect();
    for (group, chain) in chains(records, 0)? {
        let root = roots
            .get(group.as_str())
            .context("Fresh family absent from frozen source allocation")?;
        same_record(&chain.human, root)?;
        ensure!(
            root.split.is_some_and(|s| s != Split::Train)
                && (!phi || root.split == Some(Split::Test)),
            "Fresh source has the wrong original partition"
        );
        let key = if phi { format!("phi4:{group}") } else { group };
        let (pin, profile) = plan
            .cohorts
            .get(&key)
            .context("Missing frozen composer/profile assignment")?;
        let generation = chain.draft.generation.as_ref().unwrap();
        ensure!(
            generation.model_revision == *pin
                && generation.prompt_profile.as_ref() == Some(profile),
            "Fresh composer or original full profile differs from its pre-generation assignment"
        );
        for record in [&chain.draft, &chain.mixed] {
            let generation = record.generation.as_ref().unwrap();
            ensure!(
                generation.temperature == 0.7
                    && generation.max_tokens == 1800
                    && generation.seed.is_none(),
                "Fresh draft/edit generation settings differ from the frozen protocol"
            );
        }
    }
    Ok(())
}

fn views(
    records: &[OriginRecord],
    prefix: &str,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<Value> {
    let mut report = serde_json::Map::new();
    for (name, split) in [
        ("development", Split::Development),
        ("calibration", Split::Calibration),
        ("test", Split::Test),
    ] {
        let selected: Vec<_> = records
            .iter()
            .filter(|r| r.split == Some(split))
            .cloned()
            .collect();
        if selected.is_empty() {
            continue;
        }
        let bytes = record_bytes(&selected)?;
        let hash = sha256(&bytes);
        files.insert(format!("{prefix}/{name}.jsonl"), bytes);
        let families = chains(&selected, 2)?;
        let mut stages = Vec::new();
        for depth in 0..=2 {
            let selections: Vec<_> = families
                .iter()
                .map(|(group, c)| Selection {
                    source_group: group.clone(),
                    human_id: c.human.id.clone(),
                    mixed_id: c.mixed.id.clone(),
                    model_id: if depth == 0 {
                        c.draft.id.clone()
                    } else {
                        c.revisions[depth - 1].id.clone()
                    },
                })
                .collect();
            let view = evaluation_view::build_view(&selected, hash.clone(), split, &selections)?;
            let mut bytes = serde_json::to_vec_pretty(&view)?;
            bytes.push(b'\n');
            stages.push(json!({"stage":format!("R{depth}"),"sha256":sha256(&bytes),"records_sha256":hash,"split":split,"selected_rows":selections.len()*3,"source_groups":selections.len()}));
            files.insert(format!("{prefix}/{name}-R{depth}.json"), bytes);
        }
        report.insert(
            name.into(),
            json!({"archive_sha256":hash,"summary":dataset::summarize(&selected),"views":stages}),
        );
    }
    Ok(Value::Object(report))
}

fn assemble(args: Args) -> Result<Value> {
    ensure!(
        !args.output_dir.exists(),
        "Use a new immutable assembly directory"
    );
    let mut bindings = Bindings::new();
    let protocol = bytes(&args.protocol, "protocol", &mut bindings)?;
    ensure!(
        sha256(&protocol) == args.expected_protocol_sha256,
        "Protocol changed from explicit freeze"
    );
    let prior = archive(&args.prior_v5, "prior_v5", &mut bindings)?;
    ensure!(
        bindings["prior_v5"]["sha256"] == PRIOR,
        "Expected the exact original v5 assembly"
    );
    let train: Vec<_> = prior
        .iter()
        .filter(|r| r.split == Some(Split::Train))
        .cloned()
        .collect();
    ensure!(
        train.len() == 2925 && dataset::summarize(&train).source_groups == 471,
        "Original v5 Train coverage changed"
    );
    let seed_manifest = object(
        &args.train_seeds.join("manifest.json"),
        "seed_manifest",
        &mut bindings,
    )?;
    let seed_binding = object(
        &args.train_seeds.join("binding.json"),
        "seed_binding",
        &mut bindings,
    )?;
    let seed = archive(
        &args.train_seeds.join("input.jsonl"),
        "seed_input",
        &mut bindings,
    )?;
    ensure!(
        bindings["seed_manifest"]["sha256"] == SEED_MANIFEST
            && bindings["seed_input"]["sha256"] == SEEDS
            && seed_binding["manifest_sha256"] == SEED_MANIFEST
            && seed_binding["input_sha256"] == SEEDS
            && seed_manifest["v5_input_sha256"] == PRIOR
            && seed_manifest["input_sha256"] == SEEDS
            && seed_manifest["selection_domain"] == SEED_DOMAIN,
        "Train seeds differ from their immutable selection binding"
    );
    ensure!(
        record_bytes(&reconstruct_seeds(&train)?)? == record_bytes(&seed)?,
        "Train seeds are not the exact declared SHA-selected original pairs"
    );
    let plan = read_plan(
        &args.evaluation_plan,
        &args.expected_protocol_sha256,
        &mut bindings,
    )?;
    let train_final = archive(&args.train_chains, "train_final", &mut bindings)?;
    let primary = archive(&args.primary_chains, "primary_final", &mut bindings)?;
    let phi = args
        .phi4_chains
        .as_ref()
        .map(|p| archive(p, "phi4_final", &mut bindings))
        .transpose()?;
    let runs: RunPaths =
        serde_json::from_value(object(&args.revision_runs, "revision_runs", &mut bindings)?)?;
    ensure!(
        runs.schema == "slop_ninja_v6_revision_runs_v1" && runs.phi4.is_some() == phi.is_some(),
        "Revision run manifest schema or optional Phi route differs"
    );
    let base = args
        .revision_runs
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let train_hash = bindings["train_final"]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let primary_hash = bindings["primary_final"]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let (train_first, train_second) = read_route(
        base,
        &runs.train,
        "train",
        [QWEN, MISTRAL],
        &train_hash,
        &mut bindings,
    )?;
    ensure!(
        train_first.input_hash == SEEDS,
        "Train route did not start from the frozen seed input"
    );
    let (primary_first, primary_second) = read_route(
        base,
        &runs.primary,
        "primary",
        [QWEN, MISTRAL],
        &primary_hash,
        &mut bindings,
    )?;
    check_fresh(&primary_first.input, &plan, false)?;
    let mut routes = json!({"train":[train_first.binding,train_second.binding],"primary":[primary_first.binding,primary_second.binding]});
    if let (Some(records), Some(paths)) = (&phi, &runs.phi4) {
        let hash = bindings["phi4_final"]["sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        let (first, second) = read_route(base, paths, "phi4", [PHI, PHI], &hash, &mut bindings)?;
        check_fresh(&first.input, &plan, true)?;
        ensure!(
            records.iter().all(|r| r.split == Some(Split::Test)),
            "Phi records must remain Test only"
        );
        routes["phi4"] = json!([first.binding, second.binding]);
    }
    let old_ids: BTreeSet<_> = train.iter().map(|r| r.id.as_str()).collect();
    let seed_families = chains(&seed, 0)?;
    let complete_train = chains(&train_final, 2)?;
    let mut assembled = train.clone();
    for (group, chain) in &complete_train {
        let original = seed_families
            .get(group)
            .context("Train chain has an undeclared source family")?;
        for (a, b) in [
            (&chain.human, &original.human),
            (&chain.draft, &original.draft),
            (&chain.mixed, &original.mixed),
        ] {
            same_record(a, b)?;
        }
        for revision in &chain.revisions {
            ensure!(
                revision.split == Some(Split::Train) && !old_ids.contains(revision.id.as_str()),
                "New revision changed partition or replaced an old Train record"
            );
            assembled.push(revision.clone());
        }
    }
    ensure!(
        assembled.len() == train.len() + complete_train.len() * 2,
        "Train row accounting differs"
    );
    let prior_groups: BTreeSet<_> = prior.iter().map(|r| r.source_group.as_str()).collect();
    ensure!(
        plan.roots
            .iter()
            .all(|r| !prior_groups.contains(r.source_group.as_str())),
        "Fresh allocation overlaps a prior v5 source family"
    );
    assembled.extend(primary.iter().cloned());
    dataset::validate_records(&assembled)?;
    for split in [Split::Development, Split::Calibration, Split::Test] {
        ensure!(
            primary.iter().any(|r| r.split == Some(split)),
            "A complete primary partition is empty"
        );
    }
    if let Some(phi) = &phi {
        let mut union: BTreeMap<_, _> = assembled
            .iter()
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        for record in phi {
            if let Some(old) = union.get(&record.id) {
                same_record(record, old)?;
            } else {
                union.insert(record.id.clone(), record.clone());
            }
        }
        dataset::validate_records(&union.into_values().collect::<Vec<_>>())?;
    }
    let mut files = BTreeMap::new();
    files.insert("records.jsonl".into(), record_bytes(&assembled)?);
    files.insert("fresh-human-roots.jsonl".into(), record_bytes(&plan.roots)?);
    let primary_views = views(&primary, "primary", &mut files)?;
    let phi_views = phi
        .as_ref()
        .map(|r| views(r, "phi4", &mut files))
        .transpose()?;
    let primary_groups: BTreeSet<_> = primary
        .iter()
        .filter(|r| r.split == Some(Split::Test))
        .map(|r| &r.source_group)
        .collect();
    let phi_groups: BTreeSet<_> = phi.iter().flatten().map(|r| &r.source_group).collect();
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name.clone(), sha256(bytes)))
        .collect();
    let report = json!({"schema":"slop_ninja_v6_revision_assembly_v1","inputs":bindings,"revision_runs":routes,"files_sha256":hashes,
        "assembler_source_sha256":sha256(include_bytes!("assemble_revision_expansion.rs")),"assembler_executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "prior_train_records_preserved":train.len(),"complete_train_chain_families":complete_train.len(),"new_train_revision_records":complete_train.len()*2,
        "summary":dataset::summarize(&assembled),"primary":primary_views,"phi4":phi_views,"shared_primary_phi4_test_families":primary_groups.intersection(&phi_groups).count(),
        "allocated_human_roots":dataset::summarize(&plan.roots),"attempt_failure_rejection_counts":null,
        "attempt_accounting":"Complete-only exports cannot establish attempted, failed, rejected or unattempted denominators. Preserve the master attempt ledgers and original bound summaries separately; no failed/rejected count is inferred here.",
        "train_policy":"Every original v5 Train row remains unchanged; only both new stages of complete declared chains are appended. Use source_origin weighting.",
        "view_policy":"R0/R1/R2 share each partition's complete ancestry archive and family cohort. Primary projection bytes match unchanged export-shards order. Phi is a separate Test-only route.",
        "verification_limit":"Full rights/ancestry and exact parent/spec/style/request identities validated; raw provider responses and installed serving weights are not reverified by assembly.",
        "fit_executed":false,"detector_calls":0,"pangram_calls":0,"test_predictions_opened":false});
    files.insert("assembly.json".into(), serde_json::to_vec_pretty(&report)?);
    fs::create_dir(&args.output_dir)?;
    for (name, bytes) in files {
        let path = args.output_dir.join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        let mut file = fs::File::create_new(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(report)
}

fn main() -> Result<()> {
    let report = assemble(Args::parse())?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"summary":report["summary"],"prior_train_records_preserved":report["prior_train_records_preserved"],
        "complete_train_chain_families":report["complete_train_chain_families"],"new_train_revision_records":report["new_train_revision_records"],"fit_executed":false,"detector_calls":0,"test_predictions_opened":false})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_detector::{dataset::Generation, prompt_profiles};

    fn root(group: &str, split: Split) -> OriginRecord {
        serde_json::from_value(json!({"schema":dataset::RECORD_SCHEMA,"id":format!("{group}-human"),"source_group":group,"split":split,
            "origin":"human_only","evidence":"historical_proxy","evidence_notes":"Invented assembly contract fixture, never authorship evidence.",
            "text":format!("A paper moon marks fixture {group}."),"text_sha256":sha256(format!("A paper moon marks fixture {group}.")),
            "source":{"collection":"fixture","url":"fixture://root","version":"v1","published_at":null,"author_ids":[],"raw_path":"","raw_sha256":sha256("raw"),"extraction":"fixture"},
            "rights":{"license":"Synthetic-Original","evidence_url":"fixture://rights","evidence_sha256":sha256("rights"),"attribution":"Synthetic fixture","commercial_training":true,"model_release":true,"external_evaluation":false,"redistribute_text":true},
            "parent_id":null,"generation":null})).unwrap()
    }

    fn spec(pin: &str) -> ModelSpec {
        ModelSpec {
            id: "fixture-model".into(),
            revision: pin.into(),
            license: "Apache-2.0".into(),
            license_url: "fixture://model-license".into(),
            license_sha256: sha256("model license"),
            quantization: "fixture".into(),
            runtime: "fixture".into(),
            temperature: 0.7,
            max_tokens: 1800,
            response_format: None,
        }
    }

    fn initial(group: &str, split: Split) -> Vec<OriginRecord> {
        let human = root(group, split);
        let selection =
            prompt_profiles::Selection::new(prompt_profiles::SET_ID, Some(&["plain".into()]))
                .unwrap();
        let profile = prompt_profiles::assign(
            std::slice::from_ref(&human),
            &sha256("fixture input"),
            &selection,
        )
        .unwrap()[group]
            .clone();
        let model = spec(QWEN);
        let mut records = vec![human.clone()];
        for (operation, origin, evidence) in [
            (
                "draft",
                Origin::ModelOnly,
                Evidence::RecordedModelGeneration,
            ),
            ("edit", Origin::Mixed, Evidence::ModelEditOfHistoricalProxy),
        ] {
            let mut record = human.clone();
            record.id = format!("{group}-{operation}");
            record.parent_id = Some(human.id.clone());
            record.origin = origin;
            record.evidence = evidence;
            record.text = format!("Invented {operation} of the paper moon for {group}.");
            record.text_sha256 = sha256(&record.text);
            record.generation = Some(Generation {
                model_id: model.id.clone(),
                response_model: model.id.clone(),
                model_revision: model.revision.clone(),
                model_license: model.license.clone(),
                model_license_url: model.license_url.clone(),
                model_license_sha256: model.license_sha256.clone(),
                quantization: model.quantization.clone(),
                runtime: model.runtime.clone(),
                prompt: profile.prompt_block(),
                request_sha256: sha256("initial request"),
                response_sha256: sha256("initial response"),
                operation: operation.into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                temperature: 0.7,
                seed: None,
                max_tokens: 1800,
                finish_reason: "stop".into(),
                prompt_profile: Some(profile.clone()),
            });
            records.push(record);
        }
        dataset::validate_records(&records).unwrap();
        records
    }

    fn revised(parent: &OriginRecord, model: &ModelSpec, style: RevisionStyle) -> OriginRecord {
        let (key, request) = generation::revision(parent, model, style).unwrap();
        let mut record = parent.clone();
        record.id = format!("generation:{key}");
        record.parent_id = Some(parent.id.clone());
        record.evidence = Evidence::RecordedModelRevision;
        record.text = format!("Invented revision {} from {}.", model.revision, parent.id);
        record.text_sha256 = sha256(&record.text);
        let g = record.generation.as_mut().unwrap();
        g.model_id = model.id.clone();
        g.model_revision = model.revision.clone();
        g.prompt = request["messages"][1]["content"].as_str().unwrap().into();
        g.request_sha256 = sha256(serde_json::to_vec(&request).unwrap());
        g.operation = "revise".into();
        g.prompt_profile = None;
        record
    }

    fn complete(group: &str, split: Split) -> Vec<OriginRecord> {
        let mut records = initial(group, split);
        let first = revised(&records[1], &spec(QWEN), RevisionStyle::AntiAi);
        let second = revised(&first, &spec(MISTRAL), RevisionStyle::FixSlop);
        records.extend([first, second]);
        records
    }

    #[test]
    fn complete_chain_parser_rejects_missing_branched_and_wrongly_paired_stages() {
        let records = complete("one", Split::Development);
        let parsed = chains(&records, 2).unwrap();
        assert_eq!(parsed["one"].revisions.len(), 2);
        assert!(chains(&records[..4], 2).is_err());
        let mut branched = records.clone();
        branched[4].parent_id = Some(branched[1].id.clone());
        assert!(chains(&branched, 2).is_err());
        let mut wrong_pair = records;
        wrong_pair[2].generation.as_mut().unwrap().prompt_profile = None;
        assert!(chains(&wrong_pair, 2).is_err());
    }

    #[test]
    fn revision_identity_rejects_style_request_and_inherited_provenance_changes() {
        let records = complete("one", Split::Development);
        check_revision(&records[3], &records[1], &spec(QWEN), RevisionStyle::AntiAi).unwrap();
        check_revision(
            &records[4],
            &records[3],
            &spec(MISTRAL),
            RevisionStyle::FixSlop,
        )
        .unwrap();
        assert!(
            check_revision(
                &records[3],
                &records[1],
                &spec(QWEN),
                RevisionStyle::FixSlop
            )
            .is_err()
        );
        for field in ["request", "source", "rights"] {
            let mut changed = records[3].clone();
            match field {
                "request" => {
                    changed.generation.as_mut().unwrap().request_sha256 = sha256("different")
                }
                "source" => changed.source.version = "changed".into(),
                _ => changed.rights.attribution = "changed".into(),
            }
            assert!(
                check_revision(&changed, &records[1], &spec(QWEN), RevisionStyle::AntiAi).is_err()
            );
        }
    }

    #[test]
    fn seed_reconstruction_keeps_declared_pair_and_does_not_replace_old_train() {
        let mut old = initial("one", Split::Train);
        let mut other_draft = old[1].clone();
        other_draft.id = "another-old-draft".into();
        other_draft.generation.as_mut().unwrap().prompt_profile = None;
        let mut other_edit = old[2].clone();
        other_edit.id = "another-old-edit".into();
        other_edit.generation.as_mut().unwrap().prompt_profile = None;
        old.extend([other_draft, other_edit]);
        let seeds = reconstruct_seeds(&old).unwrap();
        assert_eq!(
            seeds.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["one-human", "one-draft", "one-edit"]
        );
        let revisions = complete("one", Split::Train);
        let mut assembled = old.clone();
        assembled.extend_from_slice(&revisions[3..]);
        assert_eq!(assembled.len(), old.len() + 2);
        for (before, after) in old.iter().zip(&assembled) {
            same_record(before, after).unwrap();
        }
    }

    #[test]
    fn stage_views_share_full_archive_family_cohort_and_export_order() {
        let records = complete("one", Split::Development);
        let mut files = BTreeMap::new();
        views(&records, "primary", &mut files).unwrap();
        let expected = record_bytes(&records).unwrap();
        assert_eq!(files["primary/development.jsonl"], expected);
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("development.jsonl");
        fs::write(&archive, &expected).unwrap();
        for depth in 0..=2 {
            let path = temp.path().join(format!("R{depth}.json"));
            fs::write(&path, &files[&format!("primary/development-R{depth}.json")]).unwrap();
            let loaded = evaluation_view::load_view(&path, &archive, Split::Development).unwrap();
            assert_eq!(loaded.archive.len(), 5);
            assert_eq!(loaded.selected.len(), 3);
            assert_eq!(loaded.view.families[0].chain.revision_depth, depth);
            assert_eq!(loaded.selected[0].id, "one-human");
            assert_eq!(loaded.selected[2].id, "one-edit");
        }
    }

    #[test]
    fn fresh_initial_generation_settings_are_bound_for_drafts_and_edits() {
        let records = initial("one", Split::Development);
        let profile = records[1]
            .generation
            .as_ref()
            .unwrap()
            .prompt_profile
            .clone()
            .unwrap();
        let plan = Plan {
            roots: vec![records[0].clone()],
            cohorts: BTreeMap::from([("one".into(), (QWEN.into(), profile))]),
        };
        check_fresh(&records, &plan, false).unwrap();
        for index in [1, 2] {
            for field in ["temperature", "max_tokens", "seed"] {
                let mut changed = records.clone();
                let generation = changed[index].generation.as_mut().unwrap();
                match field {
                    "temperature" => generation.temperature = 0.8,
                    "max_tokens" => generation.max_tokens = 1900,
                    _ => generation.seed = Some(17),
                }
                assert!(check_fresh(&changed, &plan, false).is_err());
            }
        }
    }

    #[test]
    fn closed_pass_requires_exact_export_and_does_not_reinterpret_missing_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let records = complete("one", Split::Development);
        let mut input = records[..3].to_vec();
        input.extend(initial("failed-family", Split::Development));
        let output = records[..4].to_vec();
        fs::write(
            temp.path().join("input.jsonl"),
            record_bytes(&input).unwrap(),
        )
        .unwrap();
        fs::write(
            temp.path().join("records.jsonl"),
            record_bytes(&output).unwrap(),
        )
        .unwrap();
        fs::write(temp.path().join("run.json"),serde_json::to_vec(&json!({"schema":"slop-ninja-generation-run-v1","prompt_version":"slop-ninja-model-revision-v1","revision_style":"anti-ai",
            "input_sha256":sha256(record_bytes(&input).unwrap()),"model":spec(QWEN),"split":null})).unwrap()).unwrap();
        let mut summary = json!({"status":"complete","complete_export_ready":true,"complete_families_only":true,"unattempted":[],"missing":["failure-receipt-exists-separately"],"summary":{"records":4}});
        let summary_path = temp.path().join("summary.json");
        fs::write(&summary_path, serde_json::to_vec(&summary).unwrap()).unwrap();
        let paths = PassPaths {
            run: "run.json".into(),
            input: "input.jsonl".into(),
            records: "records.jsonl".into(),
        };
        let pass = read_pass(
            temp.path(),
            &paths,
            "fixture",
            QWEN,
            RevisionStyle::AntiAi,
            &mut Bindings::new(),
        )
        .unwrap();
        assert!(pass.binding.get("failed_count").is_none());
        let lock = temp.path().join("run.lock");
        fs::write(&lock, b"pid=synthetic-active-generator\n").unwrap();
        let mut bindings = Bindings::new();
        let error = read_pass(
            temp.path(),
            &paths,
            "fixture",
            QWEN,
            RevisionStyle::AntiAi,
            &mut bindings,
        )
        .err()
        .expect("A complete summary must not override an active run.lock");
        assert!(error.to_string().contains("run.lock"));
        assert!(bindings.is_empty());
        fs::remove_file(lock).unwrap();
        summary["unattempted"] = json!(["not-yet-attempted"]);
        fs::write(&summary_path, serde_json::to_vec(&summary).unwrap()).unwrap();
        assert!(
            read_pass(
                temp.path(),
                &paths,
                "fixture",
                QWEN,
                RevisionStyle::AntiAi,
                &mut Bindings::new()
            )
            .is_err()
        );
    }
}
