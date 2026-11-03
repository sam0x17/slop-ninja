//! Diagnostic full-A bundle from the exposed stage-3 Train/Development/CAL populations.
//! Usage: assemble_full_a_existing STAGE3_DIR PROTOCOL.json NEW_BUNDLE_DIR
//!
//! Reads (never writes) the frozen stage-3 last-block bundle for routing,
//! targets and roles, the exact request texts and the source records' measured
//! replacement spans, and emits `slop_ninja_full_a_dataset_v1` for
//! `train_full_a.py`. Rows, targets, families, roster and Train-only V6 h
//! normalization are reused unchanged; only exact text and
//! `targets.ai_byte_spans` are added. Test is not read. The stage-3
//! Development and CAL populations are already exposed, so results on them
//! are engineering diagnostics, never selection evidence.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const PARTS: [&str; 3] = ["train", "development", "calibration"];

fn hex_sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex_sha(&fs::read(p).with_context(|| format!("read {}", p.display()))?))
}
fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}
fn jsonl(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)
        .with_context(|| format!("read {}", p.display()))?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
fn canonical(v: &Value) -> Result<Vec<u8>> {
    // serde_json::Value objects are BTreeMap-backed here: sorted, compact.
    let mut b = serde_json::to_vec(v)?;
    b.push(b'\n');
    Ok(b)
}

/// Validates one measured replacement span against the exact final text and
/// the recorded realized share (whitespace words inside / whitespace words).
pub(crate) fn span_for(text: &str, record: &Value, share: Option<f64>) -> Result<Vec<[usize; 2]>> {
    let e = &record["share_evidence"];
    ensure!(
        e["schema"] == "slop_ninja_realized_span_share_v1",
        "mixed row lacks a measured span record"
    );
    ensure!(
        e["final_sha256"].as_str() == Some(hex_sha(text.as_bytes()).as_str()),
        "span evidence is bound to different final text"
    );
    let s = e["final_byte_span"].as_array().context("final_byte_span")?;
    ensure!(s.len() == 2, "span shape");
    let (a, b) = (
        s[0].as_u64().context("span start")? as usize,
        s[1].as_u64().context("span end")? as usize,
    );
    ensure!(
        a < b && b <= text.len() && text.is_char_boundary(a) && text.is_char_boundary(b),
        "span outside text or splits a UTF-8 character"
    );
    let inside = text[a..b].split_whitespace().count();
    let total = text.split_whitespace().count();
    ensure!(inside > 0 && total > 0, "span covers no words");
    let realized = inside as f64 / total as f64;
    ensure!(
        e["realized_share"].as_f64().is_some_and(|r| (r - realized).abs() < 1e-12),
        "recorded realized share differs from span word count"
    );
    if let Some(y) = share {
        ensure!((y - realized).abs() < 1e-12, "share target differs from span");
    }
    Ok(vec![[a, b]])
}

fn run(stage: &Path, protocol_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new bundle directory required");
    let fb = stage.join("fit-bundle-v1");
    let fm = read(&fb.join("manifest.json"))?;
    ensure!(
        fm["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && fm["complete"] == true
            && fm["final_test_opened"] == false
            && fm["contract_sha256"].as_str() == Some(sha(&fb.join("contract.json"))?.as_str()),
        "frozen stage-3 non-Test bundle required"
    );
    for part in PARTS {
        let name = format!("{part}.jsonl");
        ensure!(
            fm["files_sha256"][&name].as_str() == Some(sha(&fb.join(&name))?.as_str()),
            "stage-3 {name} changed"
        );
    }
    let old = read(&fb.join("contract.json"))?;
    ensure!(
        old["schema"] == "slop_ninja_last_block_a_contract_v1"
            && old["input_mode"] == "text"
            && old["raw_dimension"] == 768
            && old["zero_cfg_coordinates"] == 1820
            && old["maximum_tokens_including_specials"] == 2048
            && old["token_truncation"] == false
            && old["origin_order"] == json!(["human_only", "model_only", "mixed"]),
        "unexpected stage-3 contract meaning"
    );
    let norm = &old["embedding_normalization"];
    ensure!(
        norm["fit"] == "train_query_only_equal_source_family_weight"
            && norm["dimension"] == 768
            && norm["calibration_used_for_fit"] == false
            && norm["development_used_for_fit"] == false
            && norm["final_test_opened"] == false
            && norm["fit_rows"] == fm["populations"]["train"]["rows"],
        "stage-3 normalization must be Train-query-only"
    );

    // Exact texts: stage-3 cohort requests plus retained Train requests.
    let mut texts = BTreeMap::<String, String>::new();
    let retained = stage.join("retained-train-requests-v1");
    let rm = read(&retained.join("manifest.json"))?;
    ensure!(
        rm["schema"] == "slop_ninja_retained_train_encoder_requests_v1"
            && rm["complete"] == true
            && rm["test_text_opened"] == false
            && rm["requests_sha256"].as_str() == Some(sha(&retained.join("requests.jsonl"))?.as_str()),
        "retained Train requests changed"
    );
    let dataset = stage.join("dataset-v1");
    let request_files = [dataset.join("requests.jsonl"), retained.join("requests.jsonl")];
    let mut record_files = vec![dataset.join("records.jsonl")];
    for s in rm["sources"].as_array().context("retained sources")? {
        let dir = PathBuf::from(string(s, "path")?);
        let records = dir.join("records.jsonl");
        ensure!(
            s["records_sha256"].as_str() == Some(sha(&records)?.as_str()),
            "retained source records changed"
        );
        record_files.push(records);
    }
    for path in &request_files {
        for r in jsonl(path)? {
            let id = string(&r, "id")?.to_owned();
            ensure!(
                texts.insert(id, string(&r, "text")?.to_owned()).is_none(),
                "duplicate request id"
            );
        }
    }
    let mut records = BTreeMap::<String, Value>::new();
    for path in &record_files {
        for r in jsonl(path)? {
            ensure!(
                r["frozen_split"] != "test",
                "a Test record appeared in a non-Test source"
            );
            let id = string(&r, "id")?.to_owned();
            ensure!(records.insert(id, r).is_none(), "duplicate record id");
        }
    }

    let protocol_bytes = fs::read(protocol_path)?;
    let protocol: Value = serde_json::from_slice(&protocol_bytes)?;
    ensure!(
        protocol["schema"] == "slop_ninja_full_a_protocol_v1"
            && protocol["status"] == "frozen"
            && protocol["final_test_opened"] == false,
        "frozen full-A protocol required"
    );

    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut stats = BTreeMap::<&str, Value>::new();
    let mut families = BTreeMap::<&str, BTreeSet<String>>::new();
    for part in PARTS {
        let mut buf = Vec::new();
        let (mut spans, mut full, mut human, mut model) = (0usize, 0usize, 0usize, 0usize);
        let mut ids = BTreeSet::new();
        for row in jsonl(&fb.join(format!("{part}.jsonl")))? {
            let id = string(&row, "id")?;
            ensure!(
                row["frozen_split"] == part && row["purpose"] == "query" && ids.insert(id.to_owned()),
                "row routing"
            );
            let text = texts.get(id).with_context(|| format!("no exact text for {part} row"))?;
            ensure!(
                row["text_sha256"].as_str() == Some(hex_sha(text.as_bytes()).as_str()),
                "exact text hash differs from the frozen row"
            );
            let record = records.get(id).context("no source record")?;
            ensure!(
                record["text"].as_str() == Some(text.as_str())
                    && record["frozen_split"] == part
                    && record["source_group"] == row["source_group"],
                "source record disagrees with frozen routing"
            );
            let targets = &row["targets"];
            let origin = targets["origin"].as_u64().context("origin target")?;
            let share = targets["share"].as_f64();
            let byte_spans = match origin {
                0 => {
                    human += 1;
                    ensure!(share == Some(0.), "human share");
                    Value::Null
                }
                1 => {
                    model += 1;
                    ensure!(share == Some(1.), "model-only share");
                    Value::Null
                }
                2 => {
                    let s = span_for(text, record, share)?;
                    spans += 1;
                    if s == [[0, text.len()]] {
                        full += 1;
                    }
                    json!(s)
                }
                _ => bail!("origin outside roster"),
            };
            families.entry(part).or_default().insert(string(&row, "source_group")?.to_owned());
            let mut t = targets.as_object().context("targets")?.clone();
            t.insert("ai_byte_spans".into(), byte_spans);
            let mut out_row = json!({
                "id": id, "source_group": row["source_group"], "frozen_split": part, "purpose": "query",
                "text": text, "text_sha256": row["text_sha256"], "roles": row["roles"],
                "targets": Value::Object(t), "cohort": row["cohort"],
                "stage3_normalized_text_sha256": row["normalized_text_sha256"],
            });
            if let Some(s) = row.get("acquisition_stratum") {
                out_row["acquisition_stratum"] = s.clone();
            }
            buf.extend(canonical(&out_row)?);
        }
        stats.insert(part, json!({"rows": ids.len(), "families": families[part].len(), "human_only": human,
            "model_only": model, "mixed_with_measured_span": spans, "mixed_full_text_span": full}));
        files.insert(format!("{part}.jsonl"), buf);
    }
    ensure!(
        families["train"].is_disjoint(&families["development"])
            && families["train"].is_disjoint(&families["calibration"])
            && families["development"].is_disjoint(&families["calibration"]),
        "source-family leakage across splits"
    );

    let mut normalization = norm.as_object().context("normalization")?.clone();
    normalization.insert("reference_used_for_fit".into(), json!(false));
    normalization.insert(
        "provenance".into(),
        json!("reused unchanged from the stage-3 last-block contract: Train-query-only family-weighted mean/scale of the frozen V6 h"),
    );
    let contract = json!({
        "schema": "slop_ninja_full_a_contract_v1", "parent_artifact_id": old["parent_artifact_id"],
        "input_mode": "text", "raw_dimension": 768, "zero_cfg_coordinates": 1820,
        "maximum_tokens_including_specials": 2048, "token_truncation": false,
        "origin_order": ["human_only", "model_only", "mixed"],
        "span_label_policy": "slop_ninja_token_span_label_v1",
        "span_coordinates": "utf8_byte_offsets_in_exact_final_text",
        "human_profiles": old["human_profiles"], "ai_profiles": old["ai_profiles"],
        "embedding_normalization": Value::Object(normalization),
        "learned_input_exclusions": old["learned_input_exclusions"],
        "source_contract_sha256": fm["contract_sha256"],
        "diagnostic_scope": "stage-3 Development and CAL are already exposed; results are engineering diagnostics only",
    });
    files.insert("contract.json".into(), canonical(&contract)?);
    files.insert("protocol.json".into(), protocol_bytes);
    fs::create_dir(out)?;
    let mut digest = BTreeMap::new();
    for (name, bytes) in &files {
        let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out.join(name))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        digest.insert(name.clone(), hex_sha(bytes));
    }
    let manifest = json!({
        "schema": "slop_ninja_full_a_dataset_v1", "complete": true, "final_test_opened": false,
        "test_used_for_selection": false, "diagnostic_only": true,
        "exposure": "stage-3 Development and CAL were inspected during stage 3; never selection evidence for a new Test",
        "contract_sha256": digest["contract.json"], "protocol_sha256": digest["protocol.json"],
        "files_sha256": PARTS.iter().map(|p| { let n = format!("{p}.jsonl"); (n.clone(), json!(digest[&n])) }).collect::<serde_json::Map<_,_>>(),
        "populations": stats,
        "inputs": {"stage3_bundle_manifest_sha256": sha(&fb.join("manifest.json"))?,
                   "retained_requests_manifest_sha256": sha(&retained.join("manifest.json"))?,
                   "stage3_dataset_manifest_sha256": sha(&dataset.join("manifest.json"))?,
                   "record_files": record_files.iter().map(|p| Ok(json!({"path": p, "sha256": sha(p)?}))).collect::<Result<Vec<_>>>()?},
        "span_source": "share_evidence.final_byte_span (slop_ninja_realized_span_share_v1), checked against exact final text and realized share",
        "implementation_sha256": hex_sha(include_bytes!("assemble_full_a_existing.rs")),
    });
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out.join("manifest.json"))?;
    serde_json::to_writer_pretty(&mut f, &manifest)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"complete": true, "populations": manifest["populations"]}));
    Ok(())
}

fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "STAGE3_DIR PROTOCOL.json NEW_BUNDLE_DIR");
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(text: &str, a: usize, b: usize, realized: f64) -> Value {
        json!({"share_evidence": {"schema": "slop_ninja_realized_span_share_v1",
            "final_sha256": hex_sha(text.as_bytes()), "final_byte_span": [a, b], "realized_share": realized}})
    }
    #[test]
    fn span_checks_text_binding_boundaries_and_share() {
        let text = "one two “three” four";
        let a = text.find('“').unwrap();
        let b = a + "“three”".len();
        assert_eq!(span_for(text, &record(text, a, b, 0.25), Some(0.25)).unwrap(), vec![[a, b]]);
        assert!(span_for(text, &record(text, a + 1, b, 0.25), None).is_err());
        assert!(span_for(text, &record(text, a, b, 0.5), None).is_err());
        assert!(span_for(text, &record(text, a, b, 0.25), Some(0.3)).is_err());
        assert!(span_for("other text", &record(text, a, b, 0.25), None).is_err());
    }
}
