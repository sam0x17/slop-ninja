//! Separate frozen reference preparation; never fits or changes a training file.
//! select QUERY_MANIFEST REFERENCES.jsonl NEW_OUT
//! transform FROZEN_NUMERIC REF_EXPORT REF_MANIFEST NEW_OUT
//! encode FROZEN_NUMERIC REF_NUMERIC WEIGHTS.json NEW_BUNDLE.json
//! select reads only the digest-bound 20 non-Test excerpt references, not books.
//! transform reuses the identical CFG numeric contract (all or leaf_grammar).
//! encode uses SharedModel's normal frozen encoder; no optimizer or calibration.
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::shared::{SharedInput, SharedModel};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
#[allow(dead_code, unused_imports)]
#[path = "prepare_supervised_forest/pipeline.rs"]
mod pipeline;

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn read(p: &Path) -> Result<(Vec<u8>, Value)> {
    let b = fs::read(p)?;
    let v = serde_json::from_slice(&b)?;
    Ok((b, v))
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {k}"))
}
fn jsonl(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn selected_references(query: &Value, records: &[Value]) -> Result<Vec<Value>> {
    ensure!(
        query["schema"] == "slop_ninja_author_queries_v2"
            && query["mode"] == "fresh-balanced"
            && query["test_mode"] == false
            && query["test_text_opened"] == false,
        "non-Test fresh reference manifest required"
    );
    let expected = query["emitted_reference_ids"]
        .as_array()
        .context("reference IDs")?
        .iter()
        .map(|v| Ok(v.as_str().context("ID")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut seen = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut selected = Vec::new();
    for row in records {
        let id = text(row, "id")?;
        let part = text(row, "frozen_split")?;
        ensure!(
            matches!(part, "train" | "development" | "calibration")
                && row["purpose"] == "reference"
                && seen.insert(id.to_owned()),
            "Test/query/duplicate reference"
        );
        ensure!(
            row["origin"] == "human_only"
                && row["human_role_present"] == true
                && row["ai_role_present"] == false
                && row["ai_identity"].is_null()
                && row["parent_id"].is_null(),
            "human reference roles"
        );
        ensure!(
            row["text_sha256"] == sha(text(row, "text")?.as_bytes())
                && row["source"]["source_manifest_sha256"] == query["source_manifest_sha256"],
            "reference source/hash binding"
        );
        let source = &row["source"]["record"];
        ensure!(
            row["human_identity"] == source["author_key"]
                && row["rights"] == source["rights"]
                && row["rights"]["commercial_training"] == true
                && row["rights"]["external_evaluation"] == true,
            "reference identity/rights binding"
        );
        ensure!(
            source["purpose"] == "reference"
                && text(source, "preassigned_partition")?.strip_suffix("_reference") == Some(part)
                && row["source_group"] == source["source_group"],
            "reference source partition"
        );
        if let Some(old) = groups.insert(text(row, "source_group")?.to_owned(), part.to_owned()) {
            ensure!(old == part, "reference group crosses partitions");
        }
        let planned = query["planned_records"]
            .as_array()
            .context("planned references")?
            .iter()
            .find(|v| v["id"] == id)
            .context("unplanned reference")?;
        ensure!(
            planned["purpose"] == "reference"
                && planned["frozen_split"] == part
                && planned["source_group"] == row["source_group"],
            "planned reference differs"
        );
        *counts.entry(part.into()).or_default() += 1;
        if part == "train" {
            ensure!(
                source["enrollment"] == "known",
                "Train reference must be known"
            );
        } else {
            ensure!(
                source["enrollment"] == "withheld",
                "partition reference must be withheld"
            );
            selected.push(row.clone());
        }
    }
    ensure!(
        seen == expected
            && records.len() == 20
            && counts.get("train") == Some(&12)
            && counts.get("development") == Some(&4)
            && counts.get("calibration") == Some(&4),
        "expected 12 Train and 4 references per evaluation partition"
    );
    selected.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(selected)
}
fn select(query_path: &Path, refs: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let (qb, q) = read(query_path)?;
    let b = fs::read(refs)?;
    ensure!(q["references_sha256"] == sha(&b), "reference file changed");
    let selected = selected_references(&q, &rows(&b)?)?;
    let selected_bytes = jsonl(&selected)?;
    let assignments = selected
        .iter()
        .map(|r| {
            (
                text(r, "source_group").unwrap().to_owned(),
                r["frozen_split"].clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","reference_transform_only":true,"records_sha256":sha(&selected_bytes),"non_test_ids":selected.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":q["test_ids"],"split_assignments":assignments,"sources":[{"path":query_path,"sha256":sha(&qb)},{"path":refs,"sha256":sha(&b)}],"source_manifest_sha256":q["source_manifest_sha256"],"references":8,"fit_performed":false,"test_text_opened":false,"policy":"Withheld Development/Calibration references only; retain real partitions; ordinary fitting rejects this manifest."});
    fs::create_dir(out)?;
    put(&out.join("records.jsonl"), &selected_bytes)?;
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}
fn encode(data: &Path, extra: &Path, weights: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let (mb, m) = read(&data.join("manifest.json"))?;
    let (cb, c) = read(&data.join("contract.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_shared_a_dataset_v1"
            && m["final_test_opened"] == false
            && m["contract_file_sha256"] == sha(&cb)
            && m["contract_sha256"] == sha(&serde_json::to_vec(&c)?),
        "frozen numeric contract"
    );
    let (eb, e) = read(&extra.join("manifest.json"))?;
    let transform = &e["partition_reference_transform"];
    ensure!(
        e["contract_sha256"] == m["contract_sha256"]
            && e["splits"] == m["splits"]
            && fs::read(extra.join("contract.json"))? == cb
            && transform["frozen_manifest_sha256"] == sha(&mb)
            && transform["fit_performed"] == false
            && transform["final_test_opened"] == false,
        "reference transform binding"
    );
    let audit = fs::read(extra.join("reference_population.json"))?;
    ensure!(
        transform["audit_file_sha256"] == sha(&audit),
        "reference audit changed"
    );
    let reference = fs::read(data.join("reference.jsonl"))?;
    let additional = fs::read(extra.join("partition_references.jsonl"))?;
    let bound = m["splits"]
        .as_array()
        .context("split bindings")?
        .iter()
        .find(|v| v["split"] == "reference")
        .context("known references")?;
    ensure!(
        bound["sha256"] == sha(&reference)
            && transform["rows_sha256"] == sha(&additional)
            && transform["requested"] == 8
            && transform["available"] == 8
            && transform["excluded"] == 0,
        "reference coverage/digests"
    );
    let known = rows(&reference)?;
    let withheld = rows(&additional)?;
    ensure!(known.len() == 12 && withheld.len() == 8, "reference counts");
    let mut query_ids = BTreeSet::new();
    let mut query_groups = BTreeSet::new();
    for part in ["train", "development", "calibration"] {
        let b = fs::read(data.join(format!("{part}.jsonl")))?;
        let binding = m["splits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["split"] == part)
            .context("query binding")?;
        ensure!(binding["sha256"] == sha(&b), "query file changed");
        for r in rows(&b)? {
            ensure!(
                r["purpose"] == "query" && r["frozen_split"] == part,
                "query purpose/split"
            );
            query_ids.insert(text(&r, "id")?.to_owned());
            query_groups.insert(text(&r, "source_group")?.to_owned());
        }
    }
    let (wb, w) = read(weights)?;
    ensure!(
        w["contract_sha256"] == m["contract_sha256"]
            && w["profiles"]["human"] == c["human_profiles"]
            && w["profiles"]["ai"] == c["ai_profiles"],
        "encoder contract/profile mismatch"
    );
    let model = SharedModel::from_json(&wb)?;
    let mut encoded = Vec::new();
    let mut ids = BTreeSet::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut per_identity = BTreeMap::<String, usize>::new();
    let mut groups = BTreeMap::new();
    for (r, part_reference) in known
        .iter()
        .map(|r| (r, false))
        .chain(withheld.iter().map(|r| (r, true)))
    {
        let id = text(r, "id")?;
        let part = text(r, "frozen_split")?;
        let group = text(r, "source_group")?;
        ensure!(
            ids.insert(id.to_owned()) && !query_ids.contains(id) && !query_groups.contains(group),
            "reference overlaps queries/another reference ID"
        );
        ensure!(
            r["purpose"] == "reference"
                && if part_reference {
                    matches!(part, "development" | "calibration")
                } else {
                    part == "train"
                },
            "reference partition"
        );
        if let Some(old) = groups.insert(group.to_owned(), part.to_owned()) {
            ensure!(old == part, "reference work crosses partitions");
        }
        ensure!(
            r["targets"]
                .as_object()
                .is_some_and(|v| v.values().all(Value::is_null)),
            "reference loss target"
        );
        let human = &r["roles"]["human"];
        let identity = text(human, "identity")?;
        let trained = c["human_profiles"]
            .as_array()
            .context("profiles")?
            .contains(&json!(identity));
        ensure!(
            human["present"] == true
                && r["roles"]["ai"]["present"] == false
                && human["known"] == trained
                && trained != part_reference,
            "human gallery/head enrollment"
        );
        let input: SharedInput = serde_json::from_value(r["input"].clone())?;
        let mut row = r.clone();
        row["encoding"] = json!(model.encode(&input)?);
        encoded.push(row);
        *counts.entry(part.into()).or_default() += 1;
        *per_identity
            .entry(format!("{part}/{identity}"))
            .or_default() += 1;
    }
    ensure!(
        counts.get("train") == Some(&12)
            && counts.get("development") == Some(&4)
            && counts.get("calibration") == Some(&4)
            && per_identity.len() == 10
            && per_identity.values().all(|n| *n == 2),
        "partition gallery roster"
    );
    let bundle = json!({"schema":"slop_ninja_partition_reference_encodings_v1","contract_sha256":m["contract_sha256"],"weights_sha256":sha(&wb),"frozen_manifest_sha256":sha(&mb),"known_reference_sha256":sha(&reference),"partition_reference_sha256":sha(&additional),"partition_reference_manifest_sha256":sha(&eb),"partition_reference_transform":transform,"reference_rows":20,"partition_counts":counts,"per_identity_counts":per_identity,"fit_performed":false,"normalization_fitted":false,"profiles_changed":false,"final_test_opened":false,"rows":encoded});
    put(out, &serde_json::to_vec_pretty(&bundle)?)?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    match a.first().map(String::as_str) {
        Some("select") if a.len() == 4 => {
            select(Path::new(&a[1]), Path::new(&a[2]), Path::new(&a[3]))
        }
        Some("transform") if a.len() == 5 => pipeline::transform_references(
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3]),
            Path::new(&a[4]),
        ),
        Some("encode") if a.len() == 5 => encode(
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3]),
            Path::new(&a[4]),
        ),
        _ => anyhow::bail!(
            "prepare_partition_references select QUERY_MANIFEST REFERENCES.jsonl NEW_OUT | transform FROZEN_NUMERIC REF_EXPORT REF_MANIFEST NEW_OUT | encode FROZEN_NUMERIC REF_NUMERIC WEIGHTS.json NEW_BUNDLE.json"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, Vec<Value>) {
        let mut records = Vec::new();
        let mut planned = Vec::new();
        for (part, authors) in [("train", 6), ("development", 2), ("calibration", 2)] {
            for author in 0..authors {
                for i in 0..2 {
                    let id = format!("{part}-{author}-{i}");
                    let group = format!("work-{part}-{author}");
                    let identity = format!("writer-{part}-{author}");
                    let prose = format!("Synthetic reference {id}.");
                    let rights = json!({"commercial_training":true,"external_evaluation":true});
                    records.push(json!({"id":id,"text":prose,"text_sha256":sha(prose.as_bytes()),"frozen_split":part,"purpose":"reference","source_group":group,"origin":"human_only","human_role_present":true,"human_identity":identity,"ai_role_present":false,"ai_identity":null,"parent_id":null,"rights":rights,"source":{"source_manifest_sha256":"source","record":{"author_key":identity,"purpose":"reference","preassigned_partition":format!("{part}_reference"),"source_group":group,"enrollment":if part=="train"{"known"}else{"withheld"},"rights":rights}}}));
                    planned.push(json!({"id":id,"frozen_split":part,"purpose":"reference","source_group":group}));
                }
            }
        }
        (
            json!({"schema":"slop_ninja_author_queries_v2","mode":"fresh-balanced","test_mode":false,"test_text_opened":false,"source_manifest_sha256":"source","emitted_reference_ids":records.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"planned_records":planned}),
            records,
        )
    }
    #[test]
    fn select_only_partition_references_without_relabeling() {
        let (q, r) = fixture();
        let selected = selected_references(&q, &r).unwrap();
        assert_eq!(selected.len(), 8);
        for part in ["development", "calibration"] {
            assert_eq!(
                selected
                    .iter()
                    .filter(|r| r["frozen_split"] == part)
                    .count(),
                4
            );
        }
        assert!(
            selected
                .iter()
                .all(|r| r["source"]["record"]["enrollment"] == "withheld")
        );
    }
    #[test]
    fn reject_test_queries_changed_text_and_cross_partition_sources() {
        let (q, r) = fixture();
        for field in ["test", "query", "text", "partition"] {
            let mut bad = r.clone();
            match field {
                "test" => bad[12]["frozen_split"] = json!("test"),
                "query" => bad[12]["purpose"] = json!("query"),
                "text" => bad[12]["text"] = json!("Changed text."),
                _ => {
                    bad[12]["source"]["record"]["preassigned_partition"] =
                        json!("calibration_reference")
                }
            };
            assert!(selected_references(&q, &bad).is_err());
        }
    }
}
