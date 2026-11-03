use super::*;

fn labelled() -> Pred {
    (0..6)
        .map(|i| {
            let id = format!("synthetic-{i}");
            (
                id.clone(),
                json!({"id":id,"source_group":format!("family-{}",i/3),
        "targets":{"origin":i%3,"share":if i%3==0{0.}else{1.}}}),
            )
        })
        .collect()
}
fn probabilities(rows: &Pred, p: f64) -> Pred {
    rows.keys()
        .map(|id| (id.clone(), json!({"ai_confidence_percent":100.*p})))
        .collect()
}
fn roster() -> Vec<Value> {
    final_test_binding::CANDIDATE_NAMES
        .into_iter()
        .flat_map(|name| SEEDS.map(|seed| json!({"candidate":name,"seed":seed})))
        .collect()
}

#[test]
fn exact_48_run_roster_rejects_omissions_duplicates_diagnostics_and_best_seed() {
    let good = roster();
    evidence::roster(&good).unwrap();
    let mut bad = good.clone();
    bad.pop();
    assert!(evidence::roster(&bad).is_err());
    let mut bad = good.clone();
    bad[1] = bad[0].clone();
    assert!(evidence::roster(&bad).is_err());
    let mut bad = good.clone();
    bad[0]["candidate"] = json!("origin_only");
    assert!(evidence::roster(&bad).is_err());
    let mut bad = good;
    bad[0]["seed"] = json!(7);
    assert!(evidence::roster(&bad).is_err());
}
#[test]
fn pins_reject_changed_file_and_conflicting_pin() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("synthetic.json");
    std::fs::write(&path, b"{}").unwrap();
    let hash = b::sha(&path).unwrap();
    let mut pins = evidence::Pins::default();
    pins.add(path.clone(), &hash).unwrap();
    std::fs::write(&path, b"[]").unwrap();
    assert!(pins.unchanged().is_err());
    assert!(pins.add(path, &hash).is_err());
}
#[test]
fn counts_preserve_requested_416_assembled_414_and_two_terminal_failures() {
    let input = json!({"requested":416,"assembled":414,"excluded":[],"terminal_generation_failures":[{"id":"a"},{"id":"b"}],"accounted":true});
    let out = evidence::public_upstream(&input).unwrap();
    assert_eq!(out["requested_queries"], 416);
    assert_eq!(out["terminal_generation_failures"], 2);
    warm::public_boundary(&out).unwrap();
    let mut wrong = input;
    wrong["assembled"] = json!(415);
    assert!(evidence::public_upstream(&wrong).is_err());
}
#[test]
fn pairing_intersects_all_fixed_seeds_and_counts_documents_not_cells() {
    let mut rows = labelled();
    let mut extra = rows["synthetic-0"].clone();
    extra["id"] = json!("synthetic-extra");
    rows.insert("synthetic-extra".into(), extra);
    let a = probabilities(&rows, 0.2);
    let b = probabilities(&rows, 0.6);
    let mut missing = a.clone();
    missing.remove("synthetic-5");
    let ids = metrics::common(&rows, &[&a, &a, &missing, &b]);
    assert_eq!(ids.len(), 6);
    let result = metrics::comparison(
        "left",
        "right",
        &metrics::rows(&rows, &ids),
        &[&a, &a, &missing],
        &[&b, &b, &b],
    )
    .unwrap();
    let balanced = &result["mean_over_fixed_seeds_then_family_bootstrap"]["family_origin_balanced"];
    assert_eq!(balanced["documents"], 6);
    assert_eq!(balanced["balanced_cells"], 5);
    assert_eq!(balanced["families"], 2);
    let full = metrics::rows(&rows, &rows.keys().cloned().collect());
    assert!(metrics::comparison("a", "b", &full, &[&a, &a, &missing], &[&b, &b, &b]).is_err());
}
#[test]
fn fixed_seed_average_is_not_best_seed_or_three_independent_populations() {
    let rows = labelled();
    let rr = metrics::rows(&rows, &rows.keys().cloned().collect());
    let a = probabilities(&rows, 0.1);
    let b = probabilities(&rows, 0.5);
    let c = probabilities(&rows, 0.9);
    let q =
        metrics::comparison("selected", "comparator", &rr, &[&a, &b, &c], &[&b, &b, &b]).unwrap();
    let x = q["mean_over_fixed_seeds_then_family_bootstrap"]["family"]["estimate"]
        .as_f64()
        .unwrap();
    assert!((x - 0.10666666666666667).abs() < 1e-12);
    assert_eq!(
        q["mean_over_fixed_seeds_then_family_bootstrap"]["family"]["families"],
        2
    );
    let absent = metrics::repeated(&[json!(0.1), Value::Null, json!(0.2)]).unwrap();
    assert_eq!(absent["available_seeds"], 2);
    assert!(absent["mean"].is_null());
}
#[test]
fn empty_common_support_is_null_not_zero_success() {
    let rows = labelled();
    let p = probabilities(&rows, 0.2);
    let empty = Pred::new();
    let ids = metrics::common(&rows, &[&p, &empty]);
    assert!(ids.is_empty());
    let result =
        metrics::comparison("left", "right", &[], &[&p, &p, &empty], &[&p, &p, &p]).unwrap();
    assert_eq!(result["documents"], 0);
    assert!(
        result["mean_over_fixed_seeds_then_family_bootstrap"]["family_origin_balanced"]["estimate"]
            .is_null()
    );
}
#[test]
fn frozen_mapping_interpolates_clamps_and_rejects_nonmonotonic_knots() {
    let cal =
        json!({"knots":[{"fraction":0.2,"involvement":0.7},{"fraction":0.6,"involvement":0.9}]});
    assert_eq!(evidence::mapped(&cal, 0.).unwrap(), 0.7);
    assert_eq!(evidence::mapped(&cal, 1.).unwrap(), 0.9);
    assert!((evidence::mapped(&cal, 0.4).unwrap() - 0.8).abs() < 1e-12);
    let mut wrong = cal.clone();
    wrong["knots"][1]["involvement"] = json!(0.2);
    assert!(evidence::mapped(&wrong, 0.3).is_err());
    let mut wrong = cal;
    wrong["knots"][1]["fraction"] = json!(0.2);
    assert!(evidence::mapped(&wrong, 0.3).is_err());
}
#[test]
fn production_history_keeps_full_rewrite_mixed_and_unknown_writer_masked() {
    let row = json!({"targets":{"origin":2,"share":1.}});
    let m = json!({"human_role_present":true,"human_identity":null,"ai_role_present":true,"ai_identity":"synthetic-withheld",
        "generation":{"mode":"controlled_rewrite","position":"start"},"share_evidence":{"full_rewrite_is_mixed_history":true}});
    let c = json!({"human_profiles":["synthetic-writer"],"ai_profiles":["synthetic-model"]});
    let categories = strata::categories(&row, &m, &c)
        .unwrap()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        categories["production_history"],
        "full_rewrite_mixed_history"
    );
    assert_eq!(categories["writer_status"], "identity_unknown");
    assert_eq!(categories["generator"], "withheld");
    assert_eq!(categories["replacement_position"], "full_document");
}
#[test]
fn public_table_cannot_project_profiles_paths_or_private_dynamic_metric_keys() {
    let runs=SEEDS.iter().map(|seed|Run {candidate:"warm_text".into(),seed:*seed,pred:Pred::new(),pg:Pred::new(),
        report:json!({"fit":{"path":"/private/synthetic"},"model":{"profile":"synthetic-secret","calibration_bins":[]},
            "attribution":{"human":{"conditional_top1_by_identity":{"synthetic-secret":1.},"open_set":{"synthetic-secret":1.}}},
            "family_origin_balanced":{"source_group":"synthetic-secret","calibration_bins":[]}}),pg_report:json!({}),support:json!({}),evidence:json!({})}).collect::<Vec<_>>();
    let value = metrics::table(&runs).unwrap();
    warm::public_boundary(&value).unwrap();
    let s = serde_json::to_string(&value).unwrap();
    assert!(!s.contains("synthetic-secret"));
    assert!(!s.contains("/private"));
    assert_eq!(
        value["fixed_seed_statistics"]["human_conditional_accuracy"]["available_seeds"],
        0
    );
}
#[test]
fn changed_aggregate_is_rejected_not_silently_published() {
    metrics::equal(&json!({"estimate":0.1}), &json!({"estimate":0.1})).unwrap();
    assert!(metrics::equal(&json!({"estimate":0.1}), &json!({"estimate":0.2})).is_err());
    assert!(metrics::equal(&json!({"estimate":0.1,"extra":0}), &json!({"estimate":0.1})).is_err());
}

#[test]
fn same_provider_support_is_required_across_all_candidate_reports() {
    let mut evidence = evidence::ProviderEvidence::default();
    evidence.supported("synthetic-document", true).unwrap();
    evidence.supported("synthetic-document", true).unwrap();
    assert!(evidence.supported("synthetic-document", false).is_err());
}

#[test]
fn fixed_share_bins_and_reference_retrieval_are_projected_without_private_fields() {
    let bins = ["0", "(0,25]", "(25,50]", "(50,75]", "(75,100)", "100"];
    let share_metrics = [
        "mae_percentage_points",
        "mse_percentage_points_squared",
        "bias_percentage_points",
    ];
    let retrieval_metrics = [
        "top1",
        "top3",
        "mrr",
        "head_known_top1",
        "head_withheld_top1",
    ];
    let mut keys = vec![
        "balanced_origin_accuracy".to_owned(),
        "balanced_origin_brier_sum3".to_owned(),
        "prior_involvement_brier".to_owned(),
    ];
    for bin in bins {
        keys.extend(share_metrics.map(|metric| format!("share_bin_{bin}_{metric}")));
        keys.push(format!("share_bin_{bin}_documents"));
    }
    for role in ["human", "ai"] {
        for metric in retrieval_metrics {
            keys.push(format!("reference_{role}_{metric}"));
            keys.push(format!("reference_{role}_{metric}_documents"));
        }
        keys.push(format!("reference_{role}_gallery_identities"));
    }
    let mut runs = SEEDS.iter().enumerate().map(|(i, seed)| {
        let mut report = json!({
            "model":{"share":{"by_realized_share":{
                "synthetic-secret":{"identity":"synthetic-secret"}
            }}},
            "family_origin_balanced":{"origin_accuracy":0.5+i as f64*0.1,"origin_brier_sum3":0.3+i as f64*0.1},
            "prior_baseline":{"involvement_brier":0.2+i as f64*0.1},
            "reference_matching":{"available":true,"metrics":{
                "human":{"gallery_identities":3,"query_diagnostics":[{"id":"synthetic-secret","top1":"synthetic-secret"}],"identities":["synthetic-secret"]},
                "ai":{"gallery_identities":2,"query_diagnostics":[{"id":"synthetic-secret","top1":"synthetic-secret"}],"identities":["synthetic-secret"]}
            }}
        });
        for (bin_index, bin) in bins.iter().enumerate() {
            report["model"]["share"]["by_realized_share"][*bin] = json!({});
            for (metric_index, metric) in share_metrics.iter().enumerate() {
                report["model"]["share"]["by_realized_share"][*bin][*metric] = json!({
                    "estimate":bin_index as f64+i as f64+metric_index as f64*0.25,
                    "documents":bin_index+i+1,"source_group":"synthetic-secret"
                });
            }
        }
        for role in ["human", "ai"] {
            for (metric_index, metric) in retrieval_metrics.iter().enumerate() {
                report["reference_matching"]["metrics"][role][*metric] = json!({
                    "estimate":0.1+metric_index as f64*0.1+i as f64*0.05,
                    "documents":metric_index+i+1,"identity":"synthetic-secret"
                });
            }
        }
        Run {candidate:"warm_text".into(),seed:*seed,pred:Pred::new(),pg:Pred::new(),
            report,pg_report:json!({}),support:json!({}),evidence:json!({})}
    }).collect::<Vec<_>>();
    let value = metrics::table(&runs).unwrap();
    for key in &keys {
        for (i, run) in runs.iter().enumerate() {
            let expected = if let Some(metric) = key.strip_prefix("balanced_") {
                &run.report["family_origin_balanced"][metric]
            } else if key == "prior_involvement_brier" {
                &run.report["prior_baseline"]["involvement_brier"]
            } else {
                // Every fixed bin and role metric must have a populated scalar.
                &value["per_seed"][i]["metrics"][key]
            };
            assert!(expected.is_number(), "missing {key} in seed {i}");
            assert_eq!(
                number(&value["per_seed"][i]["metrics"][key]).unwrap(),
                number(expected).unwrap()
            );
        }
        assert_eq!(value["fixed_seed_statistics"][key]["available_seeds"], 3);
        assert!(
            (number(&value["fixed_seed_statistics"][key]["mean"]).unwrap()
                - number(&value["per_seed"][1]["metrics"][key]).unwrap())
            .abs()
                < 1e-12
        );
    }
    // Check exact distinct-bin and known/withheld paths, not just presence.
    assert_eq!(
        number(
            &value["per_seed"][0]["metrics"]["share_bin_(75,100)_mse_percentage_points_squared"]
        )
        .unwrap(),
        4.25
    );
    assert_eq!(
        number(&value["per_seed"][2]["metrics"]["share_bin_100_documents"]).unwrap(),
        8.
    );
    assert_eq!(
        number(&value["per_seed"][2]["metrics"]["reference_ai_head_withheld_top1_documents"])
            .unwrap(),
        7.
    );
    assert_eq!(
        number(&value["per_seed"][0]["metrics"]["reference_human_gallery_identities"]).unwrap(),
        3.
    );
    warm::public_boundary(&value).unwrap();
    let encoded = serde_json::to_string(&value).unwrap();
    assert!(!encoded.contains("synthetic-secret"));
    assert!(!encoded.contains("query_diagnostics"));

    // Explicit unavailability masks even stale metric objects; missing bins stay null.
    runs[2].report["reference_matching"]["available"] = json!(false);
    runs[2].report["model"]["share"]["by_realized_share"]
        .as_object_mut()
        .unwrap()
        .remove("0");
    let unavailable = metrics::table(&runs).unwrap();
    for key in [
        "share_bin_0_mae_percentage_points",
        "share_bin_0_documents",
        "reference_ai_head_withheld_top1",
        "reference_ai_head_withheld_top1_documents",
        "reference_human_gallery_identities",
    ] {
        assert!(unavailable["per_seed"][2]["metrics"][key].is_null());
        assert!(unavailable["fixed_seed_statistics"][key]["mean"].is_null());
        assert_eq!(
            unavailable["fixed_seed_statistics"][key]["available_seeds"],
            2
        );
    }
}
