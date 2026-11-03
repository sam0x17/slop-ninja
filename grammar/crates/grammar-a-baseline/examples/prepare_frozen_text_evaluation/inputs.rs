//! Digest-bound evaluation inputs. Failed rows are evidence, never replacement vectors.
use super::*;

pub(super) struct Cfg {
    pub rows: Rows,
    pub population: Rows,
    pub bindings: Value,
    pub snapshots: Files,
}
pub(super) fn cfg(
    dir: &Path,
    parent: &Parent,
    db: &[u8],
    dataset: &supervision::Dataset,
    records: &Rows,
    profiles: &supervision::Profiles,
) -> Result<Cfg> {
    let (mb, m) = read(&dir.join("manifest.json"))?;
    let cb = bound(dir, "contract.json", &m["contract_file_sha256"])?;
    let c: Value = serde_json::from_slice(&cb)?;
    ensure!(
        m["schema"] == "slop_ninja_shared_a_dataset_v1"
            && m["final_test_opened"] == true
            && c == parent.contract["cfg_parent_contract"]
            && m["contract_sha256"] == parent.contract["cfg_parent_contract_sha256"]
            && m["contract_file_sha256"] == parent.contract["cfg_parent_contract_file_sha256"],
        "CFG frozen contract differs"
    );
    let t = &m["test_transform"];
    ensure!(
        t["fit_performed"] == false
            && t["dataset_manifest_sha256"] == sha(db)
            && t["frozen_manifest_sha256"] == parent.contract["parents"]["cfg"]["manifest_sha256"],
        "CFG Test transform parent/dataset binding"
    );
    let ab = bound(dir, "test_population.json", &t["audit_file_sha256"])?;
    let audit: Value = serde_json::from_slice(&ab)?;
    ensure!(
        audit["schema"] == "slop_ninja_supervised_a_test_transform_v1"
            && audit["dataset_manifest_sha256"] == sha(db)
            && audit["records_sha256"] == dataset.records_sha256
            && audit["contract_sha256"] == m["contract_sha256"]
            && audit["frozen_manifest_sha256"] == t["frozen_manifest_sha256"]
            && audit["fit_performed"] == false
            && audit["final_test_opened"] == true,
        "CFG audit binding"
    );
    let bindings = array(&m, "splits")?
        .iter()
        .filter(|v| v["split"] == "test")
        .collect::<Vec<_>>();
    ensure!(bindings.len() == 1, "exact CFG Test binding required");
    let rb = bound(dir, "test.jsonl", &bindings[0]["sha256"])?;
    let rr = rows(&rb)?;
    let mut population = Rows::new();
    for p in array(&audit, "rows")? {
        ensure!(
            population
                .insert(str_at(p, "id")?.into(), p.clone())
                .is_none(),
            "duplicate CFG population ID"
        );
    }
    ensure!(
        population.keys().eq(records.keys()),
        "CFG population must account for every requested record"
    );
    let mut absent = BTreeSet::new();
    for (id, r) in records {
        let p = &population[id];
        ensure!(
            p["group"] == r["source_group"]
                && p["split"] == "test"
                && p["purpose"] == "query"
                && p["metadata"] == supervision::without_text(r)
                && p["metadata_sha256"] == sha(&serde_json::to_vec(r)?)
                && p["source_sha256"] == r["text_sha256"],
            "CFG metadata/text join differs"
        );
        let available = p["available"].as_bool().context("CFG availability")?;
        ensure!(available == rr.contains_key(id), "CFG support/row mismatch");
        if let Some(row) = rr.get(id) {
            let supervision = dataset.supervision(r)?;
            ensure!(
                row["source_group"] == r["source_group"]
                    && row["frozen_split"] == "test"
                    && row["purpose"] == "query"
                    && row["input"]["contract_sha256"] == m["contract_sha256"]
                    && row["roles"] == profiles.roles(&supervision)
                    && row["targets"] == profiles.targets(&supervision)
                    && row["exposed_to_grammar"] == supervision.exposed,
                "CFG labels/profile/mask binding differs"
            );
            vector(&row["input"]["x"], parent.cfg_dimension)?;
        } else {
            str_at(p, "exclusion_reason")?;
            absent.insert(id.clone());
        }
    }
    ensure!(
        rr.keys().all(|id| records.contains_key(id)),
        "unrequested CFG ID"
    );
    let mut excluded = BTreeSet::new();
    for e in array(&audit, "excluded")? {
        ensure!(
            excluded.insert(str_at(e, "id")?.to_owned()),
            "duplicate CFG exclusion"
        );
    }
    ensure!(
        excluded == absent
            && t["requested"] == records.len()
            && audit["requested"] == records.len()
            && t["available"] == rr.len()
            && audit["available"] == rr.len()
            && t["excluded"] == absent.len()
            && bindings[0]["rows"] == rr.len(),
        "CFG coverage accounting differs"
    );
    Ok(Cfg {
        rows: rr,
        population,
        bindings: json!({"manifest_sha256":sha(&mb),"contract_file_sha256":sha(&cb),
        "rows_sha256":sha(&rb),"audit_sha256":sha(&ab)}),
        snapshots: Files::from([
            ("cfg-evaluation-manifest.json".into(), mb),
            ("cfg-test-population.json".into(), ab),
        ]),
    })
}

pub(super) struct Embeddings {
    pub rows: Rows,
    pub bindings: Value,
    pub snapshots: Files,
}
pub(super) const V6_TEMPERATURE: f64 = 1.8776105792643432;
pub(super) fn embeddings(dir: &Path, contract: &Value, records: &Rows) -> Result<Embeddings> {
    let (mb, m) = read(&dir.join("manifest.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_v6_embeddings_run_v1"
            && m["status"] == "complete"
            && m["dimension"] == TEXT
            && m["requested"] == records.len()
            && m["processed"] == records.len(),
        "incomplete embedding run"
    );
    let cb = fs::read(dir.join("contract/manifest.json"))?;
    let canonical = cb
        .strip_suffix(b"\n")
        .context("embedding contract final newline")?;
    let rep: Value = serde_json::from_slice(canonical)?;
    ensure!(
        rep["schema"] == "slop_ninja_v6_penultimate_contract_v1"
            && rep["artifact_id"] == ARTIFACT
            && rep["dimension"] == TEXT
            && rep["embedding_dtype"] == "float32"
            && rep["hook"]["module"] == "model.classifier"
            && rep["hook"]["argument"] == 0
            && rep["hook"]["normalization"] == "none"
            && rep["hook"]["projection"] == "none"
            && rep["input"]["truncation"] == false
            && rep["input"]["max_tokens_including_special_tokens"] == 2048
            && rep["model"]["temperature"] == V6_TEMPERATURE
            && m["representation_contract_sha256"] == sha(canonical)
            && m["representation_contract"] == rep
            && rep == contract["representation_contract"]
            && m["representation_contract_sha256"]
                == contract["parents"]["representation_contract_sha256"],
        "changed CPU representation; another runtime needs separate qualification"
    );
    let rb = bound(dir, "rows.jsonl", &m["rows_sha256"])?;
    let requests = bound(dir, "requests.jsonl", &m["requests_sha256"])?;
    let rr = rows(&rb)?;
    ensure!(
        rr.keys().eq(records.keys()),
        "embedding roster must equal all requested records, including unsupported"
    );
    let mut seen = BTreeSet::new();
    for (index, line) in requests.split_inclusive(|b| *b == b'\n').enumerate() {
        let request: Value = serde_json::from_slice(line)?;
        let id = str_at(&request, "id")?;
        ensure!(
            seen.insert(id.to_owned())
                && request.as_object().is_some_and(|m| m.len() == 2)
                && records
                    .get(id)
                    .is_some_and(|r| request["text"] == r["text"]),
            "request text/roster/label-free keys differ"
        );
        let e = &rr[id];
        ensure!(
            e["request_index"] == index + 1
                && e["request_sha256"] == sha(line)
                && e["text_sha256"] == records[id]["text_sha256"]
                && e["artifact_id"] == ARTIFACT
                && e["representation_contract_sha256"] == m["representation_contract_sha256"],
            "embedding request/identity binding differs"
        );
    }
    ensure!(seen.iter().eq(records.keys()), "request population differs");
    let mut supported = 0;
    let mut unsupported = 0;
    let mut failures = 0;
    for e in rr.values() {
        ensure!(
            e["runtime"] == "cpu_float32_eager"
                && e["labels"] == json!(["human_only", "model_only", "mixed"])
                && e.get("truncated").is_none_or(|v| v == false),
            "V6 row runtime/labels/truncation differs"
        );
        let tokens = counter(e, "token_count")?;
        match str_at(e, "status")? {
            "ok" => {
                ensure!((1..=2048).contains(&tokens), "supported token range");
                let v = vector(&e["embedding"], TEXT)?;
                let mut b = Vec::with_capacity(TEXT * 4);
                for x in v {
                    ensure!((x as f32) as f64 == x, "raw embedding is not exact float32");
                    b.extend_from_slice(&(x as f32).to_le_bytes());
                }
                ensure!(
                    e["embedding_sha256"] == sha(&b),
                    "raw embedding bytes changed"
                );
                let logits = vector(&e["logits"], 3)?;
                let p = vector(&e["probabilities"], 3)?;
                ensure!(
                    p.iter().all(|x| (0.0..=1.).contains(x))
                        && (p.iter().sum::<f64>() - 1.).abs() < 1e-7,
                    "V6 probability vector"
                );
                ensure!(
                    (finite(&e["model_or_assistance_probability"])? - p[1] - p[2]).abs() < 1e-7,
                    "V6 involvement mismatch"
                );
                // Match evaluate_frozen_v6/final_test.rs::checked_row: preserve
                // the historical origin head, with no Test recalibration.
                let largest = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let exp = logits
                    .iter()
                    .map(|l| ((l - largest) / V6_TEMPERATURE).exp())
                    .collect::<Vec<_>>();
                let total: f64 = exp.iter().sum();
                ensure!(
                    p.iter()
                        .zip(&exp)
                        .all(|(p, e)| (p - e / total).abs() < 1e-7),
                    "V6 probabilities changed frozen calibration"
                );
                supported += 1;
            }
            status @ ("unsupported_input" | "error" | "failed") => {
                let reason = str_at(e, "reason")?;
                if status == "unsupported_input" {
                    ensure!(
                        (tokens == 0 && reason == "empty_text")
                            || (tokens > 2048
                                && reason == format!("unsupported_length:{tokens}>2048")),
                        "unsupported V6 reason/token mismatch"
                    );
                    unsupported += 1;
                } else {
                    failures += 1;
                }
                ensure!(
                    e["embedding"].is_null()
                        && e["embedding_sha256"].is_null()
                        && e["logits"].is_null()
                        && e["probabilities"].is_null()
                        && e["model_or_assistance_probability"].is_null(),
                    "failed embedding contains inferred values"
                );
            }
            _ => anyhow::bail!("unrecognized embedding status"),
        }
    }
    // Original CPU exports count all non-success rows as unsupported. Match
    // the comparator's optional separate failure-count convention as well.
    let failure_counts = match m.get("failed") {
        None => m["unsupported"] == unsupported + failures,
        Some(_) => m["failed"] == failures && m["unsupported"] == unsupported,
    };
    ensure!(
        m["supported"] == supported
            && failure_counts
            && supported + unsupported + failures == records.len(),
        "embedding support counts differ"
    );
    Ok(Embeddings {
        rows: rr,
        bindings: json!({"manifest_sha256":sha(&mb),"rows_sha256":sha(&rb),"requests_sha256":sha(&requests),
        "contract_file_sha256":sha(&cb),"representation_contract_sha256":m["representation_contract_sha256"]}),
        snapshots: Files::from([
            ("embedding-manifest.json".into(), mb),
            ("embedding-contract.json".into(), cb),
        ]),
    })
}
