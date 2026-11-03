//! Admit the acquired, sealed Test without changing its roster or model choice.
//! STAGE PRIOR_LEDGER CONSUMED_TEST_RECORDS CONSUMED_TEST_REFERENCES NEW_OUT
#[allow(dead_code)]
#[path = "admit_encoder_sources.rs"]
mod prior;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use anyhow::{Context, Result, ensure};
use seal::{read, sha, text};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
fn digest(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn add_reason(reasons: &mut BTreeMap<String, BTreeSet<String>>, group: &str, reason: &str) {
    reasons
        .entry(group.into())
        .or_default()
        .insert(reason.into());
}
fn visit(v: &Value, f: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
    match v {
        Value::Array(a) => {
            for item in a {
                visit(item, f)?;
            }
        }
        Value::Object(m) => {
            if let Some(t) = m.get("text").and_then(Value::as_str) {
                f(t)?;
            }
            for (k, v) in m {
                if k != "text" {
                    visit(v, f)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn run(
    stage: &Path,
    ledger: &Path,
    consumed: &Path,
    consumed_refs: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.exists(), "new admission output required");
    let root = stage.join("final-test-v1");
    let selected = seal::validate(&root.join("seal.json"))?;
    let seal_hash = sha(&root.join("seal.json"))?;
    let input = root.join("queries-v1");
    let m = read(&input.join("manifest.json"))?;
    let source_manifest_path = root.join("sources-v1/manifest.json");
    let source_manifest_hash = sha(&source_manifest_path)?;
    let source_manifest = read(&source_manifest_path)?;
    ensure!(source_manifest_hash == m["source_manifest_sha256"]
        && source_manifest["selection_receipt_sha256"] == seal_hash
        && source_manifest["complete"] == true
        && source_manifest["query_count"] == 20 && source_manifest["reference_count"] == 2
        && source_manifest["sources"].as_array().context("source roster")?.len() == 22,
        "acquired source manifest is not bound to the frozen roster");
    ensure!(
        m["schema"] == "slop_ninja_author_queries_v2"
            && m["test_mode"] == true
            && m["test_text_opened"] == true,
        "acquired Test extraction required"
    );
    let mut all = Vec::new();
    for (name, key) in [
        ("humans.jsonl", "records_sha256"),
        ("references.jsonl", "references_sha256"),
    ] {
        ensure!(sha(&input.join(name))? == m[key], "extraction changed");
        all.extend(rows(&input.join(name))?);
    }
    let mut ids = BTreeSet::new();
    let mut target_shingles = BTreeMap::<String, BTreeSet<String>>::new();
    let mut reasons = BTreeMap::<String, BTreeSet<String>>::new();
    let l = read(ledger)?;
    ensure!(l["complete"] == true, "prior exclusion ledger incomplete");
    let get_set = |key: &str| -> Result<BTreeSet<String>> {
        l[key]
            .as_array()
            .context("ledger set")?
            .iter()
            .map(|v| Ok(v.as_str().context("ledger value")?.to_owned()))
            .collect()
    };
    let mut exact = get_set("protected_text_sha256")?;
    let mut normalized = get_set("protected_normalized_text_sha256")?;
    let mut protected_groups = get_set("protected_source_groups")?;
    for row in &all {
        let id = text(row, "id")?;
        let group = text(row, "source_group")?;
        let content = text(row, "text")?;
        ensure!(
            ids.insert(id.to_owned())
                && row["frozen_split"] == "test"
                && row["origin"] == "human_only"
                && row["documented_human_only"] == false
                && row["rights"]["commercial_training"] == true
                && row["text_sha256"] == digest(content.as_bytes()),
            "Test history/rights/text"
        );
        let source = &row["source"]["record"];
        ensure!(row["source"]["source_manifest_sha256"] == source_manifest_hash
            && source_manifest["sources"].as_array().unwrap().contains(source),
            "source is not from sealed acquisition");
        let bytes = fs::read(text(source, "text_path")?)?;
        ensure!(
            digest(&bytes) == source["text_sha256"],
            "original source changed"
        );
        let span = row["source"]["byte_span"]
            .as_array()
            .context("exact span")?;
        ensure!(span.len() == 2, "span width");
        let lo = span[0].as_u64().context("span start")? as usize;
        let hi = span[1].as_u64().context("span end")? as usize;
        ensure!(
            lo < hi && hi <= bytes.len() && &bytes[lo..hi] == content.as_bytes(),
            "excerpt does not match source"
        );
        if !(150..=450).contains(&content.split_whitespace().count()) {
            add_reason(&mut reasons, group, "fixed_word_bound");
        }
        for h in prior::shingles(content) {
            target_shingles.entry(h).or_default().insert(group.into());
        }
    }
    for groups in target_shingles.values().filter(|g| g.len() > 1) {
        for group in groups {
            add_reason(
                &mut reasons,
                group,
                "shared_50_word_sequence_across_Test_works",
            );
        }
    }
    // Only retain candidate Test shingles, then stream the historical populations
    // through them. Corpus growth does not multiply the in-memory shingle index.
    let mut scanned = 0usize;
    let mut scan = |v: &Value| -> Result<()> {
        visit(v, &mut |t| {
            scanned += 1;
            exact.insert(digest(t.as_bytes()));
            normalized.insert(prior::norm(t));
            for h in prior::shingles(t) {
                if let Some(groups) = target_shingles.get(&h) {
                    for group in groups {
                        add_reason(&mut reasons, group, "prior_shared_50_word_sequence");
                    }
                }
            }
            Ok(())
        })
    };
    let mut inputs = Vec::new();
    let mut files = BTreeSet::new();
    for binding in l["protected_files"].as_array().context("protected files")? {
        let p = Path::new(text(binding, "path")?);
        ensure!(
            !p.starts_with(&root) && sha(p)? == binding["sha256"],
            "prior input changed or includes new Test"
        );
        inputs.push(binding.clone());
        if binding["metadata_only"] == true {
            continue;
        }
        let key = format!("{}:{}", p.display(), binding["pointer"]);
        if !files.insert(key) {
            continue;
        }
        match text(binding, "format")? {
            "jsonl" => {
                for v in rows(p)? {
                    scan(&v)?;
                }
            }
            "json" => {
                let v = read(p)?;
                let selected = if binding["pointer"].is_null() {
                    &v
                } else {
                    v.pointer(text(binding, "pointer")?)
                        .context("prior pointer")?
                };
                scan(selected)?;
            }
            _ => anyhow::bail!("unsupported prior format"),
        }
    }
    for p in [
        consumed.to_path_buf(),
        consumed_refs.to_path_buf(),
        stage.join("dataset-v1/records.jsonl"),
    ] {
        let hash = sha(&p)?;
        if p.starts_with(stage) {
            ensure!(
                read(&stage.join("dataset-v1/manifest.json"))?["records_sha256"] == hash,
                "non-Test dataset changed"
            );
        }
        for v in rows(&p)? {
            ensure!(
                p != stage.join("dataset-v1/records.jsonl") || v["frozen_split"] != "test",
                "new holdout in protected set"
            );
            if let Some(g) = v["source_group"].as_str() {
                protected_groups.insert(g.into());
            }
            scan(&v)?;
        }
        inputs.push(json!({"path":p,"sha256":hash,"scope":"consumed_Test_or_current_nonTest"}));
    }
    drop(scan);
    // The numeric fitting bundle supplies a further exact text/family audit;
    // these metadata do not become learned inputs or redefine supervision.
    for part in ["train", "development", "calibration", "reference"] {
        let p = stage.join(format!("fit-bundle-v1/{part}.jsonl"));
        for r in rows(&p)? {
            exact.insert(text(&r, "text_sha256")?.into());
            protected_groups.insert(text(&r, "source_group")?.into());
        }
    }
    for row in &all {
        let group = text(row, "source_group")?;
        if exact.contains(text(row, "text_sha256")?)
            || normalized.contains(&prior::norm(text(row, "text")?))
        {
            add_reason(
                &mut reasons,
                group,
                "prior_exact_or_whitespace_normalized_overlap",
            );
        }
        if protected_groups.contains(group) {
            add_reason(&mut reasons, group, "prior_source_family");
        }
    }
    fs::create_dir(out)?;
    let mut queries = Vec::new();
    let mut refs = Vec::new();
    let mut exclusions = Vec::new();
    for mut row in all {
        if let Some(reason) = reasons.get(text(&row, "source_group")?) {
            exclusions
                .push(json!({"id":row["id"],"source_group":row["source_group"],"reasons":reason}));
            continue;
        }
        let stratum = text(&row["source"]["record"]["selection_row"], "stratum")?.to_owned();
        row["metadata"] = json!({"acquisition_stratum":stratum,"source_collection":"pg_stage3_historical","normalization_or_split_metadata_is_not_learned_input":true});
        if row["purpose"] == "reference" {
            refs.push(row);
        } else {
            ensure!(row["purpose"] == "query", "purpose");
            queries.push(row);
        }
    }
    let mut hashes = BTreeMap::new();
    for (name, rs) in [
        ("humans.jsonl", &queries),
        ("references.jsonl", &refs),
        ("exclusions.jsonl", &exclusions),
    ] {
        let p = out.join(name);
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&p)?;
        for r in rs {
            serde_json::to_writer(&mut f, r)?;
            f.write_all(b"\n")?;
        }
        f.sync_all()?;
        hashes.insert(name, sha(&p)?);
    }
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_test_admission_v1","complete":true,
        "test_text_opened":true,"selection_receipt_sha256":seal_hash,"selected_condition":selected["selected_condition"],
        "source_manifest_sha256":source_manifest_hash,"verified_source_works":22,
        "original_manifest_expected_test_sources":source_manifest["expected_test_sources"],
        "source_manifest_count_note":"Actual reserved roster is checked independently; an earlier acquisition serializer wrote a stale expected-count field. Original receipts and texts are retained unchanged.",
        "test_used_for_selection":false,"query_records":queries.len(),"reference_records":refs.len(),"excluded_records":exclusions.len(),
        "excluded_families":reasons,"extraction_failures":m["failures"],"requested_test":m["planned_records"].as_array().context("Test roster")?.len(),
        "files_sha256":hashes,"protected_text_occurrences_scanned":scanned,"protected_inputs":inputs,"prior_ledger_sha256":sha(ledger)?,
        "extraction_manifest_sha256":sha(&input.join("manifest.json"))?,"implementation_sha256":digest(include_bytes!("admit_last_block_test.rs")),
        "limitations":["Historical human proxies, not documented modern human-only workflows.","Overlap coverage follows the retained ledger and current cohort; no universal pretraining or full-work audit.","Exclude entire source families, without replacements or changes to weights/calibration."]}),
    )?;
    println!(
        "{}",
        json!({"queries":queries.len(),"references":refs.len(),"excluded_records":exclusions.len(),"protected_text_occurrences":scanned})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 5,
        "STAGE LEDGER CONSUMED_TEST_RECORDS CONSUMED_TEST_REFERENCES NEW_OUT"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        Path::new(&a[4]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_scan_includes_nested_text_but_never_metadata_strings() {
        let mut found = Vec::new();
        visit(&json!({"text":"One synthetic paragraph.","source":{"rows":[{"text":"Another."}]},"title":"not prose"}),&mut |s|{found.push(s.to_owned());Ok(())}).unwrap();
        assert_eq!(found.len(), 2);
        assert!(!found.contains(&"not prose".into()));
    }
}
