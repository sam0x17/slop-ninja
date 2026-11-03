//! Assemble the reserved non-Test encoder experiment from terminal receipts.
//! STAGE_ROOT NEW_OUTPUT. Reads no Test prose; every local generation task must
//! have an accepted record or a checked terminal failure. No network calls.
#![recursion_limit = "256"]
#[allow(dead_code, unused_attributes)]
#[path = "assemble_balanced_dataset.rs"]
mod evidence;
use anyhow::{Result, ensure};
use evidence::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
fn slot_check(slot: usize, c: &Value, part: &str) -> Result<()> {
    ensure!(
        slot < 3 && c["identity"] == MODELS[slot],
        "generator identity changed"
    );
    ensure!(
        ["train", "development", "calibration"].contains(&part),
        "Test input forbidden"
    );
    let expected = if slot == 2 {
        json!(["calibration"])
    } else {
        json!(["train", "development", "calibration"])
    };
    ensure!(
        c["allowed_splits"] == expected && (slot != 2 || part == "calibration"),
        "generator partition scope"
    );
    ensure!(
        c["rights"]["generation_permitted"] == true,
        "generator rights missing"
    );
    Ok(())
}
fn run(stage: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let mut inputs = Vec::new();
    let (protocol, pb) = load(&stage.join("protocol-v1.json"), &mut inputs)?;
    ensure!(
        protocol["schema"] == "slop_ninja_encoder_comparison_protocol_v1",
        "protocol schema"
    );
    let (admission, _) = load(&stage.join("admitted-v1/manifest.json"), &mut inputs)?;
    ensure!(
        admission["complete"] == true && admission["test_text_opened"] == false,
        "admission incomplete"
    );
    let mut base = Vec::new();
    let mut human_bytes = Vec::new();
    for name in ["humans.jsonl", "references.jsonl"] {
        let b = bytes(&stage.join("admitted-v1").join(name))?;
        ensure!(
            admission["files_sha256"][name] == sha(&b),
            "source admission changed"
        );
        inputs.push(json!({"path":stage.join("admitted-v1").join(name),"sha256":sha(&b)}));
        base.extend(records(&b)?);
        if name == "humans.jsonl" {
            human_bytes = b;
        }
    }
    let humans = records(&human_bytes)?;
    unique_rows(&base)?;
    for r in &base {
        ensure!(
            r["origin"] == "human_only"
                && ["train", "development", "calibration"].contains(&str_at(r, "frozen_split")?),
            "unexpected source history/partition"
        );
        ensure!(
            r["text_sha256"] == sha(str_at(r, "text")?.as_bytes()),
            "source hash"
        );
    }
    let genroot = stage.join("generation-v1");
    let source = bytes(&genroot.join("source/a_balanced_generate.rs"))?;
    let helper = bytes(&genroot.join("source/a_balanced_generate/encoder_recipe.rs"))?;
    let producer_sha = sha(&source);
    let helper_sha = sha(&helper);
    inputs
        .push(json!({"path":genroot.join("source/a_balanced_generate.rs"),"sha256":producer_sha}));
    inputs.push(json!({"path":genroot.join("source/a_balanced_generate/encoder_recipe.rs"),"sha256":helper_sha}));
    let mut reports = Vec::new();
    let mut prompts = BTreeMap::new();
    for (slot, name) in SLOTS.iter().enumerate() {
        let (config, cb) = load(
            &genroot.join("configs").join(format!("{name}.json")),
            &mut inputs,
        )?;
        slot_check(
            slot,
            &config,
            if slot == 2 { "calibration" } else { "train" },
        )?;
        let dir = genroot.join(name);
        let (binding, _) = load(&dir.join("binding.json"), &mut inputs)?;
        ensure!(
            binding["recipe"] == "encoder_stage3_v1"
                && binding["scratch_implementation_sha256"] == helper_sha,
            "scratch recipe changed"
        );
        let requested = humans
            .iter()
            .filter(|r| slot != 2 || r["frozen_split"] == "calibration")
            .count()
            * 5;
        let policy = GenerationPolicy {
            requested_tasks: requested,
            producer_sha256: &producer_sha,
            slot_check,
            plan_check: check_encoder_plan,
        };
        let (generated, report) = generator_for_roster_recipe(
            &dir,
            slot,
            &config,
            &sha(&cb),
            &humans,
            &sha(&human_bytes),
            &mut inputs,
            &mut prompts,
            &policy,
            true,
        )?;
        base.extend(generated);
        reports.push(report);
    }
    let mut groups = BTreeMap::new();
    let mut duplicate_text = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut splits = BTreeMap::<String, usize>::new();
    for r in &base {
        let id = str_at(r, "id")?;
        ensure!(ids.insert(id.to_owned()), "duplicate output id");
        let group = str_at(r, "source_group")?;
        let part = str_at(r, "frozen_split")?;
        if let Some(old) = groups.insert(group.to_owned(), part.to_owned()) {
            ensure!(old == part, "family crosses partitions");
        }
        let normalized = sha(str_at(r, "text")?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes());
        if let Some(old) = duplicate_text.insert(normalized, part.to_owned()) {
            ensure!(
                old == part,
                "cross-partition duplicate needs explicit exclusion accounting"
            );
        }
        *splits
            .entry(format!("{}:{}", part, str_at(r, "purpose")?))
            .or_default() += 1;
    }
    base.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut rb = Vec::new();
    let mut requests = Vec::new();
    for r in &base {
        serde_json::to_writer(&mut rb, r)?;
        rb.push(b'\n');
        serde_json::to_writer(&mut requests, &json!({"id":r["id"],"text":r["text"]}))?;
        requests.push(b'\n');
    }
    fs::create_dir(out)?;
    write_new(&out.join("records.jsonl"), &rb)?;
    write_new(&out.join("requests.jsonl"), &requests)?;
    let report = json!({"schema":"slop_ninja_encoder_supervised_dataset_v1","complete":true,"records":base.len(),"records_sha256":sha(&rb),"requests_sha256":sha(&requests),"source_families":groups.len(),"split_assignments":groups,"splits":splits,"test_text_opened":false,"protocol_sha256":sha(&pb),"generation_reports":reports,"requested_human_records":admission["requested_non_test"],"human_extraction_failures":admission["extraction_failures"],"excluded_human_records":admission["excluded_records"],"inputs":inputs,"implementation_sha256":sha(include_bytes!("assemble_encoder_dataset.rs")),"validator_sha256":sha(include_bytes!("assemble_balanced_dataset.rs")),"share_labels":"measured final replacement-span word fraction, independent of origin history; requested percent not target","limitations":admission["limitations"]});
    write_new(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"records":base.len(),"families":report["source_families"],"splits":report["splits"]})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 2, "STAGE_ROOT NEW_OUTPUT");
    run(Path::new(&a[0]), Path::new(&a[1]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eighty_percent_recipe_cannot_be_reinterpreted_as_legacy_plan() {
        let p = json!({"id":"synthetic-parent","text":"A synthetic passage. More words.","text_sha256":sha(b"A synthetic passage. More words.")});
        let b = json!({"generator_config":{"alias":"synthetic-local"}});
        let prompt = "Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.\n\nA synthetic passage.";
        let plan = json!({"id":"synthetic-child","parent_id":p["id"],"source_sha256":p["text_sha256"],"source_record_sha256":compact_sha(&p).unwrap(),"binding_sha256":compact_sha(&b).unwrap(),"mode":"controlled_rewrite","requested_source_span_percent":80,"position":position("synthetic-parent",80),"sham_reassembly_exact":true,"source_byte_span":[0,20],"request":{"model":"synthetic-local","seed":1729,"temperature":0.7,"top_p":0.9,"max_tokens":1024,"stream":false,"chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none","messages":[{"role":"user","content":prompt}]}});
        check_encoder_plan(&plan, &p, &b, 80, "synthetic-child").unwrap();
        assert!(check_plan(&plan, &p, &b, 80, "synthetic-child").is_err());
    }
    #[test]
    fn heldout_generator_cannot_enter_train_or_development() {
        let c = json!({"identity":MODELS[2],"allowed_splits":["calibration"],"rights":{"generation_permitted":true}});
        assert!(slot_check(2, &c, "calibration").is_ok());
        for part in ["train", "development", "test"] {
            assert!(slot_check(2, &c, part).is_err());
        }
    }
}
