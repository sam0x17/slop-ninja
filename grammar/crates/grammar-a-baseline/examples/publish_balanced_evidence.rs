//! Project evaluated fresh-family evidence to an aggregate-only public report.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn take(v: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|k| v.get(*k).map(|v| ((*k).into(), v.clone())))
            .collect(),
    )
}
fn reference_summary(v: &Value) -> Result<Value> {
    let mut out = take(
        v,
        &[
            "aggregation",
            "final_test_opened",
            "fit_performed",
            "gpu_used",
            "gallery_human_identities_per_partition",
            "gallery_reference_excerpts_per_partition",
        ],
    );
    out["groups"] = json!(
        v["groups"]
            .as_array()
            .context("reference groups")?
            .iter()
            .map(|g| take(
                g,
                &[
                    "ablation",
                    "partition",
                    "known_top1",
                    "withheld_top1",
                    "overall_mrr",
                    "overall_top1",
                    "overall_top3",
                ]
            ))
            .collect::<Vec<_>>()
    );
    Ok(out)
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 2, "publish_balanced_evidence COHORT NEW_REPORT");
    let c = Path::new(&a[0]);
    let out = Path::new(&a[1]);
    ensure!(!out.exists(), "new report required");
    let selection = read(&c.join("selection-v1.json"))?;
    ensure!(selection["final_test_opened"] == false, "Test selection");
    let mut selected = take(
        &selection,
        &[
            "schema",
            "selected_candidate",
            "representative_seed",
            "evaluation_seeds",
            "selection",
            "protocol_sha256",
            "fitting_records_sha256",
            "dataset_manifest_sha256",
            "test_used_for_selection",
            "final_test_opened",
            "model_calibration_frozen",
            "grammar_frozen",
            "serving_qualified",
            "parity_established",
        ],
    );
    selected["candidates"] = json!(
        selection["candidates"]
            .as_array()
            .context("candidates")?
            .iter()
            .map(|candidate| {
                let mut v = take(
                    candidate,
                    &[
                        "candidate",
                        "numeric_manifest_sha256",
                        "contract_file_sha256",
                        "contract_sha256",
                        "active_input_dimension",
                        "mean_balanced_development_brier",
                        "mean_human_fpr_at_0_5",
                    ],
                );
                v["runs"] = json!(
                    candidate["runs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| take(
                            r,
                            &[
                                "seed",
                                "development_report_sha256",
                                "weights_sha256",
                                "checkpoint_sha256",
                                "fit_sha256",
                                "rust_replay_sha256",
                                "development_balanced_brier",
                                "human_fpr_at_0_5",
                                "share_mae_percentage_points"
                            ]
                        ))
                        .collect::<Vec<_>>()
                );
                v
            })
            .collect::<Vec<_>>()
    );
    let mut extra = Vec::new();
    for (name, path, inside) in [
        ("cfg_all", "fit-cfg-v1/all", true),
        ("cfg_leaf", "fit-cfg-v1/leaf_grammar", true),
        ("ordered_cfg", "fit-ordered-v1/ordered", false),
        ("global_cfg_control", "fit-ordered-v1/global", false),
    ] {
        for seed in [17, 29, 41] {
            let p = if inside {
                c.join(path)
                    .join(format!("joint-{seed}/development-report.json"))
            } else {
                c.join(path).join(format!("joint-{seed}-development.json"))
            };
            let r = read(&p)?;
            let mut roles = serde_json::Map::new();
            for role in ["human", "ai"] {
                roles.insert(
                    role.into(),
                    take(
                        &r["attribution"][role],
                        &[
                            "training_status",
                            "conditional_accuracy",
                            "conditional_mrr",
                            "conditional_top3",
                            "open_set",
                            "documented_present",
                            "documented_absent_not_scored",
                        ],
                    ),
                );
            }
            extra.push(json!({"candidate":name,"seed":seed,"model":r["model"],"family_origin_balanced":r["family_origin_balanced"],"uniform_origin_prior_balanced":r["uniform_origin_prior_balanced"],"attribution":roles,"report_sha256":sha(&p)?}));
        }
    }
    let export = read(&c.join("export-v1/report.json"))?;
    let embeddings = read(&c.join("embeddings-v1/manifest.json"))?;
    let source = read(&c.join("dataset-v1/manifest.json"))?;
    let pg = read(&c.join("pangram/final-audit.json"))?;
    let frozen = read(&c.join("scored-v1/frozen-v6-development.json"))?;
    ensure!(
        frozen["excluded"]
            .as_array()
            .context("unsupported roster")?
            .is_empty(),
        "review unsupported IDs before publication"
    );
    let report = json!({"schema":"slop_ninja_balanced_a_public_evidence_v1","selection_before_user_warm_start_extension":selected,
  "warm_start_extension_pending":c.join("WARM_START_AMENDMENT.md").exists(),"population":take(&source,&["requested_records","records","source_families","requested_human_records","requested_generation_records","separate_withheld_reference_records","clean_holdout","exposure","test_text_opened"]),
  "fusion":read(&c.join("scored-v1/fusion-summary.json"))?,"additional_cfg":extra,"frozen_v6":frozen,
  "extraction":{"cfg":take(&export,&["complete","engine_identity","parser_load_millis","document_latency_micros","totals","rows_sha256"]),"v6":take(&embeddings,&["status","requested","supported","unsupported","elapsed_seconds","rows_sha256","representation_contract_sha256"])},
        "pangram_collection":take(&pg,&["selector","partitions","quota_balance","quota_status","test_opened","final_exact_request_and_raw_body_audit"]),
        "pangram_isotonic_diagnostic":take(&read(&c.join("pangram/isotonic-summary.json"))?,&["schema","runs","native_pangram_confidence","CAL_brier_in_sample","calibration_rows","development_rows","human_dev_fpr_at_0_5","human_dev_rows","all_six_maps_identical","all_six_legacy_reports_byte_identical","calibration_weight_sum_verified","new_paid_calls","test_opened","limits"]),
        "partition_reference_retrieval":reference_summary(&read(&c.join("partition-references-v1/summary.json"))?)?,
  "bindings":{"fusion_summary_sha256":sha(&c.join("scored-v1/fusion-summary.json"))?,"selection_sha256":sha(&c.join("selection-v1.json"))?,"frozen_v6_sha256":sha(&c.join("scored-v1/frozen-v6-development.json"))?,"source_sha256":hex::encode(Sha256::digest(include_bytes!("publish_balanced_evidence.rs")))},
  "privacy":"aggregate metrics and digests only; no text, profiles, per-document predictions, filesystem paths or provider bodies"});
    let mut report = report;
    if c.join("selection-v2.json").exists() {
        let revised = read(&c.join("selection-v2.json"))?;
        ensure!(
            revised["warm_start_included"] == true && revised["final_test_opened"] == false,
            "invalid warm-start selection"
        );
        report["warm_start_extension_pending"] = json!(false);
        report["current_selection"] = take(
            &revised,
            &[
                "schema",
                "selected_candidate",
                "selected_contract_sha256",
                "representative_seed",
                "evaluation_seeds",
                "selection",
                "protocol_sha256",
                "fitting_records_sha256",
                "dataset_manifest_sha256",
                "test_used_for_selection",
                "final_test_opened",
                "model_calibration_frozen",
                "grammar_frozen",
                "serving_qualified",
                "parity_established",
                "previous_selection_sha256",
                "qualification",
            ],
        );
        report["current_selection"]["receipt_sha256"] = json!(sha(&c.join("selection-v2.json"))?);
        report["warm_start_aggregate_evidence"] = json!("a-warm-start-results.json");
    }
    let bytes = serde_json::to_vec_pretty(&report)?;
    for forbidden in [
        "/home/",
        "/Users/",
        "PANGRAM_API_KEY",
        "conditional_named_scores",
        "per_document_ranks",
    ] {
        ensure!(
            !std::str::from_utf8(&bytes)?.contains(forbidden),
            "public projection contains forbidden field or path"
        );
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_reference_summary_excludes_private_run_evidence() {
        let v = json!({"final_test_opened":false,"groups":[{
            "partition":"development", "known_top1":{"mean_across_seeds":0.2},
            "runs":[{"report":"/home/private/report.json","per_document_ranks":[1]}],
            "future_private_field":"private text"
        }],"future_private_field":"private text"});
        let safe = reference_summary(&v).unwrap();
        assert_eq!(safe["groups"][0]["known_top1"]["mean_across_seeds"], 0.2);
        assert_eq!(safe["final_test_opened"], false);
        assert!(!safe.to_string().contains("private"));
        assert!(!safe.to_string().contains("runs"));
    }
}
