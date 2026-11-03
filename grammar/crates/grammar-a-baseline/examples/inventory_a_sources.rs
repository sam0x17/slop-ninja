//! Metadata-only inventory of an explicit public Train/Dev/Cal file allowlist.
//! Usage: inventory_a_sources WORKTREE OUTPUT_DIR
//! Never opens Test shards, private corpora, acquisition archives, or model APIs.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};

const INPUTS: &[(&str, &str, &str)] = &[
    ("detector-v6", "train.jsonl", "train"),
    ("detector-v6", "development.jsonl", "development"),
    ("detector-v6", "calibration.jsonl", "calibration"),
    ("detector-origin-v1", "train/pilot.jsonl", "train"),
    ("detector-origin-v1", "train/second.jsonl", "train"),
    ("detector-origin-v1", "train/third.jsonl", "train"),
    ("detector-origin-v1", "train/fourth.jsonl", "train"),
    (
        "detector-origin-v1",
        "development/initial.jsonl",
        "development",
    ),
    ("detector-revisions-v1", "train.jsonl", "train"),
];

// Omitted prose, prompt, author names, and attribution fields are discarded by serde.
#[derive(Deserialize)]
struct Row {
    schema: String,
    id: String,
    source_group: String,
    split: String,
    origin: String,
    evidence: String,
    text_sha256: String,
    source: Source,
    rights: Value,
    parent_id: Option<String>,
    generation: Option<Generation>,
}
#[derive(Deserialize)]
struct Source {
    collection: String,
    author_ids: Vec<serde::de::IgnoredAny>,
}
#[derive(Deserialize)]
struct Generation {
    model_id: String,
    response_model: String,
    model_revision: String,
    model_license: String,
    model_license_url: String,
    model_license_sha256: String,
    quantization: String,
    runtime: String,
    operation: String,
    request_sha256: String,
    response_sha256: String,
    prompt_profile: Option<Profile>,
}
#[derive(Deserialize)]
struct Profile {
    profile: ProfileId,
}
#[derive(Deserialize)]
struct ProfileId {
    id: String,
}
#[derive(Default)]
struct Count {
    rows: usize,
    families: BTreeSet<String>,
}
#[derive(Default)]
struct SetSummary {
    ids: BTreeSet<String>,
    hashes: BTreeSet<String>,
    groups: BTreeSet<String>,
}
struct Entry {
    dataset: String,
    file: String,
    row: Row,
}

fn sha(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}
fn add(counts: &mut BTreeMap<Vec<String>, Count>, key: Vec<String>, group: &str) {
    let c = counts.entry(key).or_default();
    c.rows += 1;
    c.families.insert(group.to_owned());
}
fn table(counts: BTreeMap<Vec<String>, Count>, columns: &[&str]) -> Vec<Value> {
    counts
        .into_iter()
        .map(|(keys, c)| {
            let mut obj = serde_json::Map::new();
            for (k, v) in columns.iter().zip(keys) {
                obj.insert((*k).to_owned(), json!(v));
            }
            obj.insert("rows".into(), json!(c.rows));
            obj.insert("source_families".into(), json!(c.families.len()));
            Value::Object(obj)
        })
        .collect()
}
fn generator(row: &Row) -> &str {
    row.generation
        .as_ref()
        .map(|g| g.model_revision.as_str())
        .unwrap_or("human_proxy")
}
fn profile(row: &Row) -> &str {
    row.generation
        .as_ref()
        .and_then(|g| g.prompt_profile.as_ref())
        .map(|p| p.profile.id.as_str())
        .unwrap_or("none_recorded")
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "Usage: inventory_a_sources WORKTREE OUTPUT_DIR"
    );
    let repo = Path::new(&args[0]).canonicalize()?;
    let out = Path::new(&args[1]);
    fs::create_dir_all(out)?;
    let mut entries = Vec::new();
    let mut inputs = Vec::new();
    for &(dataset, file, split) in INPUTS {
        let path = repo.join("datasets").join(dataset).join(file);
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Symlink input refused"
        );
        let manifest: Value = serde_json::from_slice(&fs::read(
            repo.join("datasets").join(dataset).join("manifest.json"),
        )?)?;
        let expected = if dataset == "detector-v6" {
            &manifest["partitions"][split]
        } else {
            manifest["files"]
                .as_array()
                .context("Missing files")?
                .iter()
                .find(|v| v["path"] == file)
                .context("Missing file binding")?
        };
        let mut reader = BufReader::new(File::open(&path)?);
        let mut buf = Vec::new();
        let mut hash = Sha256::new();
        let mut rows = 0;
        let mut bytes = 0;
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            hash.update(&buf);
            bytes += buf.len();
            let row: Row = serde_json::from_slice(&buf)
                .with_context(|| format!("Metadata parse: {dataset}/{file} row {}", rows + 1))?;
            ensure!(
                row.schema == "slop-ninja-origin-record-v1" && row.split == split,
                "Unexpected schema/split in allowlisted input"
            );
            entries.push(Entry {
                dataset: dataset.into(),
                file: file.into(),
                row,
            });
            rows += 1;
        }
        let digest = hex::encode(hash.finalize());
        let expected_rows = expected["rows"]
            .as_u64()
            .or_else(|| expected["records"].as_u64())
            .context("Missing count")?;
        ensure!(
            expected["sha256"] == digest && expected_rows == rows,
            "Frozen input binding mismatch: {dataset}/{file}"
        );
        inputs.push(json!({"dataset":dataset,"file":file,"split":split,"rows":rows,"bytes":bytes,"sha256":digest,"manifest_verified":true}));
    }
    let mut sets = BTreeMap::<String, SetSummary>::new();
    let mut split_table = BTreeMap::new();
    let mut source_table = BTreeMap::new();
    let mut generator_table = BTreeMap::new();
    let mut full_table = BTreeMap::new();
    let mut evidence_table = BTreeMap::new();
    let mut rights_table = BTreeMap::new();
    let mut identities = BTreeMap::<String, Value>::new();
    let mut index = BTreeMap::new();
    let mut family_splits = BTreeMap::<String, BTreeSet<String>>::new();
    let mut hash_splits = BTreeMap::<String, BTreeSet<String>>::new();
    for e in &entries {
        let r = &e.row;
        if let Some(previous) = index.insert((e.dataset.clone(), r.id.clone()), r) {
            ensure!(
                previous.text_sha256 == r.text_sha256
                    && previous.split == r.split
                    && previous.source_group == r.source_group,
                "Conflicting duplicate ID"
            );
        }
        family_splits
            .entry(r.source_group.clone())
            .or_default()
            .insert(r.split.clone());
        hash_splits
            .entry(r.text_sha256.clone())
            .or_default()
            .insert(r.split.clone());
    }
    let mut family_rows = BTreeMap::<(String, String, String, String), Vec<Value>>::new();
    let mut missing_parents = Vec::new();
    let mut ancestry_table = BTreeMap::new();
    let mut metadata = BufWriter::new(File::create(out.join("rows.metadata.jsonl"))?);
    let mut rights_false = 0usize;
    for e in &entries {
        let r = &e.row;
        let g = generator(r).to_owned();
        let p = profile(r).to_owned();
        let operation = r
            .generation
            .as_ref()
            .map(|g| g.operation.as_str())
            .unwrap_or("human_root")
            .to_owned();
        let set = sets.entry(e.dataset.clone()).or_default();
        set.ids.insert(r.id.clone());
        set.hashes.insert(r.text_sha256.clone());
        set.groups.insert(r.source_group.clone());
        let ds = e.dataset.clone();
        let sp = r.split.clone();
        let col = r.source.collection.clone();
        add(
            &mut split_table,
            vec![ds.clone(), sp.clone()],
            &r.source_group,
        );
        add(
            &mut source_table,
            vec![ds.clone(), sp.clone(), col.clone(), r.origin.clone()],
            &r.source_group,
        );
        add(
            &mut generator_table,
            vec![
                ds.clone(),
                sp.clone(),
                g.clone(),
                r.origin.clone(),
                operation.clone(),
            ],
            &r.source_group,
        );
        add(
            &mut full_table,
            vec![
                ds.clone(),
                sp.clone(),
                col.clone(),
                g.clone(),
                r.origin.clone(),
                operation.clone(),
                p.clone(),
            ],
            &r.source_group,
        );
        add(
            &mut evidence_table,
            vec![
                ds.clone(),
                sp.clone(),
                col.clone(),
                r.evidence.clone(),
                r.source.author_ids.len().to_string(),
            ],
            &r.source_group,
        );
        let admitted = [
            "commercial_training",
            "model_release",
            "external_evaluation",
            "redistribute_text",
        ]
        .iter()
        .all(|k| r.rights[*k] == true);
        if !admitted {
            rights_false += 1;
        }
        add(
            &mut rights_table,
            vec![
                ds.clone(),
                sp.clone(),
                col.clone(),
                r.rights["license"].as_str().unwrap_or("missing").to_owned(),
                admitted.to_string(),
                r.rights
                    .get("share_alike")
                    .is_some_and(|v| !v.is_null())
                    .to_string(),
            ],
            &r.source_group,
        );
        let identity = if let Some(gen_info) = &r.generation {
            let label = sha(serde_json::to_vec(&(
                "slop-ninja-recorded-generator-identity-v1",
                &gen_info.model_id,
                &gen_info.model_revision,
                &gen_info.response_model,
            ))?);
            let covariate = sha(serde_json::to_vec(&(
                &label,
                &gen_info.runtime,
                &gen_info.quantization,
            ))?);
            identities.entry(covariate).or_insert_with(|| json!({"generator_label":label,"requested_model":gen_info.model_id,"reported_model":gen_info.response_model,"revision":gen_info.model_revision,"license":gen_info.model_license,"license_url":gen_info.model_license_url,"license_sha256":gen_info.model_license_sha256,"runtime":gen_info.runtime,"quantization":gen_info.quantization}));
            Some(label)
        } else {
            None
        };
        let mut ancestor = r;
        let mut contributors = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut complete = true;
        loop {
            ensure!(seen.insert(ancestor.id.clone()), "Cyclic ancestry");
            if let Some(gen_info) = &ancestor.generation {
                contributors.insert(gen_info.model_revision.clone());
            }
            let Some(parent) = &ancestor.parent_id else {
                break;
            };
            match index.get(&(ds.clone(), parent.clone())) {
                Some(found) => {
                    ensure!(
                        found.source_group == r.source_group && found.split == r.split,
                        "Ancestry crosses family/split"
                    );
                    ancestor = found;
                }
                None => {
                    missing_parents.push(json!({"dataset":ds,"id":r.id,"parent_id":parent}));
                    complete = false;
                    break;
                }
            }
        }
        let original_composer = {
            let mut cursor = r;
            let mut found = "none_recorded";
            loop {
                if let Some(gen_info) = &cursor.generation {
                    if gen_info.operation == "draft" {
                        found = &gen_info.model_revision;
                    }
                }
                let Some(parent) = &cursor.parent_id else {
                    break;
                };
                let Some(next) = index.get(&(ds.clone(), parent.clone())) else {
                    break;
                };
                cursor = next;
            }
            found.to_owned()
        };
        add(
            &mut ancestry_table,
            vec![
                ds.clone(),
                sp.clone(),
                col.clone(),
                original_composer.clone(),
                g.clone(),
                contributors.iter().cloned().collect::<Vec<_>>().join(" | "),
            ],
            &r.source_group,
        );
        let row = json!({"dataset":ds,"file":e.file,"id":r.id,"source_group":r.source_group,"text_sha256":r.text_sha256,"split":sp,"collection":col,"origin":r.origin,"evidence":r.evidence,"parent_id":r.parent_id,"final_generator_revision":g,"generator_label":identity,"operation":operation,"prompt_profile":p,"original_draft_generator":original_composer,"contributor_revisions":contributors,"ancestry_complete":complete,"source_author_id_count":r.source.author_ids.len(),"rights_flags_all_true":admitted,"license":r.rights["license"],"request_sha256":r.generation.as_ref().map(|x|&x.request_sha256),"response_sha256":r.generation.as_ref().map(|x|&x.response_sha256)});
        serde_json::to_writer(&mut metadata, &row)?;
        writeln!(metadata)?;
        family_rows
            .entry((ds, sp, col, r.source_group.clone()))
            .or_default()
            .push(row);
    }
    metadata.flush()?;
    let mut overlaps = Vec::new();
    let names: Vec<_> = sets.keys().cloned().collect();
    for (i, a) in names.iter().enumerate() {
        for b in names.iter().skip(i + 1) {
            let x = &sets[a];
            let y = &sets[b];
            overlaps.push(json!({"left":a,"right":b,"shared_record_ids":x.ids.intersection(&y.ids).count(),"shared_text_sha256":x.hashes.intersection(&y.hashes).count(),"shared_source_families":x.groups.intersection(&y.groups).count(),"right_ids_absent_from_left":y.ids.difference(&x.ids).count(),"left_ids_absent_from_right":x.ids.difference(&y.ids).count()}));
        }
    }
    let mut families = BufWriter::new(File::create(out.join("families.metadata.jsonl"))?);
    for ((dataset, split, collection, group), rows) in family_rows {
        serde_json::to_writer(
            &mut families,
            &json!({"dataset":dataset,"split":split,"collection":collection,"source_group":group,"rows":rows.len(),"records":rows}),
        )?;
        writeln!(families)?;
    }
    families.flush()?;
    let summary = json!({
        "schema":"slop_ninja_a_source_inventory_v2","scope":"Fixed public Train/Development/Calibration allowlist only; no Test shard opened; no private input; text and prompt omitted from all outputs",
        "inputs":inputs,"by_dataset_split":table(split_table,&["dataset","split"]),
        "by_source_origin":table(source_table,&["dataset","split","collection","origin"]),
        "by_generator_operation":table(generator_table,&["dataset","split","revision","origin","operation"]),
        "by_source_generator_profile":table(full_table,&["dataset","split","collection","revision","origin","operation","profile"]),
        "by_evidence_author_count":table(evidence_table,&["dataset","split","collection","evidence","source_author_id_count"]),
        "by_rights":table(rights_table,&["dataset","split","collection","license","all_four_rights_flags","share_alike_present"]),
        "by_ancestry":table(ancestry_table,&["dataset","split","collection","original_composer","final_generator","all_contributor_revisions"]),
        "recorded_generator_identities_and_runtime_variants":identities.into_values().collect::<Vec<_>>(),
        "overlap":overlaps,"rights_flag_failures":rights_false,"missing_parents":missing_parents,
        "family_split_conflicts":family_splits.into_iter().filter(|(_,s)|s.len()>1).collect::<BTreeMap<_,_>>(),
        "exact_hash_split_conflicts":hash_splits.into_iter().filter(|(_,s)|s.len()>1).collect::<BTreeMap<_,_>>(),
        "limits":["No text-hash recomputation; file bindings verified against frozen manifests","No normalized/semantic duplicate comparison","Final Test overlap not independently audited","Whole-row workflow provenance does not provide final-word rewrite-span supervision","Source author IDs and writing personas do not establish a documented individual human attribution label","Rights flags describe recorded eligibility only"]
    });
    write_json(&out.join("inventory.json"), &summary)?;
    println!(
        "Inventory ready: {} ({} allowlisted files; no Test/private input)",
        out.join("inventory.json").display(),
        INPUTS.len()
    );
    Ok(())
}
