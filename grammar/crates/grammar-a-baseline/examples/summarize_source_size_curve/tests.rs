use super::*;
#[test]
fn seed_means_exclude_ci_endpoints_and_missing_training_gaps() {
    let v = vec![
        json!({"metric":{"estimate":1.,"ci95":[0.,2.]},"selected_epoch_gap":0.2}),
        json!({"metric":{"estimate":2.,"ci95":[1.,3.]},"selected_epoch_gap":null}),
        json!({"metric":{"estimate":3.,"ci95":[2.,4.]},"selected_epoch_gap":0.4}),
    ];
    let m = mean_values(&v).unwrap();
    assert_eq!(m["metric"]["estimate"], 2.);
    assert!(m["metric"].get("ci95").is_none());
    assert!(m["selected_epoch_gap"].is_null());
    assert!(mean_values(&v[..2]).is_err());
}
#[test]
fn learning_curve_keeps_uncalibrated_gap_and_does_not_interpolate_checkpoint() {
    let history=(0..=10).map(|i|json!({"epoch":i*25,"development_involvement_brier":0.2,"train_involvement_brier":0.1,"train_joint_loss":0.8})).collect::<Vec<_>>();
    let fit = json!({"history":history,"selected_epoch":33});
    let c = learning_curve(&fit, true).unwrap();
    assert!(c["selected_epoch_gap"].is_null());
    assert_eq!(c["points"][0]["development_minus_train_brier"], 0.1);
    let mut old = fit;
    for p in old["history"].as_array_mut().unwrap() {
        p.as_object_mut().unwrap().remove("train_involvement_brier");
        p.as_object_mut().unwrap().remove("train_joint_loss");
    }
    assert_eq!(
        learning_curve(&old, false).unwrap()["train_history_available"],
        false
    );
    assert!(learning_curve(&old, true).is_err());
}
fn prediction() -> Value {
    let role = json!({"profile":"other_human","fallback_asserts_presence":false,"involvement_probability":0.7,"conditional_named_scores":{"synthetic_writer":1.0}});
    json!({"ai_confidence_percent":20.,"ai_share_percent":10.,"origin":{"human_only":0.8,"model_only":0.1,"mixed":0.1},"attribution":{"human":role,"ai":role}})
}
#[test]
fn replay_checks_actual_predictions_not_only_claimed_delta() {
    let p = prediction();
    let expected = BTreeMap::from([("synthetic".into(), p.clone())]);
    let r = json!({"predictions":[{"id":"synthetic","prediction":p}],"max_absolute_delta_percentage_points":0.});
    evidence::replay(&r, &expected).unwrap();
    let mut bad = r.clone();
    bad["predictions"][0]["prediction"]["ai_confidence_percent"] = json!(90.);
    assert!(evidence::replay(&bad, &expected).is_err());
    let mut bad = r.clone();
    bad["predictions"]
        .as_array_mut()
        .unwrap()
        .push(r["predictions"][0].clone());
    assert!(evidence::replay(&bad, &expected).is_err());
    let mut bad = r;
    bad["predictions"][0]["prediction"]["attribution"]["human"]["profile"] =
        json!("synthetic_writer");
    assert!(evidence::replay(&bad, &expected).is_err());
}
#[test]
fn paired_curve_preserves_family_origin_weighting_and_seed_support() {
    let rows = vec![
        json!({"id":"a","source_group":"one","targets":{"origin":0,"share":0.}}),
        json!({"id":"b","source_group":"one","targets":{"origin":2,"share":0.5}}),
        json!({"id":"c","source_group":"two","targets":{"origin":1,"share":1.}}),
    ];
    let left = BTreeMap::from([
        (
            "a".into(),
            json!({"ai_confidence_percent":0.,"ai_share_percent":0.}),
        ),
        (
            "b".into(),
            json!({"ai_confidence_percent":100.,"ai_share_percent":50.}),
        ),
        (
            "c".into(),
            json!({"ai_confidence_percent":100.,"ai_share_percent":100.}),
        ),
    ]);
    let right = BTreeMap::from([
        (
            "a".into(),
            json!({"ai_confidence_percent":100.,"ai_share_percent":100.}),
        ),
        (
            "b".into(),
            json!({"ai_confidence_percent":0.,"ai_share_percent":0.}),
        ),
        (
            "c".into(),
            json!({"ai_confidence_percent":0.,"ai_share_percent":0.}),
        ),
    ]);
    let result = pair(&rows, &[&left; 3], &[&right; 3], "involvement_brier").unwrap();
    assert_eq!(
        result["mean_over_fixed_seeds_then_family_bootstrap"]["estimate"],
        -1.
    );
    assert_eq!(
        result["mean_over_fixed_seeds_then_family_bootstrap"]["families"],
        2
    );
    assert!(pair(&rows, &[&left; 2], &[&right; 3], "involvement_brier").is_err());
    warm::public_boundary(&result).unwrap();
}
#[test]
fn paired_brier_distinguishes_documents_and_cells_without_changing_estimates_or_cis() {
    let mut rows = Vec::new();
    for (family, origin, documents) in [("one", 0, 3), ("one", 2, 2), ("two", 1, 4)] {
        for i in 0..documents {
            rows.push(json!({"id":format!("{family}-{origin}-{i}"),"source_group":family,"targets":{"origin":origin}}));
        }
    }
    let left = (0..3)
        .map(|seed| {
            rows.iter()
                .enumerate()
                .map(|(i, row)| {
                    (
                        row["id"].as_str().unwrap().to_owned(),
                        json!({"ai_confidence_percent":10. + 7. * i as f64 + 3. * seed as f64}),
                    )
                })
                .collect::<Pred>()
        })
        .collect::<Vec<_>>();
    let right = rows
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap().to_owned(),
                json!({"ai_confidence_percent":45.}),
            )
        })
        .collect::<Pred>();
    let samples = left
        .iter()
        .map(|pred| {
            balance_origins(
                &rows,
                &difference(&rows, pred, &right, "involvement_brier").unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let result = pair(
        &rows,
        &left.iter().collect::<Vec<_>>(),
        &[&right; 3],
        "involvement_brier",
    )
    .unwrap();
    // Exact equality of all other fields retains the existing estimates/CIs.
    let expected = |mut legacy: Value| {
        assert_eq!(legacy["documents"], 3);
        legacy["documents"] = json!(9);
        legacy["balanced_cells"] = json!(3);
        legacy
    };
    for (i, values) in samples.iter().enumerate() {
        assert_eq!(
            result["per_seed"][i]["difference"],
            expected(family_summary(values))
        );
    }
    assert_eq!(
        result["mean_over_fixed_seeds_then_family_bootstrap"],
        expected(warm::fixed_seed_summary(&samples).unwrap())
    );
    warm::public_boundary(&result).unwrap();
}
#[test]
fn nested_contrasts_use_direct_differences_and_publish_only_aggregates() {
    let rows = vec![
        json!({"id":"private-query-a","source_group":"author_work_v2:private-a","targets":{"origin":0,"share":0.}}),
        json!({"id":"private-query-b","source_group":"/home/private/family-b","targets":{"origin":0,"share":0.}}),
    ];
    let mut runs = BTreeMap::new();
    for (tag, confidence) in [
        ("original:text", 60.),
        ("12:text", 50.),
        ("24:text", 40.),
        ("all:text", 30.),
        ("all:combined", 20.),
    ] {
        let pred = rows
            .iter()
            .map(|r| {
                (
                    r["id"].as_str().unwrap().to_owned(),
                    json!({"ai_confidence_percent":confidence,"ai_share_percent":confidence}),
                )
            })
            .collect::<Pred>();
        runs.insert(
            tag.into(),
            SEEDS
                .iter()
                .map(|_| Run {
                    public: Value::Null,
                    pred: PARTS
                        .iter()
                        .map(|p| (p.to_string(), pred.clone()))
                        .collect(),
                    pangram: Pred::new(),
                    pangram_observations: Value::Null,
                    shape: Value::Null,
                })
                .collect(),
        );
    }
    for part in PARTS {
        let comparisons = model_comparisons(&rows, &runs, part).unwrap();
        assert_eq!(comparisons.len(), 32);
        for (left, right, expected) in [
            ("24:text", "12:text", -0.09),
            ("all:text", "24:text", -0.07),
            ("all:text", "12:text", -0.16),
        ] {
            let p = comparisons
                .iter()
                .find(|p| {
                    p["left"] == left && p["right"] == right && p["metric"] == "involvement_brier"
                })
                .unwrap();
            assert_eq!(
                p["comparison_role"],
                "primary_data_size_fixed_stratum_mixture"
            );
            assert_eq!(p["split"], part);
            assert!(
                (p["mean_over_fixed_seeds_then_family_bootstrap"]["estimate"]
                    .as_f64()
                    .unwrap()
                    - expected)
                    .abs()
                    < 1e-12
            );
            assert_eq!(p["per_seed"].as_array().unwrap().len(), 3);
        }
        assert!(
            comparisons
                .iter()
                .filter(|p| p["right"] == "original:text")
                .all(|p| p["comparison_role"]
                    == "original_selected_comparator_also_changes_stratum_mixture")
        );
        let public = json!(comparisons);
        warm::public_boundary(&public).unwrap();
        let encoded = public.to_string();
        for private in ["private-query", "author_work_v2:", "/home/private"] {
            assert!(!encoded.contains(private));
        }
    }
}
fn selection_fixture(
    original_families: usize,
    added_families: usize,
    rows_per_family: usize,
) -> (
    Value,
    BTreeMap<String, Value>,
    BTreeMap<String, BTreeMap<String, Value>>,
) {
    let mut rows = Vec::new();
    let mut selected = Vec::new();
    for st in std::iter::once("original").chain(STRATA) {
        let (families, mass) = if st == "original" {
            (original_families, 0.5)
        } else {
            (added_families, 0.1)
        };
        for i in 0..families {
            let family = format!("{st}:{i}");
            if st != "original" {
                selected.push(family.clone());
            }
            for j in 0..rows_per_family {
                rows.push(json!({"id":format!("root-{family}-{j}"),"source_group":family,"stratum":st,"selected":true,
                    "weight":mass / families as f64 / rows_per_family as f64}));
            }
        }
    }
    let original_rows = original_families * rows_per_family;
    let p = json!({"schema":"slop_ninja_a_source_size_sampling_v1","level":added_families.to_string(),"epochs":250,"seeds":SEEDS,"source_strata":STRATA,
        "original_cohort_mass":0.5,"added_stratum_mass":0.1,"within_stratum":"family_origin_balanced","apply_to":"train_only","normalization_fitted":false,
        "profiles_changed":false,"final_test_opened":false,"initialization_sha256":"init","design_protocol_sha256":"design","allowed_modes":["text"],
        "numeric_manifest_sha256":{"text":"numeric","combined":"numeric"},"parent_manifest_sha256":{"text":"parent","combined":"parent"},
        "extension_dataset_manifest_sha256":"dataset","original_rows":original_rows,"total_train_rows":rows.len(),"rows":rows,"selected_added_families":selected});
    let m = json!({"training_extension":{"parent_manifest_sha256":"parent","extension_dataset_manifest_sha256":"dataset","original_partitions":{"train":{"rows":original_rows}}}});
    let manifests = BTreeMap::from([
        ("large:text".into(), m.clone()),
        ("large:combined".into(), m),
    ]);
    let b = BTreeMap::from([("development".into(), json!({"manifest_sha256":"numeric"}))]);
    let bindings = BTreeMap::from([
        ("large:text".into(), b.clone()),
        ("large:combined".into(), b),
    ]);
    (p, manifests, bindings)
}
#[test]
fn sampling_population_counts_and_bindings_fail_closed() {
    let (p, m, b) = selection_fixture(1, 12, 1);
    let (view, _) = evidence::selection(&p, "12", &m, &b, "init", "design").unwrap();
    assert_eq!(view["selected_train_families"], 61);
    warm::public_boundary(&view).unwrap();
    for (key, value) in [
        ("original_cohort_mass", json!(0.6)),
        ("initialization_sha256", json!("changed")),
        ("final_test_opened", json!(true)),
    ] {
        let mut bad = p.clone();
        bad[key] = value;
        assert!(evidence::selection(&bad, "12", &m, &b, "init", "design").is_err());
    }
    let mut bad = p;
    bad["rows"][1]["selected"] = json!(false);
    assert!(evidence::selection(&bad, "12", &m, &b, "init", "design").is_err());
}
#[test]
fn effective_family_count_aggregates_frozen_row_mass_before_squaring() {
    for (n, expected) in [(12, 40.), (24, 480. / 11.)] {
        for descendants in [1, 3] {
            let (mut p, m, b) = selection_fixture(12, n, descendants);
            // An available but unselected family must not affect concentration.
            p["rows"].as_array_mut().unwrap().push(json!({
                "id":"private-unselected-query","source_group":"/home/private/unselected-family",
                "stratum":STRATA[0],"selected":false,"weight":0.
            }));
            p["total_train_rows"] = json!(p["rows"].as_array().unwrap().len());
            let (view, _) =
                evidence::selection(&p, &n.to_string(), &m, &b, "init", "design").unwrap();
            assert_eq!(view["selected_train_families"], 12 + 5 * n);
            let c = &view["origin_loss_weight_concentration"];
            assert!((c["effective_family_count"].as_f64().unwrap() - expected).abs() < 1e-12);
            assert!((c["total_family_mass"].as_f64().unwrap() - 1.).abs() < 1e-12);
            assert!(
                (c["sum_squared_family_mass"].as_f64().unwrap() - (0.25 / 12. + 0.05 / n as f64))
                    .abs()
                    < 1e-12
            );
            warm::public_boundary(&view).unwrap();
            let encoded = view.to_string();
            for row in p["rows"].as_array().unwrap() {
                for key in ["id", "source_group"] {
                    assert!(!encoded.contains(row[key].as_str().unwrap()));
                }
            }
        }
    }
}
#[test]
fn public_result_projections_omit_names_and_paths() {
    let r = json!({"attribution":{"human":{"conditional_accuracy":{"estimate":0.4},"conditional_top1_by_identity":{"PRIVATE_WRITER":1.}}},
        "reference_matching":{"metrics":{"human":{"top1":{"estimate":0.2},"query_diagnostics":[{"id":"private-query"}]}}},"fit":{"path":"/home/private"}});
    let public = warm::evaluation_metrics(&r);
    warm::public_boundary(&public).unwrap();
    assert!(!public.to_string().contains("PRIVATE_WRITER"));
    assert!(
        warm::public_boundary(&json!({"selected_added_families":["author_work_v2:private"]}))
            .is_err()
    );
}
