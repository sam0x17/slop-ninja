//! Exact, label-free requests. Final Test requires the frozen selection receipt.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn string<'a>(r: &'a Value, k: &str) -> Result<&'a str> {
    r[k].as_str().with_context(|| format!("missing {k}"))
}
fn requests(bytes: &[u8], m: &Value) -> Result<Vec<u8>> {
    requests_with_selection(bytes, m, None)
}
fn requests_with_selection(
    bytes: &[u8],
    m: &Value,
    selection_sha: Option<&str>,
) -> Result<Vec<u8>> {
    let test = selection_sha.is_some();
    ensure!(
        m["schema"] == "slop_ninja_a_supervised_dataset_v1" && m["test_text_opened"] == test,
        "dataset schema/Test mode mismatch"
    );
    if let Some(hash) = selection_sha {
        ensure!(
            m["complete"] == true
                && m["test_used_for_selection"] == false
                && m["selection_receipt_sha256"] == hash,
            "unbound or incomplete Test dataset"
        );
    }
    ensure!(m["records_sha256"] == sha(bytes), "records changed");
    let ids = |k: &str| -> Result<BTreeSet<String>> {
        let a = m[k].as_array().context("ID allowlist")?;
        let s = a
            .iter()
            .map(|v| v.as_str().map(str::to_owned).context("ID string"))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(s.len() == a.len(), "duplicate ID");
        Ok(s)
    };
    let allowed = ids(if test { "test_ids" } else { "non_test_ids" })?;
    let reserved = ids(if test { "non_test_ids" } else { "test_ids" })?;
    if test {
        ensure!(
            reserved.is_empty(),
            "final-Test export contains non-Test IDs"
        );
    }
    ensure!(allowed.is_disjoint(&reserved), "Test overlaps allowlist");
    let mut rows = BTreeMap::new();
    for line in std::str::from_utf8(bytes)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        let id = string(&r, "id")?;
        let split = string(&r, "frozen_split")?;
        ensure!(
            allowed.contains(id) && !reserved.contains(id),
            "unauthorized ID"
        );
        ensure!(
            if test {
                split == "test"
            } else {
                matches!(split, "train" | "development" | "calibration")
            },
            "input partition differs"
        );
        if test {
            ensure!(r["purpose"] == "query", "query export contains a reference");
        }
        ensure!(
            m["split_assignments"][string(&r, "source_group")?] == split,
            "source-family split changed"
        );
        ensure!(r["rights"]["commercial_training"] == true, "rights missing");
        let text = string(&r, "text")?;
        ensure!(r["text_sha256"] == sha(text.as_bytes()), "text changed");
        ensure!(
            rows.insert(id.to_owned(), json!({"id":id,"text":text}))
                .is_none(),
            "duplicate row"
        );
    }
    ensure!(
        rows.keys().cloned().collect::<BTreeSet<_>>() == allowed,
        "incomplete roster"
    );
    let mut out = Vec::new();
    for row in rows.values() {
        serde_json::to_writer(&mut out, row)?;
        out.push(b'\n');
    }
    Ok(out)
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 3 || (a.len() == 5 && a[3] == "--final-test"),
        "prepare_text_requests RECORDS MANIFEST NEW_OUTPUT [--final-test SELECTION]"
    );
    // Verify the complete freeze before opening any Test input.
    let selection_sha = if a.len() == 5 {
        final_test_binding::validate_selection_receipt(Path::new(&a[4]))?;
        Some(final_test_binding::sha(Path::new(&a[4]))?)
    } else {
        None
    };
    let b = fs::read(&a[0])?;
    let mb = fs::read(&a[1])?;
    let m: Value = serde_json::from_slice(&mb)?;
    let out = if let Some(hash) = selection_sha.as_deref() {
        requests_with_selection(&b, &m, Some(hash))?
    } else {
        requests(&b, &m)?
    };
    let root = Path::new(&a[2]);
    fs::create_dir(root)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("requests.jsonl"))?;
    f.write_all(&out)?;
    f.sync_all()?;
    let mut receipt = json!({"schema":"slop_ninja_non_test_text_requests_v1","records_sha256":sha(&b),"dataset_manifest_sha256":sha(&mb),"requests_sha256":sha(&out),"requested":m[if selection_sha.is_some() {"test_ids"} else {"non_test_ids"}].as_array().unwrap().len(),"test_text_opened":selection_sha.is_some(),"learned_inputs":"exact text only; ID routing only","source_sha256":sha(include_bytes!("prepare_text_requests.rs"))});
    if let Some(hash) = selection_sha {
        receipt["schema"] = json!("slop_ninja_final_test_text_requests_v1");
        receipt["selection_receipt_sha256"] = json!(hash);
        receipt["test_used_for_selection"] = json!(false);
    }
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    println!("{}", receipt);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Vec<u8>, Value) {
        let r = json!({"id":"a","source_group":"w","frozen_split":"train","rights":{"commercial_training":true},"text":"Synthetic text.","text_sha256":sha(b"Synthetic text.")});
        let b = serde_json::to_vec(&r).unwrap();
        let m = json!({"schema":"slop_ninja_a_supervised_dataset_v1","test_text_opened":false,"records_sha256":sha(&b),"non_test_ids":["a"],"test_ids":["heldout"],"split_assignments":{"w":"train"}});
        (b, m)
    }
    #[test]
    fn routing_fields_only_and_exact_text() {
        let (b, m) = fixture();
        let out = requests(&b, &m).unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "{\"id\":\"a\",\"text\":\"Synthetic text.\"}\n"
        );
    }
    #[test]
    fn changed_bytes_and_reserved_families_fail() {
        let (mut b, mut m) = fixture();
        b.push(b' ');
        assert!(requests(&b, &m).is_err());
        m["records_sha256"] = sha(&b).into();
        m["split_assignments"]["w"] = "test".into();
        assert!(requests(&b, &m).is_err());
    }
    #[test]
    fn test_requires_matching_freeze_and_only_emits_routing_and_text() {
        let (b, mut m) = fixture();
        let mut row: Value = serde_json::from_slice(&b).unwrap();
        row["frozen_split"] = json!("test");
        row["purpose"] = json!("query");
        let b = serde_json::to_vec(&row).unwrap();
        m["records_sha256"] = json!(sha(&b));
        m["test_text_opened"] = json!(true);
        m["test_used_for_selection"] = json!(false);
        m["complete"] = json!(true);
        m["selection_receipt_sha256"] = json!("frozen");
        m["test_ids"] = json!(["a"]);
        m["non_test_ids"] = json!([]);
        m["split_assignments"]["w"] = json!("test");
        assert!(requests(&b, &m).is_err());
        assert!(requests_with_selection(&b, &m, Some("different")).is_err());
        let output = requests_with_selection(&b, &m, Some("frozen")).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            json!({"id":"a","text":"Synthetic text."})
        );
        row["purpose"] = json!("reference");
        let b = serde_json::to_vec(&row).unwrap();
        m["records_sha256"] = json!(sha(&b));
        assert!(requests_with_selection(&b, &m, Some("frozen")).is_err());
    }
}
