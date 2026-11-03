//! Freeze the numeric/prefix interface for the matched last-block experiment.
//! STAGE_ROOT NEW_DATASET NEW_PREFIXES NEW_OUTPUT
//! Only retained Train plus the new non-Test cohort are admitted. No old Dev,
//! CAL or Test numeric files are read. Rust owns all labels and normalization.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};
const PARTS: [&str; 3] = ["train", "development", "calibration"];
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn str_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {k}"))
}
fn digest_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = str_at(v, k)?;
    ensure!(
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid hash {k}"
    );
    Ok(s)
}
fn counter(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64()
        .with_context(|| format!("missing counter {k}"))
}
fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite() && n >= 0., "nonfinite/negative target");
    Ok(n)
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn jput(p: &Path, v: &Value) -> Result<()> {
    put(p, &serde_json::to_vec_pretty(v)?)
}
fn pin(p: &Path) -> Result<Value> {
    Ok(json!({"path":p.canonicalize()?,"sha256":sha(&fs::read(p)?)}))
}
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;

fn parse_tensor(b: &[u8], n: usize, expected: &Value) -> Result<Vec<f64>> {
    ensure!(b.len() >= 8, "safetensors header missing");
    let header = u64::from_le_bytes(b[..8].try_into()?) as usize;
    ensure!(
        header <= 1024 * 1024 && 8 + header <= b.len(),
        "safetensors header bound"
    );
    let h: Value = serde_json::from_slice(&b[8..8 + header])?;
    let object = h.as_object().context("tensor header object")?;
    ensure!(
        object.len() == 2
            && object.contains_key("prefix")
            && object.contains_key("frozen_encoding"),
        "unexpected tensors"
    );
    let mut used = BTreeSet::new();
    let mut result = Vec::new();
    for (key, shape, size) in [
        ("prefix", json!([1, n, 768]), n * 768),
        ("frozen_encoding", json!([1, 768]), 768),
    ] {
        let v = &h[key];
        ensure!(
            v["dtype"] == "F32" && v["shape"] == shape,
            "tensor dtype/shape"
        );
        let offsets = v["data_offsets"].as_array().context("offsets")?;
        ensure!(offsets.len() == 2, "offset pair");
        let lo = offsets[0].as_u64().context("lo")? as usize;
        let hi = offsets[1].as_u64().context("hi")? as usize;
        ensure!(
            hi >= lo && hi - lo == size * 4 && hi <= b.len() - 8 - header,
            "tensor data bounds"
        );
        used.insert((lo, hi));
        let values = b[8 + header + lo..8 + header + hi]
            .chunks_exact(4)
            .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
            .collect::<Vec<_>>();
        ensure!(
            values.iter().all(|x| x.is_finite()),
            "nonfinite prefix value"
        );
        if key == "frozen_encoding" {
            let e = expected.as_array().context("frozen encoding")?;
            ensure!(e.len() == 768, "encoding dimension");
            for (actual, declared) in values.iter().zip(e) {
                let y = declared.as_f64().context("encoding coordinate")?;
                ensure!(
                    y.is_finite() && f64::from(*actual) == y,
                    "JSON encoding differs from tensor"
                );
            }
            result = values.into_iter().map(f64::from).collect();
        }
    }
    let ranges = used.into_iter().collect::<Vec<_>>();
    ensure!(
        ranges.len() == 2
            && ranges[0].0 == 0
            && ranges[0].1 == ranges[1].0
            && ranges[1].1 == b.len() - 8 - header,
        "tensor data overlap/gaps"
    );
    Ok(result)
}
pub(crate) struct Prefixes {
    pub(crate) contract: Value,
    contract_sha: String,
    entries: BTreeMap<String, Value>,
    root: std::path::PathBuf,
    receipt: Value,
}
impl Prefixes {
    pub(crate) fn require_ids(&self, ids: &BTreeSet<String>) -> Result<()> {
        ensure!(*ids == self.entries.keys().cloned().collect(), "prefix population not exact");
        Ok(())
    }
    pub(crate) fn unsupported_reason(&self, id: &str) -> Result<&Value> {
        let r = self.entries.get(id).context("missing prefix id")?;
        ensure!(r["status"] == "unsupported", "not an unsupported prefix");
        Ok(&r["reason"])
    }
    pub(crate) fn load(root: &Path, request_sha: &str) -> Result<Self> {
        let m = read(&root.join("manifest.json"))?;
        let b = read(&root.join("binding.json"))?;
        ensure!(
            m["schema"] == "slop_ninja_v6_prefix_export_v1"
                && m["complete"] == true
                && b["requests_sha256"] == request_sha,
            "prefix completion/input binding"
        );
        ensure!(
            m["binding_sha256"] == sha(&fs::read(root.join("binding.json"))?),
            "prefix binding changed"
        );
        let contract = &b["contract"];
        let contract_sha = digest_at(&b, "contract_sha256")?.to_owned();
        ensure!(
            m["contract_sha256"] == contract_sha
                && sha(&serde_json::to_vec(contract)?) == contract_sha,
            "prefix contract identity"
        );
        ensure!(
            contract["schema"] == "slop_ninja_v6_final_block_prefix_v1"
                && contract["last_layer"] == 21
                && contract["dtype"] == "float32"
                && contract["dimension"] == 768
                && contract["maximum_tokens_including_specials"] == 2048
                && contract["padding"] == "none_one_request"
                && contract["truncation"] == "forbidden",
            "unsupported prefix meaning"
        );
        let bytes = fs::read(root.join("rows.jsonl"))?;
        ensure!(m["rows_sha256"] == sha(&bytes), "prefix index changed");
        let mut entries = BTreeMap::new();
        let mut supported = 0;
        let mut unsupported = 0;
        for r in rows(&bytes)? {
            ensure!(r["contract_sha256"] == contract_sha, "row contract differs");
            match str_at(&r, "status")? {
                "ok" => supported += 1,
                "unsupported" => unsupported += 1,
                _ => anyhow::bail!("incomplete prefix row"),
            };
            ensure!(
                entries.insert(str_at(&r, "id")?.to_owned(), r).is_none(),
                "duplicate prefix id"
            );
        }
        ensure!(
            m["requested"] == entries.len()
                && b["requested"] == entries.len()
                && m["supported"] == supported
                && m["unsupported"] == unsupported,
            "prefix accounting"
        );
        Ok(Self {
            contract: contract.clone(),
            contract_sha,
            entries,
            root: root.canonicalize()?,
            receipt: pin(&root.join("manifest.json"))?,
        })
    }
    pub(crate) fn get(&self, id: &str, text_hash: &str) -> Result<Option<(Value, Vec<f64>)>> {
        let r = self.entries.get(id).context("missing prefix id")?;
        ensure!(r["text_sha256"] == text_hash, "prefix/text mismatch");
        if r["status"] == "unsupported" {
            return Ok(None);
        }
        let relative = Path::new(str_at(r, "prefix_path")?);
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "unsafe prefix path"
        );
        let path = self.root.join(relative).canonicalize()?;
        ensure!(path.starts_with(&self.root), "prefix escaped root");
        let b = fs::read(&path)?;
        ensure!(r["prefix_sha256"] == sha(&b), "prefix bytes changed");
        let n = counter(r, "tokens")? as usize;
        ensure!((1..=2048).contains(&n), "token bound");
        let h = parse_tensor(&b, n, &r["frozen_encoding"])?;
        ensure!(
            r["prefix_replay_max_error"]
                .as_f64()
                .is_some_and(|x| x.is_finite() && (0.0..=1e-6).contains(&x)),
            "unqualified prefix replay"
        );
        Ok(Some((
            json!({"path":path,"sha256":r["prefix_sha256"],"contract_sha256":self.contract_sha,"text_sha256":text_hash,"tokens":n}),
            h,
        )))
    }
}

fn normalization(rows: &[Value], h: &BTreeMap<String, Vec<f64>>) -> Result<Value> {
    let train = rows
        .iter()
        .filter(|r| r["frozen_split"] == "train" && r["purpose"] == "query")
        .collect::<Vec<_>>();
    let mut counts = BTreeMap::<String, usize>::new();
    for r in &train {
        *counts.entry(str_at(r, "source_group")?.into()).or_default() += 1;
    }
    ensure!(!train.is_empty(), "empty Train normalization");
    let mut mean = vec![0.; 768];
    let mut variance = vec![0.; 768];
    for r in &train {
        let weight = 1. / (counts.len() * counts[str_at(r, "source_group")?]) as f64;
        for (m, x) in mean.iter_mut().zip(&h[str_at(r, "id")?]) {
            *m += weight * x;
        }
    }
    for r in &train {
        let weight = 1. / (counts.len() * counts[str_at(r, "source_group")?]) as f64;
        for ((v, m), x) in variance.iter_mut().zip(&mean).zip(&h[str_at(r, "id")?]) {
            *v += weight * (x - m).powi(2);
        }
    }
    let scale = variance
        .iter()
        .map(|v| v.sqrt().max(0.01))
        .collect::<Vec<_>>();
    ensure!(
        mean.iter().chain(&scale).all(|v| v.is_finite()),
        "nonfinite normalization"
    );
    Ok(
        json!({"schema":"slop_ninja_v6_train_family_normalization_v1","fit":"train_query_only_equal_source_family_weight","dimension":768,"fit_rows":train.len(),"fit_families":counts.len(),"mean":mean,"variance":variance,"scale":scale,"scale_floor":0.01,"development_used_for_fit":false,"calibration_used_for_fit":false,"reference_used_for_fit":false,"final_test_opened":false,"labels_used_for_fit":false}),
    )
}
fn profiles(rows: &[Value]) -> Result<BTreeMap<String, Vec<String>>> {
    let mut result = BTreeMap::new();
    for role in ["human", "ai"] {
        let mut names = BTreeSet::new();
        for r in rows
            .iter()
            .filter(|r| r["frozen_split"] == "train" && r["purpose"] == "query")
        {
            if r["roles"][role]["present"] == true {
                if let Some(id) = r["roles"][role]["identity"].as_str() {
                    ensure!(
                        !id.is_empty() && !["other_human", "other_ai"].contains(&id),
                        "invalid enrollment identity"
                    );
                    names.insert(id.to_owned());
                }
            }
        }
        ensure!(names.len() >= 2, "insufficient {role} supervision");
        result.insert(role.into(), names.into_iter().collect());
    }
    Ok(result)
}
fn run(stage: &Path, dataset: &Path, new_prefixes: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let protocol_path = stage.join("protocol-v1.json");
    let protocol = fs::read(&protocol_path)?;
    ensure!(
        serde_json::from_slice::<Value>(&protocol)?["schema"]
            == "slop_ninja_encoder_comparison_protocol_v1",
        "protocol schema"
    );
    let dm = read(&dataset.join("manifest.json"))?;
    ensure!(
        dm["complete"] == true
            && dm["test_text_opened"] == false
            && dm["protocol_sha256"] == sha(&protocol),
        "new dataset protocol/scope"
    );
    let rb = fs::read(dataset.join("records.jsonl"))?;
    ensure!(dm["records_sha256"] == sha(&rb), "new dataset changed");
    let rm = read(&stage.join("retained-train-requests-v1/manifest.json"))?;
    ensure!(
        rm["complete"] == true
            && rm["permitted_split"] == "train"
            && rm["old_development_calibration_exported"] == false
            && rm["test_text_opened"] == false,
        "retained scope"
    );
    let lb = fs::read(stage.join("retained-train-requests-v1/labels.jsonl"))?;
    ensure!(rm["labels_sha256"] == sha(&lb), "retained labels changed");
    let requests = fs::read(stage.join("retained-train-requests-v1/requests.jsonl"))?;
    ensure!(
        rm["requests_sha256"] == sha(&requests),
        "retained exact requests changed"
    );
    let mut old_text = BTreeMap::new();
    for r in rows(&requests)? {
        ensure!(
            old_text
                .insert(str_at(&r, "id")?.to_owned(), str_at(&r, "text")?.to_owned())
                .is_none(),
            "duplicate retained text request"
        );
    }
    let old = Prefixes::load(
        &stage.join("retained-train-prefixes-v1"),
        digest_at(&rm, "requests_sha256")?,
    )?;
    let new = Prefixes::load(new_prefixes, digest_at(&dm, "requests_sha256")?)?;
    ensure!(
        old.contract == new.contract && old.contract_sha == new.contract_sha,
        "different historical prefix profiles are not interchangeable"
    );
    let mut result = Vec::new();
    let mut activations = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut families = BTreeMap::new();
    let mut excluded = Vec::new();
    let mut text_parts = BTreeMap::new();
    for (retained, source_rows, prefixes) in [(true, rows(&lb)?, &old), (false, rows(&rb)?, &new)] {
        let supplied: BTreeSet<_> = source_rows
            .iter()
            .map(|r| str_at(r, "id").map(str::to_owned))
            .collect::<Result<_>>()?;
        ensure!(
            supplied == prefixes.entries.keys().cloned().collect(),
            "prefix population not exact"
        );
        for r in source_rows {
            let id = str_at(&r, "id")?;
            let part = str_at(&r, "frozen_split")?;
            let group = str_at(&r, "source_group")?;
            ensure!(
                PARTS.contains(&part)
                    && (!retained || (part == "train" && r["purpose"] == "query")),
                "retained non-Train or Test row"
            );
            ensure!(ids.insert(id.to_owned()), "duplicate id");
            if let Some(prior) = families.insert(group.to_owned(), part.to_owned()) {
                ensure!(prior == part, "source family crosses partitions");
            }
            let text = if retained {
                old_text
                    .get(id)
                    .context("missing retained exact text")?
                    .as_str()
            } else {
                str_at(&r, "text")?
            };
            ensure!(
                r["text_sha256"] == sha(text.as_bytes()),
                "source text binding differs"
            );
            let normalized = sha(text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .as_bytes());
            if let Some(previous) = text_parts.insert(normalized.clone(), part.to_owned()) {
                ensure!(
                    previous == part,
                    "cross-partition normalized text collision needs exclusion accounting"
                );
            }
            let mut row = if retained {
                r.clone()
            } else {
                let role = |key: &str| json!({"present":r[format!("{key}_role_present")],"identity":r[format!("{key}_identity")],"known":false});
                let target = if r["purpose"] == "reference" {
                    ensure!(
                        r["origin"] == "human_only"
                            && r["human_role_present"] == true
                            && r["ai_role_present"] == false,
                        "reference history"
                    );
                    json!({"origin":null,"share":null,"human":null,"ai":null,"ai_involvement":null})
                } else {
                    let sup = supervision::Supervision::parse(&r)?;
                    json!({"origin":sup.origin,"share":sup.share,"human":null,"ai":null,"ai_involvement":sup.origin!=0})
                };
                json!({"id":id,"source_group":group,"frozen_split":part,"purpose":r["purpose"],"targets":target,"roles":{"human":role("human"),"ai":role("ai")},"text_sha256":r["text_sha256"],"exposed_to_grammar":r["exposed_to_grammar"],"acquisition_stratum":r["metadata"]["acquisition_stratum"],"documented_human_only":r["documented_human_only"]})
            };
            let Some((prefix, h)) = prefixes.get(id, digest_at(&r, "text_sha256")?)? else {
                excluded.push(json!({"id":id,"source_group":group,"split":part,"reason":prefixes.entries[id]["reason"]}));
                continue;
            };
            row["prefix"] = prefix;
            row["normalized_text_sha256"] = json!(normalized);
            row["cohort"] = json!(if retained {
                "retained_train"
            } else {
                "new_stage3"
            });
            activations.insert(id.to_owned(), h);
            result.push(row);
        }
    }
    let profiles = profiles(&result)?;
    for r in &mut result {
        for role in ["human", "ai"] {
            let index = r["roles"][role]["identity"]
                .as_str()
                .and_then(|s| profiles[role].binary_search_by(|x| x.as_str().cmp(s)).ok());
            r["roles"][role]["known"] = json!(index.is_some());
            r["targets"][role] = if r["purpose"] == "query" && r["roles"][role]["present"] == true {
                json!(index)
            } else {
                Value::Null
            };
        }
    }
    let norm = normalization(&result, &activations)?;
    let contract = json!({"schema":"slop_ninja_last_block_a_contract_v1","parent_artifact_id":old.contract["artifact_id"],"prefix_contract_sha256":old.contract_sha,"prefix_contract":old.contract,"human_profiles":profiles["human"],"ai_profiles":profiles["ai"],"embedding_normalization":norm,"input_mode":"text","raw_dimension":768,"zero_cfg_coordinates":1820,"maximum_tokens_including_specials":2048,"token_truncation":false,"origin_order":["human_only","model_only","mixed"],"shared_encoding":"V6 final-block encoding plus residual 2588->64 tanh->768; all output heads consume the same768 encoding","learned_input_exclusions":["IDs","source/author/generator identity metadata","hashes","provenance","parser/review metadata","partition","history and labels"],"output_contract":"ai_confidence=100*(p_model_only+p_mixed); ai_share=100*(p_model_only+p_mixed*sigmoid(mixed_share_head)); both conditional role outputs with involvement probabilities and no presence cutoff"});
    let cb = serde_json::to_vec_pretty(&contract)?;
    fs::create_dir(out)?;
    put(&out.join("contract.json"), &cb)?;
    put(&out.join("protocol.json"), &protocol)?;
    result.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut files = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for part in ["train", "development", "calibration", "reference"] {
        let selected = result
            .iter()
            .filter(|r| {
                if part == "reference" {
                    r["purpose"] == "reference"
                } else {
                    r["frozen_split"] == part && r["purpose"] == "query"
                }
            })
            .collect::<Vec<_>>();
        ensure!(!selected.is_empty(), "empty {part}");
        let mut b = Vec::new();
        let mut groups = BTreeSet::new();
        for r in &selected {
            serde_json::to_writer(&mut b, r)?;
            b.push(b'\n');
            groups.insert(str_at(r, "source_group")?);
        }
        let name = format!("{part}.jsonl");
        put(&out.join(&name), &b)?;
        files.insert(name, sha(&b));
        counts.insert(part, json!({"rows":selected.len(),"families":groups.len()}));
    }
    jput(&out.join("exclusions.json"), &json!(excluded))?;
    let m = json!({"schema":"slop_ninja_last_block_a_dataset_v1","complete":true,"final_test_opened":false,"contract_sha256":sha(&cb),"protocol_sha256":sha(&protocol),"files_sha256":files,"populations":counts,"requested":ids.len(),"supported":result.len(),"excluded":excluded.len(),"exclusions_sha256":sha(&fs::read(out.join("exclusions.json"))?),"source_dataset":pin(&dataset.join("manifest.json"))?,"retained_training":pin(&stage.join("retained-train-requests-v1/manifest.json"))?,"prefix_receipts":[old.receipt,new.receipt],"source_families":families,"implementation_sha256":sha(include_bytes!("prepare_last_block_fit.rs")),"supervision_validator_sha256":sha(include_bytes!("prepare_supervised_forest/supervision.rs")),"limitations":["Final Test not acquired or used; Development is selection data.","Historical proxy labels and unknown foundation-model exposure remain.","No CFG features enter this matched encoder comparison."]});
    jput(&out.join("manifest.json"), &m)?;
    println!(
        "{}",
        json!({"populations":counts,"excluded":excluded.len(),"human_profiles":profiles["human"].len(),"ai_profiles":profiles["ai"].len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
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
    fn normalization_weights_works_and_excludes_development_and_references() {
        let r = vec![
            json!({"id":"a","source_group":"a","frozen_split":"train","purpose":"query"}),
            json!({"id":"b","source_group":"b","frozen_split":"train","purpose":"query"}),
            json!({"id":"c","source_group":"b","frozen_split":"train","purpose":"query"}),
            json!({"id":"d","source_group":"d","frozen_split":"development","purpose":"query"}),
            json!({"id":"r","source_group":"r","frozen_split":"train","purpose":"reference"}),
        ];
        let h = BTreeMap::from([
            ("a".into(), vec![0.; 768]),
            ("b".into(), vec![2.; 768]),
            ("c".into(), vec![2.; 768]),
            ("d".into(), vec![100.; 768]),
            ("r".into(), vec![100.; 768]),
        ]);
        let n = normalization(&r, &h).unwrap();
        assert_eq!(n["mean"][0], 1.);
        assert_eq!(n["variance"][0], 1.);
        assert_eq!(n["fit_rows"], 3);
    }
    #[test]
    fn safetensors_encoding_must_equal_the_index_and_ranges_cannot_overlap() {
        let header = json!({"prefix":{"dtype":"F32","shape":[1,1,768],"data_offsets":[0,3072]},"frozen_encoding":{"dtype":"F32","shape":[1,768],"data_offsets":[3072,6144]}});
        let build = |h: &Value| {
            let h = serde_json::to_vec(h).unwrap();
            let mut b = (h.len() as u64).to_le_bytes().to_vec();
            b.extend(h);
            b.extend(vec![0; 6144]);
            b
        };
        assert!(parse_tensor(&build(&header), 1, &json!(vec![0.; 768])).is_ok());
        assert!(parse_tensor(&build(&header), 1, &json!(vec![1.; 768])).is_err());
        let mut bad = header;
        bad["frozen_encoding"]["data_offsets"] = json!([0, 3072]);
        assert!(parse_tensor(&build(&bad), 1, &json!(vec![0.; 768])).is_err());
    }
}
