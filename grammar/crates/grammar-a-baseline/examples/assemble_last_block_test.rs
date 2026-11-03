//! Assemble the sealed encoder experiment's Test. No fitting or model calls.
//! STAGE_ROOT NEW_OUTPUT; all generation must have terminal receipts.
#![recursion_limit = "256"]
#[allow(dead_code, unused_attributes)]
#[path = "assemble_balanced_dataset.rs"]
mod evidence;
#[path = "assemble_last_block_test/overlap.rs"]
mod overlap;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
use anyhow::{Context, Result, ensure};
use evidence::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
#[path = "../../../../examples/a_balanced_generate/encoder_recipe.rs"]
mod encoder_recipe;
const PARTS: [&str; 3] = ["train", "development", "calibration"];

fn slot_check(slot: usize, c: &Value, part: &str) -> Result<()> {
    ensure!(
        slot < 3
            && c["schema"] == "slop_ninja_local_generator_v1"
            && c["identity"] == TEST_MODELS[slot]
            && part == "test"
            && c["allowed_splits"] == json!(["test"])
            && c["rights"]["generation_permitted"] == true,
        "sealed Test generator scope/rights"
    );
    Ok(())
}
fn plan_check(p: &Value, parent: &Value, binding: &Value, percent: u64, id: &str) -> Result<()> {
    check_encoder_plan(p, parent, binding, percent, id)?;
    if percent == 0 {
        ensure!(
            p["request"]["messages"][0]["content"] == encoder_recipe::prompt(parent)?,
            "source-independent scratch recipe changed"
        );
    }
    Ok(())
}
fn jsonl(rs: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rs {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn assemble(stage: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let root = stage.join("final-test-v1");
    let selected = seal::validate(&root.join("seal.json"))?;
    let seal_sha = seal::sha(&root.join("seal.json"))?;
    let mut inputs = vec![json!({"path":root.join("seal.json"),"sha256":seal_sha})];
    let (admission, ab) = load(&root.join("admitted-v1/manifest.json"), &mut inputs)?;
    ensure!(
        admission["schema"] == "slop_ninja_last_block_test_admission_v1"
            && admission["complete"] == true
            && admission["test_text_opened"] == true
            && admission["test_used_for_selection"] == false
            && admission["selection_receipt_sha256"] == seal_sha,
        "sealed Test admission required"
    );
    let test_binding = json!({"selection_receipt_sha256":seal_sha,
        "admission_manifest_sha256":sha(&ab),"source_manifest_sha256":admission["source_manifest_sha256"]});
    // Refuse partial generation before reading any generated text.
    for name in TEST_SLOTS {
        let (m, _) = load(
            &root.join(format!("generation-v1/{name}/report.json")),
            &mut inputs,
        )?;
        ensure!(
            m["complete"] == true && m["test_opened"] == true,
            "generation incomplete"
        );
    }
    let read_rows = |name: &str, inputs: &mut Vec<Value>| -> Result<(Vec<Value>, Vec<u8>)> {
        let p = root.join("admitted-v1").join(name);
        let b = bytes(&p)?;
        ensure!(
            admission["files_sha256"][name] == sha(&b),
            "admitted records changed"
        );
        inputs.push(json!({"path":p,"sha256":sha(&b)}));
        Ok((records(&b)?, b))
    };
    let (humans, hb) = read_rows("humans.jsonl", &mut inputs)?;
    let (mut refs, _) = read_rows("references.jsonl", &mut inputs)?;
    let (source_exclusions, _) = read_rows("exclusions.jsonl", &mut inputs)?;
    ensure!(
        admission["query_records"] == humans.len()
            && admission["reference_records"] == refs.len()
            && admission["excluded_records"] == source_exclusions.len(),
        "source counts differ"
    );
    for r in humans.iter().chain(&refs) {
        ensure!(
            r["frozen_split"] == "test"
                && r["origin"] == "human_only"
                && r["source"]["source_manifest_sha256"] == admission["source_manifest_sha256"]
                && r["text_sha256"] == sha(str_at(r, "text")?.as_bytes()),
            "source binding changed"
        );
        if r["purpose"] == "query" {
            supervision::Supervision::parse(r)?;
        }
    }
    ensure!(
        humans.iter().all(|r| r["purpose"] == "query")
            && refs.iter().all(|r| r["purpose"] == "reference"),
        "source purpose"
    );
    let genroot = root.join("generation-v1");
    let producer = bytes(&genroot.join("source/a_balanced_generate.rs"))?;
    let helper = bytes(&genroot.join("source/a_balanced_generate/encoder_recipe.rs"))?;
    ensure!(
        helper == include_bytes!("../../../../examples/a_balanced_generate/encoder_recipe.rs"),
        "captured scratch recipe differs from validator"
    );
    let producer_sha = sha(&producer);
    inputs
        .push(json!({"path":genroot.join("source/a_balanced_generate.rs"),"sha256":producer_sha}));
    inputs.push(json!({"path":genroot.join("source/a_balanced_generate/encoder_recipe.rs"),"sha256":sha(&helper)}));
    let mut all = humans.clone();
    let mut prompts = BTreeMap::new();
    let mut reports = Vec::new();
    for (slot, name) in TEST_SLOTS.iter().enumerate() {
        let path = root.join(format!("configs/{name}.json"));
        let (config, cb) = load(&path, &mut inputs)?;
        slot_check(slot, &config, "test")?;
        ensure!(
            array(&selected, "binding_files")?
                .iter()
                .any(|p| p["path"] == path.to_str().unwrap() && p["sha256"] == sha(&cb)),
            "unsealed generator config"
        );
        let dir = genroot.join(name);
        let (binding, _) = load(&dir.join("binding.json"), &mut inputs)?;
        ensure!(
            binding["scratch_implementation_sha256"] == sha(&helper),
            "scratch helper changed"
        );
        let policy = GenerationPolicy {
            requested_tasks: humans.len() * 5,
            producer_sha256: &producer_sha,
            slot_check,
            plan_check,
        };
        let (generated, report) = generator_for_sealed_encoder_test(
            &dir,
            slot,
            &config,
            &sha(&cb),
            &humans,
            &sha(&hb),
            &mut inputs,
            &mut prompts,
            &policy,
            &test_binding,
        )?;
        all.extend(generated);
        reports.push(report);
    }
    // Source admission alone is insufficient: generated prose and scratch prompts
    // are audited too, including failures' prompts. The sealed plan excludes
    // entire conflicting families with receipts and no replacement sources.
    let audit = overlap::check(&all, &refs, &prompts, &admission, &mut inputs)?;
    let accepted = all.len();
    let conflicts = array(&audit, "conflicting_families")?;
    let mut excluded_rows = Vec::new();
    let mut exclusions = Vec::new();
    all.retain(|r| {
        if conflicts.contains(&r["source_group"]) {
            exclusions.push(json!({"id":r["id"],"source_group":r["source_group"],
                "reason":"sealed_plan_cross_split_family_overlap","text_sha256":r["text_sha256"]}));
            excluded_rows.push(r.clone());
            false
        } else {
            true
        }
    });
    let mut excluded_refs = Vec::new();
    refs.retain(|r| {
        if conflicts.contains(&r["source_group"]) {
            excluded_refs.push(r.clone());
            false
        } else {
            true
        }
    });
    let (mut rows, duplicate_exclusions, duplicate_rows) = overlap::within_test(all, &refs)?;
    exclusions.extend(duplicate_exclusions);
    excluded_rows.extend(duplicate_rows);
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let groups = rows
        .iter()
        .map(|r| Ok((str_at(r, "source_group")?.to_owned(), json!("test"))))
        .collect::<Result<BTreeMap<_, _>>>()?;
    // The admitted roster is immutable. Missing source examples and all 15
    // dependent generation tasks remain in requested-population accounting.
    let rb = jsonl(&rows)?;
    let refs_bytes = jsonl(&refs)?;
    let mut requests = rows
        .iter()
        .chain(&refs)
        .map(|r| json!({"id":r["id"],"text":r["text"]}))
        .collect::<Vec<_>>();
    requests.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let requests_bytes = jsonl(&requests)?;
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":"a-last-block-final-test-20261026",
        "complete":true,"test_text_opened":true,"test_used_for_selection":false,
        "selection_receipt_sha256":seal_sha,"records_sha256":sha(&rb),"test_records_sha256":sha(&rb),
        "records":rows.len(),"non_test_ids":[],"test_ids":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
        "split_assignments":groups,"requested_human_records":40,"requested_generation_records":600,
        "requested_records":640,"accepted_before_exclusions":accepted,"excluded":exclusions,
        "source_exclusions":source_exclusions,"generation_reports":reports,
        "extraction_failures":admission["extraction_failures"],"sources":inputs,
        "source_manifest_sha256":admission["source_manifest_sha256"],
        "frozen_selection_sha256":selected["source_plan"]["sha256"],
        "requested_reference_records":4,"separate_reference_records":refs.len(),
        "references_file":"references.jsonl","references_sha256":sha(&refs_bytes),
        "excluded_references_sha256":sha(&jsonl(&excluded_refs)?),"excluded_reference_records":excluded_refs.len(),
        "reference_ids":refs.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
        "excluded_records_file":"excluded-records.jsonl","excluded_records_sha256":sha(&jsonl(&excluded_rows)?),
        "requests_sha256":sha(&requests_bytes),"overlap_audit":audit,"clean_holdout":false,
        "historical_human_proxy":true,"exposure":"New work families relative to retained overlap ledger; pretraining and full grammar exposure are not universally known. Historical proxies do not authenticate modern human-only production.",
        "labels_are_not_inputs":true,"test_binding":test_binding,
        "implementation_sha256":sha(include_bytes!("assemble_last_block_test.rs")),
        "support_sha256":{"generation":sha(include_bytes!("assemble_balanced_dataset.rs")),
            "overlap":sha(include_bytes!("assemble_last_block_test/overlap.rs")),
            "supervision":sha(include_bytes!("prepare_supervised_forest/supervision.rs"))}});
    let mb = serde_json::to_vec_pretty(&manifest)?;
    let dataset = supervision::Dataset::parse_test(&mb)?;
    dataset.require_exact_ids(rows.iter().map(|r| r["id"].as_str().unwrap()))?;
    for r in &rows {
        dataset.validate_metadata(str_at(r, "id")?, r)?;
        dataset.supervision(r)?;
    }
    let ref_manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1",
        "reference_transform_only":true,"final_test_reference_transform_only":true,
        "records_sha256":sha(&refs_bytes),"test_ids":refs.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
        "non_test_ids":[],"split_assignments":refs.iter().map(|r|(r["source_group"].as_str().unwrap(),"test")).collect::<BTreeMap<_,_>>(),
        "provenance":{"selection_receipt_sha256":seal_sha,"query_manifest_sha256":sha(&mb)}});
    let ref_manifest_bytes = serde_json::to_vec_pretty(&ref_manifest)?;
    let rd = supervision::Dataset::parse_final_references(&ref_manifest_bytes)?;
    rd.require_exact_ids(refs.iter().map(|r| r["id"].as_str().unwrap()))?;
    for r in &refs {
        rd.validate_metadata(str_at(r, "id")?, r)?;
        rd.supervision(r)?;
    }
    fs::create_dir(out)?;
    for (name, b) in [
        ("records.jsonl", rb),
        ("references.jsonl", refs_bytes),
        ("requests.jsonl", requests_bytes),
        ("excluded-records.jsonl", jsonl(&excluded_rows)?),
        ("excluded-references.jsonl", jsonl(&excluded_refs)?),
        ("exclusions.jsonl", jsonl(&exclusions)?),
        ("seal.json", bytes(&root.join("seal.json"))?),
        ("reference-manifest.json", ref_manifest_bytes),
        ("manifest.json", mb),
    ] {
        write_new(&out.join(name), &b)?;
    }
    fs::File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"complete":true,"records":rows.len(),"requested":640,"references":refs.len(),"excluded":exclusions.len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 2,
        "assemble_last_block_test STAGE_ROOT NEW_OUTPUT"
    );
    assemble(&Path::new(&a[0]).canonicalize()?, Path::new(&a[1]))
}
