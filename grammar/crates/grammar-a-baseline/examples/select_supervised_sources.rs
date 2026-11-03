//! Select a small public multi-generator cohort without opening Test/private inputs.
//! Usage: select_supervised_sources WORKTREE INVENTORY_DIR NEW_OUTPUT_DIR
//! Run inventory_a_sources first. Parent owns Cargo scheduling and execution.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

const QWEN: &str =
    "mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit@e9675aa3ca5f900ccef55267914466d55ab325fa";
const MISTRAL: &str = "mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit@2a1d5eabfc504747bdc24178394821a1efc0edde";
const OLMO: &str = "mlx-community/Olmo-3-7B-Instruct-4bit@d732c91ae02e90cd5d86e810fbeb9741794b4dd9";
const DOMAIN: &str = "slop_ninja_a_supervised_source_selection_v2";

fn supervised_id(original_id: &str) -> String {
    format!(
        "historical-{}",
        &sha(format!("slop_ninja_a_historical_record_v1:{original_id}"))[..24]
    )
}

fn adapt_record(
    r: &Meta,
    original: &Value,
    original_bytes: &[u8],
    original_file: &str,
    input_binding: &Value,
) -> Result<Value> {
    let (share, share_evidence) = match r.origin.as_str() {
        "human_only" => (
            Some(0.0),
            json!({"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true}),
        ),
        "model_only" => (
            Some(1.0),
            json!({"basis":"recorded_model_only_workflow_endpoint","source_conditioned":true,"not_measured_word_spans":true}),
        ),
        "mixed" => (
            None,
            json!({"basis":"model_edit_of_historical_proxy_without_realized_span_provenance","fractional_share_supervision_masked":true}),
        ),
        _ => anyhow::bail!("Unsupported origin"),
    };
    let ai_identity = if r.contributor_revisions.len() == 1 {
        r.contributor_revisions.iter().next().cloned()
    } else {
        None
    };
    let contains_withheld = r.contributor_revisions.contains(OLMO);
    ensure!(
        r.split != "train" || !contains_withheld,
        "Withheld lineage entered Train"
    );
    Ok(json!({
        "schema":"slop_ninja_a_supervised_record_v1",
        "id":supervised_id(&r.id),"text":original["text"],"text_sha256":r.text_sha256,
        "source_group":r.source_group,"frozen_split":r.split,
        "origin":r.origin,"evidence":r.evidence,"purpose":"query",
        "exposed_to_grammar":true,"grammar_exposure":"conservative_unknown_prior_exposure;historical_public_detector_corpus",
        "share_target":share,"share_evidence":share_evidence,
        "human_identity":null,"human_role_present":r.origin != "model_only",
        "ai_identity":ai_identity,"ai_role_present":r.origin != "human_only",
        "rights":original["rights"],"generation":original["generation"],
        "parent_source_id":r.id,"parent_id":r.parent_id.as_deref().map(supervised_id),
        "metadata":{
            "all_model_contributor_revisions":r.contributor_revisions,
            "final_generator_revision":r.final_generator_revision,
            "original_record_id":r.id,"original_parent_id":r.parent_id,
            "operation":r.operation,"source_collection":r.collection,
            "human_identity_supervision_mask":false,
            "ai_identity_supervision_mask":ai_identity.is_some(),
            "multi_model_identity_masked":r.contributor_revisions.len()>1,
            "contains_train_withheld_generator":contains_withheld,
            "eligible_for_parameter_fit":r.split=="train",
            "eligible_for_calibration_temperature_and_rejection_threshold":r.split=="calibration",
            "included_in_primary_evaluation_population":r.split!="train",
            "open_set_claim":"OLMo is excluded from Train and named-profile enrollment; OLMo Calibration observations may fit temperature/rejection thresholds, so open-set claims are conditional on this calibration population.",
            "lineage_policy":"Single-contributor identity only; mask the identity target for multiple model contributors while retaining every contributor and role-presence label."
        },
        "source":{
            "record":original,"path":format!("datasets/{}/{}",r.dataset,r.file),
            "record_id":r.id,"record_sha256":sha(original_bytes),
            "record_checksum_basis":"exact original JSONL line bytes including terminating newline when present",
            "file_sha256":input_binding["sha256"],"rights":original["rights"],
            "retained_original_file":format!("originals/{original_file}"),
            "original_parent_id":r.parent_id
        }
    }))
}

#[derive(Clone, Deserialize)]
struct Meta {
    dataset: String,
    file: String,
    id: String,
    source_group: String,
    text_sha256: String,
    split: String,
    collection: String,
    origin: String,
    evidence: String,
    parent_id: Option<String>,
    final_generator_revision: String,
    operation: String,
    contributor_revisions: BTreeSet<String>,
    ancestry_complete: bool,
    rights_flags_all_true: bool,
}
#[derive(Default)]
struct Family {
    rows: Vec<String>,
    datasets: BTreeSet<String>,
}

fn sha(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}
fn write_json(path: &Path, v: &Value) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut w, v)?;
    writeln!(w)?;
    w.flush()?;
    Ok(())
}
fn allowed_file(dataset: &str, file: &str) -> bool {
    match dataset {
        "detector-v6" => matches!(
            file,
            "train.jsonl" | "development.jsonl" | "calibration.jsonl"
        ),
        "detector-origin-v1" => matches!(
            file,
            "train/pilot.jsonl"
                | "train/second.jsonl"
                | "train/third.jsonl"
                | "train/fourth.jsonl"
                | "development/initial.jsonl"
        ),
        "detector-revisions-v1" => file == "train.jsonl",
        _ => false,
    }
}
fn known_lineage(r: &Meta) -> bool {
    r.contributor_revisions
        .iter()
        .all(|g| matches!(g.as_str(), QWEN | MISTRAL))
}
fn pair(f: &Family, rows: &BTreeMap<String, Meta>, generator: &str) -> bool {
    ["draft", "edit"].into_iter().all(|op| {
        f.rows.iter().any(|id| {
            let r = &rows[id];
            r.operation == op && r.final_generator_revision == generator
        })
    })
}

// Round-robin source collections, stable SHA256 ordering within each collection.
// The profile text, detector observations, and source prose cannot affect rank.
fn choose(candidates: Vec<(String, String)>, n: usize, bucket: &str) -> Result<Vec<String>> {
    let mut bins = BTreeMap::<String, Vec<(String, String)>>::new();
    for (collection, group) in candidates {
        bins.entry(collection)
            .or_default()
            .push((sha(serde_json::to_vec(&(DOMAIN, bucket, &group))?), group));
    }
    let mut bins: BTreeMap<_, VecDeque<_>> = bins
        .into_iter()
        .map(|(key, mut list)| {
            list.sort();
            (key, list.into_iter().map(|(_, g)| g).collect())
        })
        .collect();
    let mut result = Vec::new();
    while result.len() < n {
        let before = result.len();
        for bin in bins.values_mut() {
            if let Some(group) = bin.pop_front() {
                result.push(group);
            }
            if result.len() == n {
                break;
            }
        }
        ensure!(
            result.len() > before,
            "Insufficient eligible families for {bucket}: wanted {n}, found {}",
            result.len()
        );
    }
    Ok(result)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "Usage: select_supervised_sources WORKTREE INVENTORY_DIR NEW_OUTPUT_DIR"
    );
    let repo = Path::new(&args[0]).canonicalize()?;
    let inventory = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    ensure!(!out.exists(), "Use a new output directory");
    let inventory_bytes = fs::read(inventory.join("inventory.json"))?;
    let summary: Value = serde_json::from_slice(&inventory_bytes)?;
    ensure!(
        summary["schema"] == "slop_ninja_a_source_inventory_v2",
        "Unexpected inventory schema"
    );
    ensure!(
        summary["rights_flag_failures"] == 0
            && summary["missing_parents"] == json!([])
            && summary["family_split_conflicts"] == json!({})
            && summary["exact_hash_split_conflicts"] == json!({}),
        "Inventory has eligibility/ancestry/split conflicts; resolve without changing original splits"
    );
    let metadata_bytes = fs::read(inventory.join("rows.metadata.jsonl"))?;
    let mut rows = BTreeMap::<String, Meta>::new();
    let mut families = BTreeMap::<(String, String), Family>::new();
    let mut deduplicated_occurrences = 0usize;
    for line in metadata_bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
    {
        let r: Meta = serde_json::from_slice(line)?;
        ensure!(
            allowed_file(&r.dataset, &r.file)
                && matches!(r.split.as_str(), "train" | "development" | "calibration"),
            "Input outside nonTest public allowlist"
        );
        ensure!(
            r.ancestry_complete && r.rights_flags_all_true,
            "Ineligible record"
        );
        ensure!(
            r.contributor_revisions
                .iter()
                .all(|g| matches!(g.as_str(), QWEN | MISTRAL | OLMO)),
            "Unreviewed generator identity"
        );
        let family = families
            .entry((r.split.clone(), r.source_group.clone()))
            .or_default();
        family.datasets.insert(r.dataset.clone());
        if let Some(old) = rows.get(&r.id) {
            ensure!(
                old.text_sha256 == r.text_sha256
                    && old.source_group == r.source_group
                    && old.split == r.split
                    && old.origin == r.origin
                    && old.parent_id == r.parent_id
                    && old.contributor_revisions == r.contributor_revisions
                    && old.final_generator_revision == r.final_generator_revision,
                "Conflicting duplicate ID; do not silently merge lineage"
            );
            deduplicated_occurrences += 1;
            continue;
        }
        family.rows.push(r.id.clone());
        rows.insert(r.id.clone(), r);
    }
    let mut selected_families = BTreeMap::<(String, String), (String, String)>::new();
    let mut availability = Vec::new();
    for split in ["train", "development", "calibration"] {
        let requests: Vec<(&str, usize)> = if split == "train" {
            vec![("both_known", 30)]
        } else {
            vec![(MISTRAL, 4), (QWEN, 3), (OLMO, 3)]
        };
        for (generator, n) in requests {
            let candidates: Vec<_> = families
                .iter()
                .filter(|((s, _), f)| s == split && f.datasets.contains("detector-v6"))
                .filter(|(_, f)| {
                    if generator == "both_known" {
                        pair(f, &rows, QWEN) && pair(f, &rows, MISTRAL)
                    } else {
                        pair(f, &rows, generator)
                    }
                })
                .map(|((_, g), f)| (rows[&f.rows[0]].collection.clone(), g.clone()))
                .collect();
            availability.push(json!({"split":split,"required_composer":generator,"eligible_families":candidates.len(),"requested_families":n}));
            let lane = if generator == OLMO {
                "withheld_olmo"
            } else {
                "fit"
            };
            for group in choose(candidates, n, &format!("{split}/{generator}"))? {
                ensure!(
                    selected_families
                        .insert((split.into(), group), (lane.into(), generator.into()))
                        .is_none(),
                    "Family selected twice"
                );
            }
        }
    }
    ensure!(
        selected_families.len() == 50 && selected_families.len() <= 120,
        "Family budget changed"
    );
    let mut selected = BTreeMap::<String, String>::new();
    let mut excluded_lineage = Vec::new();
    let mut plan_families = Vec::new();
    for ((split, group), (lane, composer)) in &selected_families {
        let f = &families[&(split.clone(), group.clone())];
        let mut ids = Vec::new();
        for id in &f.rows {
            let r = &rows[id];
            if split == "train" && !known_lineage(r) {
                excluded_lineage.push(json!({"id":id,"split":split,"source_group":group,"contributor_revisions":r.contributor_revisions,"reason":"Contains withheld generator; exclude complete descendant, never relabel final stage as known-only"}));
                continue;
            }
            let filename = if split == "train" {
                "train.jsonl".to_owned()
            } else {
                format!("{split}.{lane}.jsonl")
            };
            selected.insert(id.clone(), filename);
            ids.push(id.clone());
        }
        let human_roots = ids
            .iter()
            .filter(|id| rows[*id].origin == "human_only")
            .count();
        ensure!(
            human_roots == 1,
            "Expected one retained human proxy per family"
        );
        plan_families.push(json!({"source_group":group,"split":split,"lane":lane,"required_composer":composer,"collection":rows[&f.rows[0]].collection,"record_ids":ids}));
    }
    for (id, filename) in &selected {
        let r = &rows[id];
        if let Some(parent) = &r.parent_id {
            ensure!(
                selected.get(parent) == Some(filename),
                "Selection must retain parent in same lane and original family"
            );
        }
        if filename == "train.jsonl" {
            ensure!(
                !r.contributor_revisions.contains(OLMO),
                "Withheld identity leaked into fitting"
            );
        }
    }
    // Re-open only the nine exact public files bound by the inventory. Preserve
    // selected original JSONL bytes, including text, full prompts, rights and ancestry.
    let mut raw = BTreeMap::<String, Vec<u8>>::new();
    let mut validated_inputs = Vec::new();
    for binding in summary["inputs"]
        .as_array()
        .context("Missing input bindings")?
    {
        let dataset = binding["dataset"].as_str().context("Missing dataset")?;
        let file = binding["file"].as_str().context("Missing file")?;
        ensure!(allowed_file(dataset, file), "Forbidden file binding");
        let path = repo.join("datasets").join(dataset).join(file);
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Symlink refused"
        );
        let bytes = fs::read(path)?;
        ensure!(
            binding["sha256"] == sha(&bytes),
            "Inventory input hash no longer matches"
        );
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            let v: Value = serde_json::from_slice(line)?;
            let id = v["id"].as_str().context("Missing record ID")?;
            if !selected.contains_key(id) {
                continue;
            }
            let r = &rows[id];
            ensure!(
                v["split"] == r.split
                    && v["source_group"] == r.source_group
                    && v["text_sha256"] == r.text_sha256,
                "Selection metadata mismatch"
            );
            ensure!(
                sha(v["text"]
                    .as_str()
                    .context("Missing selected text")?
                    .as_bytes())
                    == r.text_sha256,
                "Selected text hash mismatch"
            );
            if let Some(previous) = raw.get(id) {
                ensure!(
                    serde_json::from_slice::<Value>(previous)? == v,
                    "Duplicate ID differs in full provenance"
                );
            } else {
                raw.insert(id.into(), line.to_vec());
            }
        }
        validated_inputs.push(binding.clone());
    }
    ensure!(
        raw.len() == selected.len(),
        "Selected records missing from bound files"
    );
    fs::create_dir(out)?;
    fs::create_dir(out.join("originals"))?;
    let mut outputs = Vec::new();
    let mut metadata = BufWriter::new(File::create(out.join("selected.metadata.jsonl"))?);
    let mut output_buffers = BTreeMap::<String, Vec<u8>>::new();
    let mut text_ids = BTreeMap::<String, String>::new();
    let mut supervised = Vec::new();
    let mut adapted_ids = BTreeSet::new();
    for (id, filename) in &selected {
        let r = &rows[id];
        ensure!(
            text_ids.insert(r.text_sha256.clone(), id.clone()).is_none(),
            "Selected distinct IDs share exact text; review without discarding lineage"
        );
        output_buffers
            .entry(filename.clone())
            .or_default()
            .extend_from_slice(&raw[id]);
        let diagnostic = filename.contains("withheld_olmo");
        let item = json!({"id":id,"supervised_id":supervised_id(id),"source_group":r.source_group,"split":r.split,"output_file":format!("originals/{filename}"),"supervised_output_file":"records.jsonl","text_sha256":r.text_sha256,"collection":r.collection,"origin":r.origin,"evidence":r.evidence,"parent_id":r.parent_id,"operation":r.operation,"final_generator_revision":r.final_generator_revision,"all_model_contributor_revisions":r.contributor_revisions,"eligible_for_parameter_fit":r.split=="train","eligible_for_calibration_temperature_and_rejection_threshold":r.split=="calibration","included_in_primary_evaluation_population":r.split!="train","withheld_generator_family":diagnostic,"human_attribution_target":null,"measured_mixed_share_target":null,"model_identity_policy":"Single entire-lineage contributor gives its exact revision label; multiple contributors mask the identity target while retaining full lineage."});
        let original: Value = serde_json::from_slice(&raw[id])?;
        let input_binding = validated_inputs
            .iter()
            .find(|b| b["dataset"] == r.dataset && b["file"] == r.file)
            .context("Missing original file binding")?;
        let adapted = adapt_record(r, &original, &raw[id], filename, input_binding)?;
        ensure!(
            adapted_ids.insert(supervised_id(id)),
            "Adapted ID collision"
        );
        serde_json::to_writer(&mut supervised, &adapted)?;
        supervised.push(b'\n');
        serde_json::to_writer(&mut metadata, &item)?;
        writeln!(metadata)?;
    }
    metadata.flush()?;
    for (filename, bytes) in output_buffers {
        fs::write(out.join("originals").join(&filename), &bytes)?;
        let ids: Vec<_> = selected
            .iter()
            .filter(|(_, f)| **f == filename)
            .map(|(id, _)| id)
            .collect();
        let groups: BTreeSet<_> = ids.iter().map(|id| &rows[*id].source_group).collect();
        let mut generators = BTreeMap::<String, usize>::new();
        for id in &ids {
            for g in &rows[*id].contributor_revisions {
                *generators.entry(g.clone()).or_default() += 1;
            }
        }
        outputs.push(json!({"file":format!("originals/{filename}"),"sha256":sha(&bytes),"rows":ids.len(),"source_families":groups.len(),"rows_involving_each_generator":generators}));
    }
    fs::write(out.join("records.jsonl"), &supervised)?;
    outputs.push(json!({"file":"records.jsonl","schema":"slop_ninja_a_supervised_record_v1","sha256":sha(&supervised),"rows":selected.len(),"source_families":selected_families.len(),"includes_all_selected_olmo_development_and_calibration_records":true}));
    let plan = json!({"schema":DOMAIN,"inventory_sha256":sha(inventory_bytes),"metadata_sha256":sha(metadata_bytes),"source_code_sha256":sha(include_bytes!("select_supervised_sources.rs")),"inputs":validated_inputs,"deduplicated_record_occurrences":deduplicated_occurrences,"pool_unique_records":rows.len(),"pool_unique_families":families.len(),"availability":availability,"selected_families":plan_families,"selected_family_count":selected_families.len(),"outputs":outputs,"excluded_withheld_lineage_records":excluded_lineage,"known_historical_generator_profiles":[QWEN,MISTRAL],"withheld_generator":OLMO,"fresh_generator_policy":"Keep the parent's fresh Qwen3.6 checkpoint/revision separately identified; it is not the historical Qwen3-30B checkpoint. Fresh-author data are not read or selected here.","family_budget":{"historical_selected":50,"combined_cap":120,"remaining_for_fresh_or_other_independent_families":70},"selection_policy":"Preserve every original split. Deduplicate overlapping IDs after provenance agreement. Prefer v6 families to retain its fresh Dev/Cal allocation; origin adds only 19 legacy Dev families and is not needed. Source-collection round robin and domain-separated family hash rank; no detector feedback.","withheld_policy":"Exclude every OLMo-containing row and descendant from Train parameters, Train normalization and named-profile enrollment. Include selected OLMo Dev/Cal in the primary evaluation population. Calibration may fit temperature and unknown-rejection thresholds using these OLMo records; open-set claims are conditional on this calibration. No final Test text is read.","supervised_record_policy":"records.jsonl is directly usable supervised input for every selected row. human_identity is null; role flags follow origin; single model contributor receives exact full revision identity, multiple contributors receive null with complete lineage metadata. Share endpoints are 0/1, mixed share null with evidence. Original records and rights retained.","rights_policy":"Keep full rights, attribution, licenses, change notices and prompts in exact copied rows. Wikipedia-derived lane carries the project's CC-BY-SA-4.0 fine-tuned-weight release policy plus base checkpoint notices.","supervision_limits":["All historical human roots are proxies, not verified individual-author labels","Mixed final-word share remains unmeasured; do not substitute edit distance for causal model spans","Source-conditioned drafts are not generation from scratch","Retain every model contributor when scoring documented roles","Three OLMo families per evaluation split are a smoke check, not a precise generalization estimate","No private input, final Test text, network, model or detector calls"]});
    write_json(&out.join("selection.json"), &plan)?;
    println!(
        "Selected {} records / 50 families; plan: {}",
        selected.len(),
        out.join("selection.json").display()
    );
    Ok(())
}
