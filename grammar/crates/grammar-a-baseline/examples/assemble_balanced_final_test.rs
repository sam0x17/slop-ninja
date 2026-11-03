//! Receipt-gated reserved final-Test assembly, entirely offline.
//! assemble_balanced_final_test PLAN.json NEW_OUTPUT
//! See assemble_balanced_final_test/README.md for the mandatory plan/ledger.
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::Path,
};
#[allow(dead_code, unused_attributes)]
#[path = "assemble_balanced_dataset.rs"]
mod balanced;
use balanced::{
    array, bytes, compact_sha, counter, digest_at, ids, load, number, records, sha, str_at,
    unique_rows, write_new,
};
#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod evaluation_binding;
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
// Reuse the exact v2 recipe and supplemental runtime/attempt validators.
#[allow(dead_code)]
#[path = "assemble_book_augmentation/generation.rs"]
mod evidence;
#[path = "assemble_balanced_final_test/generation.rs"]
mod generation;
#[path = "assemble_balanced_final_test/overlap.rs"]
mod overlap;
#[allow(dead_code)]
#[path = "assemble_book_augmentation/recipe.rs"]
mod recipe;
#[path = "assemble_balanced_final_test/sources.rs"]
mod sources;
#[cfg(test)]
#[path = "assemble_balanced_final_test/tests.rs"]
mod tests;

const PARTS: [&str; 3] = ["train", "development", "calibration"];
const PERCENTAGES: [u64; 4] = [0, 20, 60, 100];
const SLOTS: [&str; 3] = ["known_model_1", "known_model_2", "heldout_Test_only"];
const MODELS: [&str; 3] = [
    balanced::MODELS[0],
    balanced::MODELS[1],
    "mistralai/Mistral-7B-Instruct-v0.3:1270d22c0fbb3d092fb725d4d96c457b7b687a5f5a715abe1e818da303e562b6",
];
const TSV: &str = include_str!("../../../../docs/a-balanced-sources-20261026.tsv");
const TSV_SHA: &str = "3271a46bf0b1dd70a2429ee5a9ce3002fba8c8235f7a629219162c46471f903f";
const PLAN_SCHEMA: &str = "slop_ninja_balanced_final_test_assembly_plan_v1";
const RECIPE: &str = "default_v2_narrative_scratch_20_60_100";

fn pinned(pin: &Value, inputs: &mut Vec<Value>) -> Result<Vec<u8>> {
    let path = Path::new(str_at(pin, "path")?);
    ensure!(path.is_absolute(), "absolute pinned path required");
    let b = bytes(path)?;
    ensure!(
        sha(&b) == digest_at(pin, "sha256")?,
        "pinned input changed: {}",
        path.display()
    );
    inputs.push(pin.clone());
    Ok(b)
}
fn pinned_json(pin: &Value, inputs: &mut Vec<Value>) -> Result<Value> {
    Ok(serde_json::from_slice(&pinned(pin, inputs)?)?)
}
fn jsonl(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn normalized(text: &str) -> String {
    sha(text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn new_dir(plan: &Value, out: &Path) -> Result<()> {
    ensure!(!out.try_exists()?, "new output directory required");
    let parent = out.parent().context("output parent")?.canonicalize()?;
    let candidate = parent.join(out.file_name().context("output basename")?);
    for g in array(plan, "generators")? {
        let run = Path::new(str_at(g, "run_directory")?).canonicalize()?;
        ensure!(
            !candidate.starts_with(run),
            "output inside immutable generation evidence"
        );
    }
    Ok(())
}

fn assemble(plan_path: &Path, out: &Path) -> Result<()> {
    let mut inputs = Vec::new();
    let (plan, plan_bytes) = load(plan_path, &mut inputs)?;
    ensure!(
        plan["schema"] == PLAN_SCHEMA
            && plan["recipe"] == RECIPE
            && plan["requested_human_records"] == 32
            && plan["requested_generation_records"] == 384
            && plan["requested_records"] == 416
            && plan["requested_reference_records"] == 4,
        "fixed final-Test plan/denominators required"
    );
    new_dir(&plan, out)?;
    let selection_bytes = pinned(&plan["selection"], &mut inputs)?;
    let selection = final_test_binding::validate_selection_receipt(Path::new(str_at(
        &plan["selection"],
        "path",
    )?))?;
    ensure!(
        serde_json::from_slice::<Value>(&selection_bytes)? == selection,
        "selection snapshot changed"
    );
    let selection_sha = sha(&selection_bytes);
    ensure!(
        plan["selection_receipt_sha256"] == selection_sha,
        "plan selection binding"
    );
    let source_plan = pinned(&plan["source_plan"], &mut inputs)?;
    ensure!(
        source_plan == TSV.as_bytes()
            && sha(&source_plan) == TSV_SHA
            && selection["source_plan"]["sha256"] == TSV_SHA,
        "exact original source reservations required"
    );
    // Metadata gates and complete reports are checked before reading query prose.
    let ledger = overlap::Ledger::load(&plan, &selection, &selection_sha, &mut inputs)?;
    let specs = array(&plan, "generators")?;
    ensure!(specs.len() == 3, "three exact generators required");
    let mut configs = Vec::new();
    for (slot, spec) in specs.iter().enumerate() {
        ensure!(
            spec["slot"] == SLOTS[slot]
                && spec["identity"] == MODELS[slot]
                && spec["requested"] == 128,
            "generator roster/order/task denominator"
        );
        let config = pinned_json(&spec["config"], &mut inputs)?;
        generation::check_slot(slot, &config, "test")?;
        let run = Path::new(str_at(spec, "run_directory")?);
        ensure!(run.is_absolute(), "absolute run directory");
        let (report, _) = load(&run.join("report.json"), &mut inputs)?;
        ensure!(
            report["schema"] == "slop_ninja_controlled_generation_report_v2"
                && report["complete"] == true
                && report["test_opened"] == true,
            "all generation reports must be complete Test reports"
        );
        configs.push(config);
    }
    let producer = pinned(&plan["generator_source"], &mut inputs)?;
    ensure!(
        sha(&producer)
            == sha(include_bytes!(
                "../../../../examples/a_balanced_generate.rs"
            )),
        "qualified generator source changed"
    );
    pinned(&plan["generator_binary"], &mut inputs)?;
    pinned(&plan["generation_protocol"], &mut inputs)?;
    let source = sources::read(&plan, &selection_sha, &mut inputs)?;
    let mut accepted = source.humans.clone();
    let mut reports = Vec::new();
    let mut prompts = BTreeMap::new();
    for (slot, (spec, config)) in specs.iter().zip(&configs).enumerate() {
        let (mut rows, report) = generation::read(
            spec,
            slot,
            config,
            &source.humans,
            &source.humans_sha,
            &sha(&producer),
            &mut inputs,
            &mut prompts,
        )?;
        accepted.append(&mut rows);
        reports.push(report);
    }
    unique_rows(&accepted)?;
    ledger.check(&source.works, &accepted, &source.references, &prompts)?;
    let before = accepted.len();
    let (rows, mut excluded, excluded_records) =
        overlap::within_test(accepted, &source.references)?;
    excluded.extend(source.query_exclusions.clone());
    let output = jsonl(&rows)?;
    let refs = jsonl(&source.references)?;
    let assigned = source
        .works
        .values()
        .map(|w| (str_at(w, "source_group").unwrap().to_owned(), json!("test")))
        .collect::<BTreeMap<_, _>>();
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":"a-balanced-final-test-20261026",
        "complete":true,"test_text_opened":true,"test_used_for_selection":false,
        "selection_receipt_sha256":selection_sha,"assembly_plan_sha256":sha(&plan_bytes),
        "records_sha256":sha(&output),"test_records_sha256":sha(&output),"records":rows.len(),
        "non_test_ids":[],"test_ids":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
        "split_assignments":assigned,"requested_human_records":32,"requested_generation_records":384,"requested_records":416,
        "accepted_before_exclusions":before,"excluded":excluded,"generation_reports":reports,
        "extraction_failures":source.extraction_failures,"sources":inputs,"source_manifest_sha256":source.source_sha,
        "query_manifest_sha256":source.query_sha,"frozen_selection_sha256":TSV_SHA,
        "requested_reference_records":4,"separate_reference_records":source.references.len(),
        "references_file":"references.jsonl","references_sha256":sha(&refs),"reference_exclusions":source.reference_exclusions,
        "reference_ids":source.references.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
        "overlap_ledger_sha256":plan["overlap_ledger"]["sha256"],"cross_split_collision_policy":"hard_fail_no_Test_only_removal",
        "excluded_records_file":"excluded-records.jsonl","excluded_records_sha256":sha(&jsonl(&excluded_records)?),
        "historical_human_proxy":true,"clean_holdout":false,"exposure":"Unknown pretraining and grammar exposure; historical proxies do not authenticate human-only production histories",
        "reference_policy":"Separate Test references only; never query/generation/fitting inputs",
        "implementation_sha256":sha(include_bytes!("assemble_balanced_final_test.rs")),
        "support_sha256":{"sources":sha(include_bytes!("assemble_balanced_final_test/sources.rs")),
            "generation":sha(include_bytes!("assemble_balanced_final_test/generation.rs")),"overlap":sha(include_bytes!("assemble_balanced_final_test/overlap.rs")),
            "balanced":sha(include_bytes!("assemble_balanced_dataset.rs")),"recipe":sha(include_bytes!("assemble_book_augmentation/recipe.rs")),
            "evidence":sha(include_bytes!("assemble_book_augmentation/generation.rs")),"supervision":sha(include_bytes!("prepare_supervised_forest/supervision.rs"))}});
    let mb = serde_json::to_vec_pretty(&manifest)?;
    let dataset = supervision::Dataset::parse_test(&mb)?;
    dataset.require_exact_ids(rows.iter().map(|r| r["id"].as_str().unwrap()))?;
    for r in &rows {
        dataset.validate_metadata(str_at(r, "id")?, r)?;
        dataset.supervision(r)?;
    }
    let accounting = evaluation_binding::upstream_accounting(
        &manifest,
        &rows
            .iter()
            .map(|r| str_at(r, "id").unwrap().to_owned())
            .collect(),
    )?;
    let report = json!({"schema":"slop_ninja_balanced_final_test_assembly_report_v1","complete":true,
        "selection_receipt_sha256":selection_sha,"manifest_sha256":sha(&mb),"accounting":accounting,
        "references":{"requested":4,"accepted":source.references.len(),"excluded":source.reference_exclusions},
        "source_families":16,"reference_families":2,"model_calls":0,"provider_calls":0});
    // All validations precede publication. Manifest is the last file written.
    fs::create_dir(out)?;
    for (name, b) in [
        ("records.jsonl", output),
        ("references.jsonl", refs),
        ("excluded-records.jsonl", jsonl(&excluded_records)?),
        ("exclusion-audit.jsonl", jsonl(&excluded)?),
        ("plan.json", plan_bytes),
        ("selection-receipt.json", selection_bytes),
        ("REPORT.json", serde_json::to_vec_pretty(&report)?),
        ("manifest.json", mb),
    ] {
        write_new(&out.join(name), &b)?;
    }
    File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"records":rows.len(),"requested":416,"references":source.references.len(),"out":out})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 2,
        "assemble_balanced_final_test PLAN.json NEW_OUTPUT"
    );
    assemble(Path::new(&a[0]), Path::new(&a[1]))
}
