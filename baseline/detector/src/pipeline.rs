//! Corpus-to-feature handoff and explicit, separated fit/evaluation commands.

use anyhow::{Context, Result, ensure};
use grammar_core::features::{FEATURE_VERSION, Family, Features, extract, words};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Evidence, Origin, OriginRecord, Split, sha256},
    features::{FeatureConfig, FeatureMode, FeatureRow, FeatureSpace},
    metrics, model,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

pub const MIN_WORDS: usize = 40;
pub const MAX_WORDS: usize = 2048;
pub const MAX_BYTES: usize = 100_000;
pub const FEATURE_RECORD_SCHEMA: &str = "slop_ninja_origin_feature_record_v1";
pub const BRIDGE: &str =
    include_str!("../../../grammar/crates/grammar-spacy/python/syntax_bridge.py");

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeatureHeader {
    schema: String,
    id: String,
    group: String,
    origin: Origin,
    evidence: Evidence,
    split: Split,
    text_sha256: String,
    normalized_text_sha256: String,
    word_count: usize,
    source_manifest_sha256: String,
    features_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    share_alike_rights: Option<dataset::Rights>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeatureRecord {
    #[serde(flatten)]
    header: FeatureHeader,
    features: Features,
}

fn split_name(split: Split) -> &'static str {
    match split {
        Split::Train => "train",
        Split::Development => "development",
        Split::Calibration => "calibration",
        Split::Test => "test",
    }
}

fn normal_records(input: &Path) -> Result<Vec<OriginRecord>> {
    let records = dataset::read_records(input)?;
    ensure!(
        records
            .iter()
            .all(|record| record.evidence != Evidence::SyntheticFixture),
        "synthetic protocol fixtures cannot enter normal corpus training/export"
    );
    ensure!(
        records.iter().all(|record| record.split.is_some()),
        "freeze source-family splits before export or feature extraction"
    );
    Ok(records)
}

pub fn export_shards(input: &Path, output: &Path) -> Result<Value> {
    let records = normal_records(input)?;
    fs::create_dir_all(output)?;
    let mut manifest = BTreeMap::new();
    for split in [
        Split::Train,
        Split::Development,
        Split::Calibration,
        Split::Test,
    ] {
        let selected: Vec<_> = records
            .iter()
            .filter(|record| record.split == Some(split))
            .cloned()
            .collect();
        if selected.is_empty() {
            continue;
        }
        let path = output.join(format!("{}.jsonl", split_name(split)));
        dataset::write_records(&path, &selected)?;
        manifest.insert(
            split_name(split),
            json!({"rows":selected.len(), "sha256":sha256(fs::read(&path)?)}),
        );
    }
    let rights =
        slop_ninja_detector::rights::release_manifest(&records, &serde_json::to_value(&manifest)?);
    write_json(&output.join("data_rights.json"), &rights)?;
    let summary = json!({"schema":"slop_ninja_origin_shards_v1", "source_manifest_sha256":sha256(fs::read(input)?), "partitions":manifest,"data_rights_sha256":sha256(fs::read(output.join("data_rights.json"))?)});
    write_json(&output.join("manifest.json"), &summary)?;
    Ok(summary)
}

fn lexical_schema() -> String {
    format!("slop_ninja_words_only_v1;{FEATURE_VERSION}")
}

fn validate_input(text: &str) -> Result<usize> {
    ensure!(
        text.len() <= MAX_BYTES,
        "input exceeds {MAX_BYTES} UTF-8 bytes"
    );
    let count = words(text).len();
    ensure!(
        (MIN_WORDS..=MAX_WORDS).contains(&count),
        "input contains {count} words; supported range is {MIN_WORDS}..={MAX_WORDS}; text is never truncated"
    );
    Ok(count)
}

fn lexical_features(text: &str) -> Features {
    let tokens = words(text);
    let mut counts = BTreeMap::new();
    for word in &tokens {
        *counts.entry(word.clone()).or_insert(0_u64) += 1;
    }
    Features {
        schema: lexical_schema(),
        parser_identity: "none;grammar-core-words-v1".into(),
        source_sha256: sha256(text),
        families: BTreeMap::from([(
            "word".into(),
            Family::Distribution {
                counts,
                opportunities: tokens.len() as u64,
            },
        )]),
    }
}

fn parsed_features(texts: &[&str], python: &str) -> Result<Vec<Features>> {
    let mut bridge = tempfile::Builder::new()
        .prefix("slop-ninja-syntax-")
        .suffix(".py")
        .tempfile()?;
    bridge.write_all(BRIDGE.as_bytes())?;
    bridge.flush()?;
    let annotations = grammar_spacy::parse_batch_with_bridge(texts, python, bridge.path())?;
    annotations
        .into_iter()
        .enumerate()
        .map(|(index, annotation)| {
            let doc = annotation
                .map_err(anyhow::Error::msg)
                .with_context(|| format!("annotation failed at batch index {index}"))?;
            let mut features = extract(&doc)?;
            features
                .parser_identity
                .push_str(&format!(";detector-bridge-sha256={}", sha256(BRIDGE)));
            Ok(features)
        })
        .collect()
}

pub fn featurize(
    input: &Path,
    output: &Path,
    mode: FeatureMode,
    python: Option<&str>,
    batch_size: usize,
) -> Result<Value> {
    ensure!(
        (1..=512).contains(&batch_size),
        "batch size must be 1..=512"
    );
    let records = normal_records(input)?;
    let word_counts: Vec<usize> = records
        .iter()
        .map(|record| {
            validate_input(&record.text)
                .with_context(|| format!("{} is outside the input contract", record.id))
        })
        .collect::<Result<_>>()?;
    let source_manifest_sha256 = sha256(fs::read(input)?);
    let mut result = Vec::with_capacity(records.len());
    for (batch_index, batch) in records.chunks(batch_size).enumerate() {
        let features = if mode == FeatureMode::Word {
            batch
                .iter()
                .map(|record| lexical_features(&record.text))
                .collect()
        } else {
            parsed_features(
                &batch
                    .iter()
                    .map(|record| record.text.as_str())
                    .collect::<Vec<_>>(),
                python.context("--python is required for grammar/combined feature extraction")?,
            )?
        };
        for (offset, (record, features)) in batch.iter().zip(features).enumerate() {
            ensure!(
                features.source_sha256 == record.text_sha256,
                "parser/extractor changed source identity"
            );
            result.push(FeatureRecord {
                header: FeatureHeader {
                    schema: FEATURE_RECORD_SCHEMA.into(),
                    id: record.id.clone(),
                    group: record.source_group.clone(),
                    origin: record.origin,
                    evidence: record.evidence,
                    split: record.split.context("missing frozen split")?,
                    text_sha256: record.text_sha256.clone(),
                    normalized_text_sha256: sha256(words(&record.text).join(" ")),
                    word_count: word_counts[batch_index * batch_size + offset],
                    source_manifest_sha256: source_manifest_sha256.clone(),
                    features_sha256: sha256(serde_json::to_vec(&features)?),
                    share_alike_rights: record
                        .rights
                        .share_alike
                        .as_ref()
                        .map(|_| record.rights.clone()),
                },
                features,
            });
        }
        eprintln!("Extracted {} / {} records", result.len(), records.len());
    }
    write_jsonl(output, &result)?;
    Ok(
        json!({"schema":FEATURE_RECORD_SCHEMA, "rows":result.len(), "mode":mode, "feature_file_sha256":sha256(fs::read(output)?), "source_manifest_sha256":source_manifest_sha256, "contains_source_text":false, "input_limits":{"min_words":MIN_WORDS,"max_words":MAX_WORDS,"max_utf8_bytes":MAX_BYTES}}),
    )
}

fn validate_header(header: &FeatureHeader) -> Result<()> {
    ensure!(
        header.schema == FEATURE_RECORD_SCHEMA,
        "unsupported feature record schema"
    );
    ensure!(
        !header.id.is_empty() && !header.group.is_empty(),
        "feature record lacks ID/group"
    );
    ensure!(
        !matches!(
            header.evidence,
            Evidence::SyntheticFixture | Evidence::Unknown
        ),
        "{}: synthetic/unknown evidence cannot enter normal training",
        header.id
    );
    let compatible = match header.evidence {
        Evidence::HistoricalProxy | Evidence::DocumentedHuman => header.origin == Origin::HumanOnly,
        Evidence::RecordedModelGeneration | Evidence::RecordedModelRevision => {
            header.origin == Origin::ModelOnly
        }
        Evidence::ModelEditOfHistoricalProxy => header.origin == Origin::Mixed,
        Evidence::RecordedMixedWorkflow | Evidence::SyntheticFixture | Evidence::Unknown => false,
    };
    ensure!(
        compatible,
        "{}: unsupported feature origin/evidence combination",
        header.id
    );
    ensure!(
        (MIN_WORDS..=MAX_WORDS).contains(&header.word_count),
        "{}: feature record outside supported word count",
        header.id
    );
    for hash in [
        &header.text_sha256,
        &header.normalized_text_sha256,
        &header.source_manifest_sha256,
        &header.features_sha256,
    ] {
        ensure!(
            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid feature provenance hash"
        );
    }
    Ok(())
}

fn load_features(input: &Path, selected: &[Split]) -> Result<BTreeMap<Split, Vec<FeatureRow>>> {
    let mut result: BTreeMap<Split, Vec<FeatureRow>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut texts = BTreeMap::new();
    for (line_number, line) in BufReader::new(File::open(input)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // Inspect identity/split headers for leakage without materializing the
        // held-out test feature payload in the fitting path.
        let header: FeatureHeader = serde_json::from_str(&line)
            .with_context(|| format!("feature header at line {}", line_number + 1))?;
        validate_header(&header)?;
        if let Some(rights) = &header.share_alike_rights {
            slop_ninja_detector::rights::validate(rights)?;
            ensure!(
                rights.share_alike.is_some(),
                "Missing feature attribution obligations"
            );
        }
        ensure!(
            ids.insert(header.id.clone()),
            "duplicate feature record ID: {}",
            header.id
        );
        if let Some(previous) = groups.insert(header.group.clone(), header.split) {
            ensure!(
                previous == header.split,
                "feature source group crosses splits: {}",
                header.group
            );
        }
        for hash in [&header.text_sha256, &header.normalized_text_sha256] {
            if let Some(previous) = texts.insert(hash.clone(), header.split) {
                ensure!(
                    previous == header.split,
                    "duplicate feature source crosses splits: {}",
                    header.id
                );
            }
        }
        if !selected.contains(&header.split) {
            continue;
        }
        let record: FeatureRecord = serde_json::from_str(&line)?;
        ensure!(
            record.features.source_sha256 == header.text_sha256,
            "feature/source text hash mismatch for {}",
            header.id
        );
        ensure!(
            sha256(serde_json::to_vec(&record.features)?) == header.features_sha256,
            "feature payload hash mismatch for {}",
            header.id
        );
        result.entry(header.split).or_default().push(FeatureRow {
            id: header.id,
            group: header.group,
            label: header.origin.index(),
            features: record.features,
        });
    }
    for split in selected {
        ensure!(
            result.contains_key(split),
            "no {} feature rows",
            split_name(*split)
        );
    }
    Ok(result)
}

fn feature_rights(input: &Path) -> Result<Value> {
    let mut notices = Vec::new();
    for line in BufReader::new(File::open(input)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let header: FeatureHeader = serde_json::from_str(&line)?;
        if header.split != Split::Test
            && let Some(rights) = &header.share_alike_rights
        {
            notices.extend(slop_ninja_detector::rights::notice(
                &header.id,
                &header.text_sha256,
                rights,
            ));
        }
    }
    Ok(slop_ninja_detector::rights::manifest_for_notices(
        notices,
        &json!({"feature_file_sha256":sha256(fs::read(input)?)}),
    ))
}

/// Fit coordinates on Train only. Export full archives; scoring views stay separate.
pub fn export_vectors(input: &Path, output: &Path, config: &FeatureConfig) -> Result<Value> {
    ensure!(
        !output.exists(),
        "vector output already exists; use a new directory"
    );
    let source_hash = sha256(fs::read(input)?);
    let splits = [Split::Train, Split::Development, Split::Calibration];
    let partitions = load_features(input, &splits)?;
    let space = FeatureSpace::fit(&partitions[&Split::Train], config)?;
    space.validate()?;
    let space_hash = sha256(serde_json::to_vec(&space)?);
    // Validate and transform all rows before creating a completed export.
    let mut transformed = BTreeMap::new();
    for split in splits {
        let rows = partitions[&split]
            .iter()
            .map(|row| {
                Ok(json!({
                    "id":row.id,"source_group":row.group,"split":split,
                    "origin":model::CLASS_NAMES[row.label],"text_sha256":row.features.source_sha256,
                    "features_sha256":sha256(serde_json::to_vec(&row.features)?),
                    "feature_space_sha256":space_hash,"vector":space.transform(&row.features)?,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        transformed.insert(split, rows);
    }
    let rights = feature_rights(input)?;
    ensure!(
        sha256(fs::read(input)?) == source_hash,
        "feature cache changed during export"
    );
    fs::create_dir_all(output)?;
    write_json(
        &output.join("feature_space.json"),
        &json!({"content_sha256":space_hash,"content":space}),
    )?;
    write_json(&output.join("data_rights.json"), &rights)?;
    let mut summaries = BTreeMap::new();
    for split in splits {
        let name = split_name(split);
        let path = output.join(format!("{name}.jsonl"));
        let rows = &transformed[&split];
        write_jsonl(&path, rows)?;
        let mut unavailable_counts = BTreeMap::<String, usize>::new();
        for row in rows {
            for family in row["vector"]["unavailable_families"]
                .as_array()
                .context("Missing availability")?
            {
                *unavailable_counts
                    .entry(family.as_str().context("Invalid family")?.to_owned())
                    .or_default() += 1;
            }
        }
        let raw = &partitions[&split];
        summaries.insert(name, json!({
            "file":format!("{name}.jsonl"),"sha256":sha256(fs::read(&path)?),
            "records":rows.len(),"source_groups":raw.iter().map(|r| &r.group).collect::<BTreeSet<_>>().len(),
            "class_counts":model::CLASS_NAMES.iter().enumerate().map(|(i,label)|(*label,raw.iter().filter(|r|r.label==i).count())).collect::<BTreeMap<_,_>>(),
            "rows_with_unknown_coordinates":rows.iter().filter(|r|r["vector"]["unknown_coordinates"].as_u64().unwrap_or(0)>0).count(),
            "unavailable_families":unavailable_counts,
        }));
    }
    let result = json!({
        "schema":"slop_ninja_neural_feature_export_v1","feature_file_sha256":source_hash,
        "feature_space_sha256":space_hash,"feature_space_file_sha256":sha256(fs::read(output.join("feature_space.json"))?),
        "dimensions":space.coordinates.len(),"parser_identity":space.parser_identity,
        "feature_schema":space.feature_schema,"labels":model::CLASS_NAMES,"config":config,
        "fitting_split":"train","partitions":summaries,
        "data_rights_sha256":sha256(fs::read(output.join("data_rights.json"))?),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "source_payload_sha256":source_payload_hashes(),
        "classifier_fitted":false,"calibration_fitted":false,"test_vectors_exported":false,
        "vector_precision":"Rust f64; ML conversion must be declared by the consumer",
        "model_input":"vector.values only; IDs, labels, groups and hashes are metadata",
        "unavailable_policy":"Family availability is encoded separately from its values; unknown coordinates and unavailable families are retained as diagnostics.",
    });
    write_json(&output.join("manifest.json"), &result)?;
    Ok(result)
}

pub fn train(input: &Path, output: &Path, config: &model::TrainConfig) -> Result<Value> {
    let partitions = load_features(
        input,
        &[Split::Train, Split::Development, Split::Calibration],
    )?;
    let (artifact, report) = model::train(
        &partitions[&Split::Train],
        &partitions[&Split::Development],
        &partitions[&Split::Calibration],
        config,
    )?;
    fs::create_dir_all(output)?;
    let data_rights = feature_rights(input)?;
    write_json(&output.join("data_rights.json"), &data_rights)?;
    write_json(&output.join("model.json"), &artifact)?;
    write_json(&output.join("training_report.json"), &report)?;
    let contract = json!({
        "schema":"slop_ninja_detector_runtime_contract_v1", "artifact_sha256":artifact.content_sha256,
        "data_rights_sha256":sha256(fs::read(output.join("data_rights.json"))?),
        "classes":model::CLASS_NAMES, "model_file_sha256":sha256(fs::read(output.join("model.json"))?),
        "feature_file_sha256":sha256(fs::read(input)?), "input_language_scope":"English; no language detector is fitted",
        "min_words":MIN_WORDS,"max_words":MAX_WORDS,"max_utf8_bytes":MAX_BYTES,"truncation":false,
        "word_tokenizer_feature_version":FEATURE_VERSION,"embedded_bridge_sha256":sha256(BRIDGE),
        "source_payload_sha256":source_payload_hashes(),
        "reference_executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "reference_platform":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"crate_version":env!("CARGO_PKG_VERSION")},
        "arithmetic":"Rust f64 CPU; exact strict-greater-than threshold rule; cross-platform numerical qualification remains pending",
        "parser_identity":artifact.content.feature_space.parser_identity,
        "parser_assets":"The parser identity binds every installed model-package file except generated Python bytecode, including weights, config, tokenizer and loader source; pre/post-load hashes must agree.",
        "probability_event":"document production history, not percentage of AI-written words",
        "evidence_policy":"Historical human proxies and model edits of those proxies are weak labels; this pilot is not a documented contemporary collaboration dataset.",
        "test_feature_payloads_used_in_training":false,
        "qualification":"experimental reference control; no Pangram parity or reward qualification claimed"
    });
    let bound_contract =
        json!({"content_sha256":sha256(serde_json::to_vec(&contract)?),"content":contract});
    write_json(&output.join("runtime.json"), &bound_contract)?;
    Ok(serde_json::to_value(report)?)
}

pub fn evaluate(
    input: &Path,
    artifact_path: &Path,
    output: &Path,
    split: Split,
    open_final_test: bool,
) -> Result<Value> {
    ensure!(
        split != Split::Train,
        "use development/calibration diagnostics or explicitly open the final test"
    );
    ensure!(
        split != Split::Test || open_final_test,
        "final test remains sealed; pass --open-final-test for a frozen candidate"
    );
    ensure!(
        !output.exists(),
        "evaluation report already exists; use a new report path"
    );
    let artifact = load_bundle(artifact_path)?;
    let partitions = load_features(input, &[split])?;
    let report = metrics::evaluate(&artifact, &partitions[&split])?;
    let result = json!({"schema":"slop_ninja_detector_evaluation_v1", "split":split,"final_test_opened":split == Split::Test,"feature_file_sha256":sha256(fs::read(input)?),"report":report});
    write_json(output, &result)?;
    Ok(result)
}

pub fn infer(artifact_path: &Path, text_path: &Path, python: Option<&str>) -> Result<Value> {
    let artifact = load_bundle(artifact_path)?;
    let text = fs::read_to_string(text_path)?;
    if let Err(error) = validate_input(&text) {
        return Ok(
            json!({"status":"unsupported_input","artifact_sha256":artifact.content_sha256,"reason":error.to_string()}),
        );
    }
    let features = if artifact.content.feature_space.feature_schema == lexical_schema() {
        lexical_features(&text)
    } else {
        parsed_features(
            &[&text],
            python.context("--python is required by this artifact's parser")?,
        )?
        .remove(0)
    };
    let prediction = artifact.predict_validated(&features)?;
    Ok(
        json!({"status":"ok", "prediction":prediction, "qualification":"experimental reference; accuracy outside its evaluation population is unknown"}),
    )
}

/// Materialize an inference vector without fitting coordinates or inventing labels.
pub fn project_text(space_path: &Path, text_path: &Path, python: Option<&str>) -> Result<Value> {
    let space_bytes = fs::read(space_path)?;
    let binding: Value = serde_json::from_slice(&space_bytes)?;
    let space: FeatureSpace = serde_json::from_value(binding["content"].clone())?;
    space.validate()?;
    let space_hash = sha256(serde_json::to_vec(&space)?);
    ensure!(
        binding["content_sha256"] == space_hash,
        "frozen feature-space content hash mismatch"
    );
    let text = fs::read_to_string(text_path)?;
    let text_hash = sha256(&text);
    if let Err(error) = validate_input(&text) {
        return Ok(json!({"status":"unsupported_input","text_sha256":text_hash,
            "feature_space_sha256":space_hash,"reason":error.to_string()}));
    }
    let features = if space.feature_schema == lexical_schema() {
        lexical_features(&text)
    } else {
        parsed_features(
            &[&text],
            python.context("--python is required by this frozen space's parser")?,
        )?
        .remove(0)
    };
    let vector = space.transform(&features)?;
    Ok(
        json!({"schema":"slop_ninja_projected_feature_vector_v1","status":"ok",
        "text_sha256":text_hash,"features_sha256":sha256(serde_json::to_vec(&features)?),
        "feature_space_sha256":space_hash,"feature_space_file_sha256":sha256(space_bytes),
        "feature_schema":space.feature_schema,"parser_identity":space.parser_identity,
        "dimensions":space.coordinates.len(),"vector":vector,
        "fitting_performed":false,"input_limits":{"min_words":MIN_WORDS,"max_words":MAX_WORDS,"max_utf8_bytes":MAX_BYTES},
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "source_payload_sha256":source_payload_hashes()}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionRequest {
    id: String,
    text: String,
}

/// Parse each request once, then project it into compatible, already fitted spaces.
pub fn project_requests(
    space_paths: &[std::path::PathBuf],
    requests_path: &Path,
    output: &Path,
    python: Option<&str>,
    batch_size: usize,
) -> Result<Value> {
    ensure!(!output.exists(), "projection output already exists");
    ensure!(
        !space_paths.is_empty() && batch_size > 0,
        "spaces and positive batch size required"
    );
    let mut spaces = BTreeMap::new();
    for path in space_paths {
        let bytes = fs::read(path)?;
        let binding: Value = serde_json::from_slice(&bytes)?;
        let space: FeatureSpace = serde_json::from_value(binding["content"].clone())?;
        space.validate()?;
        let hash = sha256(serde_json::to_vec(&space)?);
        ensure!(
            binding["content_sha256"] == hash,
            "frozen feature-space content hash mismatch"
        );
        ensure!(
            spaces.insert(hash, (space, sha256(bytes))).is_none(),
            "duplicate feature space"
        );
    }
    let first = &spaces.values().next().unwrap().0;
    ensure!(
        spaces
            .values()
            .all(|(s, _)| s.feature_schema == first.feature_schema
                && s.parser_identity == first.parser_identity),
        "batch spaces require the same parser and feature schema"
    );
    let bytes = fs::read(requests_path)?;
    let requests: Vec<ProjectionRequest> = BufReader::new(bytes.as_slice())
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect::<Result<_>>()?;
    ensure!(!requests.is_empty(), "empty projection request list");
    let mut ids = BTreeSet::new();
    ensure!(
        requests
            .iter()
            .all(|r| !r.id.is_empty() && ids.insert(&r.id)),
        "missing or duplicate projection ID"
    );
    let mut projected: BTreeMap<String, Vec<Value>> =
        spaces.keys().map(|k| (k.clone(), vec![])).collect();
    let mut unsupported = 0;
    for chunk in requests.chunks(batch_size) {
        let valid: Vec<&str> = chunk
            .iter()
            .filter(|r| validate_input(&r.text).is_ok())
            .map(|r| r.text.as_str())
            .collect();
        let features = if valid.is_empty() {
            vec![]
        } else if first.feature_schema == lexical_schema() {
            valid.iter().map(|text| lexical_features(text)).collect()
        } else {
            parsed_features(
                &valid,
                python.context("--python is required by these frozen spaces")?,
            )?
        };
        ensure!(
            features.len() == valid.len(),
            "parser returned incomplete batch"
        );
        let mut features = features.into_iter();
        for request in chunk {
            if let Err(error) = validate_input(&request.text) {
                unsupported += 1;
                for (hash, rows) in &mut projected {
                    rows.push(json!({"id":request.id,"status":"unsupported_input",
                        "text_sha256":sha256(&request.text),"feature_space_sha256":hash,"reason":error.to_string()}));
                }
                continue;
            }
            let features = features.next().context("missing parsed request")?;
            ensure!(
                features.source_sha256 == sha256(&request.text),
                "parser changed request text"
            );
            for (hash, (space, file_hash)) in &spaces {
                projected.get_mut(hash).unwrap().push(json!({
                    "schema":"slop_ninja_projected_feature_vector_v1","status":"ok","id":request.id,
                    "text_sha256":features.source_sha256,"features_sha256":sha256(serde_json::to_vec(&features)?),
                    "feature_space_sha256":hash,"feature_space_file_sha256":file_hash,
                    "feature_schema":space.feature_schema,"parser_identity":space.parser_identity,
                    "dimensions":space.coordinates.len(),"vector":space.transform(&features)?,"fitting_performed":false}));
            }
        }
    }
    fs::create_dir_all(output)?;
    let mut outputs = BTreeMap::new();
    for (hash, rows) in &projected {
        let filename = format!("{hash}.jsonl");
        write_jsonl(&output.join(&filename), rows)?;
        outputs.insert(hash, json!({"file":filename,"sha256":sha256(fs::read(output.join(&filename))?),"records":rows.len()}));
    }
    let summary = json!({"schema":"slop_ninja_batch_feature_projection_v1","requests_sha256":sha256(bytes),
        "records":requests.len(),"unsupported_records":unsupported,"spaces":outputs,"batch_size":batch_size,
        "fitting_performed":false,"source_payload_sha256":source_payload_hashes(),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?)});
    write_json(&output.join("manifest.json"), &summary)?;
    Ok(summary)
}

fn source_payload_hashes() -> BTreeMap<&'static str, String> {
    [
        ("detector/main.rs", include_bytes!("main.rs").as_slice()),
        (
            "detector/pipeline.rs",
            include_bytes!("pipeline.rs").as_slice(),
        ),
        (
            "detector/features.rs",
            include_bytes!("features.rs").as_slice(),
        ),
        ("detector/model.rs", include_bytes!("model.rs").as_slice()),
        (
            "detector/metrics.rs",
            include_bytes!("metrics.rs").as_slice(),
        ),
        (
            "detector/Cargo.toml",
            include_bytes!("../Cargo.toml").as_slice(),
        ),
        (
            "detector/Cargo.lock",
            include_bytes!("../Cargo.lock").as_slice(),
        ),
        (
            "grammar/Cargo.lock",
            include_bytes!("../../../grammar/Cargo.lock").as_slice(),
        ),
        (
            "grammar-core/features.rs",
            include_bytes!("../../../grammar/crates/grammar-core/src/features.rs").as_slice(),
        ),
        (
            "grammar-core/syntax.rs",
            include_bytes!("../../../grammar/crates/grammar-core/src/syntax.rs").as_slice(),
        ),
        (
            "grammar-spacy/lib.rs",
            include_bytes!("../../../grammar/crates/grammar-spacy/src/lib.rs").as_slice(),
        ),
        ("grammar-spacy/syntax_bridge.py", BRIDGE.as_bytes()),
    ]
    .into_iter()
    .map(|(name, bytes)| (name, sha256(bytes)))
    .collect()
}

fn load_bundle(artifact_path: &Path) -> Result<model::DetectorArtifact> {
    let artifact_bytes = fs::read(artifact_path)?;
    let artifact: model::DetectorArtifact = serde_json::from_slice(&artifact_bytes)?;
    artifact.validate()?;
    let runtime_path = artifact_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("runtime.json");
    let runtime: Value = serde_json::from_slice(
        &fs::read(&runtime_path)
            .with_context(|| format!("missing runtime contract {}", runtime_path.display()))?,
    )?;
    let content = &runtime["content"];
    ensure!(
        runtime["content_sha256"] == sha256(serde_json::to_vec(content)?),
        "runtime contract content hash mismatch"
    );
    ensure!(
        content["schema"] == "slop_ninja_detector_runtime_contract_v1",
        "unsupported runtime contract schema"
    );
    ensure!(
        content["artifact_sha256"] == artifact.content_sha256
            && content["model_file_sha256"] == sha256(&artifact_bytes),
        "runtime contract binds a different model payload"
    );
    ensure!(
        content["classes"] == json!(model::CLASS_NAMES),
        "runtime class order mismatch"
    );
    ensure!(
        content["parser_identity"] == artifact.content.feature_space.parser_identity,
        "runtime parser identity mismatch"
    );
    ensure!(
        content["min_words"] == MIN_WORDS
            && content["max_words"] == MAX_WORDS
            && content["max_utf8_bytes"] == MAX_BYTES
            && content["truncation"] == false,
        "runtime input contract differs from this executable"
    );
    ensure!(
        content["embedded_bridge_sha256"] == sha256(BRIDGE)
            && content["source_payload_sha256"] == json!(source_payload_hashes()),
        "runtime source/dependency payloads differ from this executable; use the artifact's frozen source revision"
    );
    Ok(artifact)
}

fn output_temp(path: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    Ok(tempfile::NamedTempFile::new_in(parent)?)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = output_temp(path)?;
    {
        let mut writer = BufWriter::with_capacity(64 * 1024, file.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .with_context(|| format!("cannot create {} without overwriting", path.display()))?;
    Ok(())
}

fn write_jsonl(path: &Path, rows: &[impl Serialize]) -> Result<()> {
    let mut file = output_temp(path)?;
    {
        let mut writer = BufWriter::with_capacity(64 * 1024, file.as_file_mut());
        for row in rows {
            serde_json::to_writer(&mut writer, row)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .with_context(|| format!("cannot create {} without overwriting", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_projection_preserves_space_and_rejects_changed_scales() {
        let root = tempfile::tempdir().unwrap();
        let text = "anchor ".repeat(MIN_WORDS);
        let features = lexical_features(&text);
        let space = FeatureSpace::fit(
            &[FeatureRow {
                id: "source".into(),
                group: "source".into(),
                label: 0,
                features,
            }],
            &FeatureConfig {
                mode: FeatureMode::Word,
                min_document_frequency: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let space_hash = sha256(serde_json::to_vec(&space).unwrap());
        let path = root.path().join("space.json");
        let text_path = root.path().join("text.txt");
        let mut binding = json!({"content":space,"content_sha256":space_hash});
        write_json(&path, &binding).unwrap();
        fs::write(&text_path, text).unwrap();
        let original = project_text(&path, &text_path, None).unwrap();
        fs::write(&text_path, "novel ".repeat(MIN_WORDS)).unwrap();
        let novel = project_text(&path, &text_path, None).unwrap();
        assert_eq!(
            original["feature_space_sha256"],
            novel["feature_space_sha256"]
        );
        assert_eq!(original["dimensions"], novel["dimensions"]);
        assert!(novel["vector"]["unknown_coordinates"].as_u64().unwrap() > 0);
        fs::write(&text_path, "too short").unwrap();
        let short = project_text(&path, &text_path, None).unwrap();
        assert_eq!(short["status"], "unsupported_input");
        assert!(short.get("vector").is_none());
        binding["content"]["coordinates"][0]["scale"] = json!(2.0);
        fs::write(&path, serde_json::to_vec(&binding).unwrap()).unwrap();
        assert!(project_text(&path, &text_path, None).is_err());
    }

    #[test]
    fn batch_projection_matches_single_and_preserves_unsupported_requests() {
        let root = tempfile::tempdir().unwrap();
        let text = "anchor ".repeat(MIN_WORDS);
        let space = FeatureSpace::fit(
            &[FeatureRow {
                id: "a".into(),
                group: "a".into(),
                label: 0,
                features: lexical_features(&text),
            }],
            &FeatureConfig {
                mode: FeatureMode::Word,
                min_document_frequency: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let hash = sha256(serde_json::to_vec(&space).unwrap());
        let space_path = root.path().join("space.json");
        write_json(&space_path, &json!({"content":space,"content_sha256":hash})).unwrap();
        let text_path = root.path().join("text.txt");
        fs::write(&text_path, &text).unwrap();
        let single = project_text(&space_path, &text_path, None).unwrap();
        let requests = root.path().join("requests.jsonl");
        write_jsonl(
            &requests,
            &[
                json!({"id":"a","text":text}),
                json!({"id":"short","text":"short"}),
            ],
        )
        .unwrap();
        let output = root.path().join("out");
        let result = project_requests(
            std::slice::from_ref(&space_path),
            &requests,
            &output,
            None,
            1,
        )
        .unwrap();
        assert_eq!(result["records"], 2);
        assert_eq!(result["unsupported_records"], 1);
        let lines: Vec<Value> = fs::read_to_string(output.join(format!("{hash}.jsonl")))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines[0]["vector"], single["vector"]);
        assert_eq!(lines[1]["id"], "short");
        assert!(lines[1].get("vector").is_none());
        assert!(project_requests(&[space_path], &requests, &output, None, 1).is_err());
    }

    // Synthetic metadata fixtures exercise the export contract, not origin claims.
    fn vector_fixture(split: Split, word: &str, available: bool) -> FeatureRecord {
        let text = format!("{word} ").repeat(MIN_WORDS);
        let mut features = lexical_features(&text);
        features.families.insert(
            "synthetic_optional".into(),
            Family::Rates {
                counts: BTreeMap::from([("event".into(), 0)]),
                opportunities: u64::from(available),
            },
        );
        FeatureRecord {
            header: FeatureHeader {
                schema: FEATURE_RECORD_SCHEMA.into(),
                id: format!("synthetic-{}", split_name(split)),
                group: format!("synthetic-group-{}", split_name(split)),
                origin: Origin::HumanOnly,
                evidence: Evidence::DocumentedHuman,
                split,
                text_sha256: sha256(&text),
                normalized_text_sha256: sha256(words(&text).join(" ")),
                word_count: MIN_WORDS,
                source_manifest_sha256: sha256("synthetic exporter contract"),
                features_sha256: sha256(serde_json::to_vec(&features).unwrap()),
                share_alike_rights: None,
            },
            features,
        }
    }

    #[test]
    fn vector_export_freezes_train_space_and_preserves_unknown_and_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let config = FeatureConfig {
            max_coordinates: 16,
            word_coordinate_cap: Some(8),
            min_document_frequency: 1,
            ..Default::default()
        };
        let make_rows = |dev| {
            vec![
                vector_fixture(Split::Train, "anchor", true),
                vector_fixture(Split::Development, dev, false),
                vector_fixture(Split::Calibration, "calibration", true),
            ]
        };
        let first = root.path().join("first.jsonl");
        write_jsonl(&first, &make_rows("heldout")).unwrap();
        let output = root.path().join("vectors");
        let report = export_vectors(&first, &output, &config).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(output.join("feature_space.json")).unwrap()).unwrap();
        let space: FeatureSpace = serde_json::from_value(raw["content"].clone()).unwrap();
        assert_eq!(space.training_rows, 1);
        assert!(space.coordinates.iter().all(|c| !matches!(&c.key,
            slop_ninja_detector::features::CoordinateKey::Value { name, .. } if name=="heldout" || name=="calibration")));
        let dev: Value = serde_json::from_str(
            fs::read_to_string(output.join("development.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(dev["vector"]["unknown_coordinates"], 1);
        assert_eq!(
            dev["vector"]["unavailable_families"],
            json!(["synthetic_optional"])
        );
        let availability=space.coordinates.iter().position(|c|matches!(&c.key,
            slop_ninja_detector::features::CoordinateKey::Available {family} if family=="synthetic_optional")).unwrap();
        assert!(
            dev["vector"]["values"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p[0] != availability)
        );
        assert!(!output.join("test.jsonl").exists());
        let second = root.path().join("second.jsonl");
        write_jsonl(&second, &make_rows("differentheldout")).unwrap();
        let second_report =
            export_vectors(&second, &root.path().join("second-vectors"), &config).unwrap();
        assert_eq!(
            report["feature_space_sha256"],
            second_report["feature_space_sha256"]
        );
        assert!(export_vectors(&first, &output, &config).is_err());
    }

    #[test]
    fn vector_export_rejects_cross_split_groups_and_changed_feature_payloads() {
        let root = tempfile::tempdir().unwrap();
        let mut rows = vec![
            vector_fixture(Split::Train, "anchor", true),
            vector_fixture(Split::Development, "heldout", true),
            vector_fixture(Split::Calibration, "calibration", true),
        ];
        rows[1].header.group = rows[0].header.group.clone();
        let first = root.path().join("overlap.jsonl");
        write_jsonl(&first, &rows).unwrap();
        let output = root.path().join("vectors");
        assert!(
            export_vectors(&first, &output, &FeatureConfig::default())
                .unwrap_err()
                .to_string()
                .contains("group crosses splits")
        );
        assert!(!output.exists());
        rows[1].header.group = "development-group".into();
        rows[1].header.features_sha256 = sha256("changed payload");
        let second = root.path().join("changed.jsonl");
        write_jsonl(&second, &rows).unwrap();
        assert!(
            export_vectors(&second, &output, &FeatureConfig::default())
                .unwrap_err()
                .to_string()
                .contains("payload hash mismatch")
        );
        assert!(!output.exists());
    }

    #[test]
    fn final_test_gate_precedes_any_artifact_or_feature_read() {
        let root = tempfile::tempdir().unwrap();
        let error = evaluate(
            &root.path().join("missing-features.jsonl"),
            &root.path().join("missing-model.json"),
            &root.path().join("report.json"),
            Split::Test,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("final test remains sealed"), "{error}");
        assert!(!root.path().join("report.json").exists());
    }

    #[test]
    fn normal_feature_loader_rejects_synthetic_protocol_labels() {
        let root = tempfile::tempdir().unwrap();
        let text = "violet ".repeat(MIN_WORDS);
        let features = lexical_features(&text);
        let fixture = FeatureRecord {
            header: FeatureHeader {
                schema: FEATURE_RECORD_SCHEMA.into(),
                id: "synthetic-contract-fixture".into(),
                group: "synthetic-contract-group".into(),
                origin: Origin::HumanOnly,
                evidence: Evidence::SyntheticFixture,
                split: Split::Train,
                text_sha256: sha256(&text),
                normalized_text_sha256: sha256(words(&text).join(" ")),
                word_count: MIN_WORDS,
                source_manifest_sha256: sha256("synthetic test manifest"),
                share_alike_rights: None,
                features_sha256: sha256(serde_json::to_vec(&features).unwrap()),
            },
            features,
        };
        let path = root.path().join("features.jsonl");
        write_jsonl(&path, &[fixture]).unwrap();
        assert!(
            load_features(&path, &[Split::Train])
                .unwrap_err()
                .to_string()
                .contains("synthetic/unknown evidence")
        );
    }
}
