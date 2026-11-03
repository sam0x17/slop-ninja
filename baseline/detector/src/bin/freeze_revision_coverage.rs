//! Freeze v6 Test length coverage from complete tokenizer-only audit metadata.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{Evidence, Origin, OriginRecord, Split, sha256},
    evaluation_view::{self, EvaluationView, LoadedView, Selection},
    model::CLASS_NAMES,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const V5_LIMIT: usize = 1024;
const V6_LIMIT: usize = 2048;
const PLAN_HASH: &str = "f146e8b132272c3233b43cef932d880b2ef1a5962c39c4958cae1fa4c0d0f9d7";
const CHECKPOINT_HASH: &str = "57ca5bc56f8e89044216f3fba0172418bccebbf741b2145b17de7fe6da881757";
const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "config.json",
];

#[derive(Parser)]
struct Args {
    /// Full final primary or Phi-4 Test ancestry archive; run each route separately.
    #[arg(long)]
    records: PathBuf,
    #[arg(long)]
    r0_view: PathBuf,
    #[arg(long)]
    r1_view: PathBuf,
    #[arg(long)]
    r2_view: PathBuf,
    /// Existing audit.py --record-token-counts --max-tokens 2048 output for this archive.
    #[arg(long)]
    token_audit: PathBuf,
    /// All 117 assigned Test roots, byte-identical to the frozen plan's phi4-roots.jsonl.
    #[arg(long)]
    human_roots: PathBuf,
    #[arg(long)]
    human_token_audit: PathBuf,
    #[arg(long)]
    evaluation_plan: PathBuf,
    /// Original ModernBERT checkpoint.json only; no encoder weight file is read.
    #[arg(long)]
    checkpoint_pin: PathBuf,
    /// Frozen audit.py and common.py source files used for both token audits.
    #[arg(long)]
    audit_source: PathBuf,
    #[arg(long)]
    common_source: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenCount {
    id: String,
    text_sha256: String,
    token_count: usize,
}

fn read_bound(path: &Path, label: &str, inputs: &mut BTreeMap<String, Value>) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        inputs
            .insert(label.into(), json!({"path":path,"sha256":sha256(&bytes)}))
            .is_none(),
        "Duplicate input binding"
    );
    Ok(bytes)
}

fn read_archive(
    path: &Path,
    label: &str,
    inputs: &mut BTreeMap<String, Value>,
) -> Result<(Vec<OriginRecord>, Vec<u8>)> {
    let (records, hash) = evaluation_view::read_archive(path)?;
    let bytes = read_bound(path, label, inputs)?;
    ensure!(sha256(&bytes) == hash, "Archive changed while reading");
    ensure!(
        records.iter().all(|r| r.split == Some(Split::Test)),
        "Coverage accepts a complete Test-only archive"
    );
    Ok((records, bytes))
}

fn audit_counts(
    audit: &Value,
    records: &[OriginRecord],
    records_hash: &str,
    checkpoint_hash: &str,
    provenance: &Value,
) -> Result<BTreeMap<String, TokenCount>> {
    ensure!(
        audit["schema"] == "slop_ninja.encoder_shard_audit.v1"
            && audit["max_tokens"] == V6_LIMIT
            && audit["checkpoint_pin_sha256"] == checkpoint_hash
            && audit["record_token_counts_provenance"] == *provenance,
        "Audit schema, token limit, checkpoint or source/tokenizer binding differs"
    );
    ensure!(
        audit["model_executed"] == false
            && audit["model_fitted"] == false
            && audit["calibration_fitted"] == false
            && audit["records_dropped"] == 0
            && audit["cross_partition_id_group_text_overlap"] == false
            && audit["test_inspection"] == "counts_and_token_lengths_only",
        "Require a tokenizer-only audit without filtering, fitting or scores"
    );
    let shard = &audit["shards"]["test"];
    let groups: BTreeSet<_> = records.iter().map(|r| &r.source_group).collect();
    ensure!(
        shard["sha256"] == records_hash
            && shard["records"] == records.len()
            && shard["source_groups"] == groups.len(),
        "Audit does not bind the full Test archive and counts"
    );
    for (label, index) in CLASS_NAMES.iter().zip(0..) {
        ensure!(
            shard["class_counts"][*label]
                == records.iter().filter(|r| r.origin.index() == index).count(),
            "Audit origin counts differ from the archive"
        );
    }
    let rows: Vec<TokenCount> = serde_json::from_value(shard["record_token_counts"].clone())
        .context("Require --record-token-counts with every record explicitly present")?;
    ensure!(
        rows.len() == records.len(),
        "Incomplete per-record token metadata"
    );
    let expected: BTreeMap<_, _> = records
        .iter()
        .map(|r| (r.id.as_str(), r.text_sha256.as_str()))
        .collect();
    let mut counts = BTreeMap::new();
    for row in rows {
        ensure!(
            row.token_count > 0
                && expected
                    .get(row.id.as_str())
                    .is_some_and(|hash| *hash == row.text_sha256),
            "Unknown audit ID, changed text hash or invalid token count"
        );
        ensure!(
            counts.insert(row.id.clone(), row).is_none(),
            "Duplicate per-record token metadata"
        );
    }
    ensure!(
        counts.len() == expected.len(),
        "Missing per-record token metadata"
    );
    let over: BTreeMap<_, _> = counts
        .values()
        .filter(|r| r.token_count > V6_LIMIT)
        .map(|r| (r.id.clone(), r.token_count))
        .collect();
    let stated = shard["over_limit_records"]
        .as_array()
        .context("Missing audit overflow records")?;
    let mut stated_over = BTreeMap::new();
    for row in stated {
        let id = row["id"].as_str().context("Invalid overflow ID")?;
        let count = usize::try_from(
            row["token_count"]
                .as_u64()
                .context("Invalid overflow token count")?,
        )?;
        ensure!(
            stated_over.insert(id.to_owned(), count).is_none(),
            "Duplicate audit overflow ID"
        );
    }
    ensure!(
        stated_over == over && shard["over_limit_count"] == over.len(),
        "Audit overflow summary disagrees with complete row metadata"
    );
    let lengths = &shard["token_lengths_including_special_tokens"];
    ensure!(
        lengths["min"]
            == counts
                .values()
                .map(|r| r.token_count)
                .min()
                .context("Empty token metadata")?
            && lengths["max"] == counts.values().map(|r| r.token_count).max().unwrap(),
        "Audit length bounds disagree with row metadata"
    );
    Ok(counts)
}

fn verify_stages(stages: &[EvaluationView; 3]) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let [r0, r1, r2] = stages;
    ensure!(
        stages.iter().all(|v| v.split == Split::Test
            && v.records_sha256 == r0.records_sha256
            && v.families.len() == r0.families.len())
            && !r0.families.is_empty(),
        "Stage views must share one full Test archive and nonempty family cohort"
    );
    let mut viewed = BTreeMap::new();
    for ((a, b), c) in r0.families.iter().zip(&r1.families).zip(&r2.families) {
        ensure!(
            a.source_group == b.source_group
                && a.source_group == c.source_group
                && a.human == b.human
                && a.human == c.human
                && a.mixed == b.mixed
                && a.mixed == c.mixed,
            "R0/R1/R2 family sets or paired human/edit observations differ"
        );
        ensure!(
            a.chain.revision_depth == 0
                && b.chain.revision_depth == 1
                && c.chain.revision_depth == 2
                && a.chain.revisions.is_empty()
                && b.chain.revisions.len() == 1
                && c.chain.revisions.len() == 2,
            "Expected frozen R0, R1 and R2 revision depths"
        );
        ensure!(
            a.chain.composer == b.chain.composer
                && a.chain.composer == c.chain.composer
                && a.chain.initial_profile_id == b.chain.initial_profile_id
                && a.chain.initial_profile_id == c.chain.initial_profile_id
                && a.model.id == c.chain.composer.id
                && b.model.id == c.chain.revisions[0].id
                && c.model.id == c.chain.revisions[1].id
                && b.chain.revisions[0] == c.chain.revisions[0],
            "Stage views do not follow the same recorded revision chain"
        );
        let ids: BTreeSet<_> = [
            &a.human.id,
            &a.model.id,
            &a.mixed.id,
            &b.model.id,
            &c.model.id,
        ]
        .into_iter()
        .cloned()
        .collect();
        ensure!(
            ids.len() == 5 && viewed.insert(a.source_group.clone(), ids).is_none(),
            "Stage observation identity is not a unique complete trio sequence"
        );
    }
    Ok(viewed)
}

fn project_bytes(records: &[OriginRecord]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn common_projection(
    records: &[OriginRecord],
    counts: &BTreeMap<String, TokenCount>,
    viewed: &BTreeMap<String, BTreeSet<String>>,
) -> Result<(Vec<OriginRecord>, Vec<Value>)> {
    ensure!(
        records
            .iter()
            .all(|r| counts[&r.id].token_count <= V6_LIMIT),
        "Full v6 ancestry archive exceeds 2048 tokens; stop for the protocol amendment instead of dropping or truncating records"
    );
    let groups: BTreeSet<_> = records.iter().map(|r| r.source_group.as_str()).collect();
    ensure!(
        viewed.keys().map(String::as_str).collect::<BTreeSet<_>>() == groups,
        "Stage views omit an archive family"
    );
    let excluded: BTreeSet<_> = viewed
        .iter()
        .filter(|(_, ids)| ids.iter().any(|id| counts[id].token_count > V5_LIMIT))
        .map(|(group, _)| group.as_str())
        .collect();
    let mut exclusions = Vec::new();
    for group in &excluded {
        let rows: Vec<_> = records.iter().filter(|r|r.source_group == **group).map(|r|json!({"id":r.id,"origin":r.origin,"text_sha256":r.text_sha256,"token_count":counts[&r.id].token_count,
            "used_by_stage_views":viewed[*group].contains(&r.id),"exceeds_v5_limit":counts[&r.id].token_count > V5_LIMIT})).collect();
        let triggers: Vec<_> = viewed[*group]
            .iter()
            .filter(|id| counts[*id].token_count > V5_LIMIT)
            .collect();
        exclusions.push(json!({"source_group":group,"reason":"viewed_record_exceeds_v5_token_limit","triggering_ids":triggers,"excluded_records":rows}));
    }
    let selected = records
        .iter()
        .filter(|r| !excluded.contains(r.source_group.as_str()))
        .cloned()
        .collect();
    Ok((selected, exclusions))
}

fn write_common_views(
    records: &[OriginRecord],
    original_bytes: &[u8],
    original_rows: usize,
    stages: &[EvaluationView; 3],
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<Option<Value>> {
    if records.is_empty() {
        return Ok(None);
    }
    let bytes = if records.len() == original_rows {
        original_bytes.to_vec()
    } else {
        project_bytes(records)?
    };
    let hash = sha256(&bytes);
    files.insert("common/records.jsonl".into(), bytes);
    let groups: BTreeSet<_> = records.iter().map(|r| r.source_group.as_str()).collect();
    let mut bindings = Vec::new();
    for (depth, stage) in stages.iter().enumerate() {
        let selections: Vec<_> = stage
            .families
            .iter()
            .filter(|f| groups.contains(f.source_group.as_str()))
            .map(|f| Selection {
                source_group: f.source_group.clone(),
                human_id: f.human.id.clone(),
                model_id: f.model.id.clone(),
                mixed_id: f.mixed.id.clone(),
            })
            .collect();
        let view = evaluation_view::build_view(records, hash.clone(), Split::Test, &selections)?;
        let mut bytes = serde_json::to_vec_pretty(&view)?;
        bytes.push(b'\n');
        bindings.push(json!({"stage":format!("R{depth}"),"sha256":sha256(&bytes),"records_sha256":hash,"split":"test","selected_rows":selections.len()*3,"source_groups":selections.len()}));
        files.insert(format!("common/R{depth}.json"), bytes);
    }
    Ok(Some(
        json!({"records_sha256":hash,"records":records.len(),"source_groups":groups.len(),"views":bindings}),
    ))
}

fn human_coverage(
    records: &[OriginRecord],
    bytes: &[u8],
    counts: &BTreeMap<String, TokenCount>,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<Value> {
    ensure!(
        records.iter().all(|r| r.origin == Origin::HumanOnly
            && r.evidence == Evidence::HistoricalProxy
            && r.parent_id.is_none()
            && r.generation.is_none()
            && r.split == Some(Split::Test))
            && records
                .iter()
                .map(|r| &r.source_group)
                .collect::<BTreeSet<_>>()
                .len()
                == records.len(),
        "Human coverage requires every unique historical human Test root"
    );
    files.insert("humans/all.jsonl".into(), bytes.to_vec());
    let mut result = serde_json::Map::new();
    for (name, limit) in [("v5", V5_LIMIT), ("v6", V6_LIMIT)] {
        let selected: Vec<_> = records
            .iter()
            .filter(|r| counts[&r.id].token_count <= limit)
            .cloned()
            .collect();
        let excluded: Vec<_> = records.iter().filter(|r|counts[&r.id].token_count > limit).map(|r|json!({"id":r.id,"source_group":r.source_group,"text_sha256":r.text_sha256,"token_count":counts[&r.id].token_count,"reason":"unsupported_length"})).collect();
        let file = if selected.is_empty() {
            None
        } else {
            let output = if selected.len() == records.len() {
                bytes.to_vec()
            } else {
                project_bytes(&selected)?
            };
            let path = format!("humans/{name}-supported.jsonl");
            let binding = json!({"path":path,"sha256":sha256(&output)});
            files.insert(path, output);
            Some(binding)
        };
        result.insert(name.into(),json!({"max_tokens":limit,"supported_roots":selected.len(),"unsupported_roots":excluded.len(),"excluded":excluded,"archive":file}));
    }
    Ok(
        json!({"assigned_roots":records.len(),"original_records_sha256":sha256(bytes),"observation_policy":"all_human_roots_v1","coverage":result,
        "policy":"Original human roots remain independent of generation completion. Unsupported lengths are explicit coverage failures; no text is truncated. This is the same human sample across routes/stages, not additional independent observations."}),
    )
}

fn freeze(args: Args) -> Result<Value> {
    ensure!(
        !args.output_dir.exists(),
        "Use a new immutable coverage directory"
    );
    let mut inputs = BTreeMap::new();
    let checkpoint_bytes = read_bound(&args.checkpoint_pin, "checkpoint_pin", &mut inputs)?;
    let checkpoint_hash = sha256(&checkpoint_bytes);
    ensure!(
        checkpoint_hash == CHECKPOINT_HASH,
        "Coverage requires the original shared v5/v6 ModernBERT checkpoint pin"
    );
    let checkpoint: Value = serde_json::from_slice(&checkpoint_bytes)?;
    let tokenizer_files: BTreeMap<_, _> = checkpoint["files"]
        .as_object()
        .context("Checkpoint lacks file bindings")?
        .iter()
        .filter(|(name, _)| TOKENIZER_FILES.contains(&name.as_str()))
        .map(|(name, hash)| (name.clone(), hash.clone()))
        .collect();
    ensure!(
        tokenizer_files.contains_key("tokenizer.json")
            && tokenizer_files.contains_key("tokenizer_config.json"),
        "Missing pinned tokenizer assets"
    );
    let audit_source = read_bound(&args.audit_source, "audit_source", &mut inputs)?;
    let common_source = read_bound(&args.common_source, "common_source", &mut inputs)?;
    let provenance = json!({"audit_py_sha256":sha256(audit_source),"common_py_sha256":sha256(common_source),"tokenizer_files_sha256":tokenizer_files});
    let plan_bytes = read_bound(
        &args.evaluation_plan.join("plan.json"),
        "evaluation_plan",
        &mut inputs,
    )?;
    ensure!(
        sha256(&plan_bytes) == PLAN_HASH,
        "Evaluation source plan differs from the frozen v6 allocation"
    );
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    let (records, record_bytes) = read_archive(&args.records, "records", &mut inputs)?;
    let mut loaded = Vec::<LoadedView>::new();
    let mut files = BTreeMap::new();
    files.insert("full/records.jsonl".into(), record_bytes.clone());
    for (depth, path) in [&args.r0_view, &args.r1_view, &args.r2_view]
        .into_iter()
        .enumerate()
    {
        let view = evaluation_view::load_view(path, &args.records, Split::Test)?;
        let bytes = read_bound(path, &format!("R{depth}_view"), &mut inputs)?;
        ensure!(
            sha256(&bytes) == view.sha256 && view.view.records_sha256 == sha256(&record_bytes),
            "Stage archive or view changed while reading"
        );
        files.insert(format!("full/R{depth}.json"), bytes);
        loaded.push(view);
    }
    let stages: [EvaluationView; 3] = std::array::from_fn(|i| loaded[i].view.clone());
    let viewed = verify_stages(&stages)?;
    let audit: Value =
        serde_json::from_slice(&read_bound(&args.token_audit, "token_audit", &mut inputs)?)?;
    let counts = audit_counts(
        &audit,
        &records,
        &sha256(&record_bytes),
        &checkpoint_hash,
        &provenance,
    )?;
    let (selected, exclusions) = common_projection(&records, &counts, &viewed)?;
    let common = write_common_views(&selected, &record_bytes, records.len(), &stages, &mut files)?;
    let (humans, human_bytes) = read_archive(&args.human_roots, "human_roots", &mut inputs)?;
    ensure!(
        humans.len() == 117 && sha256(&human_bytes) == plan["files_sha256"]["phi4-roots.jsonl"],
        "Human coverage must contain the exact original 117 assigned Test roots, independent of generation success"
    );
    let human_by_group: BTreeMap<_, _> = humans
        .iter()
        .map(|r| (r.source_group.as_str(), r))
        .collect();
    let route_by_id: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    for family in &stages[0].families {
        let root = human_by_group
            .get(family.source_group.as_str())
            .context("Route family is not in the frozen Test allocation")?;
        ensure!(
            family.human.id == root.id
                && family.human.text_sha256 == root.text_sha256
                && serde_json::to_value(route_by_id[family.human.id.as_str()])?
                    == serde_json::to_value(root)?,
            "Route human differs from the originally assigned Test root or its provenance"
        );
    }
    let human_audit: Value = serde_json::from_slice(&read_bound(
        &args.human_token_audit,
        "human_token_audit",
        &mut inputs,
    )?)?;
    let human_counts = audit_counts(
        &human_audit,
        &humans,
        &sha256(&human_bytes),
        &checkpoint_hash,
        &provenance,
    )?;
    // Identical texts under the same pinned tokenizer must have the same lengths across audits.
    for family in &stages[0].families {
        ensure!(
            counts[&family.human.id] == human_counts[&family.human.id],
            "Route and independent human audits disagree for the same root"
        );
    }
    let humans_report = human_coverage(&humans, &human_bytes, &human_counts, &mut files)?;
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name.clone(), sha256(bytes)))
        .collect();
    let report = json!({"schema":"slop_ninja_v6_revision_coverage_v1","protocol_sha256":plan["protocol_sha256"],"limits":{"v5":V5_LIMIT,"v6":V6_LIMIT},"inputs":inputs,
        "token_audit_provenance":provenance,"full_route":{"records":records.len(),"source_groups":viewed.len(),"records_sha256":sha256(&record_bytes),"views":loaded.iter().map(LoadedView::binding).collect::<Vec<_>>()},
        "common_route":common,"excluded_families":exclusions,"excluded_family_count":exclusions.len(),"excluded_record_count":records.len()-selected.len(),"human_roots":humans_report,"files_sha256":hashes,
        "projection_policy":"Exclude a whole family from v5 comparison only when at least one R0/R1/R2 observation exceeds 1024 tokens including special tokens. Keep all original ancestors of admitted families. R0/R1/R2 use one identical common family cohort. The full v6 archive/views remain byte-identical and must all fit 2048 tokens.",
        "empty_support_policy":"A null common_route or human archive means no supported observations; no empty evaluation artifact or sensitivity estimate is manufactured.",
        "source_sha256":sha256(include_bytes!("freeze_revision_coverage.rs")),"executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "selection_used_predictions":false,"text_truncated":false,"model_executed":false,"model_fitted":false,"detector_calls":0,"test_inspection":"counts_and_token_lengths_only","test_predictions_opened":false});
    files.insert("coverage.json".into(), serde_json::to_vec_pretty(&report)?);
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
    let report = freeze(Args::parse())?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"full_families":report["full_route"]["source_groups"],"common_families":report["common_route"]["source_groups"],"excluded_families":report["excluded_family_count"],
        "human_v5_supported":report["human_roots"]["coverage"]["v5"]["supported_roots"],"human_v6_supported":report["human_roots"]["coverage"]["v6"]["supported_roots"],"detector_calls":0,"test_predictions_opened":false})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_detector::dataset;

    // Entirely invented ancestry records and token counts; no model execution.
    fn family(group: &str) -> Vec<OriginRecord> {
        let human: OriginRecord = serde_json::from_value(json!({
            "schema":dataset::RECORD_SCHEMA,"id":format!("{group}-human"),"source_group":group,
            "split":"test","origin":"human_only","evidence":"historical_proxy",
            "evidence_notes":"Invented coverage fixture; not authorship evidence.",
            "text":format!("A violet jar holds a paper moon for {group}."),
            "text_sha256":sha256(format!("A violet jar holds a paper moon for {group}.")),
            "source":{"collection":"synthetic-contract-fixture","url":"fixture://coverage/root","version":"fixture-v1",
                "published_at":null,"author_ids":[],"raw_path":"","raw_sha256":sha256("fixture"),"extraction":"original-fixture"},
            "rights":{"license":"Synthetic-Original","evidence_url":"fixture://coverage/rights","evidence_sha256":sha256("fixture grant"),
                "attribution":"Slop Ninja synthetic fixture","commercial_training":true,"model_release":true,"external_evaluation":false,"redistribute_text":true},
            "parent_id":null,"generation":null
        })).unwrap();
        let mut records = vec![human.clone()];
        for (id, parent, origin, evidence, operation, model) in [
            (
                "draft",
                "human",
                Origin::ModelOnly,
                Evidence::RecordedModelGeneration,
                "draft",
                "model-a",
            ),
            (
                "mixed",
                "human",
                Origin::Mixed,
                Evidence::ModelEditOfHistoricalProxy,
                "edit",
                "model-a",
            ),
            (
                "first",
                "draft",
                Origin::ModelOnly,
                Evidence::RecordedModelRevision,
                "revise",
                "model-a",
            ),
            (
                "second",
                "first",
                Origin::ModelOnly,
                Evidence::RecordedModelRevision,
                "revise",
                "model-b",
            ),
        ] {
            let mut record = human.clone();
            record.id = format!("{group}-{id}");
            record.parent_id = Some(format!("{group}-{parent}"));
            record.origin = origin;
            record.evidence = evidence;
            record.text = format!("Invented {id} record in coverage family {group}.");
            record.text_sha256 = sha256(&record.text);
            record.generation = Some(serde_json::from_value(json!({
                "model_id":model,"response_model":model,"model_revision":format!("{model}@pinned"),
                "model_license":"Apache-2.0","model_license_url":"fixture://coverage/model-license","model_license_sha256":sha256("fixture grant"),
                "quantization":"fixture","runtime":"fixture","prompt":"Invented coverage instruction.","request_sha256":sha256("request"),
                "response_sha256":sha256("response"),"operation":operation,"created_at":"2026-01-01T00:00:00Z",
                "temperature":0.7,"seed":null,"max_tokens":100,"finish_reason":"stop"
            })).unwrap());
            records.push(record);
        }
        records
    }

    fn stages(records: &[OriginRecord]) -> [EvaluationView; 3] {
        let groups: BTreeSet<_> = records.iter().map(|r| r.source_group.as_str()).collect();
        ["draft", "first", "second"].map(|stage| {
            let selections = groups
                .iter()
                .map(|group| Selection {
                    source_group: (*group).into(),
                    human_id: format!("{group}-human"),
                    model_id: format!("{group}-{stage}"),
                    mixed_id: format!("{group}-mixed"),
                })
                .collect::<Vec<_>>();
            evaluation_view::build_view(
                records,
                sha256(project_bytes(records).unwrap()),
                Split::Test,
                &selections,
            )
            .unwrap()
        })
    }

    fn counts(records: &[OriginRecord]) -> BTreeMap<String, TokenCount> {
        records
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    TokenCount {
                        id: r.id.clone(),
                        text_sha256: r.text_sha256.clone(),
                        token_count: V5_LIMIT,
                    },
                )
            })
            .collect()
    }

    fn audit(records: &[OriginRecord], counts: &BTreeMap<String, TokenCount>) -> Value {
        let over: Vec<_> = counts
            .values()
            .filter(|r| r.token_count > V6_LIMIT)
            .map(|r| json!({"id":r.id,"token_count":r.token_count}))
            .collect();
        let class_counts: BTreeMap<_, _> = CLASS_NAMES
            .iter()
            .enumerate()
            .map(|(i, label)| {
                (
                    *label,
                    records.iter().filter(|r| r.origin.index() == i).count(),
                )
            })
            .collect();
        json!({"schema":"slop_ninja.encoder_shard_audit.v1","max_tokens":V6_LIMIT,"checkpoint_pin_sha256":CHECKPOINT_HASH,"record_token_counts_provenance":{"fixture":true},
            "model_executed":false,"model_fitted":false,"calibration_fitted":false,"records_dropped":0,"cross_partition_id_group_text_overlap":false,"test_inspection":"counts_and_token_lengths_only",
            "shards":{"test":{"sha256":sha256(project_bytes(records).unwrap()),"records":records.len(),"source_groups":records.iter().map(|r|&r.source_group).collect::<BTreeSet<_>>().len(),
                "class_counts":class_counts,"record_token_counts":counts.values().collect::<Vec<_>>(),"over_limit_count":over.len(),"over_limit_records":over,
                "token_lengths_including_special_tokens":{"min":counts.values().map(|r|r.token_count).min(),"max":counts.values().map(|r|r.token_count).max()}}}})
    }

    #[test]
    fn audit_requires_exact_rows_and_raw_source_checkpoint_bindings() {
        let records = family("one");
        let counts = counts(&records);
        let original = audit(&records, &counts);
        let validate = |value: &Value| {
            audit_counts(
                value,
                &records,
                &sha256(project_bytes(&records).unwrap()),
                CHECKPOINT_HASH,
                &json!({"fixture":true}),
            )
        };
        assert_eq!(validate(&original).unwrap(), counts);
        for mutation in 0..8 {
            let mut value = original.clone();
            match mutation {
                0 => {
                    value["shards"]["test"]["record_token_counts"]
                        .as_array_mut()
                        .unwrap()
                        .pop();
                }
                1 => {
                    value["shards"]["test"]["record_token_counts"][1] =
                        value["shards"]["test"]["record_token_counts"][0].clone();
                }
                2 => {
                    value["shards"]["test"]["record_token_counts"][0]["text_sha256"] =
                        json!(sha256("changed"));
                }
                3 => {
                    value["shards"]["test"]["sha256"] = json!(sha256("changed whitespace"));
                }
                4 => {
                    value["record_token_counts_provenance"]["fixture"] = json!(false);
                }
                5 => {
                    value["checkpoint_pin_sha256"] = json!(sha256("other tokenizer"));
                }
                6 => {
                    value["model_executed"] = json!(true);
                }
                7 => {
                    value["shards"]["test"]["record_token_counts"][0]["token_count"] =
                        json!(V6_LIMIT + 1);
                }
                _ => unreachable!(),
            }
            assert!(validate(&value).is_err(), "Mutation {mutation} should fail");
        }
    }

    #[test]
    fn one_long_revision_removes_the_whole_family_at_every_stage() {
        let mut records = family("keep");
        records.extend(family("exclude"));
        let stages = stages(&records);
        let viewed = verify_stages(&stages).unwrap();
        let mut counts = counts(&records);
        counts.get_mut("exclude-first").unwrap().token_count = V5_LIMIT + 1;
        let original = project_bytes(&records).unwrap();
        let (selected, excluded) = common_projection(&records, &counts, &viewed).unwrap();
        assert_eq!(selected.len(), 5);
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0]["triggering_ids"], json!(["exclude-first"]));
        assert_eq!(excluded[0]["excluded_records"].as_array().unwrap().len(), 5);
        assert_eq!(project_bytes(&records).unwrap(), original);
        let mut files = BTreeMap::new();
        let common = write_common_views(&selected, &original, records.len(), &stages, &mut files)
            .unwrap()
            .unwrap();
        assert_eq!(common["source_groups"], 1);
        let temp = tempfile::tempdir().unwrap();
        for (name, bytes) in &files {
            let path = temp.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        for depth in 0..3 {
            let view = evaluation_view::load_view(
                &temp.path().join(format!("common/R{depth}.json")),
                &temp.path().join("common/records.jsonl"),
                Split::Test,
            )
            .unwrap();
            assert_eq!(view.view.families.len(), 1);
            assert_eq!(view.view.families[0].source_group, "keep");
            assert_eq!(view.view.families[0].chain.revision_depth, depth);
            assert_eq!(view.archive.len(), 5);
        }
        counts.get_mut("keep-human").unwrap().token_count = V6_LIMIT + 1;
        assert!(common_projection(&records, &counts, &viewed).is_err());
    }

    #[test]
    fn complete_and_empty_common_support_are_explicit_and_stage_identity_is_fixed() {
        let records = family("one");
        let stages = stages(&records);
        let viewed = verify_stages(&stages).unwrap();
        let mut counts = counts(&records);
        let (all, excluded) = common_projection(&records, &counts, &viewed).unwrap();
        assert_eq!(all.len(), records.len());
        assert!(excluded.is_empty());
        let mut original = project_bytes(&records).unwrap();
        original.push(b'\n');
        let mut files = BTreeMap::new();
        write_common_views(&all, &original, records.len(), &stages, &mut files).unwrap();
        assert_eq!(files["common/records.jsonl"], original);
        counts.get_mut("one-draft").unwrap().token_count = V6_LIMIT;
        let (none, excluded) = common_projection(&records, &counts, &viewed).unwrap();
        assert!(none.is_empty());
        assert_eq!(excluded.len(), 1);
        let mut files = BTreeMap::new();
        assert!(
            write_common_views(&none, &original, records.len(), &stages, &mut files)
                .unwrap()
                .is_none()
        );
        assert!(files.is_empty());
        let mut wrong = stages.clone();
        wrong[1] = stages[0].clone();
        assert!(verify_stages(&wrong).is_err());
        wrong = stages.clone();
        wrong[1].families[0].source_group = "other".into();
        assert!(verify_stages(&wrong).is_err());
    }

    #[test]
    fn independent_human_coverage_retains_failed_generation_and_unsupported_roots() {
        let humans = ["complete", "failed", "too-long"].map(|group| family(group).remove(0));
        let bytes = project_bytes(&humans).unwrap();
        let mut counts = counts(&humans);
        counts.get_mut("failed-human").unwrap().token_count = V5_LIMIT + 1;
        counts.get_mut("too-long-human").unwrap().token_count = V6_LIMIT + 1;
        let mut files = BTreeMap::new();
        let report = human_coverage(&humans, &bytes, &counts, &mut files).unwrap();
        assert_eq!(files["humans/all.jsonl"], bytes);
        assert_eq!(report["assigned_roots"], 3);
        assert_eq!(report["coverage"]["v5"]["supported_roots"], 1);
        assert_eq!(report["coverage"]["v6"]["supported_roots"], 2);
        assert_eq!(
            report["coverage"]["v6"]["excluded"][0]["id"],
            "too-long-human"
        );
        assert_eq!(
            report["coverage"]["v6"]["excluded"][0]["token_count"],
            V6_LIMIT + 1
        );
        let path = tempfile::tempdir().unwrap();
        fs::write(
            path.path().join("roots.jsonl"),
            &files["humans/v6-supported.jsonl"],
        )
        .unwrap();
        let (supported, _) =
            evaluation_view::read_archive(&path.path().join("roots.jsonl")).unwrap();
        assert!(supported.iter().any(|r| r.id == "failed-human"));
        let mut duplicate = humans.to_vec();
        duplicate.push(humans[0].clone());
        assert!(human_coverage(&duplicate, &bytes, &counts, &mut BTreeMap::new()).is_err());
        let mut mislabeled = humans.to_vec();
        mislabeled[0].evidence = Evidence::RecordedModelGeneration;
        assert!(human_coverage(&mislabeled, &bytes, &counts, &mut BTreeMap::new()).is_err());
    }
}
