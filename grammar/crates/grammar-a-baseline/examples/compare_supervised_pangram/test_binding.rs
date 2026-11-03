//! Shared, metadata-only Test comparator admission. No acquisition or fitting.
//!
//! Numeric handoff: retain the existing manifest.test_transform and its
//! test_population.json audit; add test_transform.selection_receipt_sha256 and
//! copy the hash-bound Test dataset manifest to test_dataset_manifest.json.
//! The contract remains byte-identical to a candidate in selection.candidates.
//!
//! Prediction receipt schema slop_ninja_final_test_predictions_v1:
//! complete=true, split="test", fit_performed=false, selection_receipt_sha256,
//! numeric_manifest_sha256, query_sha256, fit_sha256, weights_sha256,
//! contract_sha256, predictions_file (one basename), predictions_sha256,
//! requested, supported, excluded=[{id,reason,...}]. Predictions are the existing
//! [{id,prediction}] array. requested counts numeric-supported queries; the full
//! source/projection denominators are retained separately. Never write into FIT.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
pub fn sha(p: &Path) -> Result<String> {
    super::final_test_binding::sha(p)
}
pub fn digest(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
pub fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("nonempty {k}"))
}
pub fn count(v: &Value, k: &str) -> Result<usize> {
    usize::try_from(v[k].as_u64().with_context(|| format!("count {k}"))?).context("count overflow")
}
pub fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().with_context(|| format!("array {k}"))
}
pub fn hash_text(v: &Value, k: &str) -> Result<String> {
    let h = text(v, k)?;
    ensure!(
        h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid digest {k}"
    );
    Ok(h.to_owned())
}
pub fn member(root: &Path, name: &str) -> Result<PathBuf> {
    let mut c = Path::new(name).components();
    ensure!(
        matches!(c.next(), Some(Component::Normal(_)))
            && c.next().is_none()
            && !name.contains('\\'),
        "unsafe file/ID"
    );
    Ok(root.join(name))
}
pub fn indexed(rows: &[Value]) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for r in rows {
        let id = text(r, "id")?;
        member(Path::new("."), id)?;
        ensure!(
            out.insert(id.to_owned(), r.clone()).is_none(),
            "duplicate ID"
        );
    }
    Ok(out)
}
pub fn jsonl(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
pub fn write_new(p: &Path, value: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
pub fn bound(selection: &Value, p: &Path) -> Result<String> {
    let actual = p.canonicalize()?;
    let mut found = None;
    for r in array(selection, "binding_files")? {
        if Path::new(text(r, "path")?).canonicalize()? == actual {
            ensure!(found.is_none(), "aliased duplicate frozen binding");
            let hash = hash_text(r, "sha256")?;
            ensure!(sha(p)? == hash, "changed frozen file: {}", p.display());
            found = Some(hash);
        }
    }
    found.with_context(|| format!("file absent from selection bindings: {}", p.display()))
}

pub struct TestData {
    pub manifest: Value,
    pub manifest_sha: String,
    pub contract_sha: String,
    pub parent_sha: String,
    pub selection_sha: String,
    pub query_sha: String,
    pub population_sha: String,
    pub dataset_sha: String,
    pub rows: BTreeMap<String, Value>,
    pub numeric: BTreeMap<String, Value>,
    pub hashes: BTreeMap<String, String>,
    pub projection_excluded: Vec<Value>,
    pub upstream: Value,
}

fn ids(v: &Value, key: &str) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for id in array(v, key)? {
        let id = id.as_str().context("ID string")?;
        member(Path::new("."), id)?;
        ensure!(out.insert(id.to_owned()), "duplicate allow-list ID");
    }
    Ok(out)
}

// This projects labels already validated by the assembler. It never reconstructs
// fractional labels from model output or reads source/replacement prose.
fn labelled(r: &Value) -> Result<Value> {
    let m = &r["metadata"];
    ensure!(
        m.get("text").is_none(),
        "comparator population must omit prose"
    );
    ensure!(
        r["split"] == "test"
            && r["purpose"] == "query"
            && m["frozen_split"] == "test"
            && m["purpose"] == "query"
            && r["id"] == m["id"]
            && r["group"] == m["source_group"],
        "Test query metadata binding differs"
    );
    text(m, "source_group")?;
    hash_text(m, "text_sha256")?;
    let y = match text(m, "origin")? {
        "human_only" => 0,
        "model_only" => 1,
        "mixed" => 2,
        _ => anyhow::bail!("origin label"),
    };
    let share = m
        .get("share_target")
        .context("explicit nullable share_target")?;
    if !share.is_null() {
        let s = share.as_f64().context("share target")?;
        ensure!(
            s.is_finite() && (0.0..=1.).contains(&s) && (y != 0 || s == 0.) && (y != 1 || s == 1.),
            "share target range/history"
        );
    }
    Ok(
        json!({"id":r["id"],"source_group":r["group"],"frozen_split":"test","purpose":"query",
        "targets":{"origin":y,"ai_involvement":y!=0,"share":share}}),
    )
}

pub fn upstream_accounting(m: &Value, accepted: &BTreeSet<String>) -> Result<Value> {
    ensure!(
        m["schema"] == "slop_ninja_a_supervised_dataset_v1"
            && m["complete"] == true
            && m["test_text_opened"] == true
            && m["non_test_ids"] == json!([])
            && ids(m, "test_ids")? == *accepted
            && count(m, "records")? == accepted.len(),
        "Test source roster differs"
    );
    let exclusions = array(m, "excluded")?;
    let mut accounted = accepted.clone();
    for e in exclusions {
        ensure!(
            accounted.insert(text(e, "id")?.to_owned()),
            "upstream duplicate/exclusion ID"
        );
        text(e, "reason")?;
    }
    let mut failures = Vec::new();
    for report in array(m, "generation_reports")? {
        let failed = array(report, "failures")?;
        ensure!(
            report["complete"] == true
                && count(report, "failed")? == failed.len()
                && count(report, "requested")? == count(report, "accepted")? + failed.len(),
            "generation report incomplete"
        );
        for e in failed {
            ensure!(
                accounted.insert(text(e, "id")?.to_owned()),
                "upstream repeated terminal ID"
            );
            failures.push(e.clone());
        }
    }
    ensure!(
        count(m, "requested_records")? == accounted.len(),
        "upstream requested accounting differs"
    );
    Ok(
        json!({"requested":accounted.len(),"assembled":accepted.len(),"excluded":exclusions,
        "terminal_generation_failures":failures,"accounted":true}),
    )
}

pub fn load_data(root: &Path, selection: &Value, selection_sha: &str) -> Result<TestData> {
    let m = read(&root.join("manifest.json"))?;
    let t = &m["test_transform"];
    ensure!(
        m["schema"] == "slop_ninja_shared_a_dataset_v1"
            && m["final_test_opened"] == true
            && m["test_used_for_selection"] == false
            && t["fit_performed"] == false
            && t["selection_receipt_sha256"] == selection_sha,
        "unbound Test projection"
    );
    let parent_sha = hash_text(t, "frozen_manifest_sha256")?;
    let candidates = array(selection, "candidates")?
        .iter()
        .filter(|c| c["numeric_manifest_sha256"] == parent_sha)
        .collect::<Vec<_>>();
    ensure!(
        !candidates.is_empty(),
        "Test parent absent from frozen candidates"
    );
    let cb = fs::read(root.join("contract.json"))?;
    let contract: Value = serde_json::from_slice(&cb)?;
    let compact = digest(&serde_json::to_vec(&contract)?);
    ensure!(
        contract["final_test_opened"] == false
            && m["contract_file_sha256"] == digest(&cb)
            && m["contract_sha256"] == compact,
        "changed Test contract"
    );
    for c in &candidates {
        ensure!(
            c["contract_file_sha256"] == digest(&cb) && c["contract_sha256"] == compact,
            "candidate contract differs"
        );
        let parent = Path::new(text(c, "numeric_path")?);
        bound(selection, &parent.join("manifest.json"))?;
        bound(selection, &parent.join("contract.json"))?;
    }
    let qp = root.join("test.jsonl");
    let qsha = sha(&qp)?;
    let splits = array(&m, "splits")?
        .iter()
        .filter(|s| s["split"] == "test")
        .collect::<Vec<_>>();
    ensure!(
        splits.len() == 1 && splits[0]["sha256"] == qsha,
        "Test split digest differs"
    );
    let numeric = indexed(&jsonl(&qp)?)?;
    ensure!(
        count(splits[0], "rows")? == numeric.len(),
        "numeric support count differs"
    );
    let ap = member(root, text(t, "audit_file")?)?;
    ensure!(
        sha(&ap)? == hash_text(t, "audit_file_sha256")?,
        "Test population audit changed"
    );
    let audit = read(&ap)?;
    ensure!(
        audit["fit_performed"] == false
            && audit["final_test_opened"] == true
            && audit["contract_sha256"] == compact
            && audit["frozen_manifest_sha256"] == parent_sha
            && audit["dataset_manifest_sha256"] == t["dataset_manifest_sha256"],
        "population parent binding differs"
    );
    let raw = indexed(array(&audit, "rows")?)?;
    let mut all = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for (id, r) in &raw {
        let row = labelled(r)?;
        // No reserved fitting family may be relabelled as Test.
        if let Some(split) = contract["split_assignments"].get(text(&row, "source_group")?) {
            ensure!(
                *split == "test",
                "Test family intersects frozen fitting/gallery families"
            );
        }
        ensure!(
            r["available"].as_bool() == Some(numeric.contains_key(id)),
            "audit/numeric availability differs"
        );
        hashes.insert(id.clone(), hash_text(&r["metadata"], "text_sha256")?);
        all.insert(id.clone(), row);
    }
    let projection_excluded = array(&audit, "excluded")?.clone();
    let ex = indexed(&projection_excluded)?;
    ensure!(
        count(t, "requested")? == raw.len()
            && count(&audit, "requested")? == raw.len()
            && count(t, "available")? == numeric.len()
            && count(&audit, "available")? == numeric.len()
            && count(t, "excluded")? == ex.len()
            && numeric.len() + ex.len() == raw.len(),
        "projection accounting differs"
    );
    for (id, row) in &numeric {
        let expected = all.get(id).context("unexpected numeric ID")?;
        ensure!(
            !ex.contains_key(id)
                && row["input"]["contract_sha256"] == compact
                && row["frozen_split"] == "test"
                && row["purpose"] == "query"
                && row["source_group"] == expected["source_group"],
            "numeric Test family/scope differs"
        );
        for key in ["origin", "ai_involvement", "share"] {
            ensure!(
                row["targets"][key] == expected["targets"][key],
                "numeric Test supervision differs"
            );
        }
    }
    ensure!(
        ex.keys().all(|id| all.contains_key(id)),
        "unlisted projection exclusion"
    );
    for e in ex.values() {
        text(e, "reason")?;
    }
    let ds = root.join("test_dataset_manifest.json");
    let dataset_sha = sha(&ds)?;
    ensure!(
        t["dataset_manifest_sha256"] == dataset_sha,
        "source dataset manifest changed"
    );
    let source = read(&ds)?;
    ensure!(
        source["selection_receipt_sha256"] == selection_sha
            && source["test_used_for_selection"] == false
            && hash_text(&source, "records_sha256")? == hash_text(&audit, "records_sha256")?,
        "source selection/record binding differs"
    );
    let upstream = upstream_accounting(&source, &all.keys().cloned().collect())?;
    for r in all.values() {
        ensure!(
            source["split_assignments"][text(r, "source_group")?] == "test",
            "source split differs"
        );
    }
    Ok(TestData {
        manifest: m,
        manifest_sha: sha(&root.join("manifest.json"))?,
        contract_sha: compact,
        parent_sha,
        selection_sha: selection_sha.into(),
        query_sha: qsha,
        population_sha: sha(&ap)?,
        dataset_sha,
        rows: all,
        numeric,
        hashes,
        projection_excluded,
        upstream,
    })
}

pub fn fit_binding(
    selection: &Value,
    data: &TestData,
    fit: &Path,
) -> Result<(Value, Value, Value)> {
    let canonical = fit.canonicalize()?;
    let mut found = None;
    for c in array(selection, "candidates")? {
        if c["numeric_manifest_sha256"] != data.parent_sha {
            continue;
        }
        for r in array(c, "runs")? {
            if Path::new(text(r, "model_path")?).canonicalize()? == canonical {
                ensure!(found.is_none(), "ambiguous frozen candidate/run");
                found = Some((c.clone(), r.clone()));
            }
        }
    }
    let (candidate, run) = found.context("fit absent from this frozen candidate parent")?;
    let f = read(&fit.join("fit.json"))?;
    ensure!(
        bound(selection, &fit.join("fit.json"))? == text(&run, "fit_sha256")?
            && bound(selection, &fit.join("weights.json"))? == text(&run, "weights_sha256")?
            && f["weights_sha256"] == run["weights_sha256"]
            && f["seed"] == run["seed"]
            && f["control"] == "joint"
            && f["final_test_opened"] == false
            && f["data_manifest_sha256"] == data.parent_sha,
        "frozen fit differs"
    );
    let w = read(&fit.join("weights.json"))?;
    ensure!(
        w["contract_sha256"] == data.contract_sha,
        "weights contract differs"
    );
    Ok((candidate, run, f))
}

pub struct Predictions {
    pub values: BTreeMap<String, Value>,
    pub excluded: Vec<Value>,
    pub receipt_sha: String,
    pub file_sha: String,
}
pub fn predictions(path: &Path, data: &TestData, run: &Value) -> Result<Predictions> {
    let m = read(path)?;
    ensure!(
        m["schema"] == "slop_ninja_final_test_predictions_v1"
            && m["complete"] == true
            && m["split"] == "test"
            && m["fit_performed"] == false
            && m["selection_receipt_sha256"] == data.selection_sha
            && m["numeric_manifest_sha256"] == data.manifest_sha
            && m["query_sha256"] == data.query_sha
            && m["contract_sha256"] == data.contract_sha
            && m["weights_sha256"] == run["weights_sha256"]
            && m["fit_sha256"] == run["fit_sha256"],
        "prediction receipt binding differs"
    );
    let file = member(
        path.parent().context("prediction receipt parent")?,
        text(&m, "predictions_file")?,
    )?;
    let file_sha = sha(&file)?;
    ensure!(m["predictions_sha256"] == file_sha, "predictions changed");
    let rows = read(&file)?;
    let map = indexed(rows.as_array().context("prediction array")?)?;
    let excluded = array(&m, "excluded")?.clone();
    let ex = indexed(&excluded)?;
    ensure!(
        count(&m, "requested")? == data.numeric.len()
            && count(&m, "supported")? == map.len()
            && map.len() + ex.len() == data.numeric.len(),
        "prediction support accounting differs"
    );
    for id in map.keys().chain(ex.keys()) {
        ensure!(
            data.numeric.contains_key(id),
            "prediction ID outside numeric roster"
        );
    }
    ensure!(
        map.keys().all(|id| !ex.contains_key(id)),
        "prediction/exclusion overlap"
    );
    for r in ex.values() {
        text(r, "reason")?;
    }
    let mut values = BTreeMap::new();
    for (id, r) in map {
        let p = &r["prediction"];
        for key in ["ai_confidence_percent", "ai_share_percent"] {
            ensure!(
                p[key]
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && (0.0..=100.).contains(&v)),
                "invalid A probability/share"
            );
        }
        values.insert(id, p.clone());
    }
    Ok(Predictions {
        values,
        excluded,
        receipt_sha: sha(path)?,
        file_sha,
    })
}

#[cfg(test)]
#[path = "test_binding_tests.rs"]
mod tests;
