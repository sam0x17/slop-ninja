//! Offline verification of the receipt evidence in a private cohort export.
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Component, Path};

use serde_json::{Value, json};
use slop_ninja_coordinator::eval::{plan::CohortPlan, receipt_format, receipts};
use slop_ninja_protocol::receipt_chunks::{
    CHUNK_ENTRIES, ReceiptIndexBuilder, ReceiptIndexChunk, ReceiptIndexCommitment,
};
use slop_ninja_protocol::{
    AttemptReceipt, CohortManifestV2, ReceiptIndex, ReceiptIndexBinding, ReceiptIndexEntry,
    Record, ScorePackageV2, Signed, TrustedKeys, object_sha256,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn read(dir: &Path, relative: &str) -> Result<Vec<u8>> {
    let suffix = Path::new(relative);
    if relative.is_empty()
        || !suffix
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err("invalid export object path".into());
    }
    let file = dir.join(suffix);
    if file.canonicalize()? != file || !file.is_file() {
        return Err("object outside export or not a regular file".into());
    }
    Ok(fs::read(file)?)
}

fn key(v: &Value) -> Result<[u8; 32]> {
    let key = v.as_str().ok_or("missing trusted public key")?;
    Ok(hex::decode(key.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| "public key size")?)
}

fn check_descriptor(obj: &Value, key: &str, bytes: &[u8]) -> Result<()> {
    if obj["key"].as_str() != Some(key)
        || obj["bytes"].as_u64() != Some(bytes.len() as u64)
        || obj["sha256"] != object_sha256(bytes).to_string()
    {
        return Err("object key, digest or length differs from its descriptor".into());
    }
    Ok(())
}

// Keep only the compact entry from each decoded receipt. Read the job bundles
// one at a time so their complete bodies need not coexist in memory.
fn entries(
    dir: &Path,
    label: &str,
    map: &Value,
    binding: ReceiptIndexBinding,
) -> Result<Vec<ReceiptIndexEntry>> {
    if map["format"] != receipts::CHUNK_FORMAT
        || map["cohort_manifest"] != binding.cohort_manifest.to_string()
        || map["receipt_root"] != binding.root.to_string()
        || map["receipt_count"].as_u64() != Some(binding.count)
    {
        return Err("chunk map differs from signed index".into());
    }
    let prefix = format!("cohorts/{label}/");
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for obj in map["objects"]
        .as_array()
        .ok_or("missing receipt bundle descriptors")?
    {
        let key = obj["key"].as_str().ok_or("missing receipt bundle key")?;
        let relative = key
            .strip_prefix(&prefix)
            .ok_or("receipt bundle outside cohort")?;
        if !relative.starts_with("receipts/") || !seen.insert(relative) {
            return Err("invalid or duplicate receipt bundle key".into());
        }
        let bytes = read(dir, relative)?;
        check_descriptor(obj, key, &bytes)?;
        let records = receipts::decode_chunk(&bytes).map_err(|e| e.0)?;
        if obj["receipts"].as_u64() != Some(records.len() as u64) {
            return Err("receipt bundle count differs".into());
        }
        for raw in records {
            let r = AttemptReceipt::from_bytes(raw)?;
            rows.push(ReceiptIndexEntry {
                attempt: r.attempt,
                outcome: r.outcome,
                object_sha256: object_sha256(raw),
                object_len: raw.len().try_into()?,
            });
        }
        if rows.len() as u64 > binding.count {
            return Err("too many receipts".into());
        }
    }
    if rows.len() as u64 != binding.count {
        return Err("missing receipts".into());
    }
    rows.sort_unstable_by_key(|e| e.attempt);
    if rows.windows(2).any(|w| w[0].attempt == w[1].attempt) {
        return Err("duplicate receipt attempt".into());
    }
    Ok(rows)
}

fn check_chunks(
    dir: &Path,
    label: &str,
    map: &Value,
    index: &ReceiptIndexCommitment,
    rows: &[ReceiptIndexEntry],
) -> Result<()> {
    let relative = "receipts/index-v2";
    if map["index_chunks"]
        != json!({"format":"receipt-index-chunk/v1","prefix":format!("cohorts/{label}/{relative}/"),
        "count":index.chunk_count(),"key_suffix":"{ordinal:016x}-{sha256}.bin"})
    {
        return Err("invalid index chunk discovery metadata".into());
    }
    let mut builder = ReceiptIndexBuilder::new(index.cohort_manifest);
    for (ordinal, part) in rows.chunks(CHUNK_ENTRIES).enumerate() {
        let chunk = ReceiptIndexChunk {
            cohort_manifest: index.cohort_manifest,
            ordinal: ordinal.try_into()?,
            entries: part.to_vec(),
        };
        let bytes = chunk.to_bytes()?;
        let key = format!("{relative}/{ordinal:016x}-{}.bin", object_sha256(&bytes));
        if read(dir, &key)? != bytes {
            return Err("stored index chunk differs from decoded receipts".into());
        }
        builder.push(&chunk)?;
    }
    builder.verify(index)?;
    Ok(())
}

pub(super) fn audit(pos: &[String]) -> Result<Value> {
    let [kind, directory, label, trust] = pos else {
        return Err("receipt-audit cohort|probe DIR LABEL TRUST.json".into());
    };
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("invalid cohort label".into());
    }
    let dir = Path::new(directory).canonicalize()?;
    let trust: Value = serde_json::from_slice(&fs::read(trust)?)?;
    let keys = TrustedKeys::new(
        key(&trust["package_public_key"])?,
        key(&trust["receipt_public_key"])?,
        key(&trust["authorization_public_key"])?,
    )?;
    let signed = read(&dir, "receipt-index.signed")?;
    let map: Value = serde_json::from_slice(&read(&dir, "chunk-map.json")?)?;
    let (binding, format) = match kind.as_str() {
        "cohort" => {
            let m = Signed::<CohortManifestV2>::open(&read(&dir, "manifest.signed")?, &keys)?;
            let p = Signed::<ScorePackageV2>::open(&read(&dir, "package.signed")?, &keys)?;
            p.check_manifest(&m)?;
            if m.cohort_label != *label || map.get("probe_binding").is_some() {
                return Err("cohort identity".into());
            }
            // Only absence selects the legacy reader. A broken link or bad
            // declaration is an error, including when a v1 index is present.
            let format = match fs::symlink_metadata(dir.join(receipt_format::OBJECT)) {
                Ok(_) => {
                    let record = serde_json::from_slice(&read(&dir, receipt_format::OBJECT)?)?;
                    receipt_format::verify(&record, m.limits)
                        .map_err(|_| "manifest receipt format")?;
                    let index = Signed::<ReceiptIndexCommitment>::open(&signed, &keys)?;
                    let b = index.binding()?;
                    b.check(&p)?;
                    check_chunks(&dir, label, &map, &index, &entries(&dir, label, &map, b)?)?;
                    "chunked-v2"
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let index = Signed::<ReceiptIndex>::open(&signed, &keys)?;
                    p.check_receipts(&index)?;
                    let b = ReceiptIndexBinding {
                        cohort_manifest: index.cohort_manifest,
                        root: index.digest()?,
                        count: index.count(),
                    };
                    if map.get("index_chunks").is_some()
                        || entries(&dir, label, &map, b)? != index.entries
                    {
                        return Err("flat index differs from stored receipts".into());
                    }
                    "legacy-v1"
                }
                Err(e) => return Err(e.into()),
            };
            (
                ReceiptIndexBinding {
                    cohort_manifest: p.cohort_manifest,
                    root: p.receipt_root,
                    count: p.receipt_count,
                },
                format,
            )
        }
        "probe" => {
            let index = Signed::<ReceiptIndexCommitment>::open(&signed, &keys)?;
            let base = "receipts/index-v2";
            let plan_bytes = read(&dir, &format!("{base}/probe-plan.json"))?;
            let record_bytes = read(&dir, &format!("{base}/receipt-format.json"))?;
            check_descriptor(
                &map["probe_binding"]["plan"],
                &format!("cohorts/{label}/{base}/probe-plan.json"),
                &plan_bytes,
            )?;
            check_descriptor(
                &map["probe_binding"]["limits"],
                &format!("cohorts/{label}/{base}/receipt-format.json"),
                &record_bytes,
            )?;
            let record = serde_json::from_slice(&record_bytes)?;
            receipt_format::verify_probe(&plan_bytes, index.cohort_manifest, &record)
                .map_err(|_| "probe receipt format")?;
            let plan: CohortPlan = serde_json::from_slice(&plan_bytes)?;
            if plan.label != *label {
                return Err("probe label differs from signed plan".into());
            }
            let b = index.binding()?;
            check_chunks(&dir, label, &map, &index, &entries(&dir, label, &map, b)?)?;
            (b, "chunked-v2")
        }
        _ => return Err("receipt-audit mode must be cohort or probe".into()),
    };
    Ok(
        json!({"verified":true,"mode":kind,"label":label,"receipt_format":format,
        "receipt_root":binding.root.to_string(),"receipt_count":binding.count}),
    )
}
