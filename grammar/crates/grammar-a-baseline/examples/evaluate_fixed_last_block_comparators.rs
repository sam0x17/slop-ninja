//! Frozen V6/prior A on the exact new Dev/CAL population, without refitting.
//! STAGE PREPARED_COMPARATORS V6_PREDICTIONS NEW_OUTPUT
#[allow(dead_code, unused_imports)]
#[path = "summarize_warm_start.rs"]
mod aggregate;
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
#[allow(dead_code, unused_imports)]
#[path = "evaluate_last_block.rs"]
mod stage_eval;
use aggregate::fusion::{balance_origins, difference, number, read, sha, text};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use stage_eval::evaluation;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
type Pred = BTreeMap<String, Value>;
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
fn names(c: &Value, role: &str) -> Result<Vec<String>> {
    c[format!("{role}_profiles")]
        .as_array()
        .context("profiles")?
        .iter()
        .map(|v| Ok(v.as_str().context("profile")?.into()))
        .collect()
}
fn role_view(data: &[Value], c: &Value) -> Result<Vec<Value>> {
    let mut out = data.to_vec();
    for role in ["human", "ai"] {
        let profiles = names(c, role)?;
        for row in &mut out {
            row["roles"][role]["known"] = json!(
                row["roles"][role]["identity"]
                    .as_str()
                    .is_some_and(|s| profiles.iter().any(|p| p == s))
            );
        }
    }
    Ok(out)
}
fn validate_support(data: &[Value], pred: &Pred) -> Result<()> {
    let ids = data
        .iter()
        .map(|r| Ok(text(r, "id")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        ids.len() == data.len() && ids == pred.keys().cloned().collect(),
        "exact prediction population required"
    );
    Ok(())
}
fn metrics(data: &[Value], pred: &Pred) -> Result<Value> {
    Ok(
        json!({"family":evaluation::origin_share_metrics(data,pred,None)?,"family_origin_balanced":evaluation::balanced_origin_metrics(data,pred,None)?}),
    )
}
fn run(stage: &Path, prepared: &Path, v6: &Path, out: &Path, final_test: bool) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let bundle = stage.join(if final_test {
        "final-test-v1/numeric-v1"
    } else {
        "fit-bundle-v1"
    });
    let dm = read(&bundle.join("manifest.json"))?;
    let pm = read(&prepared.join("manifest.json"))?;
    let vm = read(&v6.join("manifest.json"))?;
    ensure!(
        dm["schema"]
            == if final_test {
                "slop_ninja_last_block_test_dataset_v1"
            } else {
                "slop_ninja_last_block_a_dataset_v1"
            }
            && dm["complete"] == true
            && dm["final_test_opened"] == final_test
            && pm["schema"]
                == if final_test {
                    "slop_ninja_last_block_test_fixed_comparators_v1"
                } else {
                    "slop_ninja_last_block_fixed_comparators_v1"
                }
            && pm["complete"] == true
            && pm["final_test_opened"] == final_test
            && pm["dataset_manifest_sha256"] == sha(&bundle.join("manifest.json"))?
            && pm["fitting_or_calibration_performed"] == false,
        "frozen inputs required"
    );
    if final_test {
        let path = stage.join("final-test-v1/seal.json");
        seal::validate(&path)?;
        ensure!(
            dm["selection_receipt_sha256"] == sha(&path)?
                && pm["selection_receipt_sha256"] == sha(&path)?
                && dm["test_used_for_selection"] == false
                && pm["test_used_for_selection"] == false,
            "comparator Test seal differs"
        );
    }
    for (name, hash) in pm["files_sha256"]
        .as_object()
        .context("bound comparator files")?
    {
        ensure!(
            Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
                && sha(&prepared.join(name))? == *hash,
            "comparator member changed"
        );
    }
    ensure!(
        vm["schema"]
            == if final_test {
                "slop_ninja_frozen_v6_test_head_predictions_v1"
            } else {
                "slop_ninja_frozen_v6_head_predictions_v1"
            }
            && vm["complete"] == true
            && vm["final_test_opened"] == final_test
            && vm["prepared_manifest_sha256"] == sha(&prepared.join("manifest.json"))?
            && vm["predictions_sha256"] == sha(&v6.join("predictions.json"))?
            && vm["initializer_sha256"] == pm["v6_initializer_sha256"]
            && vm["fitting_or_calibration_performed"] == false,
        "fixed V6 binding differs"
    );
    if final_test {
        ensure!(
            vm["selection_receipt_sha256"] == pm["selection_receipt_sha256"],
            "V6 Test seal differs"
        );
    }
    let original = read(&v6.join("predictions.json"))?;
    let c = read(&prepared.join("prior-contract.json"))?;
    let ref_path = if final_test {
        prepared.join("references.jsonl")
    } else {
        bundle.join("reference.jsonl")
    };
    let refs = rows(&ref_path)?;
    ensure!(
        sha(&ref_path)?
            == if final_test {
                pm["files_sha256"]["references.jsonl"].clone()
            } else {
                dm["files_sha256"]["reference.jsonl"].clone()
            },
        "reference binding changed"
    );
    let refs = role_view(&refs, &c)?;
    fs::create_dir(out)?;
    let mut reports = BTreeMap::new();
    let mut receipt_bindings = BTreeMap::new();
    for &part in if final_test {
        &["test"][..]
    } else {
        &["development", "calibration"][..]
    } {
        let input = bundle.join(format!("{part}.jsonl"));
        ensure!(
            sha(&input)? == dm["files_sha256"][format!("{part}.jsonl")],
            "partition changed"
        );
        let data = rows(&input)?;
        let prior_data = role_view(&data, &c)?;
        let mut v6pred = Pred::new();
        for r in original
            .as_array()
            .context("V6 predictions")?
            .iter()
            .filter(|r| r["part"] == part)
        {
            let id = text(r, "id")?;
            let truth = data
                .iter()
                .find(|t| t["id"] == id)
                .context("extra V6 row")?;
            ensure!(
                r["text_sha256"] == truth["text_sha256"]
                    && r["prefix_sha256"] == truth["prefix"]["sha256"]
                    && v6pred.insert(id.into(), r["prediction"].clone()).is_none(),
                "V6 row binding"
            );
        }
        validate_support(&data, &v6pred)?;
        let mut origin_only = data.clone();
        for r in &mut origin_only {
            r["targets"]["share"] = Value::Null;
        }
        let v6report = json!({"metrics":metrics(&origin_only,&v6pred)?,"share_supervision_masked_for_unsupported_output":true,
            "share_or_attribution_supported":false,"calibration":"original V6 temperature unchanged","requested":data.len(),"supported":v6pred.len(),"unsupported":0});
        put(
            &out.join(format!("v6-{part}-predictions.json")),
            &json!(v6pred),
        )?;
        let mut prior_reports = Vec::new();
        let mut predictions = BTreeMap::<String, Vec<Pred>>::new();
        for seed in [17, 29, 41] {
            let matches = pm["fixed_prior_runs"]
                .as_array()
                .context("prior runs")?
                .iter()
                .filter(|r| r["seed"] == seed)
                .collect::<Vec<_>>();
            ensure!(matches.len() == 1, "seed missing/duplicate");
            let run = matches[0];
            let pred: Pred = serde_json::from_slice(&fs::read(
                prepared.join(format!("prior-{seed}-{part}.json")),
            )?)?;
            validate_support(&data, &pred)?;
            let reference: Pred = serde_json::from_slice(&fs::read(
                prepared.join(format!("prior-{seed}-reference.json")),
            )?)?;
            validate_support(&refs, &reference)?;
            let (gallery, scope) = stage_eval::gallery(&refs, &reference, &prior_data, part)?;
            let mut attribution = BTreeMap::new();
            for role in ["human", "ai"] {
                let roster = names(&c, role)?;
                ensure!(
                    json!(roster) == run["profiles"][role],
                    "profile roster changed"
                );
                attribution.insert(
                    role,
                    evaluation::attribution_metrics(
                        &prior_data,
                        &pred,
                        role,
                        &roster,
                        number(&run["thresholds"][role])?,
                    )?,
                );
            }
            prior_reports.push(json!({"seed":seed,"requested":data.len(),"supported":pred.len(),"unsupported":0,"metrics":metrics(&data,&pred)?,
                "attribution":attribution,"reference_retrieval":evaluation::retrieval_metrics(&prior_data,&pred,&gallery,&c)?,"reference_scope":scope,
                "known_identities_recomputed_against_prior_roster":true,"prior_calibration_refitted":false}));
            predictions.entry("prior_a".into()).or_default().push(pred);
            predictions
                .entry("v6".into())
                .or_default()
                .push(v6pred.clone());
            for condition in ["adapt_last_block", "frozen_last_block"] {
                let folder = if final_test {
                    stage
                        .join("final-test-v1/evaluation-v2")
                        .join(format!("{condition}-seed-{seed}"))
                } else {
                    stage
                        .join("evaluation-job-v1/results")
                        .join(format!("{condition}-seed-{seed}"))
                };
                let manifest_path = if final_test {
                    stage.join("final-test-v1/evaluation-v2/manifest.json")
                } else {
                    folder.join("manifest.json")
                };
                let m = read(&manifest_path)?;
                let file = folder.join(format!("{part}-predictions.json"));
                if final_test {
                    ensure!(
                        m["schema"] == "slop_ninja_last_block_test_evaluation_v1"
                            && m["complete"] == true
                            && m["test_used_for_selection"] == false
                            && m["dataset_manifest_sha256"] == sha(&bundle.join("manifest.json"))?
                            && m["selection_receipt_sha256"] == dm["selection_receipt_sha256"],
                        "current Test evaluation binding"
                    );
                    let matches = m["reports"]
                        .as_array()
                        .context("Test reports")?
                        .iter()
                        .filter(|r| r["condition"] == condition && r["seed"] == seed)
                        .collect::<Vec<_>>();
                    ensure!(
                        matches.len() == 1 && matches[0]["predictions_sha256"] == sha(&file)?,
                        "current Test prediction changed"
                    );
                } else {
                    ensure!(
                        m["schema"] == "slop_ninja_last_block_evaluation_v1"
                            && m["complete"] == true
                            && m["final_test_opened"] == false
                            && m["dataset_manifest_sha256"] == sha(&bundle.join("manifest.json"))?
                            && m["condition"] == condition
                            && m["seed"] == seed
                            && m["selection_sha256"]
                                == sha(&stage.join("calibration-run-v1/selection.json"))?
                            && m["reports"][part]["predictions_sha256"] == sha(&file)?,
                        "current evaluation binding"
                    );
                }
                receipt_bindings.insert(format!("{condition}-{seed}"), sha(&manifest_path)?);
                let p: Pred = serde_json::from_slice(&fs::read(file)?)?;
                validate_support(&data, &p)?;
                predictions.entry(condition.into()).or_default().push(p);
            }
        }
        let mut paired = BTreeMap::new();
        for (a, b) in [
            ("adapt_last_block", "frozen_last_block"),
            ("adapt_last_block", "prior_a"),
            ("adapt_last_block", "v6"),
            ("prior_a", "v6"),
        ] {
            let samples = (0..3)
                .map(|i| {
                    difference(
                        &data,
                        &predictions[a][i],
                        &predictions[b][i],
                        "involvement_brier",
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let balanced = samples
                .iter()
                .map(|v| balance_origins(&data, v))
                .collect::<Result<Vec<_>>>()?;
            let mut result = aggregate::fixed_seed_summary(&balanced)?;
            let cells = result.as_object_mut().unwrap().remove("documents").unwrap();
            result["family_origin_cells"] = cells;
            result["documents"] = json!(data.len());
            paired.insert(format!("{a}_minus_{b}"),json!({"family":aggregate::fixed_seed_summary(&samples)?,"family_origin_balanced":result,
                "negative_favors_first":true,"fixed_seeds":[17,29,41],"V6_single_fixed_prediction_reused_across_pairings":true}));
        }
        let report = json!({"partition":part,"requested":data.len(),"v6":v6report,"prior_a":prior_reports,"paired_involvement_brier":paired,
            "new_test_opened":final_test,"upstream_requested_queries":dm["upstream_requested_queries"],
            "diagnostic_only_no_reselection":true,"CFG_features_in_this_experiment":0});
        put(&out.join(format!("{part}.json")), &report)?;
        reports.insert(part, sha(&out.join(format!("{part}.json")))?);
    }
    put(
        &out.join("manifest.json"),
        &json!({"schema":if final_test {"slop_ninja_fixed_last_block_test_comparator_evaluation_v1"} else {"slop_ninja_fixed_last_block_comparator_evaluation_v1"},"complete":true,
        "prepared_manifest_sha256":sha(&prepared.join("manifest.json"))?,"v6_manifest_sha256":sha(&v6.join("manifest.json"))?,
        "current_evaluations":receipt_bindings,"reports":reports,"final_test_opened":final_test,"fitting_or_calibration_performed":false,
        "selection_receipt_sha256":dm["selection_receipt_sha256"],
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_fixed_last_block_comparators.rs")))}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.len() == 5 && a[0] == "--test" {
        return run(
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3]),
            Path::new(&a[4]),
            true,
        );
    }
    ensure!(a.len() == 4, "STAGE PREPARED V6 NEW_OUTPUT");
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        false,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prior_roster_controls_known_status_without_changing_presence() {
        let rows = vec![
            json!({"roles":{"human":{"identity":"new-writer","present":true,"known":true},"ai":{"identity":null,"present":false,"known":false}}}),
        ];
        let c = json!({"human_profiles":["old-writer"],"ai_profiles":["model"]});
        let view = role_view(&rows, &c).unwrap();
        assert_eq!(view[0]["roles"]["human"]["known"], false);
        assert_eq!(view[0]["roles"]["human"]["present"], true);
        assert_eq!(rows[0]["roles"]["human"]["known"], true);
    }
}
