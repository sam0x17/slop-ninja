//! Numeric-only shared A inputs from a completed, non-Test document export.
//!
//! Reads contract/report/rows and the reported non-Test receipt files only: never
//! source prose, window forests, a cohort manifest, or a reserved Test artifact.
//! Retained forest expectations are not complete-syntax claims. In particular,
//! projection/search flags describe the whole document, not per-key presence.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

const PARTS: [&str; 3] = ["train", "development", "calibration"];
const FEATURE_POLICY: &str = "slop_ninja_shared_a_forest_numeric_v2";
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
    origin: usize,
    available: bool,
    tokens: f64,
    counts: BTreeMap<String, f64>,
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

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "prepare_forest_shared DOCUMENT_EXPORT_DIR NEW_OUTPUT_DIR"
    );
    let input = Path::new(&args[0]);
    let out = Path::new(&args[1]);
    ensure!(!out.try_exists()?, "output must be new");
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
    ensure!(
        source_contract["test_populations_excluded"] == true,
        "requires upstream Test exclusion"
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
    let bytes = fs::read(input.join("rows.jsonl"))?;
    ensure!(
        sha(&bytes) == digest_at(&report, "rows_sha256")?,
        "row digest mismatch"
    );
    let documents = report["documents"].as_array().context("report documents")?;
    let mut entries = BTreeMap::new();
    let mut prefixes = BTreeSet::new();
    for entry in documents {
        let id = str_at(entry, "id")?;
        let prefix = str_at(entry, "prefix")?;
        ensure!(
            prefix.bytes().all(|b| b.is_ascii_digit()),
            "invalid receipt prefix"
        );
        ensure!(prefixes.insert(prefix), "duplicate receipt prefix");
        ensure!(entries.insert(id, entry).is_none(), "duplicate report id");
    }

    let mut rows = Vec::new();
    let mut population = Vec::new();
    let mut exclusions = Vec::new();
    let mut split_assignments = BTreeMap::<String, String>::new();
    let mut ids = BTreeSet::new();
    for (line_number, line) in std::str::from_utf8(&bytes)?.lines().enumerate() {
        let raw: Value = serde_json::from_str(line)
            .with_context(|| format!("invalid export row {}", line_number + 1))?;
        let metadata = &raw["metadata"];
        let split = str_at(metadata, "frozen_split")?;
        ensure!(
            PARTS.contains(&split),
            "reserved Test or unknown split in export"
        );
        ensure!(
            metadata["exposed_to_grammar"] == true,
            "missing grammar exposure marker"
        );
        let id = str_at(&raw, "id")?;
        ensure!(ids.insert(id.to_owned()), "duplicate row id");
        let group = str_at(metadata, "source_group")?;
        if let Some(previous) = split_assignments.insert(group.into(), split.into()) {
            ensure!(previous == split, "source family crosses frozen splits");
        }
        let origin = match str_at(metadata, "origin")? {
            "human_only" => 0,
            "model_only" => 1,
            "mixed" => 2,
            _ => anyhow::bail!("unsupported origin label"),
        };
        ensure!(!metadata["evidence"].is_null(), "missing origin evidence");
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
        digest_at(receipt, "source_sha256")?;
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
            origin,
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
            "exclusion_reason":reason,"metadata":metadata,"summary":summary,
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
    let train: Vec<_> = rows
        .iter()
        .filter(|r| r.available && r.split == "train")
        .collect();
    ensure!(train.len() >= 2, "need at least two eligible Train rows");
    for part in PARTS {
        ensure!(
            rows.iter().any(|r| r.available && r.split == part),
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
        .filter(|(_, n)| **n >= 2)
        .map(|(key, _)| key.clone())
        .collect();
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
        "feature_policy":FEATURE_POLICY,"feature_policy_version":2,
        "evidence":"native_cfg_retained_forest_expectations_with_named_pools",
        "exposure":"all_exported_families_grammar_exposed;frozen_splits_are_not_clean_holdouts",
        "source_export_sha256":sha(&report_bytes),
        "source_contract_file_sha256":sha(&contract_bytes),
        "source_contract_sha256":sha(&serde_json::to_vec(&source_contract)?),
        "source_rows_sha256":sha(&bytes),
        "source_binding":{"report_engine_matches_contract":true,"all_receipts_match_engine_and_parser":true,
            "receipt_file_digests_verified":true,"window_file_digests_verified":false,
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
        "fixed_availability_coordinates":FIXED,
        "vocabulary":"sorted keys declared in at least two eligible Train rows, including zero-valued keys; fixed flags declared in every row; no Development/Calibration vocabulary",
        "aggregation":"use exported document sums; never average window ratios; preserve pool prefixes",
        "coordinates":keys,"coordinate_metadata":coordinate_metadata,
        "normalization":{"transform":"log1p(count/denominator)","fit":"train_only_equal_source_family_weight",
            "weight":"mask / eligible_Train_rows_in_source_family, then renormalize per coordinate",
            "mean":means,"scale":scales,"scale_floor":0.01,"all_masked_mean":0.0,"all_masked_scale":1.0},
        "layout":"standardized_values_then_train_centered_availability_masks",
        "availability_mask_center":mask_centers,
        "availability_mask_transform":"mask_minus_equal_source_family_Train_presence_mean; invert by adding frozen mean; do not rescale",
        "availability_center_reason":"avoid thousands of duplicated constant-one inputs saturating the shared encoder; no Development/Calibration information used",
        "masked_standardized_value":0.0,"input_dimension":keys.len()*2,
        "split_assignments":split_assignments,"split_policy":"preserve metadata.frozen_split; no resplit",
        "execution_failures":"unavailable explicit population exclusion; never fitted or scored",
        "human_profiles":[],"ai_profiles":[],"origin_order":["human_only","model_only","mixed"],
        "supervision_limits":["origin copied from supplied evidence, not independently authenticated",
            "only endpoint shares 0 and 1; mixed share null without spans",
            "no trained fractional-share interpolation qualification","no attribution supervision or enrolled profiles",
            "grammar-exposed diagnostic only; no clean holdout or final Test evaluation"],
        "labels_not_inputs":true,"metadata_not_inputs":true,"final_test_opened":false
    });
    // Match prepare_shared: compact-JSON semantic contract hash, separately bound
    // pretty-file hash, exact newline-terminated JSONL split hashes.
    let contract_hash = sha(&serde_json::to_vec(&contract)?);
    let contract_output = serde_json::to_vec_pretty(&contract)?;
    let mut output = BTreeMap::<&str, Vec<Value>>::new();
    for row in rows.iter().filter(|r| r.available) {
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
        let share = match row.origin {
            0 => json!(0.0),
            1 => json!(1.0),
            _ => Value::Null,
        };
        output.entry(&row.split).or_default().push(json!({
            "id":row.id,"source_group":row.group,
            "input":{"contract_sha256":contract_hash,"x":values},
            "targets":{"origin":row.origin,"ai_involvement":row.origin!=0,"share":share,"human":null,"ai":null}
        }));
    }
    let available = rows.iter().filter(|r| r.available).count();
    let population_output = serde_json::to_vec_pretty(&json!({
        "requested":rows.len(),"available":available,"rows":population,"excluded":exclusions,
        "excluded_reserved_test_documents":counter(&report,"excluded_reserved_test_documents")?,
        "reserved_test_policy":"upstream exclusion count only; no Test rows read, manufactured or emitted",
        "clean_holdout":false,"final_test_opened":false
    }))?;
    let mut split_files = Vec::new();
    let mut bindings = Vec::new();
    for part in PARTS {
        let subset = output.get(part).context("empty split")?;
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
        "contract_file_sha256":sha(&contract_output),"population_file_sha256":sha(&population_output),
        "splits":bindings,"requested":rows.len(),"available":available,"excluded":rows.len()-available,
        "clean_holdout":false,"final_test_opened":false,"feature_dimension":keys.len()*2,
        "implementation_sha256":sha(include_bytes!("prepare_forest_shared.rs"))
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
