//! Freeze identical non-Test comparator inputs; no fitting or provider calls.
//! STAGE PRIOR_SELECTION V6_INITIALIZER NEW_OUT
#[path = "prepare_last_block_comparators/test.rs"]
mod final_test;
#[allow(dead_code, unused_imports)]
#[path = "prepare_last_block_fit.rs"]
mod prefix;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::{shared::SharedInput, warm_shared::WarmSharedModel};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn jsonl(p: &Path, rs: &[Value]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    for r in rs {
        serde_json::to_writer(&mut f, r)?;
        f.write_all(b"\n")?;
    }
    f.sync_all()?;
    Ok(())
}
fn run(stage: &Path, old: &Path, init: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new comparator root required");
    let data = stage.join("dataset-v1");
    let bundle = stage.join("fit-bundle-v1");
    let dm = read(&data.join("manifest.json"))?;
    let fm = read(&bundle.join("manifest.json"))?;
    let c = read(&bundle.join("contract.json"))?;
    ensure!(
        dm["schema"] == "slop_ninja_encoder_supervised_dataset_v1"
            && dm["complete"] == true
            && dm["test_text_opened"] == false
            && dm["records_sha256"] == sha(&data.join("records.jsonl"))?,
        "frozen non-Test source required"
    );
    ensure!(
        fm["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && fm["complete"] == true
            && fm["final_test_opened"] == false
            && fm["contract_sha256"] == sha(&bundle.join("contract.json"))?
            && fm["source_dataset"]["sha256"] == sha(&data.join("manifest.json"))?,
        "fitting bundle binding changed"
    );
    let old_selection = read(old)?;
    let initializer = read(init)?;
    ensure!(
        old_selection["selected_candidate"] == "warm_text"
            && old_selection["test_used_for_selection"] == false
            && old_selection["model_calibration_frozen"] == true,
        "fixed prior A selection required"
    );
    ensure!(
        initializer["schema"] == "slop_ninja_v6_classifier_init_v1"
            && initializer["artifact_id"] == c["parent_artifact_id"]
            && initializer["dimension"] == 768
            && initializer["origin_order"] == json!(["human_only", "model_only", "mixed"]),
        "immutable V6 initializer"
    );
    let prefixes = prefix::Prefixes::load(
        &stage.join("prefixes-v1"),
        &sha(&data.join("requests.jsonl"))?,
    )?;
    ensure!(
        prefixes.contract == c["prefix_contract"],
        "encoder runtime/profile differs"
    );
    let mut source = BTreeMap::new();
    for r in rows(&data.join("records.jsonl"))? {
        ensure!(
            source.insert(s(&r, "id")?.to_owned(), r).is_none(),
            "duplicate source id"
        );
    }
    let candidates = old_selection["candidates"]
        .as_array()
        .context("prior candidates")?
        .iter()
        .filter(|r| r["candidate"] == "warm_text")
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 1,
        "prior selected candidate missing/duplicated"
    );
    let prior = candidates[0];
    let numeric = Path::new(s(prior, "numeric_path")?);
    ensure!(
        sha(&numeric.join("contract.json"))? == prior["contract_file_sha256"],
        "prior contract changed"
    );
    let old_contract = read(&numeric.join("contract.json"))?;
    let mut models = Vec::new();
    let mut fixed = Vec::new();
    let mut seeds = BTreeSet::new();
    for r in prior["runs"].as_array().context("prior runs")? {
        let path = Path::new(s(r, "model_path")?);
        let weights = path.join("weights.json");
        let w = read(&weights)?;
        let seed = r["seed"].as_u64().context("seed")?;
        ensure!(
            seeds.insert(seed)
                && sha(&weights)? == r["weights_sha256"]
                && sha(&path.join("fit.json"))? == r["fit_sha256"]
                && w["input_mode"] == "text"
                && w["control"] == "joint"
                && w["contract_sha256"] == prior["contract_sha256"],
            "fixed prior artifact changed"
        );
        models.push((
            seed,
            WarmSharedModel::from_json(&fs::read(&weights)?)?,
            w.clone(),
        ));
        fixed.push(json!({"seed":seed,"weights_path":weights,"weights_sha256":r["weights_sha256"],"fit_sha256":r["fit_sha256"],
            "profiles":w["profiles"],"thresholds":w["thresholds"],"temperature":w["temperature"],"contract_sha256":w["contract_sha256"]}));
    }
    ensure!(
        seeds == BTreeSet::from([17, 29, 41]),
        "three fixed seeds required"
    );
    fs::create_dir(out)?;
    put(&out.join("prior-contract.json"), &old_contract)?;
    let mut roster = Vec::new();
    let mut part_receipts = BTreeMap::new();
    for part in ["development", "calibration", "reference"] {
        let input = bundle.join(format!("{part}.jsonl"));
        ensure!(
            sha(&input)? == fm["files_sha256"][format!("{part}.jsonl")],
            "bound partition changed"
        );
        let mut allowed = Vec::new();
        let mut originals = Vec::new();
        let mut predictions = BTreeMap::<u64, BTreeMap<String, Value>>::new();
        let truth = rows(&input)?;
        for r in &truth {
            let id = s(r, "id")?;
            let original = source.get(id).context("missing original")?;
            ensure!(
                (if part == "reference" {
                    r["purpose"] == "reference"
                } else {
                    r["purpose"] == "query" && r["frozen_split"] == part
                }) && original["text_sha256"] == r["text_sha256"]
                    && original["frozen_split"] == r["frozen_split"]
                    && original["purpose"] == r["purpose"]
                    && original["source_group"] == r["source_group"]
                    && hex::encode(Sha256::digest(s(original, "text")?.as_bytes()))
                        == s(r, "text_sha256")?,
                "query/source/purpose mismatch"
            );
            let (binding, h) = prefixes
                .get(id, s(r, "text_sha256")?)?
                .context("unsupported comparator input; retain coverage failure")?;
            ensure!(
                binding == r["prefix"],
                "different prefix from fitting experiment"
            );
            roster.push(json!({"id":id,"part":part,"text_sha256":r["text_sha256"],"prefix_sha256":binding["sha256"],"h_raw_f32":h}));
            for (seed, model, w) in &models {
                let mut x = h.clone();
                x.resize(2588, 0.);
                let numeric = SharedInput {
                    contract_sha256: s(w, "contract_sha256")?.into(),
                    x,
                };
                let mut pred = serde_json::to_value(model.predict(&numeric)?)?;
                pred["encoding"] = json!(model.encode(&numeric)?);
                predictions
                    .entry(*seed)
                    .or_default()
                    .insert(id.into(), pred);
            }
            if part != "reference" {
                ensure!(
                    original["rights"]["external_evaluation"] == true,
                    "Pangram permission absent"
                );
                allowed.push(id.to_owned());
                originals.push(original.clone());
            }
        }
        for (seed, pred) in predictions {
            put(&out.join(format!("prior-{seed}-{part}.json")), &json!(pred))?;
        }
        if part != "reference" {
            let path = out.join(format!("pangram-{part}.jsonl"));
            jsonl(&path, &originals)?;
            put(
                &out.join(format!("pangram-{part}-manifest.json")),
                &json!({"input_sha256":sha(&path)?,"frozen_split":part,
                "allowed_ids":allowed,"source_dataset_manifest_sha256":sha(&data.join("manifest.json"))?,"new_test_opened":false,"purpose":"fixed same-document comparator; not ground truth"}),
            )?;
        }
        part_receipts.insert(part,json!({"requested":truth.len(),"supported":truth.len(),"unsupported":0,"truth_sha256":sha(&input)?}));
    }
    jsonl(&out.join("v6-numeric-requests.jsonl"), &roster)?;
    let files = fs::read_dir(out)?
        .map(|e| {
            let p = e?.path();
            Ok((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                sha(&p)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_fixed_comparators_v1","complete":true,
        "dataset_manifest_sha256":sha(&bundle.join("manifest.json"))?,"current_contract_sha256":fm["contract_sha256"],"prior_selection_sha256":sha(old)?,
        "v6_initializer_path":init.canonicalize()?,"v6_initializer_sha256":sha(init)?,"fixed_prior_runs":fixed,"prefix_contract":prefixes.contract,
        "partitions":part_receipts,"files_sha256":files,"final_test_opened":false,"fitting_or_calibration_performed":false,
        "runtime":"cached qualified CUDA original V6 h; unchanged Rust f64 prior shared heads; V6 classifier forward still required",
        "learned_inputs":"h only plus constant-zero CFG block for prior A; all IDs/hashes/provenance outside inputs",
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("prepare_last_block_comparators.rs")))}),
    )?;
    println!(
        "{}",
        json!({"complete":true,"parts":part_receipts,"fixed_prior_seeds":seeds})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.len() == 3 && a[0] == "--test" {
        return final_test::run(Path::new(&a[1]), Path::new(&a[2]));
    }
    ensure!(a.len() == 4, "STAGE PRIOR_SELECTION V6_INITIALIZER NEW_OUT");
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
