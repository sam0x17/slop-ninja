//! Additional prerequisites and comparisons for the frozen larger-backbone study.
use super::*;
use std::collections::BTreeSet;

const CAPACITY_POLICY: &str = include_str!("../../CAPACITY_PRIVATE_TRANSFER_V1.md");
const TOKENIZERS: [&str; 3] = [
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
];

pub(super) struct Evidence {
    base_rows: Vec<Value>,
}

pub(super) fn validate_arguments(args: &Args, plan: &Value) -> Result<()> {
    let present = [
        args.base_public_review.is_some(),
        args.base_private_output.is_some(),
        args.cross_size_review.is_some(),
    ];
    if plan["capacity_protocol"].is_null() {
        ensure!(
            present.iter().all(|p| !p)
                && plan["require_both_sizes_public_comparisons"].is_null()
                && plan["require_complete_base_private_evidence"].is_null(),
            "Base plans cannot accept capacity evidence or partial capacity requirements"
        );
    } else {
        ensure!(
            present.iter().all(|p| *p)
                && plan["require_both_sizes_public_comparisons"] == true
                && plan["require_complete_base_private_evidence"] == true,
            "Capacity plans require base-public-review, base-private-output and cross-size-review"
        );
    }
    Ok(())
}

fn probability(row: &Value) -> Result<f64> {
    row["whole_observation"]["model_or_assistance_probability"]
        .as_f64()
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .context("Missing whole probability")
}

fn flags(p: f64, points: &Value) -> Result<Value> {
    Ok(json!(
        points
            .as_array()
            .context("Missing operating points")?
            .iter()
            .map(|t| Ok(p > t["threshold"].as_f64().context("Missing threshold")?))
            .collect::<Result<Vec<_>>>()?
    ))
}

fn check_paired_report(
    compared: &Value,
    base: &Value,
    large: &Value,
    base_hash: &str,
    large_hash: &str,
    base_candidate: &Value,
    large_candidate: &Value,
) -> Result<()> {
    ensure!(
        compared["schema"] == "slop_ninja_paired_encoder_comparison_v1"
            && compared["baseline_report_sha256"] == base_hash
            && compared["candidate_report_sha256"] == large_hash
            && compared["baseline_artifact_id"] == base_candidate["id"]
            && compared["candidate_artifact_id"] == large_candidate["id"]
            && base_candidate["id"] != large_candidate["id"],
        "Wrong source report, artifact or comparison direction"
    );
    for (report, candidate, side) in [
        (base, base_candidate, "baseline"),
        (large, large_candidate, "candidate"),
    ] {
        ensure!(
            report["schema"] == "slop_ninja_frozen_encoder_evaluation_v1"
                && report["artifact_id"] == candidate["id"]
                && report["split"] == "test"
                && report["final_test_opened"] == true
                && report["records_sha256"] == compared["records_sha256"]
                && report["evaluation_view"] == compared["evaluation_view"]
                && report["observation_policy"] == compared["observation_policy"]
                && compared["split"] == "test",
            "Cross-size corpus, view, policy or artifact differs"
        );
        let points = report["report"]["operating_points"]
            .as_array()
            .context("Missing scored operating points")?;
        ensure!(
            json!(points.iter().map(|p| &p["calibration"]).collect::<Vec<_>>())
                == candidate["points"]
                && compared[format!("{side}_operating_points")]
                    == report["report"]["operating_points"]
                && compared[format!("{side}_metrics")] == report["report"]["model"],
            "Cross-size thresholds or reported metrics differ"
        );
    }
    for (key, objective) in [
        ("comparison", "three_class"),
        ("human_model_comparison", "human_model"),
    ] {
        ensure!(
            compared[key]["objective"] == objective
                && compared[key]["bootstrap_seed"] == "243f6a8885a308d3"
                && compared[key]["bootstrap_replicates"] == 512,
            "Paired comparison recipe differs"
        );
    }
    Ok(())
}

struct PublicSide<'a> {
    plan_path: &'a Path,
    public: &'a Value,
    candidates: &'a BTreeMap<&'a str, Value>,
}

fn cross_size_evidence(
    receipt_path: &Path,
    base: PublicSide<'_>,
    large: PublicSide<'_>,
    bindings: &mut BTreeMap<PathBuf, String>,
) -> Result<()> {
    let review = read(receipt_path)?;
    ensure!(
        review["schema"] == "slop_ninja_capacity_cross_size_review_v1"
            && !string(&review, "reviewer")?.trim().is_empty()
            && review["notes"].as_array().is_some_and(|r| !r.is_empty())
            && review["base_public_plan_sha256"] == digest(base.plan_path)?
            && review["large_public_plan_sha256"] == digest(large.plan_path)?,
        "Missing or stale cross-size review"
    );
    let cases = large.public["cases"]
        .as_array()
        .context("Missing large cases")?;
    ensure!(
        large.public["cases"] == base.public["cases"] && cases.len() == 7,
        "The two sizes must retain the same seven cases"
    );
    let expected: BTreeSet<_> = ROLES
        .into_iter()
        .flat_map(|role| {
            cases
                .iter()
                .map(move |case| Ok((role.to_string(), string(case, "name")?.to_string())))
        })
        .collect::<Result<_>>()?;
    let mut seen = BTreeSet::new();
    for item in review["comparisons"]
        .as_array()
        .context("Missing reviewed comparisons")?
    {
        let role = string(item, "role")?;
        let case = string(item, "case")?;
        let key = (role.to_string(), case.to_string());
        ensure!(
            expected.contains(&key) && seen.insert(key),
            "Unexpected or duplicate cross-size case"
        );
        let p = path(item, "report")?;
        ensure!(
            item["sha256"] == digest(&p)?,
            "Cross-size report changed after review"
        );
        let bp = path(base.public, "output")?
            .join(role)
            .join(case)
            .join("report.json");
        let lp = path(large.public, "output")?
            .join(role)
            .join(case)
            .join("report.json");
        check_paired_report(
            &read(&p)?,
            &read(&bp)?,
            &read(&lp)?,
            &digest(&bp)?,
            &digest(&lp)?,
            &base.candidates[role],
            &large.candidates[role],
        )?;
        bind(bindings, &p)?;
    }
    ensure!(
        seen == expected,
        "Cross-size review must cover both roles on all seven cases"
    );
    bind(bindings, receipt_path)
}

fn base_private_evidence(
    output: &Path,
    base_plan_path: &Path,
    base_plan: &Value,
    candidates: &BTreeMap<&str, Value>,
    bindings: &mut BTreeMap<PathBuf, String>,
) -> Result<Vec<Value>> {
    let completion = read(&output.join("completion.json"))?;
    ensure!(
        completion["private_comparison_complete"] == true
            && completion["error"].is_null()
            && completion["provider_calls"] == 0
            && completion["reference_promoted"] == false
            && completion["automatic_retry"] == false,
        "Base private comparison must close successfully"
    );
    let preflight = read(&output.join("preflight.json"))?;
    let prior_bindings: BTreeMap<PathBuf, String> =
        serde_json::from_value(preflight["bindings"].clone())?;
    ensure!(
        prior_bindings.get(&base_plan_path.canonicalize()?) == Some(&digest(base_plan_path)?),
        "Base private execution used another plan"
    );
    check(&prior_bindings)?;
    for p in prior_bindings.keys() {
        bind(bindings, p)?;
    }
    let result = read(&output.join("report.json"))?;
    ensure!(
        result["schema"] == "slop_ninja_private_origin_transfer_results_v1"
            && result["source_groups"] == 1
            && result["source_versions"] == 3
            && result["candidate_roles"] == json!(ROLES)
            && result["provider_calls"] == 0
            && result["training_admission"] == false
            && result["reference_promoted"] == false,
        "Unexpected base private evidence"
    );
    let rows = result["rows"].as_array().context("Missing base rows")?;
    ensure!(
        rows.len() == 6,
        "Base evidence must include both candidates and all variants"
    );
    let mut seen = BTreeSet::new();
    let prior = read(&path(base_plan, "existing_study")?.join("comparison.json"))?;
    for row in rows {
        let role = string(row, "role")?;
        let variant = string(row, "variant")?;
        ensure!(
            ROLES.contains(&role) && VARIANTS.contains(&variant) && seen.insert((role, variant)),
            "Wrong or repeated base candidate/version"
        );
        let case = base_plan["cases"]
            .as_array()
            .context("Missing base sources")?
            .iter()
            .find(|c| c["variant"] == variant)
            .context("Missing source version")?;
        let candidate = &candidates[role];
        let dir = output.join(role).join(variant);
        let prepared = read(&dir.join("plan.json"))?;
        let report = read(&dir.join("report.json"))?;
        complete_document(&report, &prepared, &dir.join("plan.json"))?;
        let mut responses = BTreeMap::new();
        for line in fs::read_to_string(dir.join("responses.jsonl"))?.lines() {
            let response: Value = serde_json::from_str(line)?;
            let id = string(&response, "id")?.to_string();
            ensure!(
                responses.insert(id, response).is_none(),
                "Duplicate base response"
            );
        }
        let observations = report["observations"]
            .as_array()
            .context("Missing base observations")?;
        ensure!(
            observations.len() == responses.len(),
            "Base response coverage differs"
        );
        for observation in observations {
            let score = &observation["observation"];
            ensure!(
                responses.get(string(score, "id")?) == Some(score),
                "Base raw response differs from report"
            );
        }
        let old = read(&path(case, "prior_document_plan")?)?;
        for key in [
            "schema",
            "policy",
            "bridge_sha256",
            "source",
            "source_sha256",
            "windows",
        ] {
            ensure!(prepared[key] == old[key], "Base private geometry changed");
        }
        let direct = whole(
            &report,
            string(case, "text_sha256")?,
            string(candidate, "id")?,
            &case["tokens"],
        )?;
        let baseline = whole(
            &read(&path(case, "prior_document_report")?)?,
            string(case, "text_sha256")?,
            string(base_plan, "reference_artifact_id")?,
            &case["tokens"],
        )?;
        let pairs: Vec<_> = prior["comparisons"]
            .as_array()
            .context("Missing saved Pangram comparisons")?
            .iter()
            .filter(|p| p["label"] == variant)
            .collect();
        ensure!(
            pairs.len() == 1
                && pairs[0]["a"] == baseline
                && pairs[0]["pangram"]["text_sha256"] == case["text_sha256"]
                && pairs[0]["pangram"]["stage"] == "STAGE_SUCCESS",
            "Base prior evidence changed"
        );
        ensure!(
            row["source_sha256"] == case["text_sha256"]
                && row["whole_observation"] == direct
                && row["baseline_whole_observation"] == baseline
                && row["pangram"] == pairs[0]["pangram"]
                && row["operating_points"] == candidate["points"]
                && row["flags"] == flags(probability(row)?, &candidate["points"])?
                && row["candidate_all_windows_scored"] == true
                && row["report_sha256"] == digest(&dir.join("report.json"))?,
            "Base private aggregate differs from complete observations or original thresholds"
        );
        for name in [
            "plan.json",
            "report.json",
            "responses.jsonl",
            "detector-requests.jsonl",
        ] {
            bind(bindings, &dir.join(name))?;
        }
    }
    for name in ["preflight.json", "report.json", "completion.json"] {
        bind(bindings, &output.join(name))?;
    }
    Ok(rows.clone())
}

pub(super) fn prepare(
    args: &Args,
    plan: &Value,
    large_public: &Value,
    large_candidates: &BTreeMap<&str, Value>,
    bindings: &mut BTreeMap<PathBuf, String>,
) -> Result<Option<Evidence>> {
    if plan["capacity_protocol"].is_null() {
        return Ok(None);
    }
    let root = args.plan.parent().context("Missing private root")?;
    let policy = root.join(string(plan, "capacity_protocol")?);
    ensure!(
        fs::read_to_string(&policy)? == CAPACITY_POLICY,
        "Capacity protocol changed"
    );
    let base_path = path(plan, "base_private_plan")?;
    ensure!(
        plan["base_private_plan_sha256"] == digest(&base_path)?,
        "Base private plan changed"
    );
    let base = read(&base_path)?;
    ensure!(
        base["capacity_protocol"].is_null()
            && base["cases"] == plan["cases"]
            && base["public_comparison"] == plan["base_public_comparison"]
            && base["reference_artifact_id"] == plan["reference_artifact_id"]
            && base["existing_study"] == plan["existing_study"],
        "Capacity plan retargets original comparison"
    );
    let base_public_path = path(plan, "base_public_comparison")?;
    let (base_public, artifacts) = reviewed_public(
        &base_public_path,
        args.base_public_review
            .as_deref()
            .context("Missing base review")?,
        bindings,
    )?;
    ensure!(
        base_public["fits"] == base["fits"] && base_public["fits"] != large_public["fits"],
        "Base and large candidate fits differ from plans"
    );
    let base_candidates = candidate_catalog(&base_public, &artifacts)?;
    let mut ids = BTreeSet::new();
    ensure!(
        plan["tokenizer_assets"]
            .as_object()
            .context("Missing tokenizer bindings")?
            .len()
            == TOKENIZERS.len(),
        "Unexpected tokenizer asset set"
    );
    for public in [&base_public, large_public] {
        let completed = path(public, "fits")?.join("completion.json");
        ensure!(
            read(&completed)?
                == json!({"phase":"both_fits_closed_unevaluated","exit_code":0,"evaluation_complete":false}),
            "Both sizes' fits must close successfully"
        );
        bind(bindings, &completed)?;
    }
    for candidate in base_candidates.values().chain(large_candidates.values()) {
        ensure!(
            ids.insert(string(candidate, "id")?),
            "All four candidates must remain distinct"
        );
        for name in TOKENIZERS {
            let p = path(candidate, "artifact")?.join("classifier").join(name);
            ensure!(
                plan["tokenizer_assets"][name] == digest(&p)?,
                "Candidate tokenizer differs from private plan"
            );
        }
    }
    cross_size_evidence(
        args.cross_size_review
            .as_deref()
            .context("Missing cross-size review")?,
        PublicSide {
            plan_path: &base_public_path,
            public: &base_public,
            candidates: &base_candidates,
        },
        PublicSide {
            plan_path: &path(plan, "public_comparison")?,
            public: large_public,
            candidates: large_candidates,
        },
        bindings,
    )?;
    let base_rows = base_private_evidence(
        args.base_private_output
            .as_deref()
            .context("Missing base output")?,
        &base_path,
        &base,
        &base_candidates,
        bindings,
    )?;
    bind(bindings, &base_path)?;
    bind(bindings, &policy)?;
    Ok(Some(Evidence { base_rows }))
}

pub(super) fn comparisons(evidence: Option<&Evidence>, rows: &mut [Value]) -> Result<Value> {
    let Some(evidence) = evidence else {
        return Ok(Value::Null);
    };
    ensure!(rows.len() == 6, "Expected all six large results");
    for row in rows.iter_mut() {
        let base = evidence
            .base_rows
            .iter()
            .find(|b| b["role"] == row["role"] && b["variant"] == row["variant"])
            .context("Missing corresponding base result")?;
        ensure!(
            base["source_sha256"] == row["source_sha256"]
                && base["pangram"] == row["pangram"]
                && base["baseline_whole_observation"] == row["baseline_whole_observation"],
            "Private source or anchor differs across sizes"
        );
        row["base_whole_observation"] = base["whole_observation"].clone();
        row["base_operating_points"] = base["operating_points"].clone();
        row["base_flags"] = base["flags"].clone();
        row["large_minus_base_pp"] = json!(100.0 * (probability(row)? - probability(base)?));
        let v6 = row["baseline_whole_observation"]["model_or_assistance_probability"]
            .as_f64()
            .context("Missing V6 probability")?;
        row["candidate_minus_v6_pp"] = json!(100.0 * (probability(row)? - v6));
    }
    let mut within = Vec::new();
    let mut base_rows = evidence.base_rows.clone();
    for (size, set) in [("base", base_rows.as_mut_slice()), ("large", rows)] {
        for role in ROLES {
            let original = set
                .iter()
                .find(|r| r["role"] == role && r["variant"] == "original")
                .context("Missing original")?;
            let original_p = probability(original)?;
            for row in set.iter_mut().filter(|r| r["role"] == role) {
                row["change_from_original_pp"] = json!(100.0 * (probability(row)? - original_p));
            }
        }
        for variant in VARIANTS {
            let get = |role: &str| {
                set.iter()
                    .find(|r| r["role"] == role && r["variant"] == variant)
                    .context("Missing private contrast")
            };
            within.push(json!({"size":size,"variant":variant,"consistency_minus_control_pp":100.0 * (probability(get("consistency")?)? - probability(get("control")?)?)}));
        }
    }
    Ok(
        json!({"base_rows":base_rows,"within_size_contrasts":within,"paired_public_cases_per_role":7,
        "interpretation":"Both corresponding-role and within-size comparisons retain original operating points and all source versions. These are three versions of one previously inspected document, not fresh confirmation or matched-FPR evidence."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_requirements_cannot_be_silently_ignored() -> Result<()> {
        let ordinary = Args::try_parse_from([
            "runner",
            "--plan",
            "plan",
            "--public-review",
            "review",
            "--out",
            "out",
        ])?;
        let full = Args::try_parse_from([
            "runner",
            "--plan",
            "plan",
            "--public-review",
            "review",
            "--out",
            "out",
            "--base-public-review",
            "base-review",
            "--base-private-output",
            "base-output",
            "--cross-size-review",
            "cross-review",
        ])?;
        let mut plan = json!({"capacity_protocol":"capacity.md","require_both_sizes_public_comparisons":true,"require_complete_base_private_evidence":true});
        assert!(validate_arguments(&ordinary, &plan).is_err());
        validate_arguments(&full, &plan)?;
        plan.as_object_mut().unwrap().remove("capacity_protocol");
        assert!(validate_arguments(&ordinary, &plan).is_err());
        assert!(validate_arguments(&full, &plan).is_err());
        validate_arguments(&ordinary, &json!({}))?;
        assert!(validate_arguments(&full, &json!({})).is_err());
        Ok(())
    }

    #[test]
    fn cross_size_reports_must_match_direction_corpus_and_original_thresholds() -> Result<()> {
        let bc = json!({"id":"base-control","points":[{"threshold":0.7},{"threshold":0.6}]});
        let lc = json!({"id":"large-control","points":[{"threshold":0.9},{"threshold":0.8}]});
        let make = |candidate: &Value| {
            json!({"schema":"slop_ninja_frozen_encoder_evaluation_v1",
            "artifact_id":candidate["id"],"split":"test","final_test_opened":true,"records_sha256":"corpus",
            "evaluation_view":{"sha256":"view"},"report":{"model":{"log_loss":0.3},
                "operating_points":candidate["points"].as_array().unwrap().iter().map(|p|json!({"calibration":p})).collect::<Vec<_>>()}})
        };
        let (base, large) = (make(&bc), make(&lc));
        let compared = json!({"schema":"slop_ninja_paired_encoder_comparison_v1",
            "baseline_report_sha256":"base-hash","candidate_report_sha256":"large-hash",
            "baseline_artifact_id":bc["id"],"candidate_artifact_id":lc["id"],"split":"test","records_sha256":"corpus",
            "evaluation_view":{"sha256":"view"},"baseline_operating_points":base["report"]["operating_points"],
            "candidate_operating_points":large["report"]["operating_points"],"baseline_metrics":base["report"]["model"],
            "candidate_metrics":large["report"]["model"],
            "comparison":{"objective":"three_class","bootstrap_seed":"243f6a8885a308d3","bootstrap_replicates":512},
            "human_model_comparison":{"objective":"human_model","bootstrap_seed":"243f6a8885a308d3","bootstrap_replicates":512}});
        let verify =
            |c: &Value| check_paired_report(c, &base, &large, "base-hash", "large-hash", &bc, &lc);
        verify(&compared)?;
        for (key, value) in [
            ("baseline_report_sha256", json!("other-case")),
            ("candidate_artifact_id", json!("large-consistency")),
            ("records_sha256", json!("other-corpus")),
            ("evaluation_view", json!({"sha256":"another-view"})),
            (
                "candidate_operating_points",
                base["report"]["operating_points"].clone(),
            ),
        ] {
            let mut changed = compared.clone();
            changed[key] = value;
            assert!(verify(&changed).is_err());
        }
        assert!(
            check_paired_report(
                &compared,
                &large,
                &base,
                "large-hash",
                "base-hash",
                &lc,
                &bc
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn private_contrasts_keep_variants_roles_and_distinct_operating_points() -> Result<()> {
        let make = |role: &str, variant: &str, p: f64, threshold: f64| {
            json!({"role":role,"variant":variant,
            "source_sha256":variant,"whole_observation":{"model_or_assistance_probability":p},
            "baseline_whole_observation":{"model_or_assistance_probability":0.9},"pangram":{"fraction_ai":0.96},
            "operating_points":[{"threshold":threshold}],"flags":[p>threshold]})
        };
        let mut base_rows = Vec::new();
        let mut large_rows = Vec::new();
        for (role, bonus) in [("control", 0.0), ("consistency", 0.05)] {
            for (i, variant) in VARIANTS.into_iter().enumerate() {
                base_rows.push(make(role, variant, 0.8 + bonus - i as f64 * 0.3, 0.7));
                large_rows.push(make(role, variant, 0.9 + bonus - i as f64 * 0.1, 0.97));
            }
        }
        let evidence = Evidence { base_rows };
        let result = comparisons(Some(&evidence), &mut large_rows)?;
        assert_eq!(large_rows.len(), 6);
        assert!((large_rows[2]["large_minus_base_pp"].as_f64().unwrap() - 50.0).abs() < 1e-9);
        assert!((large_rows[2]["change_from_original_pp"].as_f64().unwrap() + 20.0).abs() < 1e-9);
        assert_eq!(large_rows[0]["base_flags"], json!([true]));
        assert_eq!(large_rows[0]["flags"], json!([false]));
        assert_eq!(
            large_rows[0]["base_operating_points"],
            json!([{"threshold":0.7}])
        );
        assert_eq!(
            large_rows[0]["operating_points"],
            json!([{"threshold":0.97}])
        );
        assert_eq!(result["within_size_contrasts"].as_array().unwrap().len(), 6);
        assert!(
            result["within_size_contrasts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| (r["consistency_minus_control_pp"].as_f64().unwrap() - 5.0).abs() < 1e-9)
        );
        large_rows[0]["source_sha256"] = json!("changed-source");
        assert!(comparisons(Some(&evidence), &mut large_rows).is_err());
        Ok(())
    }
}
