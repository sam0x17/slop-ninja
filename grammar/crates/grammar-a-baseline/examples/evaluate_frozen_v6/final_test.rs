//! --frozen-test NUMERIC TEST_EMBEDDINGS FROZEN_EMBEDDING_MANIFEST SELECTION NEW_REPORT
//! FROZEN_EMBEDDING_MANIFEST must be in selection.binding_files. It supplies the
//! original immutable V6 representation/runtime/temperature reference. No Test
//! calibration, share estimate, attribution, fitting or forward execution occurs.
use super::test_binding as b;
use super::*;
use std::collections::BTreeSet;

const TEMPERATURE: f64 = 1.8776105792643432;

fn representation(frozen: &Value, current: &Value) -> Result<()> {
    let r = &frozen["representation_contract"];
    ensure!(
        frozen["schema"] == "slop_ninja_v6_embeddings_run_v1"
            && frozen["status"] == "complete"
            && frozen["dimension"] == 768
            && r["schema"] == "slop_ninja_v6_penultimate_contract_v1"
            && r["artifact_id"] == V6
            && r["dimension"] == 768
            && r["embedding_dtype"] == "float32"
            && r["hook"]["module"] == "model.classifier"
            && r["hook"]["argument"] == 0
            && r["hook"]["normalization"] == "none"
            && r["hook"]["projection"] == "none"
            && r["input"]["truncation"] == false
            && r["input"]["max_tokens_including_special_tokens"] == 2048
            && r["model"]["temperature"] == TEMPERATURE,
        "unsupported frozen V6 reference"
    );
    ensure!(
        current["schema"] == "slop_ninja_v6_embeddings_run_v1"
            && current["status"] == "complete"
            && current["dimension"] == 768
            && current["representation_contract"] == *r
            && current["representation_contract_sha256"]
                == frozen["representation_contract_sha256"],
        "Test changed frozen V6 representation/runtime"
    );
    b::hash_text(frozen, "representation_contract_sha256")?;
    Ok(())
}

// Bounds are checked even when an upstream producer labels the row successful.
// The probabilities must still be the unchanged frozen-temperature origin head.
fn checked_row(row: &Value, representation_sha: &str) -> Result<Option<Value>> {
    ensure!(
        row["artifact_id"] == V6
            && row["representation_contract_sha256"] == representation_sha
            && row["labels"] == json!(["human_only", "model_only", "mixed"])
            && row["runtime"] == "cpu_float32_eager"
            && row.get("truncated").is_none_or(|v| v == false),
        "V6 row identity/truncation differs"
    );
    let tokens = b::count(row, "token_count")?;
    match row["status"].as_str() {
        Some("ok") => {
            ensure!(
                (1..=2048).contains(&tokens),
                "V6 successful input outside token boundary"
            );
            let logits = b::array(row, "logits")?;
            ensure!(logits.len() == 3, "V6 logit shape");
            let logits = logits
                .iter()
                .map(|x| {
                    x.as_f64()
                        .filter(|x| x.is_finite())
                        .context("V6 finite logit")
                })
                .collect::<Result<Vec<_>>>()?;
            let largest = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let e = logits
                .iter()
                .map(|l| ((l - largest) / TEMPERATURE).exp())
                .collect::<Vec<_>>();
            let total: f64 = e.iter().sum();
            let p = prediction(row)?;
            for (i, k) in ["human_only", "model_only", "mixed"].iter().enumerate() {
                ensure!(
                    (p["origin"][*k].as_f64().context("origin probability")? - e[i] / total).abs()
                        < 1e-7,
                    "V6 probabilities changed frozen calibration"
                );
            }
            Ok(Some(p))
        }
        Some("unsupported_input") => {
            let reason = b::text(row, "reason")?;
            ensure!(
                (tokens == 0 && reason == "empty_text")
                    || (tokens > 2048 && reason == format!("unsupported_length:{tokens}>2048")),
                "unsupported V6 reason/token mismatch"
            );
            ensure!(
                row["probabilities"].is_null()
                    && row["logits"].is_null()
                    && row["embedding"].is_null()
                    && row["model_or_assistance_probability"].is_null(),
                "unsupported row contains invented outputs"
            );
            Ok(None)
        }
        Some("error" | "failed") => {
            b::text(row, "reason")?;
            ensure!(
                row["probabilities"].is_null()
                    && row["model_or_assistance_probability"].is_null()
                    && row["embedding"].is_null()
                    && row["logits"].is_null(),
                "failed V6 row contains predictions"
            );
            Ok(None)
        }
        _ => anyhow::bail!("unknown V6 status"),
    }
}

pub(super) fn run(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 5,
        "--frozen-test NUMERIC TEST_EMBEDDINGS FROZEN_EMBEDDING_MANIFEST SELECTION NEW_REPORT"
    );
    let out = Path::new(&a[4]);
    ensure!(!out.exists(), "new V6 Test report required");
    let selection_path = Path::new(&a[3]);
    let selection = final_test_binding::validate_selection_receipt(selection_path)?;
    let selection_sha = sha(selection_path)?;
    let frozen_path = Path::new(&a[2]);
    let frozen_sha = b::bound(&selection, frozen_path)?;
    let frozen = read(frozen_path)?;
    let data = b::load_data(Path::new(&a[0]), &selection, &selection_sha)?;
    let embeddings = Path::new(&a[1]);
    let em = read(&embeddings.join("manifest.json"))?;
    representation(&frozen, &em)?;
    let eb = &data.manifest["test_transform"]["bindings"]["embeddings"];
    ensure!(
        eb["manifest_sha256"] == sha(&embeddings.join("manifest.json"))?
            && eb["rows_sha256"] == em["rows_sha256"]
            && eb["representation_contract_sha256"] == em["representation_contract_sha256"],
        "V6 run differs from frozen Test projection inputs"
    );
    ensure!(
        em["rows_sha256"] == sha(&embeddings.join("rows.jsonl"))?,
        "V6 Test rows changed"
    );
    let all = b::indexed(&rows(&embeddings.join("rows.jsonl"))?)?;
    ensure!(
        all.keys().eq(data.rows.keys())
            && b::count(&em, "requested")? == all.len()
            && b::count(&em, "processed")? == all.len(),
        "V6 Test requested roster differs"
    );
    let mut supported = Vec::new();
    let mut pred = BTreeMap::new();
    let mut excluded = Vec::new();
    let mut unsupported = 0;
    let mut failures = 0;
    for (id, v) in &all {
        ensure!(
            v["text_sha256"] == data.hashes[id],
            "V6 Test exact text differs"
        );
        if let Some(p) = checked_row(v, b::text(&em, "representation_contract_sha256")?)? {
            pred.insert(id.clone(), p);
            supported.push(data.rows[id].clone());
        } else {
            if v["status"] == "unsupported_input" {
                unsupported += 1;
            } else {
                failures += 1;
            }
            excluded.push(json!({"id":id,"status":v["status"],"reason":v["reason"],"token_count":v["token_count"],"text_sha256":v["text_sha256"]}));
        }
    }
    // The qualified producer's `unsupported` counts all non-success rows. A
    // producer with a separate `failed` count must instead partition the total.
    let declared_unsupported = b::count(&em, "unsupported")?;
    let failure_counts = match em.get("failed") {
        None => declared_unsupported == unsupported + failures,
        Some(_) => b::count(&em, "failed")? == failures && declared_unsupported == unsupported,
    };
    ensure!(
        b::count(&em, "supported")? == supported.len()
            && failure_counts
            && supported.len() + unsupported + failures == all.len(),
        "V6 support/failure accounting differs"
    );
    let pure = supported
        .iter()
        .filter(|r| r["targets"]["origin"] != 2)
        .cloned()
        .collect::<Vec<_>>();
    let mixed = supported
        .iter()
        .filter(|r| r["targets"]["origin"] == 2)
        .cloned()
        .collect::<Vec<_>>();
    let report = json!({"schema":"slop_ninja_frozen_v6_test_origin_report_v1","complete":true,"split":"test",
        "requested":all.len(),"supported":supported.len(),"unsupported":unsupported,"failed":failures,"excluded":excluded,
        "support":{"upstream":data.upstream,"assembled_queries":data.rows.len(),"shared_model_projection_supported":data.numeric.len(),
            "V6_supported":supported.len(),"independent_of_CFG_support":true,
            "supported_source_families":supported.iter().map(|r|r["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len()},
        "all_origins":metrics(&supported,&pred)?,"human_model_only":metrics(&pure,&pred)?,"mixed_only":metrics(&mixed,&pred)?,
        "artifact_id":V6,"recalibrated":false,"temperature":TEMPERATURE,"max_tokens_including_special_tokens":2048,"truncation":false,
        "bindings":{"selection_receipt_sha256":selection_sha,"numeric_manifest_sha256":data.manifest_sha,
            "test_population_sha256":data.population_sha,"test_dataset_manifest_sha256":data.dataset_sha,"query_sha256":data.query_sha,
            "frozen_embedding_manifest_sha256":frozen_sha,"embedding_manifest_sha256":sha(&embeddings.join("manifest.json"))?,
            "embedding_rows_sha256":em["rows_sha256"],"representation_contract_sha256":em["representation_contract_sha256"],
            "implementation_sha256":b::digest(include_bytes!("final_test.rs")),"admission_helper_sha256":b::digest(include_bytes!("../compare_supervised_pangram/test_binding.rs"))},
        "unsupported_outputs":["fractional_ai_share","human_attribution","generator_attribution"],
        "test_opened":true,"test_used_for_selection":false,"fitting_performed":false,"model_calls":0,
        "limits":["Frozen V6 is an origin-only comparator; no share or attribution is inferred",
            "Original historical temperature and full classifier-input representation remain frozen",
            "V6 supports are reported independently of CFG/shared-model projection support",
            "Historical proxies and unknown pretrained exposure remain; no deployment or broad generalization claim"]});
    ensure!(
        sha(selection_path)? == selection_sha && sha(frozen_path)? == frozen_sha,
        "frozen bindings changed during scoring"
    );
    b::write_new(out, &report)
}

#[cfg(test)]
#[path = "final_test_tests.rs"]
mod tests;
