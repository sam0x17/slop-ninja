//! Transform admitted Test through the unchanged fitting contract. Never fit.
//! STAGE_ROOT DATASET PREFIXES NEW_OUTPUT
#[allow(dead_code, unused_attributes)]
#[path = "assemble_balanced_dataset.rs"]
mod evidence;
#[allow(dead_code, unused_attributes)]
#[path = "prepare_last_block_fit.rs"]
mod prefix;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
use anyhow::{Context, Result, ensure};
use evidence::{bytes, counter, digest_at, number, records, sha, str_at, write_new};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
const PARTS: [&str; 3] = ["train", "development", "calibration"];
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&bytes(p)?)?)
}
fn jsonl(rs: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rs {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn numeric_row(r: &Value, contract: &Value) -> Result<Value> {
    ensure!(r["frozen_split"] == "test", "non-Test numeric source");
    let reference = r["purpose"] == "reference";
    ensure!(reference || r["purpose"] == "query", "unknown purpose");
    let mut targets = if reference {
        json!({"origin":null,"share":null,"human":null,"ai":null,"ai_involvement":null})
    } else {
        let s = supervision::Supervision::parse(r)?;
        json!({"origin":s.origin,"share":s.share,"human":null,"ai":null,"ai_involvement":s.origin!=0})
    };
    let mut roles = BTreeMap::new();
    for role in ["human", "ai"] {
        let present = &r[format!("{role}_role_present")];
        let identity = &r[format!("{role}_identity")];
        let profiles = contract[format!("{role}_profiles")]
            .as_array()
            .context("frozen profiles")?;
        let index = identity
            .as_str()
            .and_then(|id| profiles.iter().position(|p| p == id));
        if !reference && *present == true {
            targets[role] = json!(index);
        }
        roles.insert(
            role,
            json!({"present":present,"identity":identity,"known":index.is_some()}),
        );
    }
    Ok(
        json!({"id":r["id"],"source_group":r["source_group"],"frozen_split":"test",
        "purpose":r["purpose"],"targets":targets,"roles":roles,"text_sha256":r["text_sha256"],
        "exposed_to_grammar":r["exposed_to_grammar"],"acquisition_stratum":r["metadata"]["acquisition_stratum"],
        "documented_human_only":r["documented_human_only"],"cohort":"new_stage3_final_test",
        "normalized_text_sha256":sha(str_at(r,"text")?.split_whitespace().collect::<Vec<_>>().join(" ").as_bytes())}),
    )
}
fn run(stage: &Path, data: &Path, prefixes: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let seal_path = stage.join("final-test-v1/seal.json");
    let selected = seal::validate(&seal_path)?;
    let seal_sha = seal::sha(&seal_path)?;
    let fitting = stage.join("fit-bundle-v1");
    let fm = read(&fitting.join("manifest.json"))?;
    let cb = bytes(&fitting.join("contract.json"))?;
    ensure!(
        fm["contract_sha256"] == sha(&cb),
        "fitting contract changed"
    );
    let contract: Value = serde_json::from_slice(&cb)?;
    let dm = read(&data.join("manifest.json"))?;
    ensure!(
        dm["complete"] == true
            && dm["test_text_opened"] == true
            && dm["test_used_for_selection"] == false
            && dm["selection_receipt_sha256"] == seal_sha,
        "sealed assembled Test required"
    );
    let rb = bytes(&data.join("records.jsonl"))?;
    let refs = bytes(&data.join("references.jsonl"))?;
    ensure!(
        dm["records_sha256"] == sha(&rb) && dm["references_sha256"] == sha(&refs),
        "Test inputs changed"
    );
    ensure!(
        dm["requests_sha256"] == seal::sha(&data.join("requests.jsonl"))?,
        "prefix requests changed"
    );
    let qp = supervision::Dataset::parse_test(&bytes(&data.join("manifest.json"))?)?;
    let rp = supervision::Dataset::parse_final_references(&bytes(
        &data.join("reference-manifest.json"),
    )?)?;
    let query = records(&rb)?;
    let reference = records(&refs)?;
    qp.require_exact_ids(query.iter().map(|r| r["id"].as_str().unwrap_or("")))?;
    rp.require_exact_ids(reference.iter().map(|r| r["id"].as_str().unwrap_or("")))?;
    let p = prefix::Prefixes::load(prefixes, digest_at(&dm, "requests_sha256")?)?;
    ensure!(
        p.contract == contract["prefix_contract"],
        "Test prefix meaning differs from frozen fitting profile"
    );
    let mut ids = BTreeSet::new();
    let mut parts = BTreeMap::<String, Vec<Value>>::new();
    let mut excluded = Vec::new();
    let mut frozen_encodings = Vec::new();
    for (rs, validator) in [(query, &qp), (reference, &rp)] {
        for r in rs {
            let id = str_at(&r, "id")?;
            ensure!(ids.insert(id.to_owned()), "duplicate Test/reference id");
            validator.validate_metadata(id, &r)?;
            validator.supervision(&r)?;
            let mut row = numeric_row(&r, &contract)?;
            let Some((bound, h)) = p.get(id, digest_at(&r, "text_sha256")?)? else {
                excluded.push(
                    json!({"id":id,"source_group":r["source_group"],"purpose":r["purpose"],
                "reason":p.unsupported_reason(id)?,"text_sha256":r["text_sha256"]}),
                );
                continue;
            };
            row["prefix"] = bound;
            frozen_encodings.push(json!({"id":id,"text_sha256":r["text_sha256"],"h":h}));
            parts
                .entry(
                    if r["purpose"] == "reference" {
                        "reference"
                    } else {
                        "test"
                    }
                    .into(),
                )
                .or_default()
                .push(row);
        }
    }
    p.require_ids(&ids)?;
    let mut files = BTreeMap::new();
    let mut populations = BTreeMap::new();
    let mut payloads = Vec::new();
    for name in ["test", "reference"] {
        let rs = parts.entry(name.into()).or_default();
        rs.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let b = jsonl(rs)?;
        let f = format!("{name}.jsonl");
        files.insert(f.clone(), sha(&b));
        payloads.push((f, b));
        populations.insert(name,json!({"rows":rs.len(),"families":rs.iter().map(|r|r["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len()}));
    }
    let b = jsonl(&frozen_encodings)?;
    files.insert("frozen-encodings.jsonl".into(), sha(&b));
    payloads.push(("frozen-encodings.jsonl".into(), b));
    let eb = serde_json::to_vec_pretty(&excluded)?;
    let m = json!({"schema":"slop_ninja_last_block_test_dataset_v1","complete":true,"final_test_opened":true,
        "test_used_for_selection":false,"normalization_refit":false,"profile_enrollment_changed":false,
        "selection_receipt_sha256":seal_sha,"selection_sha256":selected["selection"]["sha256"],
        "fitting_dataset_manifest_sha256":seal::sha(&fitting.join("manifest.json"))?,
        "contract_sha256":sha(&cb),"files_sha256":files,"populations":populations,"requested":ids.len(),
        "supported":frozen_encodings.len(),"excluded":excluded.len(),"exclusions_sha256":sha(&eb),
        "upstream_requested_queries":dm["requested_records"],"upstream_requested_references":dm["requested_reference_records"],
        "source_dataset":{"path":data.join("manifest.json"),"sha256":seal::sha(&data.join("manifest.json"))?},
        "prefix_receipt":{"path":prefixes.join("manifest.json"),"sha256":seal::sha(&prefixes.join("manifest.json"))?},
        "implementation_sha256":sha(include_bytes!("prepare_last_block_test.rs")),
        "prefix_validator_sha256":sha(include_bytes!("prepare_last_block_fit.rs")),
        "learned_inputs":"unchanged text encoder; CFG coordinates remain zero; metadata and targets never learned inputs"});
    fs::create_dir(out)?;
    for (n, b) in payloads {
        write_new(&out.join(n), &b)?;
    }
    write_new(&out.join("contract.json"), &cb)?;
    write_new(&out.join("exclusions.json"), &eb)?;
    write_new(&out.join("seal.json"), &bytes(&seal_path)?)?;
    write_new(&out.join("manifest.json"), &serde_json::to_vec_pretty(&m)?)?;
    println!(
        "{}",
        json!({"complete":true,"populations":populations,"excluded":excluded.len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 4, "STAGE DATASET PREFIXES NEW_OUTPUT");
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_and_withheld_identity_do_not_enroll_or_create_targets() {
        let c = json!({"human_profiles":["known_writer"],"ai_profiles":["known_model"]});
        let r = json!({"id":"synthetic-reference","source_group":"separate-work","purpose":"reference",
            "frozen_split":"test","text":"Synthetic reference text.","human_role_present":true,
            "human_identity":"withheld_writer","ai_role_present":false,"ai_identity":null});
        let n = numeric_row(&r, &c).unwrap();
        assert_eq!(n["roles"]["human"]["known"], false);
        assert!(
            n["targets"]
                .as_object()
                .unwrap()
                .values()
                .all(Value::is_null)
        );
        let mut wrong = r;
        wrong["frozen_split"] = json!("train");
        assert!(numeric_row(&wrong, &c).is_err());
    }
}
