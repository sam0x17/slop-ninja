//! Project arbitrary unlabelled cfg_document_export jsonl records for inference.
//!
//! project_frozen_documents CONTRACT.json CFG_DOCUMENT_EXPORT_DIR NEW_NUMERIC.jsonl
//!
//! Only metadata.id and metadata.text are consumed. Optional labels, provenance,
//! authors, generators and splits are skipped and never required or used. This
//! performs no vocabulary, normalization, profile or model fitting. Outputs are
//! exactly {id,input:{contract_sha256,x}}, suitable for shared_predict.
//!
//! The contract semantic hash is SHA256(compact serde_json::Value serialization),
//! matching the frozen preparer; shared_predict binds this hash to its weights.
//! Receipt files and exact export rows are verified; source/window files are
//! never opened. Execution failures reject the batch, without inventing numeric
//! results. All validation completes before the new output file is created.
//!
//! Formula and grammar key validation are copied into the example-specific
//! helper to keep every already frozen preparer source/hash unchanged. A
//! separately invoked conformance test compares non-Test prepared vectors bit
//! for bit, including reference rows. Tests never discover or open Test files.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
};

include!("project_frozen_documents/semantics.rs");

const FEATURE_SCHEMA: &str = "slop_ninja_cfg_forest_expectations_v1";
const RESOLVER: &str = "acyclic-uniform-derivations-max-covered-min-islands-v1";

struct Frozen {
    hash: String,
    engine: String,
    parser: String,
    policy: Value,
    keys: Vec<String>,
    means: Vec<f64>,
    scales: Vec<f64>,
    centers: Vec<f64>,
}

impl Frozen {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let contract: Value = serde_json::from_slice(bytes)?;
        ensure!(
            contract["schema"] == "slop_ninja_shared_a_numeric_contract_v1"
                && contract["feature_policy"] == FEATURE_POLICY
                && contract["feature_policy_version"] == 1,
            "unsupported frozen feature policy/schema/version"
        );
        ensure!(
            contract["projection"]["feature_schema"] == FEATURE_SCHEMA
                && contract["projection"]["resolver"] == RESOLVER,
            "unsupported frozen projection/resolver"
        );
        ensure!(
            contract["document_policy"].is_object(),
            "frozen document policy missing"
        );
        ensure!(
            contract["layout"] == "standardized_values_then_train_centered_availability_masks"
                && contract["normalization"]["transform"] == "log1p(count/denominator)"
                && contract["normalization"]["fit"]
                    == "train_query_only_equal_source_family_weight"
                && contract["normalization"]["scale_floor"] == 0.01
                && contract["normalization"]["all_masked_mean"] == 0.0
                && contract["normalization"]["all_masked_scale"] == 1.0
                && contract["availability_mask_transform"]
                    == "mask_minus_equal_source_family_Train_presence_mean; invert by adding frozen mean; do not rescale"
                && contract["masked_standardized_value"] == 0.0,
            "unsupported frozen transform semantics"
        );
        let ablation = Ablation::parse(str_at(&contract, "ablation")?)?;
        let keys = contract["coordinates"]
            .as_array()
            .context("frozen coordinates")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .context("coordinate must be string")
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            !keys.is_empty() && keys.windows(2).all(|pair| pair[0] < pair[1]),
            "coordinates must be nonempty sorted unique keys"
        );
        for key in &keys {
            if !FIXED.contains(&key.as_str()) {
                validate_key(key)?;
            }
            ensure!(
                ablation.includes(key),
                "coordinate outside frozen ablation {}",
                ablation.name()
            );
        }
        ensure!(
            counter(&contract, "input_dimension")? == (keys.len() * 2) as u64,
            "frozen input dimension mismatch"
        );
        let vector = |v: &Value| -> Result<Vec<f64>> {
            let values = v.as_array().context("frozen normalization vector")?;
            ensure!(
                values.len() == keys.len(),
                "frozen normalization dimension mismatch"
            );
            values
                .iter()
                .map(|v| {
                    let n = v.as_f64().context("frozen coordinate must be numeric")?;
                    ensure!(n.is_finite(), "nonfinite frozen normalization");
                    Ok(n)
                })
                .collect()
        };
        let means = vector(&contract["normalization"]["mean"])?;
        let scales = vector(&contract["normalization"]["scale"])?;
        let centers = vector(&contract["availability_mask_center"])?;
        ensure!(
            scales.iter().all(|v| *v >= 0.01) && centers.iter().all(|v| (0.0..=1.0).contains(v)),
            "invalid frozen scale/mask center"
        );
        Ok(Self {
            hash: sha(&serde_json::to_vec(&contract)?),
            engine: str_at(&contract["grammar_identities"], "document_engine")?.into(),
            parser: str_at(&contract["grammar_identities"], "native_parser")?.into(),
            policy: contract["document_policy"].clone(),
            keys,
            means,
            scales,
            centers,
        })
    }

    fn validate_export(&self, contract: &Value, report: &Value) -> Result<()> {
        ensure!(
            contract["schema"] == "slop_ninja_document_export_contract_v1",
            "export contract schema"
        );
        ensure!(
            report["schema"] == "slop_ninja_document_export_report_v1"
                && report["complete"] == true,
            "incomplete or invalid export report"
        );
        ensure!(
            str_at(contract, "engine_identity")? == self.engine
                && str_at(report, "engine_identity")? == self.engine,
            "export/frozen engine mismatch"
        );
        ensure!(
            str_at(contract, "parser_identity")? == self.parser,
            "export/frozen parser mismatch"
        );
        ensure!(
            contract["policy"] == self.policy,
            "export/frozen document policy mismatch"
        );
        digest_at(contract, "input_sha256")?;
        // The input may have arbitrary JSONL whitespace/layout: its original
        // file digest is not a canonical metadata digest and is not recomputed.
        // Exact exported rows and each text/receipt binding are verified below.
        Ok(())
    }

    fn project(&self, row: &Measurements) -> Result<Vec<f64>> {
        // Intentionally the same operation order as pipeline::numeric_row.
        let mut values = Vec::with_capacity(self.keys.len() * 2);
        let mut masks = Vec::with_capacity(self.keys.len());
        for (i, key) in self.keys.iter().enumerate() {
            let (value, mask) = measurement(row, key)?;
            let standardized = if mask == 1.0 {
                (value - self.means[i]) / self.scales[i]
            } else {
                0.0
            };
            ensure!(
                standardized.is_finite() && (standardized as f32).is_finite(),
                "nonfinite trainer coordinate"
            );
            values.push(standardized);
            masks.push(mask - self.centers[i]);
        }
        values.extend(masks);
        Ok(values)
    }
}

// Serde skips every optional metadata field. Labels are never inspected,
// validated, copied to output, or used to determine a coordinate or mask.
#[derive(Deserialize)]
struct Metadata {
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct ExportRow {
    id: String,
    metadata: Metadata,
    receipt: Value,
    receipt_sha256: String,
    windows_sha256: String,
}

fn receipt_measurements(
    frozen: &Frozen,
    row: &ExportRow,
    entry: &Value,
    input: &Path,
) -> Result<Measurements> {
    ensure!(
        !row.id.is_empty() && row.id == row.metadata.id,
        "row/metadata ID mismatch"
    );
    let receipt = &row.receipt;
    let summary = &receipt["summary"];
    let features = &receipt["features"];
    ensure!(
        receipt["schema"] == "slop_ninja_cfg_document_fallback_v1",
        "receipt schema"
    );
    ensure!(
        receipt["feature_schema"] == FEATURE_SCHEMA && receipt["resolver"] == RESOLVER,
        "receipt feature schema/resolver mismatch"
    );
    ensure!(
        str_at(receipt, "engine_identity")? == frozen.engine,
        "receipt engine mismatch"
    );
    ensure!(
        str_at(receipt, "parser_identity")? == frozen.parser,
        "receipt parser mismatch"
    );
    ensure!(
        digest_at(receipt, "source_sha256")? == sha(row.metadata.text.as_bytes()),
        "receipt exact source text hash mismatch"
    );
    ensure!(
        entry["summary"] == *summary,
        "report/receipt summary mismatch"
    );
    let receipt_bytes = fs::read(input.join(format!("{}.receipt.json", str_at(entry, "prefix")?)))?;
    ensure!(
        sha(&receipt_bytes) == row.receipt_sha256,
        "receipt file digest mismatch"
    );
    ensure!(
        serde_json::from_slice::<Value>(&receipt_bytes)? == *receipt,
        "inline/file receipt mismatch"
    );
    digest_at(
        &json!({"windows_sha256":row.windows_sha256}),
        "windows_sha256",
    )?;
    let tokens = counter(features, "token_opportunities")?;
    let represented = counter(features, "represented_tokens")?;
    let unresolved = counter(features, "unresolved_tokens")?;
    ensure!(
        tokens == counter(summary, "tokens")?,
        "token denominator mismatch"
    );
    ensure!(
        represented.checked_add(unresolved) == Some(tokens),
        "token accounting mismatch"
    );
    ensure!(
        represented == counter(summary, "represented_tokens")?
            && unresolved == counter(summary, "unresolved_tokens")?,
        "represented/unresolved token mismatch"
    );
    let input_bytes = counter(summary, "input_bytes")?;
    ensure!(
        input_bytes == counter(summary, "accounted_bytes")?
            && input_bytes == row.metadata.text.len() as u64,
        "incomplete source byte accounting"
    );
    ensure!(
        number(&features["expected_fallback_leaves"])?
            == number(&summary["expected_fallback_leaves"])?,
        "fallback expectation mismatch"
    );
    let projection = boolean(features, "projection_available")?;
    let exhaustive = boolean(features, "search_exhaustive")?;
    let failures = counter(summary, "execution_failures")?;
    let mut counts = BTreeMap::new();
    for (key, count) in features["counts"]
        .as_object()
        .context("feature count map")?
    {
        validate_key(key)?;
        counts.insert(key.clone(), number(count)?);
    }
    ensure!(
        counts
            .get("availability/window/ExecutionFailure")
            .copied()
            .unwrap_or(0.0)
            == failures as f64,
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
    ensure!(
        failures == 0,
        "document execution failure: no numeric inference result"
    );
    for (key, flag) in FIXED.iter().zip([projection, exhaustive, tokens > 0]) {
        counts.insert((*key).into(), if flag { 1.0 } else { 0.0 });
    }
    let measurements = Measurements {
        tokens: tokens as f64,
        counts,
    };
    for key in measurements.counts.keys() {
        measurement(&measurements, key)?;
    }
    Ok(measurements)
}

fn project_export(contract_path: &Path, input: &Path, output: &Path) -> Result<usize> {
    ensure!(!output.try_exists()?, "output must be new");
    let frozen = Frozen::parse(&fs::read(contract_path)?)?;
    let contract: Value = serde_json::from_slice(&fs::read(input.join("contract.json"))?)?;
    let report: Value = serde_json::from_slice(&fs::read(input.join("report.json"))?)?;
    frozen.validate_export(&contract, &report)?;
    let documents = report["documents"].as_array().context("report documents")?;
    ensure!(!documents.is_empty(), "empty export");
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
        ensure!(entries.insert(id, entry).is_none(), "duplicate report ID");
    }
    let expected_rows_hash = digest_at(&report, "rows_sha256")?;
    let mut reader = BufReader::new(fs::File::open(input.join("rows.jsonl"))?);
    let mut line = Vec::new();
    let mut rows_hash = Sha256::new();
    let mut ids = BTreeSet::new();
    let mut numeric_bytes = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        rows_hash.update(&line);
        let row: ExportRow = serde_json::from_slice(&line).context("invalid export row")?;
        ensure!(ids.insert(row.id.clone()), "duplicate export row ID");
        let entry = entries
            .get(row.id.as_str())
            .context("row absent from completed report")?;
        let measurements = receipt_measurements(&frozen, &row, entry, input)?;
        let x = frozen.project(&measurements)?;
        serde_json::to_writer(
            &mut numeric_bytes,
            &json!({"id":row.id,"input":{"contract_sha256":frozen.hash,"x":x}}),
        )?;
        numeric_bytes.push(b'\n');
    }
    ensure!(
        hex::encode(rows_hash.finalize()) == expected_rows_hash,
        "export rows digest mismatch"
    );
    ensure!(
        ids.len() == documents.len(),
        "report/row population mismatch"
    );
    write_new(output, &numeric_bytes)?;
    Ok(ids.len())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "project_frozen_documents CONTRACT.json CFG_DOCUMENT_EXPORT_DIR NEW_NUMERIC.jsonl"
    );
    let count = project_export(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
    )?;
    eprintln!("projected {count} documents with frozen numeric parameters");
    Ok(())
}

#[cfg(test)]
#[path = "project_frozen_documents/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "project_frozen_documents/conformance.rs"]
mod conformance;
