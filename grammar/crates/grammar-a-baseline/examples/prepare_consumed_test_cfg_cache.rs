//! Prepare a cache-only view after a sealed Test evaluation has completed.
//! STAGE NEW_OUTPUT. Preserves all original fields and adds a checked split alias.
#[allow(dead_code, unused_imports)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use anyhow::{Context, Result, ensure};
use seal::{read, sha, text};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, io::Write, path::Path};
fn project(mut r: Value) -> Result<Value> {
    ensure!(
        r["frozen_split"] == "test" && ["query", "reference"].contains(&text(&r, "purpose")?),
        "consumed Test only"
    );
    ensure!(
        r.get("split").is_none() || r["split"] == "test",
        "conflicting split"
    );
    r["split"] = json!("test");
    Ok(r)
}
fn run(stage: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new cache preparation directory required");
    let root = stage.join("final-test-v1");
    let sp = root.join("seal.json");
    seal::validate(&sp)?;
    let seal_hash = sha(&sp)?;
    let eval_path = root.join("evaluation-v2/manifest.json");
    let evaluation = read(&eval_path)?;
    let numeric = root.join("numeric-v1");
    let nm = read(&numeric.join("manifest.json"))?;
    let source = root.join("dataset-v1");
    let sm = read(&source.join("manifest.json"))?;
    let refs = read(&source.join("reference-manifest.json"))?;
    ensure!(
        evaluation["schema"] == "slop_ninja_last_block_test_evaluation_v1"
            && evaluation["complete"] == true
            && evaluation["selection_receipt_sha256"] == seal_hash
            && evaluation["dataset_manifest_sha256"] == sha(&numeric.join("manifest.json"))?
            && nm["source_dataset"]["sha256"] == sha(&source.join("manifest.json"))?
            && nm["test_used_for_selection"] == false
            && sm["selection_receipt_sha256"] == seal_hash
            && refs["provenance"]["query_manifest_sha256"] == sha(&source.join("manifest.json"))?,
        "completed bound evaluation required"
    );
    // Check the completed predictions, not just a success flag.
    let runs = evaluation["reports"]
        .as_array()
        .context("evaluation runs")?;
    let mut names = BTreeSet::new();
    for r in runs {
        let condition = text(r, "condition")?;
        let seed = r["seed"].as_u64().context("seed")?;
        ensure!(
            ["adapt_last_block", "frozen_last_block"].contains(&condition)
                && [17, 29, 41].contains(&seed),
            "run scope"
        );
        let name = format!("{condition}-seed-{seed}");
        ensure!(
            names.insert(name.clone())
                && r["predictions_sha256"]
                    == sha(&root
                        .join("evaluation-v2")
                        .join(name)
                        .join("test-predictions.json"))?,
            "completed predictions changed"
        );
    }
    ensure!(names.len() == 6, "all fixed runs must be complete");
    let mut output = Vec::new();
    let mut ids = BTreeSet::new();
    let mut parents = Vec::new();
    for (file, key, manifest, part) in [
        ("records.jsonl", "records_sha256", &sm, "test"),
        ("references.jsonl", "records_sha256", &refs, "reference"),
    ] {
        let source_path = source.join(file);
        ensure!(manifest[key] == sha(&source_path)?, "source changed");
        let numeric_path = numeric.join(format!("{part}.jsonl"));
        ensure!(
            nm["files_sha256"][format!("{part}.jsonl")] == sha(&numeric_path)?,
            "numeric partition changed"
        );
        let mut expected = BTreeSet::new();
        for line in fs::read_to_string(&numeric_path)?.lines() {
            let r: Value = serde_json::from_str(line)?;
            ensure!(
                expected.insert((
                    text(&r, "id")?.to_owned(),
                    text(&r, "text_sha256")?.to_owned()
                )),
                "numeric duplicate"
            );
        }
        for line in fs::read_to_string(&source_path)?.lines() {
            let row = project(serde_json::from_str(line)?)?;
            let id = text(&row, "id")?;
            ensure!(
                ids.insert(id.to_owned())
                    && expected.remove(&(id.to_owned(), text(&row, "text_sha256")?.to_owned())),
                "source support differs"
            );
            serde_json::to_writer(&mut output, &row)?;
            output.push(b'\n');
        }
        ensure!(expected.is_empty(), "missing source rows");
        parents.push(json!({"path":source_path,"sha256":sha(&source_path)?}));
    }
    fs::create_dir(out)?;
    fs::write(out.join("records.jsonl"), output)?;
    let manifest = json!({"schema":"slop_ninja_recorded_corpus_preprocessing_v1","complete":true,
        "preprocessing_only":true,"new_reserved_test_included":false,"exposure":"historical_consumed_population",
        "historical_split":"test","records":ids.len(),"records_sha256":sha(&out.join("records.jsonl"))?,
        "provenance_manifest_path":eval_path,"provenance_manifest_sha256":sha(&eval_path)?,"source_files":parents,
        "selection_receipt_sha256":seal_hash,"fitting_or_calibration_permitted":false,
        "projection":"original fields and exact text retained; checked split=test alias added for cache reader",
        "scope":"Already evaluated Stage3 Test queries and references. Cache and archive only; no new features enter the frozen comparison."});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("manifest.json"))?;
    serde_json::to_writer_pretty(&mut f, &manifest)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 2, "STAGE NEW_OUTPUT");
    run(Path::new(&a[0]), Path::new(&a[1]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_preserves_fields_and_refuses_split_conflicts() {
        let r = json!({"frozen_split":"test","purpose":"query","text":"Synthetic text.","metadata":{"preserved":true}});
        let mut p = project(r.clone()).unwrap();
        assert_eq!(
            p.as_object_mut().unwrap().remove("split"),
            Some(json!("test"))
        );
        assert_eq!(p, r);
        let mut bad = r.clone();
        bad["split"] = json!("train");
        assert!(project(bad).is_err());
        let mut bad = r;
        bad["frozen_split"] = json!("calibration");
        assert!(project(bad).is_err());
    }
}
