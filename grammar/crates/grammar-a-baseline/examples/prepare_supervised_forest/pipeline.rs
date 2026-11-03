//! Supervised numeric-only shared A inputs from an explicitly allowed export.
//!
//! Usage: prepare_supervised_forest EXPORT_DIR DATASET_MANIFEST NEW_OUTPUT
//!        [--ablation all|structure|leaf_grammar|availability]
//!
//! Dataset schema: slop_ninja_a_supervised_dataset_v1. non_test_ids and test_ids
//! are disjoint arrays of IDs (Test has no records). split_assignments maps
//! source families to train/development/calibration/test. records_sha256 binds
//! the exact selected NONTEST input file and must equal export.input_sha256.
//! Input records must use the assembler's compact serde_json, one record per
//! line with a final newline. Reconstructing metadata JSONL verifies that hash.
//! sources/provenance metadata is retained through the manifest file binding.
//! Output metadata hashes use compact serde_json::Value serialization, with
//! sorted object keys and UTF-8, including the exact supplied text and labels.
//! Source hashes use exact text bytes. No source paths are opened.
//! The exact manifest file hash is frozen in the numeric contract and manifest.
//!
//! Mixed supervision uses slop_ninja_realized_span_share_v1 from
//! examples/a_controlled_generate.rs: verify source/final text hashes, source
//! and final byte spans, exact replacement assembly, and recompute replacement
//! words / final words under unicode_split_whitespace_v1. Full rewrites remain
//! mixed even at share 1. From-scratch assembly is checked separately. Evidence
//! is retained, never used as a feature or to invent a missing share_target.
//!
//! Report IDs are checked before rows are opened; lightweight row headers are
//! checked before metadata/text is materialized. Only allowed non-Test text
//! embedded in rows is accessed, to verify hashes and span bounds. No source,
//! window, Test file or final Test transform is opened or implemented here.
//! Train QUERY rows alone fit vocabulary, normalization and identity profiles.
//! Reference rows are transformed into reference.jsonl for a separate gallery.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

#[path = "supervision.rs"]
mod supervision;
use supervision::{Dataset, Profiles, Supervision};

const PARTS: [&str; 3] = ["train", "development", "calibration"];
const FEATURE_POLICY: &str = "slop_ninja_supervised_a_forest_numeric_v1";
const FIXED: [&str; 3] = [
    "document_availability/projection_available",
    "document_availability/search_exhaustive",
    "document_availability/has_token_opportunities",
];
const POOLS: [&str; 6] = [
    "native_complete",
    "fallback_complete",
    "partial",
    "search_limited",
    "lexical_recovery",
    "unavailable",
];
const TIERS: [&str; 8] = [
    "NativeComplete",
    "LexicalFallbackComplete",
    "Partial",
    "ResourceLimit",
    "BudgetSkipped",
    "TokenFragment",
    "ExecutionFailure",
    "Layout",
];

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn str_at<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let s = value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))?;
    ensure!(!s.is_empty(), "empty {key}");
    Ok(s)
}

fn boolean(value: &Value, key: &str) -> Result<bool> {
    value[key]
        .as_bool()
        .with_context(|| format!("missing boolean {key}"))
}

fn counter(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .with_context(|| format!("missing unsigned counter {key}"))
}

fn number(value: &Value) -> Result<f64> {
    let n = value.as_f64().context("missing numeric measurement")?;
    ensure!(n.is_finite() && n >= 0.0, "invalid nonnegative measurement");
    Ok(n)
}

fn digest_at<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let s = str_at(value, key)?;
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid SHA256 in {key}"
    );
    Ok(s)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

// Only these exported names can become learned coordinates. Pool prefixes are
// preserved verbatim; identities, source/grammar provenance and labels cannot
// become coordinates. Structural signatures are supplied by the bound exporter.
fn validate_key(key: &str) -> Result<()> {
    if matches!(
        key,
        "availability/unresolved_token"
            | "availability/no_real_reading_token"
            | "availability/forced_window"
    ) || key
        .strip_prefix("availability/window/")
        .is_some_and(|t| TIERS.contains(&t))
    {
        return Ok(());
    }
    let (pool, rest) = key.split_once('/').context("unpooled structural key")?;
    let (kind, signature) = rest
        .split_once('/')
        .context("missing structural signature")?;
    ensure!(
        POOLS.contains(&pool)
            && matches!(kind, "symbol" | "production" | "leaf")
            && !signature.is_empty(),
        "unsupported structural coordinate"
    );
    // Match the exporter's structural projection shape. Arbitrary named fields
    // such as lexical IDs, text, author or review evidence cannot enter a key.
    let signature: Value = serde_json::from_str(signature).context("non-JSON grammar signature")?;
    match kind {
        "symbol" => validate_symbol(&signature)?,
        "production" => {
            let tuple = signature.as_array().context("production tuple")?;
            ensure!(tuple.len() == 3, "production signature fields");
            validate_symbol(&tuple[0])?;
            let children = tuple[1].as_array().context("production children")?;
            for child in children {
                validate_symbol(child)?;
            }
            ensure!(
                tuple[2].is_null() || tuple[2].as_u64().is_some_and(|i| i < children.len() as u64),
                "production head child"
            );
        }
        "leaf" => {
            let tuple = signature.as_array().context("leaf grammar tuple")?;
            ensure!(tuple.len() == 4, "leaf grammar signature fields");
            ensure!(
                matches!(tuple[0].as_str(), Some("inventory" | "fallback")),
                "leaf marker"
            );
            ensure!(
                tuple[1]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()
                        && s.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')),
                "leaf POS must be a grammatical tag"
            );
            validate_symbol(&tuple[2])?;
            ensure!(
                tuple[3].is_null()
                    || tuple[3].as_str().is_some_and(
                        |s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())
                    ),
                "leaf morphology kind"
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn validate_symbol(value: &Value) -> Result<()> {
    let symbol = value.as_object().context("grammar symbol object")?;
    ensure!(
        symbol.len() == 2 && symbol.contains_key("category") && symbol.contains_key("features"),
        "non-structural symbol fields"
    );
    let category = str_at(value, "category")?;
    ensure!(
        category
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_'),
        "invalid grammar category"
    );
    let keys = [
        "agreement",
        "count",
        "proper",
        "form",
        "tense",
        "degree",
        "voice",
        "case",
        "person",
        "number",
        "aspect",
        "mood",
        "polarity",
    ];
    let values = [
        "first_singular",
        "second",
        "third_singular",
        "plural",
        "singular",
        "mass",
        "yes",
        "no",
        "any",
        "finite",
        "bare",
        "present_participle",
        "past_participle",
        "perfect_participle",
        "passive_participle",
        "gerund",
        "present",
        "past",
        "future",
        "positive",
        "comparative",
        "superlative",
        "active",
        "passive",
        "nominative",
        "accusative",
        "genitive",
        "first",
        "third",
        "perfect",
        "progressive",
        "indicative",
        "subjunctive",
        "imperative",
        "negative",
    ];
    for (key, value) in value["features"]
        .as_object()
        .context("symbol feature map")?
    {
        ensure!(
            keys.contains(&key.as_str()) && value.as_str().is_some_and(|v| values.contains(&v)),
            "symbol feature outside frozen grammatical vocabulary"
        );
    }
    Ok(())
}

fn document_scope(key: &str) -> bool {
    FIXED.contains(&key)
        || key.starts_with("availability/window/")
        || key == "availability/forced_window"
}

struct Row {
    id: String,
    group: String,
    split: String,
    supervision: Supervision,
    available: bool,
    tokens: f64,
    counts: BTreeMap<String, f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ablation {
    All,
    Structure,
    LeafGrammar,
    Availability,
}

impl Ablation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "structure" => Ok(Self::Structure),
            "leaf_grammar" => Ok(Self::LeafGrammar),
            "availability" => Ok(Self::Availability),
            _ => anyhow::bail!("ablation must be all, structure, leaf_grammar or availability"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Structure => "structure",
            Self::LeafGrammar => "leaf_grammar",
            Self::Availability => "availability",
        }
    }

    fn includes(self, key: &str) -> bool {
        let availability = FIXED.contains(&key) || key.starts_with("availability/");
        let leaf_grammar = key.split('/').nth(1) == Some("leaf");
        match self {
            Self::All => true,
            Self::Structure => !availability && !leaf_grammar,
            Self::LeafGrammar => leaf_grammar,
            Self::Availability => availability,
        }
    }
}

fn measurement(row: &Row, key: &str) -> Result<(f64, f64)> {
    let count = row.counts.get(key).copied().unwrap_or(0.0);
    let denominator = if document_scope(key) { 1.0 } else { row.tokens };
    if denominator == 0.0 {
        ensure!(count == 0.0, "count without token opportunities");
        return Ok((0.0, 0.0));
    }
    let value = (count / denominator).ln_1p();
    ensure!(value.is_finite(), "nonfinite transformed measurement");
    Ok((value, 1.0))
}

pub(super) fn prepare_cli() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3 || (args.len() == 5 && args[3] == "--ablation"),
        "prepare_supervised_forest EXPORT_DIR DATASET_MANIFEST NEW_OUTPUT [--ablation all|structure|leaf_grammar|availability]"
    );
    let ablation = if args.len() == 5 {
        Ablation::parse(args[4].to_str().context("non-UTF8 ablation")?)?
    } else {
        Ablation::All
    };
    let input = Path::new(&args[0]);
    let out = Path::new(&args[2]);
    prepare(input, Path::new(&args[1]), out, ablation)
}

struct Export {
    source_contract: Value,
    contract_bytes: Vec<u8>,
    report_bytes: Vec<u8>,
    report: Value,
    bytes: Vec<u8>,
    rows: Vec<Row>,
    population: Vec<Value>,
    exclusions: Vec<Value>,
}

fn read_export(input: &Path, dataset: &Dataset) -> Result<Export> {
    let contract_bytes = fs::read(input.join("contract.json"))?;
    let report_bytes = fs::read(input.join("report.json"))?;
    let source_contract: Value = serde_json::from_slice(&contract_bytes)?;
    let report: Value = serde_json::from_slice(&report_bytes)?;
    ensure!(
        source_contract["schema"] == "slop_ninja_document_export_contract_v1",
        "export schema"
    );
    ensure!(
        report["schema"] == "slop_ninja_document_export_report_v1",
        "report schema"
    );
    ensure!(report["complete"] == true, "incomplete export");
    // jsonl mode correctly reports false: the explicit dataset allow-list and
    // per-row frozen split enforce this boundary, not that upstream flag.
    let _upstream_test_exclusion = boolean(&source_contract, "test_populations_excluded")?;
    ensure!(
        digest_at(&source_contract, "input_sha256")? == dataset.records_sha256,
        "dataset records/export input hash mismatch"
    );
    ensure!(
        source_contract["policy"].is_object(),
        "missing document policy"
    );
    let engine = str_at(&source_contract, "engine_identity")?;
    let parser = str_at(&source_contract, "parser_identity")?;
    ensure!(
        str_at(&report, "engine_identity")? == engine,
        "report/contract engine mismatch"
    );
    let documents = report["documents"].as_array().context("report documents")?;
    let mut entries = BTreeMap::new();
    let mut prefixes = BTreeSet::new();
    for entry in documents {
        let id = str_at(entry, "id")?;
        dataset.authorize_id(id)?;
        let prefix = str_at(entry, "prefix")?;
        ensure!(
            prefix.bytes().all(|b| b.is_ascii_digit()),
            "invalid receipt prefix"
        );
        ensure!(prefixes.insert(prefix), "duplicate receipt prefix");
        ensure!(entries.insert(id, entry).is_none(), "duplicate report id");
    }
    dataset.require_exact_ids(entries.keys().copied())?;
    let bytes = fs::read(input.join("rows.jsonl"))?;
    ensure!(
        sha(&bytes) == digest_at(&report, "rows_sha256")?,
        "row digest mismatch"
    );
    // Validate every header before deserializing any prose/labels. Unknown fields
    // (including text) are skipped by serde rather than materialized as strings.
    dataset.check_headers(&bytes)?;

    let mut rows = Vec::new();
    let mut population = Vec::new();
    let mut exclusions = Vec::new();
    let mut ids = BTreeSet::new();
    let mut records_hash = Sha256::new();
    for (line_number, line) in std::str::from_utf8(&bytes)?.lines().enumerate() {
        let raw: Value = serde_json::from_str(line)
            .with_context(|| format!("invalid export row {}", line_number + 1))?;
        let metadata = &raw["metadata"];
        let metadata_bytes = serde_json::to_vec(metadata)?;
        records_hash.update(&metadata_bytes);
        records_hash.update(b"\n");
        let id = str_at(&raw, "id")?;
        ensure!(ids.insert(id.to_owned()), "duplicate row id");
        dataset.validate_metadata(id, metadata)?;
        let split = str_at(metadata, "frozen_split")?;
        let group = str_at(metadata, "source_group")?;
        let supervision = dataset.supervision(metadata)?;
        let receipt = &raw["receipt"];
        let summary = &receipt["summary"];
        let features = &receipt["features"];
        ensure!(
            receipt["schema"] == "slop_ninja_cfg_document_fallback_v1",
            "receipt schema"
        );
        ensure!(
            receipt["feature_schema"] == "slop_ninja_cfg_forest_expectations_v1",
            "feature schema"
        );
        ensure!(
            receipt["resolver"] == "acyclic-uniform-derivations-max-covered-min-islands-v1",
            "resolver"
        );
        ensure!(
            str_at(receipt, "engine_identity")? == engine,
            "mixed engine identities"
        );
        ensure!(
            str_at(receipt, "parser_identity")? == parser,
            "mixed parser identities"
        );
        ensure!(
            digest_at(receipt, "source_sha256")?
                == sha(metadata["text"]
                    .as_str()
                    .context("metadata text")?
                    .as_bytes()),
            "receipt/metadata source hash mismatch"
        );
        let entry = entries.get(id).context("row absent from report")?;
        ensure!(
            entry["summary"] == *summary,
            "report/receipt summary mismatch"
        );
        // The export hashes exact pretty JSON bytes plus a newline. Verify the
        // actual receipt bytes instead of assuming a re-serialization layout.
        let receipt_bytes =
            fs::read(input.join(format!("{}.receipt.json", str_at(entry, "prefix")?)))?;
        ensure!(
            sha(&receipt_bytes) == digest_at(&raw, "receipt_sha256")?,
            "receipt digest mismatch"
        );
        ensure!(
            serde_json::from_slice::<Value>(&receipt_bytes)? == *receipt,
            "inline receipt mismatch"
        );
        digest_at(&raw, "windows_sha256")?; // Retained binding, not a window-file read.
        let tokens = counter(features, "token_opportunities")?;
        ensure!(
            tokens == counter(summary, "tokens")?,
            "token denominator mismatch"
        );
        let represented = counter(features, "represented_tokens")?;
        let unresolved = counter(features, "unresolved_tokens")?;
        ensure!(
            represented.checked_add(unresolved) == Some(tokens),
            "token accounting mismatch"
        );
        ensure!(
            represented == counter(summary, "represented_tokens")?,
            "represented token mismatch"
        );
        ensure!(
            unresolved == counter(summary, "unresolved_tokens")?,
            "unresolved token mismatch"
        );
        ensure!(
            counter(summary, "input_bytes")? == counter(summary, "accounted_bytes")?,
            "incomplete source accounting"
        );
        ensure!(
            counter(summary, "input_bytes")? == metadata["text"].as_str().unwrap().len() as u64,
            "receipt/metadata byte count mismatch"
        );
        ensure!(
            number(&features["expected_fallback_leaves"])?
                == number(&summary["expected_fallback_leaves"])?,
            "fallback expectation mismatch"
        );
        let projection_available = boolean(features, "projection_available")?;
        let search_exhaustive = boolean(features, "search_exhaustive")?;
        let failures = counter(summary, "execution_failures")?;
        let available = failures == 0;
        let mut counts = BTreeMap::new();
        for (key, value) in features["counts"].as_object().context("feature counts")? {
            validate_key(key)?;
            counts.insert(key.clone(), number(value)?);
        }
        let execution_windows = counts
            .get("availability/window/ExecutionFailure")
            .copied()
            .unwrap_or(0.0);
        ensure!(
            execution_windows == failures as f64,
            "execution failure accounting mismatch"
        );
        let windows: f64 = TIERS
            .iter()
            .map(|tier| {
                counts
                    .get(&format!("availability/window/{tier}"))
                    .copied()
                    .unwrap_or(0.0)
            })
            .sum();
        ensure!(
            windows == counter(summary, "windows")? as f64,
            "window accounting mismatch"
        );
        for (key, flag) in FIXED
            .iter()
            .zip([projection_available, search_exhaustive, tokens > 0])
        {
            counts.insert((*key).into(), if flag { 1.0 } else { 0.0 });
        }
        let row = Row {
            id: id.into(),
            group: group.into(),
            split: split.into(),
            supervision,
            available,
            tokens: tokens as f64,
            counts,
        };
        // Validate even coordinates that will not survive Train vocabulary selection.
        for key in row.counts.keys() {
            measurement(&row, key)?;
        }
        let reason = if available {
            Value::Null
        } else {
            json!("document_execution_failure")
        };
        population.push(json!({
            "id":id,"group":group,"split":split,"available":available,
            "exclusion_reason":reason,"metadata":supervision::without_text(metadata),"summary":summary,
            "metadata_sha256":sha(&metadata_bytes),
            "purpose":row.supervision.purpose,"exposed_to_grammar":row.supervision.exposed,
            "exposure_was_unspecified":row.supervision.exposure_was_unspecified,
            "engine_identity":engine,"parser_identity":parser,
            "feature_schema":receipt["feature_schema"],"resolver":receipt["resolver"],
            "source_sha256":receipt["source_sha256"],
            "receipt_sha256":raw["receipt_sha256"],"windows_sha256":raw["windows_sha256"],
            "token_opportunities":tokens,"projection_available":projection_available,
            "search_exhaustive":search_exhaustive,
            "retained_pools":POOLS.iter().filter(|pool| row.counts.keys().any(|k| k.starts_with(&format!("{pool}/")))).collect::<Vec<_>>()
        }));
        if !available {
            exclusions.push(json!({"id":id,"source_group":group,"split":split,"reason":reason,"execution_failures":failures}));
        }
        rows.push(row);
    }
    ensure!(
        rows.len() == documents.len(),
        "report/row population mismatch"
    );
    ensure!(
        hex::encode(records_hash.finalize()) == dataset.records_sha256,
        "export metadata differs from manifest-bound canonical records JSONL"
    );
    Ok(Export {
        source_contract,
        contract_bytes,
        report_bytes,
        report,
        bytes,
        rows,
        population,
        exclusions,
    })
}

fn prepare(input: &Path, dataset_path: &Path, out: &Path, ablation: Ablation) -> Result<()> {
    ensure!(!out.try_exists()?, "output must be new");
    let dataset_bytes = fs::read(dataset_path)?;
    let dataset = Dataset::parse(&dataset_bytes)?;
    let Export {
        source_contract,
        contract_bytes,
        report_bytes,
        report,
        bytes,
        rows,
        population,
        exclusions,
    } = read_export(input, &dataset)?;
    let engine = str_at(&source_contract, "engine_identity")?;
    let parser = str_at(&source_contract, "parser_identity")?;
    let upstream_test_exclusion = boolean(&source_contract, "test_populations_excluded")?;
    let train: Vec<_> = rows
        .iter()
        .filter(|r| r.available && r.split == "train" && r.supervision.purpose == "query")
        .collect();
    ensure!(train.len() >= 2, "need at least two eligible Train rows");
    for part in PARTS {
        ensure!(
            rows.iter()
                .any(|r| r.available && r.split == part && r.supervision.purpose == "query"),
            "empty eligible {part} split"
        );
    }
    let mut frequency = BTreeMap::<String, usize>::new();
    let mut family_counts = BTreeMap::<&str, usize>::new();
    for row in &train {
        *family_counts.entry(&row.group).or_default() += 1;
        for key in row.counts.keys() {
            *frequency.entry(key.clone()).or_default() += 1;
        }
    }
    let keys: Vec<_> = frequency
        .iter()
        .filter(|(key, n)| **n >= 2 && ablation.includes(key))
        .map(|(key, _)| key.clone())
        .collect();
    ensure!(
        !keys.is_empty(),
        "no eligible Train query coordinates for ablation"
    );
    let profiles = Profiles::fit(train.iter().map(|row| &row.supervision));
    let mut means = Vec::new();
    let mut scales = Vec::new();
    let mut mask_centers = Vec::new();
    for key in &keys {
        let mut sum = 0.0;
        let mut total = 0.0;
        for row in &train {
            let (value, mask) = measurement(row, key)?;
            let weight = mask / family_counts[row.group.as_str()] as f64;
            sum += weight * value;
            total += weight;
        }
        let mean = if total > 0.0 { sum / total } else { 0.0 };
        // Two passes avoid cancellation for nearly constant coordinates.
        let mut variance = 0.0;
        for row in &train {
            let (value, mask) = measurement(row, key)?;
            let weight = mask / family_counts[row.group.as_str()] as f64;
            variance += weight * (value - mean).powi(2);
        }
        let scale = if total > 0.0 {
            (variance / total).sqrt().max(0.01)
        } else {
            1.0
        };
        ensure!(
            mean.is_finite() && scale.is_finite(),
            "nonfinite Train normalization"
        );
        means.push(mean);
        scales.push(scale);
        let mut mask_sum = 0.0;
        let mut family_weight = 0.0;
        for row in &train {
            let (_, mask) = measurement(row, key)?;
            let weight = 1.0 / family_counts[row.group.as_str()] as f64;
            mask_sum += weight * mask;
            family_weight += weight;
        }
        mask_centers.push(mask_sum / family_weight);
    }
    let coordinate_metadata: Vec<_> = keys.iter().map(|key| json!({
        "key":key,"train_rows_with_declared_key":frequency[key],
        "denominator":if document_scope(key) { "one_document" } else { "total_document_token_opportunities" },
        "mask":if document_scope(key) { "always_one" } else { "total_document_token_opportunities_gt_zero" }
    })).collect();
    let contract = json!({
        "schema":"slop_ninja_shared_a_numeric_contract_v1",
        "feature_policy":FEATURE_POLICY,"feature_policy_version":1,"ablation":ablation.name(),
        "evidence":"native_cfg_retained_forest_expectations_with_named_pools",
        "exposure":"per-row supplied exposure; missing/null conservatively true; no clean-holdout qualification inferred",
        "dataset_manifest_sha256":sha(&dataset_bytes),
        "dataset_schema":"slop_ninja_a_supervised_dataset_v1",
        "dataset_allow_list":"exact non_test_ids = report ids = row ids; test_ids disjoint, IDs only; per-family frozen split verified before metadata materialization",
        "metadata_binding":"manifest records_sha256 equals exporter input_sha256 AND independently reconstructed compact metadata JSONL hash; per-row metadata digest retained; exact source bytes checked against receipt",
        "records_sha256":dataset.records_sha256,
        "source_export_sha256":sha(&report_bytes),
        "source_contract_file_sha256":sha(&contract_bytes),
        "source_contract_sha256":sha(&serde_json::to_vec(&source_contract)?),
        "source_rows_sha256":sha(&bytes),
        "source_binding":{"report_engine_matches_contract":true,"all_receipts_match_engine_and_parser":true,
            "receipt_file_digests_verified":true,"window_file_digests_verified":false,
            "upstream_test_populations_excluded":upstream_test_exclusion,
            "source_input_sha256":source_contract["input_sha256"],
            "raw_export_input_file_read":false,
            "policy_binding":"export contract bytes pinned; engine identity compared, not recomputed"},
        "grammar_identities":{"document_engine":engine,"native_parser":parser},
        "document_policy":source_contract["policy"],
        "projection":{"feature_schema":"slop_ninja_cfg_forest_expectations_v1",
            "resolver":"acyclic-uniform-derivations-max-covered-min-islands-v1"},
        "pool_meanings":{
            "native_complete":"retained expectations from native-complete windows; not reviewed coverage",
            "fallback_complete":"retained expectations from complete windows using lexical fallback",
            "partial":"retained expectations over selected recognized islands in partial windows",
            "search_limited":"retained expectations after bounded search; not exhaustive grammar expectations",
            "lexical_recovery":"lexical-only recovery after projection failure; not full syntactic expectations",
            "unavailable":"exporter reserved residual pool; no invented syntax or POS"
        },
        "availability_meanings":{
            "exported_token_counts":"unresolved_token and no_real_reading_token divided by all document tokens",
            "exported_window_counts":"availability/window/* and availability/forced_window are document-scope counts, denominator one",
            "fixed_flags":"projection_available and search_exhaustive are exact receipt booleans; has_token_opportunities is token_opportunities > 0; denominator one",
            "pool_masks":"opportunity masks indicate a defined retained-count rate, not exhaustive search or presence of a production",
            "absent_keys":"zero retained count with the coordinate denominator's mask, even when the global projection flag is false",
            "empty_documents":"token-rate values and masks zero; document-scope flags and window counts remain available"
        },
        "fixed_availability_coordinates":FIXED.iter().filter(|key| ablation.includes(key)).collect::<Vec<_>>(),
        "ablation_meanings":{"all":"symbol, production, leaf and availability",
            "structure":"pooled symbol and production expectations only",
            "leaf_grammar":"pooled grammatical leaf category/POS/morphology expectations only; not persistent lexical identity",
            "availability":"document flags and availability counts only",
            "masks":"selected coordinates always retain their denominator masks"},
        "dictionary_lexical_identity_export_available":false,
        "vocabulary":"sorted keys declared in at least two eligible Train QUERY rows, including zero-valued keys; no references, Development or Calibration vocabulary",
        "aggregation":"use exported document sums; never average window ratios; preserve pool prefixes",
        "coordinates":keys,"coordinate_metadata":coordinate_metadata,
        "normalization":{"transform":"log1p(count/denominator)","fit":"train_query_only_equal_source_family_weight",
            "weight":"mask / eligible_Train_query_rows_in_source_family, then renormalize per coordinate",
            "mean":means,"scale":scales,"scale_floor":0.01,"all_masked_mean":0.0,"all_masked_scale":1.0},
        "layout":"standardized_values_then_train_centered_availability_masks",
        "availability_mask_center":mask_centers,
        "availability_mask_transform":"mask_minus_equal_source_family_Train_presence_mean; invert by adding frozen mean; do not rescale",
        "availability_center_reason":"avoid thousands of duplicated constant-one inputs saturating the shared encoder; no Development/Calibration information used",
        "masked_standardized_value":0.0,"input_dimension":keys.len()*2,
        "split_assignments":dataset.split_assignments,"split_policy":"exact metadata.frozen_split equals manifest source-family assignment; no resplit",
        "execution_failures":"unavailable explicit population exclusion; never fitted or scored",
        "human_profiles":profiles.human,"ai_profiles":profiles.ai,"origin_order":["human_only","model_only","mixed"],
        "profile_policy":"sorted present-role identities from eligible Train QUERY rows only; fewer than two identities disables role supervision and named profiles",
        "target_policy":"share copied only from explicit share_target; unknown or absent/unspecified role identity gives null loss target; supplied truth retained in roles",
        "reference_policy":"train split purpose=reference only; frozen transform and gallery export; never fitted or added to profiles; all reference loss targets null",
        "supervision_limits":["origin copied from supplied evidence, not independently authenticated",
            "mixed numeric shares recomputed from supplied source/replacement assembly and whitespace words; provenance authenticity not established",
            "named identity classification is conditional resemblance, not identity authentication",
            "reference gallery qualification is separate; no final Test transform or evaluation"],
        "feature_leak_guards":{"allowed_inputs":"validated receipt counts and three fixed document availability flags only",
            "forbidden":"text, labels, role presence, identities, purpose, splits, source groups, provenance, evidence, exposure, hashes, grammar item IDs and review status",
            "train_fit_population":"available Train QUERY rows only","reference_fit":false},
        "labels_not_inputs":true,"metadata_not_inputs":true,"final_test_opened":false
    });
    // Match prepare_shared: compact-JSON semantic contract hash, separately bound
    // pretty-file hash, exact newline-terminated JSONL split hashes.
    let contract_hash = sha(&serde_json::to_vec(&contract)?);
    let contract_output = serde_json::to_vec_pretty(&contract)?;
    let mut output = BTreeMap::<&str, Vec<Value>>::new();
    for row in rows.iter().filter(|r| r.available) {
        let part = if row.supervision.purpose == "reference" {
            "reference"
        } else {
            &row.split
        };
        output.entry(part).or_default().push(numeric_row(
            row,
            &contract_hash,
            &keys,
            &means,
            &scales,
            &mask_centers,
            &profiles,
        )?);
    }
    let available = rows.iter().filter(|r| r.available).count();
    let population_output = serde_json::to_vec_pretty(&json!({
        "requested":rows.len(),"available":available,"rows":population,"excluded":exclusions,
        "dataset_manifest_sha256":sha(&dataset_bytes),
        "reference_rows":rows.iter().filter(|r| r.available && r.supervision.purpose == "reference").count(),
        "grammar_exposed_rows":rows.iter().filter(|r| r.supervision.exposed).count(),
        "exposure_unspecified_rows":rows.iter().filter(|r| r.supervision.exposure_was_unspecified).count(),
        "excluded_reserved_test_documents":counter(&report,"excluded_reserved_test_documents")?,
        "reserved_test_ids_count":dataset.test_ids.len(),
        "reserved_test_policy":"manifest IDs only; exact non-Test report allow-list and per-row header/split checks; no Test source, receipt or transform",
        "clean_holdout":false,"final_test_opened":false
    }))?;
    let mut split_files = Vec::new();
    let mut bindings = Vec::new();
    for part in PARTS.into_iter().chain(["reference"]) {
        let subset = output.get(part).map(Vec::as_slice).unwrap_or(&[]);
        let mut split_bytes = Vec::new();
        for row in subset {
            serde_json::to_writer(&mut split_bytes, row)?;
            split_bytes.push(b'\n');
        }
        bindings.push(json!({"split":part,"rows":subset.len(),"sha256":sha(&split_bytes),
            "families":subset.iter().map(|r| r["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len()}));
        split_files.push((part, split_bytes));
    }
    let manifest = json!({
        "schema":"slop_ninja_shared_a_dataset_v1","contract_sha256":contract_hash,
        "dataset_manifest_sha256":sha(&dataset_bytes),
        "contract_file_sha256":sha(&contract_output),"population_file_sha256":sha(&population_output),
        "splits":bindings,"requested":rows.len(),"available":available,"excluded":rows.len()-available,
        "clean_holdout":false,"final_test_opened":false,"feature_dimension":keys.len()*2,
        "implementation_sha256":sha(include_bytes!("../prepare_supervised_forest.rs")),
        "pipeline_implementation_sha256":sha(include_bytes!("pipeline.rs")),
        "supervision_implementation_sha256":sha(include_bytes!("supervision.rs")),
        "ablation":ablation.name()
    });
    // Validate and serialize everything before creating output. Publish the
    // manifest last, only after every data file has been written and synced.
    let manifest_output = serde_json::to_vec_pretty(&manifest)?;
    fs::create_dir(out)?;
    write_new(&out.join("contract.json"), &contract_output)?;
    write_new(&out.join("population.json"), &population_output)?;
    for (part, split_bytes) in split_files {
        write_new(&out.join(format!("{part}.jsonl")), &split_bytes)?;
    }
    write_new(&out.join("manifest.json"), &manifest_output)?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}

fn numeric_row(
    row: &Row,
    contract_hash: &str,
    keys: &[String],
    means: &[f64],
    scales: &[f64],
    mask_centers: &[f64],
    profiles: &Profiles,
) -> Result<Value> {
    let mut values = Vec::with_capacity(keys.len() * 2);
    let mut masks = Vec::with_capacity(keys.len());
    for (i, key) in keys.iter().enumerate() {
        let (value, mask) = measurement(row, key)?;
        let standardized = if mask == 1.0 {
            (value - means[i]) / scales[i]
        } else {
            0.0
        };
        ensure!(
            standardized.is_finite() && (standardized as f32).is_finite(),
            "nonfinite trainer coordinate"
        );
        values.push(standardized);
        masks.push(mask - mask_centers[i]);
    }
    values.extend(masks);

    Ok(json!({
        "id":row.id,"source_group":row.group,
        "purpose":row.supervision.purpose,"frozen_split":row.split,
        "exposed_to_grammar":row.supervision.exposed,
        "roles":profiles.roles(&row.supervision),
        "input":{"contract_sha256":contract_hash,"x":values},
        "targets":profiles.targets(&row.supervision)
    }))
}
mod frozen;
pub(super) use frozen::transform_cli;
pub(super) use frozen::transform_references;
pub(super) use frozen::transform_train_extension;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
