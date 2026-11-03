//! Offline exact-record sharding and verified cfg_document_export merging.
//!
//! shard_document_exports split RECORDS MANIFEST NEW_SHARDS
//! shard_document_exports merge RECORDS MANIFEST SHARDS NEW_OUT EXPORT0 EXPORT1 EXPORT2
//!
//! Creates shard-0.jsonl through shard-2.jsonl and shards.json. Run the existing
//! cfg_document_export jsonl REPO SHARDS/shard-0.jsonl EXPORT0 [POLICY.json]
//! separately for each shard, using the same build/policy. This helper launches
//! no workers or parser; the parent controls concurrency and memory limits.
//! Start one worker; increase to at most three only after observing headroom.
//!
//! Accepts assemble_supervised_dataset's existing manifest and non-Test roster.
//! Input lines (including final-newline presence) are preserved byte for byte.
//! Merge requires three complete exports, identical contracts except input hash
//! and measured startup time, and exact source/metadata/receipt/window bindings.
//! Retains shard inputs, contracts, reports and rows; copies document artifacts
//! under new numeric prefixes compatible with prepare_supervised_forest.
//! Output contract binds the full original records SHA and all shard evidence.
//! parser_load_millis is SUMMED. Cold latency charges each shard's first document
//! its own startup; warm latency stays separate. Work sums are not wall latency.
//! No Test files, source paths from metadata, APIs or native parser are opened.
//! All outputs must be new, under Git-ignored data/. Completion report is last.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

const SHARDS: usize = 3;
const LIMIT: u64 = 256 * 1024 * 1024;
const SCHEMA: &str = "slop_ninja_document_export_shards_v1";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {key}"))
}
fn count(v: &Value, key: &str) -> Result<u64> {
    v[key]
        .as_u64()
        .with_context(|| format!("missing unsigned {key}"))
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).context("counter overflow")
}
fn bytes(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "regular file required"
    );
    let mut bytes = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= LIMIT, "file exceeds 256 MiB bound");
    Ok(bytes)
}
fn json(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice(bytes)?)
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn put(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write(path, &bytes)
}
fn id_set(values: &Value) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for v in values.as_array().context("ID array")? {
        let id = v
            .as_str()
            .filter(|s| !s.is_empty())
            .context("nonempty ID")?;
        ensure!(ids.insert(id.to_owned()), "duplicate ID");
    }
    Ok(ids)
}
fn new_output(path: &Path) -> Result<PathBuf> {
    ensure!(!path.try_exists()?, "output must be new");
    let parent = path.parent().context("output parent")?.canonicalize()?;
    let path = parent.join(path.file_name().context("output name")?);
    let git = Command::new("git")
        .arg("-C")
        .arg(&parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    ensure!(
        git.status.success(),
        "output parent must belong to a Git checkout"
    );
    let root = PathBuf::from(String::from_utf8(git.stdout)?.trim()).canonicalize()?;
    ensure!(
        path.starts_with(root.join("data")),
        "outputs belong in ignored data/"
    );
    ensure!(
        Command::new("git")
            .arg("-C")
            .arg(&parent)
            .args(["check-ignore", "--quiet", "--"])
            .arg(&path)
            .status()?
            .success(),
        "output must be Git-ignored"
    );
    Ok(path)
}

struct Dataset {
    bytes: Vec<u8>,
    manifest_bytes: Vec<u8>,
    rows: Vec<Value>,
}
impl Dataset {
    fn load(records: &Path, manifest: &Path) -> Result<Self> {
        let manifest_bytes = bytes(manifest)?;
        let m = json(&manifest_bytes)?;
        ensure!(
            m["schema"] == "slop_ninja_a_supervised_dataset_v1",
            "dataset manifest schema"
        );
        let allowed = id_set(&m["non_test_ids"])?;
        let test = id_set(&m["test_ids"])?;
        ensure!(
            allowed.is_disjoint(&test) && allowed.len() >= SHARDS && allowed.len() <= 10_000,
            "invalid non-Test roster"
        );
        let bytes = bytes(records)?;
        ensure!(
            m["records_sha256"] == sha(&bytes),
            "dataset input digest mismatch"
        );
        if let Some(hash) = m.get("input_sha256") {
            ensure!(*hash == m["records_sha256"], "conflicting input hashes");
        }
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            let row = json(line)?;
            let id = string(&row, "id")?;
            ensure!(
                allowed.contains(id) && seen.insert(id.to_owned()),
                "unlisted/duplicate/Test input ID"
            );
            let group = string(&row, "source_group")?;
            let split = string(&row, "frozen_split")?;
            ensure!(
                matches!(split, "train" | "development" | "calibration")
                    && m["split_assignments"][group] == split,
                "family split mismatch"
            );
            ensure!(
                row["text_sha256"] == sha(string(&row, "text")?.as_bytes()),
                "record text hash mismatch"
            );
            rows.push(row);
        }
        ensure!(seen == allowed, "incomplete dataset roster");
        Ok(Self {
            bytes,
            manifest_bytes,
            rows,
        })
    }
    fn partitions(&self) -> Vec<Vec<u8>> {
        let mut shards = vec![Vec::new(); SHARDS];
        for (i, line) in self.bytes.split_inclusive(|b| *b == b'\n').enumerate() {
            shards[i % SHARDS].extend_from_slice(line);
        }
        shards
    }
    fn plan(&self, shards: &[Vec<u8>]) -> Value {
        let entries: Vec<_> = shards.iter().enumerate().map(|(i, bytes)| {
            let ids: Vec<_> = self.rows.iter().skip(i).step_by(SHARDS).map(|r| r["id"].clone()).collect();
            json!({"index":i,"file":format!("shard-{i}.jsonl"),"input_sha256":sha(bytes),
                "records":ids.len(),"ids_sha256":sha(&serde_json::to_vec(&ids).expect("JSON IDs")),"ids":ids})
        }).collect();
        json!({"schema":SCHEMA,"records_sha256":sha(&self.bytes),
            "dataset_manifest_sha256":sha(&self.manifest_bytes),"records":self.rows.len(),
            "partition":"input_order_round_robin_3_exact_lines_v1","shards":entries})
    }
}

fn split(dataset: &Dataset, out: &Path) -> Result<()> {
    let shards = dataset.partitions();
    let plan = dataset.plan(&shards);
    let out = new_output(out)?;
    let stage = tempfile::tempdir_in(out.parent().context("parent")?)?;
    for (i, bytes) in shards.iter().enumerate() {
        write(&stage.path().join(format!("shard-{i}.jsonl")), bytes)?;
    }
    put(&stage.path().join("shards.json"), &plan)?;
    publish(stage, &out)
}
fn publish(stage: tempfile::TempDir, out: &Path) -> Result<()> {
    File::open(stage.path())?.sync_all()?;
    ensure!(!out.try_exists()?, "output appeared during work");
    fs::rename(stage.path(), out)?;
    File::open(out.parent().context("parent")?)?.sync_all()?;
    Ok(())
}

fn totals(entries: &[Value]) -> Result<BTreeMap<String, u64>> {
    let mut totals = BTreeMap::new();
    for e in entries {
        for (k, v) in e["summary"].as_object().context("summary object")? {
            if let Some(n) = v.as_u64() {
                let total = totals.entry(k.clone()).or_insert(0);
                *total = add(*total, n)?;
            }
        }
    }
    Ok(totals)
}
fn latency(mut values: Vec<u64>) -> Value {
    values.sort_unstable();
    let percentile = |q: usize| {
        values
            .get(values.len().saturating_sub(1) * q / 100)
            .copied()
    };
    json!({"p50":percentile(50),"p95":percentile(95)})
}
fn compatible(contract: &Value) -> Result<Value> {
    ensure!(
        contract["schema"] == "slop_ninja_document_export_contract_v1",
        "export contract schema"
    );
    for key in [
        "engine_identity",
        "parser_identity",
        "implementation_sha256",
    ] {
        string(contract, key)?;
    }
    ensure!(contract["policy"].is_object(), "missing parser policy");
    count(contract, "parser_load_millis")?;
    let mut common = contract.clone();
    let object = common.as_object_mut().context("contract object")?;
    object.remove("input_sha256");
    object.remove("parser_load_millis");
    Ok(common)
}
// Stream window files: forests can be large. Verify the exact bytes as copied.
fn copy_hashed(from: &Path, to: &Path, expected: &str) -> Result<()> {
    ensure!(
        fs::symlink_metadata(from)?.file_type().is_file(),
        "artifact must be a regular file"
    );
    let mut input = File::open(from)?;
    let mut output = OpenOptions::new().create_new(true).write(true).open(to)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
    ensure!(
        hex::encode(hash.finalize()) == expected,
        "artifact digest mismatch"
    );
    output.sync_all()?;
    Ok(())
}

fn merge(dataset: &Dataset, shard_root: &Path, out: &Path, exports: &[PathBuf]) -> Result<()> {
    ensure!(
        exports.len() == SHARDS,
        "exactly three exports required in shard order"
    );
    let partitions = dataset.partitions();
    let plan = dataset.plan(&partitions);
    let plan_bytes = bytes(&shard_root.join("shards.json"))?;
    ensure!(
        json(&plan_bytes)? == plan,
        "shard plan does not bind this exact dataset/manifest"
    );
    for i in 0..SHARDS {
        ensure!(
            bytes(&shard_root.join(format!("shard-{i}.jsonl")))? == partitions[i],
            "shard input changed"
        );
    }
    let out = new_output(out)?;
    let stage = tempfile::tempdir_in(out.parent().context("parent")?)?;
    let root = stage.path();
    write(&root.join("shards.json"), &plan_bytes)?;
    write(&root.join("dataset-manifest.json"), &dataset.manifest_bytes)?;
    let by_id: BTreeMap<_, _> = dataset
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r["id"].as_str().unwrap(), i))
        .collect();
    let mut common = None;
    let mut merged_contract = Value::Null;
    let mut evidence = Vec::new();
    let mut startup_sum = 0u64;
    let mut entries = BTreeMap::new();
    let mut row_lines = BTreeMap::new();
    let mut cold = Vec::new();
    let mut first_documents = Vec::new();
    for (shard, export) in exports.iter().enumerate() {
        let contract_bytes = bytes(&export.join("contract.json"))?;
        let report_bytes = bytes(&export.join("report.json"))?;
        let row_bytes = bytes(&export.join("rows.jsonl"))?;
        let contract = json(&contract_bytes)?;
        let report = json(&report_bytes)?;
        let identity = compatible(&contract)?;
        if let Some(prior) = &common {
            ensure!(
                *prior == identity,
                "shard engine/parser/policy/implementation contract differs"
            );
        } else {
            common = Some(identity);
            merged_contract = contract.clone();
        }
        ensure!(
            contract["input_sha256"] == plan["shards"][shard]["input_sha256"],
            "export/shard input mismatch"
        );
        ensure!(
            report["schema"] == "slop_ninja_document_export_report_v1"
                && report["complete"] == true,
            "incomplete export"
        );
        ensure!(
            report["engine_identity"] == contract["engine_identity"]
                && report["parser_load_millis"] == contract["parser_load_millis"],
            "report/contract identity or startup mismatch"
        );
        ensure!(
            report["excluded_reserved_test_documents"] == 0,
            "unexpected excluded input records"
        );
        ensure!(
            report["rows_sha256"] == sha(&row_bytes),
            "export rows digest mismatch"
        );
        let startup = count(&contract, "parser_load_millis")?;
        startup_sum = add(startup_sum, startup)?;
        let documents = report["documents"].as_array().context("report documents")?;
        ensure!(
            report["totals"] == json!(totals(documents)?),
            "shard report totals mismatch"
        );
        let latencies = documents
            .iter()
            .map(|e| count(e, "elapsed_micros"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            report["document_latency_micros"] == latency(latencies),
            "shard latency mismatch"
        );
        let expected_ids = id_set(&plan["shards"][shard]["ids"])?;
        let mut report_ids = BTreeSet::new();
        let mut prefixes = BTreeSet::new();
        let mut report_entries = BTreeMap::new();
        for e in documents {
            let id = string(e, "id")?;
            let prefix = string(e, "prefix")?;
            ensure!(
                prefix.len() <= 20
                    && prefix.bytes().all(|b| b.is_ascii_digit())
                    && prefixes.insert(prefix),
                "invalid/duplicate artifact prefix"
            );
            ensure!(report_ids.insert(id.to_owned()), "duplicate report ID");
            report_entries.insert(id, e);
        }
        ensure!(report_ids == expected_ids, "report/shard roster mismatch");
        let mut seen = BTreeSet::new();
        for (position, line) in row_bytes.split_inclusive(|b| *b == b'\n').enumerate() {
            let row = json(line)?;
            let id = string(&row, "id")?;
            ensure!(
                expected_ids.contains(id) && seen.insert(id.to_owned()),
                "duplicate/unlisted export row"
            );
            // cfg_document_export emits rows in input order; this also pins which
            // document bears each worker's parser startup charge.
            ensure!(
                plan["shards"][shard]["ids"][position] == id && documents[position]["id"] == id,
                "export order differs from shard"
            );
            let index = *by_id.get(id).context("unknown exported ID")?;
            let original = &dataset.rows[index];
            ensure!(
                row["metadata"] == *original,
                "export metadata differs from exact input record"
            );
            let receipt = &row["receipt"];
            let e = report_entries[id];
            ensure!(
                receipt["schema"] == "slop_ninja_cfg_document_fallback_v1"
                    && receipt["engine_identity"] == contract["engine_identity"]
                    && receipt["parser_identity"] == contract["parser_identity"],
                "receipt engine/parser mismatch"
            );
            ensure!(
                receipt["source_sha256"] == original["text_sha256"],
                "receipt text digest mismatch"
            );
            ensure!(
                receipt["summary"] == e["summary"]
                    && row["total_elapsed_micros"] == e["elapsed_micros"],
                "row/receipt/report summary or latency mismatch"
            );
            let text = string(original, "text")?;
            ensure!(
                count(&receipt["summary"], "input_bytes")? == text.len() as u64
                    && receipt["summary"]["accounted_bytes"] == receipt["summary"]["input_bytes"],
                "incomplete document byte accounting"
            );
            let old = string(e, "prefix")?;
            let new = format!("{index:05}");
            let receipt_bytes = bytes(&export.join(format!("{old}.receipt.json")))?;
            ensure!(
                sha(&receipt_bytes) == string(&row, "receipt_sha256")?
                    && json(&receipt_bytes)? == *receipt,
                "receipt file mismatch"
            );
            let source_bytes = bytes(&export.join(format!("{old}.source.json")))?;
            let source = json(&source_bytes)?;
            ensure!(
                source["id"] == id
                    && source["text"] == original["text"]
                    && source["metadata"] == *original,
                "source artifact mismatch"
            );
            write(&root.join(format!("{new}.source.json")), &source_bytes)?;
            write(&root.join(format!("{new}.receipt.json")), &receipt_bytes)?;
            copy_hashed(
                &export.join(format!("{old}.windows.jsonl")),
                &root.join(format!("{new}.windows.jsonl")),
                string(&row, "windows_sha256")?,
            )?;
            let mut entry = e.clone();
            entry["prefix"] = json!(new);
            let elapsed = count(e, "elapsed_micros")?;
            let load_us = startup.checked_mul(1000).context("startup overflow")?;
            cold.push(add(elapsed, if position == 0 { load_us } else { 0 })?);
            if position == 0 {
                first_documents.push(json!({"shard":shard,"id":id,"parser_load_millis":startup,"first_inference_micros":add(elapsed,load_us)?}));
            }
            ensure!(
                entries.insert(index, entry).is_none()
                    && row_lines.insert(index, line.to_vec()).is_none(),
                "row repeated across shards"
            );
        }
        ensure!(seen == expected_ids, "incomplete shard rows");
        for (suffix, content) in [
            ("contract.json", &contract_bytes),
            ("report.json", &report_bytes),
            ("rows.jsonl", &row_bytes),
            ("input.jsonl", &partitions[shard]),
        ] {
            write(&root.join(format!("shard-{shard}.{suffix}")), content)?;
        }
        evidence.push(json!({"shard":shard,"input_sha256":sha(&partitions[shard]),
            "contract_file":format!("shard-{shard}.contract.json"),"contract_sha256":sha(&contract_bytes),
            "report_file":format!("shard-{shard}.report.json"),"report_sha256":sha(&report_bytes),
            "rows_file":format!("shard-{shard}.rows.jsonl"),"rows_sha256":sha(&row_bytes),
            "parser_load_millis":startup,"documents":seen.len()}));
    }
    ensure!(
        entries.len() == dataset.rows.len(),
        "merged roster incomplete"
    );
    let entries: Vec<_> = entries.into_values().collect();
    let mut output_rows = Vec::new();
    for line in row_lines.into_values() {
        output_rows.extend_from_slice(&line);
        if !line.ends_with(b"\n") {
            output_rows.push(b'\n');
        }
    }
    write(&root.join("rows.jsonl"), &output_rows)?;
    let warm = entries
        .iter()
        .map(|e| count(e, "elapsed_micros"))
        .collect::<Result<Vec<_>>>()?;
    let document_work = warm.iter().try_fold(0, |a, b| add(a, *b))?;
    let startup_us = startup_sum.checked_mul(1000).context("startup overflow")?;
    merged_contract["input_sha256"] = json!(sha(&dataset.bytes));
    merged_contract["parser_load_millis"] = json!(startup_sum);
    merged_contract["merge"] = json!({"schema":SCHEMA,"dataset_manifest_sha256":sha(&dataset.manifest_bytes),
        "records_sha256":sha(&dataset.bytes),"shards_manifest_sha256":sha(&plan_bytes),"shards":evidence,
        "implementation_sha256":sha(include_bytes!("shard_document_exports.rs")),
        "non_test_roster_verified":true,"startup_accounting":"sum_of_all_shard_parser_loads; not parallel wall time"});
    put(&root.join("contract.json"), &merged_contract)?;
    put(
        &root.join("report.json"),
        &json!({"schema":"slop_ninja_document_export_report_v1",
        "engine_identity":merged_contract["engine_identity"],"parser_load_millis":startup_sum,
        "documents":entries,"totals":totals(&entries)?,"document_latency_micros":latency(warm),
        "startup_inclusive_document_latency_micros":latency(cold),"shard_first_inference":first_documents,
        "document_work_micros":document_work,"startup_inclusive_work_micros":add(document_work,startup_us)?,
        "parallel_wall_elapsed_micros":null,"excluded_reserved_test_documents":0,
        "rows_sha256":sha(&output_rows),"contract_sha256":sha(&bytes(&root.join("contract.json"))?),
        "complete":true}),
    )?;
    publish(stage, &out)
}

fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        (a.len() == 4 && a[0] == "split") || (a.len() == 8 && a[0] == "merge"),
        "shard_document_exports split RECORDS MANIFEST NEW_SHARDS\nshard_document_exports merge RECORDS MANIFEST SHARDS NEW_OUT EXPORT0 EXPORT1 EXPORT2"
    );
    let dataset = Dataset::load(Path::new(&a[1]), Path::new(&a[2]))?;
    if a[0] == "split" {
        split(&dataset, Path::new(&a[3]))?;
    } else {
        merge(
            &dataset,
            Path::new(&a[3]),
            Path::new(&a[4]),
            &a[5..].iter().map(PathBuf::from).collect::<Vec<_>>(),
        )?;
    }
    println!(
        "{}",
        json!({"command":a[0],"records":dataset.rows.len(),"shards":SHARDS,"complete":true})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partitions_preserve_exact_bytes_and_final_newline() {
        let d = Dataset {
            bytes: b"{\"id\":\"a\"}\r\n{\"id\":\"b\"}\n{\"id\":\"c\"}\n{\"id\":\"d\"}".to_vec(),
            manifest_bytes: vec![],
            rows: vec![],
        };
        let p = d.partitions();
        assert_eq!(p[0], b"{\"id\":\"a\"}\r\n{\"id\":\"d\"}");
        assert_eq!(p[1], b"{\"id\":\"b\"}\n");
        assert_eq!(p[2], b"{\"id\":\"c\"}\n");
    }
    #[test]
    fn identity_changes_are_not_erased_with_per_shard_measurements() {
        let c = json!({"schema":"slop_ninja_document_export_contract_v1","engine_identity":"e",
            "parser_identity":"p","policy":{},"implementation_sha256":"i","input_sha256":"a","parser_load_millis":4});
        let mut other = c.clone();
        other["input_sha256"] = json!("b");
        other["parser_load_millis"] = json!(7);
        assert_eq!(compatible(&c).unwrap(), compatible(&other).unwrap());
        for key in [
            "engine_identity",
            "parser_identity",
            "implementation_sha256",
            "policy",
        ] {
            let mut changed = other.clone();
            changed[key] = if key == "policy" {
                json!({"changed":true})
            } else {
                json!("different")
            };
            assert_ne!(compatible(&c).unwrap(), compatible(&changed).unwrap());
        }
    }
    #[test]
    fn tampered_window_bytes_rejected_and_latency_recomputed() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("windows.jsonl");
        fs::write(&input, b"changed\n").unwrap();
        assert!(copy_hashed(&input, &dir.path().join("copy"), &sha(b"original\n")).is_err());
        assert_eq!(latency(vec![30, 10, 20]), json!({"p50":20,"p95":20}));
        assert!(add(u64::MAX, 1).is_err());
    }
}
