//! Bound aggregate projections and offline comparator/replay checks.
use super::*;

pub(super) fn gate_by_hash(dir: &Path, expected: &str) -> Result<std::path::PathBuf> {
    let mut found = None;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with("equivalence") && name.ends_with(".json") && sha(&path)? == expected {
            found = Some(path);
        }
    }
    found.context("original fit's retained equivalence receipt is missing")
}

pub(super) fn selection(
    p: &Value,
    level: &str,
    manifests: &BTreeMap<String, Value>,
    bindings: &BTreeMap<String, BTreeMap<String, Value>>,
    init: &str,
    design: &str,
) -> Result<(Value, BTreeSet<String>)> {
    ensure!(
        p["schema"] == "slop_ninja_a_source_size_sampling_v1"
            && p["level"] == level
            && p["epochs"] == 250
            && p["seeds"] == json!(SEEDS)
            && p["source_strata"] == json!(STRATA)
            && p["original_cohort_mass"] == 0.5
            && p["added_stratum_mass"] == 0.1
            && p["within_stratum"] == "family_origin_balanced"
            && p["apply_to"] == "train_only"
            && p["normalization_fitted"] == false
            && p["profiles_changed"] == false
            && p["final_test_opened"] == false
            && p["initialization_sha256"] == init
            && p["design_protocol_sha256"] == design
            && p["allowed_modes"]
                == if level == "all" {
                    json!(["text", "combined"])
                } else {
                    json!(["text"])
                },
        "sampling protocol differs"
    );
    for mode in ["text", "combined"] {
        let key = format!("large:{mode}");
        let m = &manifests[&key];
        ensure!(
            p["numeric_manifest_sha256"][mode] == bindings[&key]["development"]["manifest_sha256"]
                && p["parent_manifest_sha256"][mode]
                    == m["training_extension"]["parent_manifest_sha256"]
                && p["extension_dataset_manifest_sha256"]
                    == m["training_extension"]["extension_dataset_manifest_sha256"]
                && p["original_rows"]
                    == m["training_extension"]["original_partitions"]["train"]["rows"],
            "sampling input binding differs"
        );
    }
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::<String, (String, bool)>::new();
    let mut selected_rows = 0;
    let mut original_rows = 0;
    let mut masses = BTreeMap::<String, f64>::new();
    let mut family_masses = BTreeMap::<String, f64>::new();
    for row in array(p, "rows")? {
        ensure!(
            ids.insert(text(row, "id")?.to_owned()),
            "duplicate sampling row"
        );
        let family = text(row, "source_group")?.to_owned();
        let st = text(row, "stratum")?;
        ensure!(
            st == "original" || STRATA.contains(&st),
            "unknown source stratum"
        );
        let chosen = row["selected"].as_bool().context("selection boolean")?;
        if let Some(prior) = groups.insert(family.clone(), (st.into(), chosen)) {
            ensure!(
                prior == (st.into(), chosen),
                "family selection/stratum differs"
            );
        }
        let w = number(&row["weight"])?;
        ensure!(
            if chosen { w > 0. } else { w == 0. },
            "selection weight/sign mismatch"
        );
        *masses.entry(st.into()).or_default() += w;
        if chosen {
            *family_masses.entry(family).or_default() += w;
        }
        selected_rows += usize::from(chosen);
        if st == "original" {
            ensure!(chosen, "original query excluded");
            original_rows += 1;
        }
    }
    ensure!(
        p["total_train_rows"] == ids.len()
            && p["original_rows"] == original_rows
            && original_rows > 0
            && (masses.get("original").copied().unwrap_or(0.) - 0.5).abs() < 1e-10,
        "sampling count/original mass"
    );
    let chosen = groups
        .iter()
        .filter(|(_, (s, c))| s != "original" && *c)
        .map(|(g, _)| g.clone())
        .collect::<BTreeSet<_>>();
    let supplied = array(p, "selected_added_families")?
        .iter()
        .map(|v| v.as_str().context("selected family").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        supplied == chosen && supplied.len() == array(p, "selected_added_families")?.len(),
        "selected family roster"
    );
    let mut strata = BTreeMap::new();
    for st in STRATA {
        let available = groups.values().filter(|(s, _)| s == st).count();
        let selected = groups.values().filter(|(s, c)| s == st && *c).count();
        ensure!(
            selected > 0
                && selected
                    == if level == "all" {
                        available
                    } else {
                        level.parse::<usize>()?
                    }
                && (masses.get(st).copied().unwrap_or(0.) - 0.1).abs() < 1e-10,
            "source-size stratum count/mass"
        );
        strata.insert(
            st,
            json!({"available_families":available,"selected_families":selected,"loss_mass":0.1}),
        );
    }
    let original = groups.values().filter(|(s, _)| s == "original").count();
    let total_family_mass = family_masses.values().sum::<f64>();
    let sum_squared_family_mass = family_masses.values().map(|m| m * m).sum::<f64>();
    ensure!(
        (total_family_mass - 1.).abs() < 1e-9 && sum_squared_family_mass > 0.,
        "origin-loss family mass is not normalized"
    );
    let concentration = json!({
        "effective_family_count":1. / sum_squared_family_mass,
        "sum_squared_family_mass":sum_squared_family_mass,
        "total_family_mass":total_family_mass,
        "formula":"1 / sum_f(family_mass_f^2)",
        "family_mass_definition":"Sum of actual frozen origin-loss row weights within each selected source family; unselected rows have zero mass.",
        "equal_family_formula":"1 / (0.5^2 / original_train_families + sum_added_strata(0.1^2 / selected_families))",
        "meaning":"Weight-concentration diagnostic: the number of equally weighted families with the same concentration. Raw family count alone overstates effective weighted diversity when the original families retain half the loss mass.",
        "limits":"Not a count of independent observations, a bootstrap sample size, or a confidence-interval/sample guarantee. This diagnostic changes neither frozen loss weights nor confidence intervals."
    });
    Ok((
        json!({"level":level,"available_train_rows":ids.len(),"selected_train_rows":selected_rows,
        "original_train_rows":original_rows,"original_train_families":original,"selected_added_family_count":chosen.len(),
        "selected_train_families":original+chosen.len(),"original_loss_mass":0.5,"strata":strata,
        "origin_loss_weight_concentration":concentration}),
        chosen,
    ))
}

pub(super) fn replay(rust: &Value, expected: &Pred) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut delta = 0.0f64;
    let mut compare = |a: &Value, b: &Value, scale: f64| -> Result<()> {
        delta = delta.max((number(a)? - number(b)?).abs() * scale);
        Ok(())
    };
    for row in array(rust, "predictions")? {
        let id = text(row, "id")?;
        ensure!(seen.insert(id), "duplicate replay ID");
        let a = &row["prediction"];
        let b = expected.get(id).context("unexpected replay ID")?;
        for k in ["ai_confidence_percent", "ai_share_percent"] {
            compare(&a[k], &b[k], 1.)?;
        }
        for k in ["human_only", "model_only", "mixed"] {
            compare(&a["origin"][k], &b["origin"][k], 100.)?;
        }
        for role in ["human", "ai"] {
            let a = &a["attribution"][role];
            let b = &b["attribution"][role];
            ensure!(
                a["profile"] == b["profile"]
                    && a["fallback_asserts_presence"] == false
                    && b["fallback_asserts_presence"] == false,
                "replay attribution/fallback differs"
            );
            compare(
                &a["involvement_probability"],
                &b["involvement_probability"],
                100.,
            )?;
            let x = a["conditional_named_scores"]
                .as_object()
                .context("replay scores")?;
            let y = b["conditional_named_scores"]
                .as_object()
                .context("expected scores")?;
            ensure!(x.keys().eq(y.keys()), "replay profile roster");
            for (name, score) in x {
                compare(score, &y[name], 100.)?;
            }
        }
    }
    let recorded = number(&rust["max_absolute_delta_percentage_points"])?;
    ensure!(
        seen.len() == expected.len()
            && recorded >= 0.
            && recorded < 0.001
            && delta < 0.001
            && (delta - recorded).abs() < 1e-9,
        "stale/incomplete/failed replay"
    );
    Ok(())
}

pub(super) fn pangram(
    pg: &Value,
    weights: &Value,
    weights_sha: &str,
    fit_sha: &str,
    numeric: &Path,
    rows: &[Value],
    predictions: &Pred,
    calibration_count: usize,
) -> Result<(Pred, Value)> {
    ensure!(
        pg["schema"] == "slop_ninja_a_supervised_pangram_comparison_v1"
            && pg["split"] == "development"
            && pg["test_used_for_selection"] == false
            && pg["weights_sha256"] == weights_sha
            && pg["contract_sha256"] == weights["contract_sha256"]
            && pg["requested"] == rows.len()
            && pg["paired"] == rows.len()
            && pg["calibration_requested"] == calibration_count
            && pg["calibration_paired"] == calibration_count
            && array(pg, "excluded")?.is_empty()
            && pg["calibration"]["method"] == "isotonic_v1"
            && pg["calibration"]["fit_split"] == "calibration"
            && pg["calibration"]["native_pangram_confidence"] == false,
        "Pangram fixture scope/support differs"
    );
    let provenance = &pg["calibration"]["provenance"];
    ensure!(
        provenance["dataset_manifest_sha256"] == sha(&numeric.join("manifest.json"))?
            && provenance["calibration_numeric_sha256"] == sha(&numeric.join("calibration.jsonl"))?
            && provenance["fit_report_sha256"] == fit_sha
            && provenance["development_used_for_fitting"] == false,
        "Pangram calibration binding differs"
    );
    let truth = rows
        .iter()
        .map(|r| Ok((text(r, "id")?, r)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut out = Pred::new();
    let mut observations = BTreeMap::new();
    for r in array(pg, "records")? {
        let id = text(r, "id")?;
        let t = truth.get(id).context("Pangram ID outside roster")?;
        ensure!(
            r["source_group"] == t["source_group"]
                && r["origin_target"] == t["targets"]["origin"]
                && r["share_target"] == t["targets"]["share"]
                && out.get(id).is_none(),
            "Pangram truth/ID mismatch"
        );
        let p = number(&r["mapped_pangram_involvement"])?;
        ensure!(
            (0.0..=1.0).contains(&p)
                && (number(&r["slop_ninja_involvement"])?
                    - number(&predictions[id]["ai_confidence_percent"])? / 100.)
                    .abs()
                    < 1e-8,
            "Pangram paired model differs"
        );
        ensure!(
            text(r, "observation_sha256")?.len() == 64,
            "Pangram observation digest"
        );
        observations.insert(
            id,
            selected(
                r,
                &[
                    "observation_sha256",
                    "response_version",
                    "selector",
                    "pangram_fraction",
                    "mapped_pangram_involvement",
                ],
            ),
        );
        out.insert(id.into(), json!({"ai_confidence_percent":p*100.}));
    }
    exact_predictions(rows, &out)?;
    Ok((out, json!(observations)))
}

pub(super) fn frozen_v6(
    original: &Path,
    init: &Value,
    rosters: &BTreeMap<String, Vec<Value>>,
) -> Result<(BTreeMap<String, Pred>, Value)> {
    let numeric = original.join("numeric-warm-v1/text");
    let m = read(&numeric.join("manifest.json"))?;
    let path = numeric.join("baseline-origin-reference.jsonl");
    let hash = sha(&path)?;
    ensure!(
        m["baseline_origin_reference_sha256"] == hash,
        "V6 reference changed"
    );
    let mut all = Pred::new();
    for line in fs::read_to_string(&path)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        let p = array(&r, "probabilities")?;
        let p = p.iter().map(number).collect::<Result<Vec<_>>>()?;
        ensure!(
            r["artifact_id"] == init["artifact_id"]
                && p.len() == 3
                && p.iter().all(|p| (0.0..=1.0).contains(p))
                && (p.iter().sum::<f64>() - 1.).abs() < 1e-6,
            "frozen V6 probabilities/artifact"
        );
        ensure!(
            all.insert(
                text(&r, "id")?.into(),
                json!({"ai_confidence_percent":100.*(p[1]+p[2])})
            )
            .is_none(),
            "duplicate V6 reference"
        );
    }
    ensure!(all.len() == 929, "original complete V6 reference roster");
    let mut pred = BTreeMap::new();
    let mut reports = BTreeMap::new();
    for part in PARTS {
        let (rows, binding) = warm::population(&original.join("numeric-fusion-v1/combined"), part)?;
        ensure!(
            rows == rosters[part],
            "frozen V6 evaluation population differs"
        );
        let path = original.join(format!("scored-v1/frozen-v6-{part}.json"));
        let r = read(&path)?;
        ensure!(
            r["schema"] == "slop_ninja_frozen_v6_origin_report_v1"
                && r["split"] == part
                && r["test_opened"] == false
                && r["artifact_id"] == init["artifact_id"]
                && r["recalibrated"] == false
                && r["temperature"] == init["base_temperature"]
                && r["requested"] == rows.len()
                && r["supported"] == rows.len()
                && array(&r, "excluded")?.is_empty()
                && r["bindings"]["numeric_manifest_sha256"] == binding["manifest_sha256"]
                && r["bindings"]["query_sha256"] == binding["numeric_rows_sha256"]
                && r["bindings"]["embedding_manifest_sha256"]
                    == sha(&original.join("embeddings-v1/manifest.json"))?,
            "frozen V6 evaluation binding"
        );
        let p = rows
            .iter()
            .map(|r| {
                let id = text(r, "id")?;
                Ok((
                    id.to_owned(),
                    all.get(id).context("missing V6 prediction")?.clone(),
                ))
            })
            .collect::<Result<Pred>>()?;
        reports.insert(part,json!({"metrics":selected(&r,&["all_origins","human_model_only","mixed_only","unsupported_outputs"]),
            "report_sha256":sha(&path)?,"baseline_reference_sha256":hash,"artifact_id":r["artifact_id"],"recalibrated":false}));
        pred.insert(part.into(), p);
    }
    Ok((pred, json!(reports)))
}
