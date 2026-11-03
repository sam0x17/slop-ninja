//! Apply a pre-Test CAL mapping, never fit it. Legacy compare() is unchanged.
//! --frozen-test NUMERIC FIT PREDICTION_RECEIPT TEST_RESULTS SELECTION NEW_REPORT
//! selection.pangram_mapping is the already retained Development comparison,
//! pinned by path+sha256 with method=isotonic_v1, fit_split=calibration. Its
//! original fit can differ from the evaluated candidate; all candidates use the
//! same frozen mapping. No Calibration rows/observations are read on this route.
use super::test_binding as b;
use super::*;

struct FrozenMapping {
    model: Isotonic,
    description: Value,
    selector: String,
    version: String,
    file_sha: String,
    requested: usize,
    paired: usize,
}

fn stored_isotonic(d: &Value) -> Result<Isotonic> {
    ensure!(
        d["method"] == "isotonic_v1"
            && d["fit_split"] == "calibration"
            && d["algorithm"]
                == "exact_score_tie_aggregation_then_weighted_pool_adjacent_violators"
            && d["input"] == "fraction_ai_plus_fraction_ai_assisted_clamped_to_1"
            && d["target"] == "binary_any_AI_involvement_from_origin"
            && d["objective"] == "weighted_squared_error"
            && d["interpolation"]
                == "linear_between_adjacent_unique_CAL_fraction_knots; constant_at_each_end_outside_CAL_support"
            && d["output_range"] == json!([0, 1])
            && d["target_clip"].is_null()
            && d["native_pangram_confidence"] == false
            && matches!(
                d["weighting"].as_str(),
                Some("family" | "family_origin_balanced")
            ),
        "unqualified frozen isotonic semantics"
    );
    let number = |v: &Value, k: &str| -> Result<f64> {
        let n = v[k]
            .as_f64()
            .with_context(|| format!("mapping number {k}"))?;
        ensure!(n.is_finite(), "nonfinite mapping");
        Ok(n)
    };
    let mut knots = Vec::<Knot>::new();
    for k in b::array(d, "knots")? {
        let next = Knot {
            x: number(k, "fraction")?,
            fitted: number(k, "involvement")?,
            weight: number(k, "weight")?,
            positives: number(k, "weighted_positives")?,
            rows: b::count(k, "rows")?,
        };
        ensure!(
            (0.0..=1.).contains(&next.x)
                && (0.0..=1.).contains(&next.fitted)
                && next.weight > 0.
                && next.positives >= 0.
                && next.positives <= next.weight + 1e-12
                && next.rows > 0,
            "invalid stored knot"
        );
        if let Some(prev) = knots.last() {
            ensure!(
                prev.x < next.x && prev.fitted <= next.fitted,
                "duplicate/nonmonotone stored knots"
            );
        }
        knots.push(next);
    }
    ensure!(!knots.is_empty(), "empty frozen map");
    let mut blocks = Vec::new();
    let mut cursor = 0;
    for block in b::array(d, "blocks")? {
        let first = b::count(block, "first_knot")?;
        let last = b::count(block, "last_knot")?;
        ensure!(
            first == cursor && first <= last && last < knots.len(),
            "stored blocks do not partition knots"
        );
        let weight = number(block, "weight")?;
        let positives = number(block, "weighted_positives")?;
        ensure!(
            weight > 0. && positives >= 0. && positives <= weight + 1e-12,
            "invalid stored block"
        );
        let w: f64 = knots[first..=last].iter().map(|k| k.weight).sum();
        let p: f64 = knots[first..=last].iter().map(|k| k.positives).sum();
        ensure!(
            (w - weight).abs() < 1e-10
                && (p - positives).abs() < 1e-10
                && knots[first..=last]
                    .iter()
                    .all(|k| (k.fitted - positives / weight).abs() < 1e-10),
            "knot/block values disagree"
        );
        blocks.push(Block {
            first,
            last,
            weight,
            positives,
        });
        cursor = last + 1;
    }
    ensure!(
        cursor == knots.len() && (knots.iter().map(|k| k.weight).sum::<f64>() - 1.).abs() < 1e-8,
        "incomplete/unnormalized frozen map"
    );
    Ok(Isotonic { knots, blocks })
}

fn frozen_mapping(selection: &Value) -> Result<FrozenMapping> {
    let binding = &selection["pangram_mapping"];
    ensure!(
        binding["method"] == "isotonic_v1" && binding["fit_split"] == "calibration",
        "selection mapping semantics"
    );
    let path = Path::new(b::text(binding, "path")?);
    let file_sha = b::bound(selection, path)?;
    ensure!(binding["sha256"] == file_sha, "selection mapping changed");
    let report = read(path)?;
    ensure!(
        report["schema"] == "slop_ninja_a_supervised_pangram_comparison_v1"
            && report["split"] == "development"
            && report["test_used_for_selection"] == false,
        "mapping must originate before Test"
    );
    let d = &report["calibration"];
    let model = stored_isotonic(d)?;
    let provenance = &d["provenance"];
    ensure!(
        provenance["development_used_for_fitting"] == false,
        "mapping was not CAL-only"
    );
    // Bind the mapping's original fit, not whichever candidate is scored later.
    let mut anchors = 0;
    for candidate in b::array(selection, "candidates")? {
        if candidate["numeric_manifest_sha256"] != provenance["dataset_manifest_sha256"] {
            continue;
        }
        for run in b::array(candidate, "runs")? {
            if run["fit_sha256"] != provenance["fit_report_sha256"] {
                continue;
            }
            ensure!(
                run["weights_sha256"] == report["weights_sha256"]
                    && candidate["contract_sha256"] == report["contract_sha256"],
                "mapping fit/contract differs"
            );
            let numeric = Path::new(b::text(candidate, "numeric_path")?);
            let m = read(&numeric.join("manifest.json"))?;
            ensure!(
                m["final_test_opened"] == false
                    && m["population_file_sha256"] == provenance["population_sha256"],
                "mapping parent differs"
            );
            let cal = b::array(&m, "splits")?
                .iter()
                .filter(|s| s["split"] == "calibration")
                .collect::<Vec<_>>();
            ensure!(
                cal.len() == 1
                    && cal[0]["sha256"] == provenance["calibration_numeric_sha256"]
                    && cal[0]["rows"] == report["calibration_requested"],
                "mapping CAL binding differs"
            );
            let fit = read(&Path::new(b::text(run, "model_path")?).join("fit.json"))?;
            let expected = if fit["query_weighting"] == "family_origin_balanced" {
                "family_origin_balanced"
            } else {
                "family"
            };
            ensure!(
                d["weighting"] == expected,
                "mapping weighting differs from frozen producer"
            );
            anchors += 1;
        }
    }
    ensure!(
        anchors > 0,
        "mapping producer absent from eligible frozen runs"
    );
    let requested = b::count(&report, "calibration_requested")?;
    let paired = b::count(&report, "calibration_paired")?;
    let omitted = b::array(&report, "excluded")?
        .iter()
        .filter(|r| r["split"] == "calibration")
        .count();
    ensure!(
        paired > 0
            && paired + omitted == requested
            && model.knots.iter().map(|k| k.rows).sum::<usize>() == paired,
        "frozen CAL denominator differs"
    );
    let mut versions = BTreeSet::new();
    for r in b::array(&report, "records")? {
        versions.insert((
            b::text(r, "selector")?.to_owned(),
            b::text(r, "response_version")?.to_owned(),
        ));
    }
    ensure!(
        versions.len() == 1,
        "frozen comparator has mixed/absent provider versions"
    );
    let (selector, version) = versions.into_iter().next().unwrap();
    Ok(FrozenMapping {
        model,
        description: d.clone(),
        selector,
        version,
        file_sha,
        requested,
        paired,
    })
}

// Missing/failed observations are exclusions. Changed identities/hashes/versions
// are binding failures, not a way to improve the apparent paired population.
fn test_observation(
    root: &Path,
    row: &Value,
    hash: &str,
    mapping: &FrozenMapping,
) -> Result<std::result::Result<(Value, String), Value>> {
    let id = b::text(row, "id")?;
    let dir = b::member(root, id)?;
    let path = dir.join("result.json");
    if !path.try_exists()? {
        return Ok(Err(
            json!({"id":id,"reason":"no_retained_result","stage":"pangram","submission_state":"unresolved_not_resubmitted"}),
        ));
    }
    ensure!(
        path.canonicalize()?.starts_with(root.canonicalize()?),
        "observation escapes retained root"
    );
    let v = read(&path)?;
    ensure!(
        v["id"] == id
            && v["source_group"] == row["source_group"]
            && v["split"] == "test"
            && v["text_sha256"] == hash
            && v["model"] == mapping.selector,
        "Test observation binding differs"
    );
    let hash = sha(&path)?;
    if v["response"]["stage"] != "STAGE_SUCCESS" {
        return Ok(Err(
            json!({"id":id,"reason":"retained_provider_failure","stage":"pangram",
            "provider_stage":v["response"]["stage"],"observation_sha256":hash}),
        ));
    }
    ensure!(
        v["response"]["version"] == mapping.version,
        "Test provider version differs from frozen CAL mapping"
    );
    for k in ["fraction_ai", "fraction_ai_assisted"] {
        ensure!(
            v["response"][k]
                .as_f64()
                .is_some_and(|x| x.is_finite() && (0.0..=1.).contains(&x)),
            "invalid Test fraction"
        );
    }
    ensure!(
        v["response"]["fraction_ai"].as_f64().unwrap()
            + v["response"]["fraction_ai_assisted"].as_f64().unwrap()
            <= 1.000001,
        "Test fraction sum"
    );
    Ok(Ok((v, hash)))
}

pub(super) fn run(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 6,
        "--frozen-test NUMERIC FIT PREDICTION_RECEIPT TEST_RESULTS SELECTION NEW_REPORT"
    );
    let output = Path::new(&a[5]);
    ensure!(!output.exists(), "new Test report required");
    let selection_path = Path::new(&a[4]);
    let selection = final_test_binding::validate_selection_receipt(selection_path)?;
    let selection_sha = sha(selection_path)?;
    let mapping = frozen_mapping(&selection)?;
    let data = b::load_data(Path::new(&a[0]), &selection, &selection_sha)?;
    let (candidate, run, fit) = b::fit_binding(&selection, &data, Path::new(&a[1]))?;
    let predictions = b::predictions(Path::new(&a[2]), &data, &run)?;
    let results = Path::new(&a[3]);
    let mut excluded = Vec::new();
    let mut pairs = Vec::new();
    let mut pg = Vec::new();
    let mut sn = Vec::new();
    let mut gap = Vec::new();
    let mut bp = Vec::new();
    let mut bs = Vec::new();
    let mut bg = Vec::new();
    let mut pg_fp = Vec::new();
    let mut sn_fp = Vec::new();
    let mut pg_share = Vec::new();
    let mut sn_share = Vec::new();
    let mut pg_supported = 0;
    let prediction_excluded = b::indexed(&predictions.excluded)?;
    let projection_excluded = b::indexed(&data.projection_excluded)?;
    for (id, row) in &data.rows {
        let observed = test_observation(results, row, &data.hashes[id], &mapping)?;
        if observed.is_ok() {
            pg_supported += 1;
        }
        let mut reasons = Vec::new();
        if let Some(r) = projection_excluded.get(id) {
            reasons.push(json!({"stage":"projection","detail":r}));
        }
        if let Some(r) = prediction_excluded.get(id) {
            reasons.push(json!({"stage":"shared_model","detail":r}));
        }
        if let Err(r) = &observed {
            reasons.push(r.clone());
        }
        let model = predictions.values.get(id);
        if model.is_none() {
            ensure!(!reasons.is_empty(), "unaccounted missing model prediction");
        }
        if !reasons.is_empty() {
            excluded.push(json!({"id":id,"reasons":reasons}));
            continue;
        }
        let (observed, observed_sha) =
            observed.map_err(|_| anyhow::anyhow!("missing paired observation"))?;
        let model = model.context("paired model")?;
        let origin = target(row)?;
        let y = if origin == 0 { 0. } else { 1. };
        let family = b::text(row, "source_group")?.to_owned();
        let fraction = score(&observed);
        let p = mapping.model.predict(fraction);
        let m = model["ai_confidence_percent"]
            .as_f64()
            .context("confidence")?
            / 100.;
        let pe = (p - y).powi(2);
        let me = (m - y).powi(2);
        pg.push((family.clone(), pe));
        sn.push((family.clone(), me));
        gap.push((family.clone(), me - pe));
        bp.push((family.clone(), origin, pe));
        bs.push((family.clone(), origin, me));
        bg.push((family.clone(), origin, me - pe));
        if origin == 0 {
            pg_fp.push((family.clone(), f64::from(p >= 0.5)));
            sn_fp.push((family.clone(), f64::from(m >= 0.5)));
        }
        if let Some(truth) = row["targets"]["share"].as_f64() {
            pg_share.push((family.clone(), (fraction - truth).abs() * 100.));
            sn_share.push((
                family.clone(),
                (model["ai_share_percent"].as_f64().context("share")? / 100. - truth).abs() * 100.,
            ));
        }
        pairs.push(json!({"id":id,"source_group":family,"text_sha256":data.hashes[id],"origin_target":origin,
            "share_target":row["targets"]["share"],"pangram_fraction":fraction,"mapped_pangram_involvement":p,
            "slop_ninja_involvement":m,"selector":mapping.selector,"response_version":mapping.version,"observation_sha256":observed_sha}));
    }
    ensure!(
        pairs.len() + excluded.len() == data.rows.len(),
        "Test paired accounting differs"
    );
    let cells = origin_balanced(&bg);
    let report = json!({"schema":"slop_ninja_frozen_test_pangram_comparison_v1","complete":true,"split":"test",
        "candidate":candidate["candidate"],"seed":run["seed"],"requested":data.rows.len(),"paired":pairs.len(),"excluded":excluded,
        "support":{"upstream":data.upstream,"assembled_queries":data.rows.len(),"numeric_supported":data.numeric.len(),
            "model_supported":predictions.values.len(),"pangram_supported":pg_supported,"paired_documents":pairs.len(),
            "paired_source_families":pairs.iter().map(|r|r["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len(),"paired_family_origin_cells":cells.len()},
        "calibration":mapping.description,"mapping_applied_without_refit":true,"calibration_requested":mapping.requested,"calibration_paired":mapping.paired,
        "family":{"pangram_brier":avg(&pg),"model_brier":avg(&sn),"model_minus_pangram_brier":avg(&gap),"paired_gap_95ci":ci(&gap)},
        "family_origin_balanced":{"pangram_brier":avg(&origin_balanced(&bp)),"slop_ninja_brier":avg(&origin_balanced(&bs)),"paired_gap":avg(&cells),"paired_gap_95ci":ci(&cells)},
        "human_fpr_at_0_5":{"pangram":avg(&pg_fp),"model":avg(&sn_fp)},
        "share":{"pangram_fraction_mae_percentage_points":avg(&pg_share),"model_mae_percentage_points":avg(&sn_share),"labelled_paired_documents":pg_share.len()},
        "records":pairs,"weights_sha256":run["weights_sha256"],"fit_sha256":run["fit_sha256"],"contract_sha256":data.contract_sha,
        "bindings":{"selection_receipt_sha256":selection_sha,"frozen_mapping_sha256":mapping.file_sha,
            "numeric_manifest_sha256":data.manifest_sha,"test_population_sha256":data.population_sha,"test_dataset_manifest_sha256":data.dataset_sha,
            "prediction_receipt_sha256":predictions.receipt_sha,"predictions_sha256":predictions.file_sha,
            "implementation_sha256":b::digest(include_bytes!("final_test.rs")),"admission_helper_sha256":b::digest(include_bytes!("test_binding.rs"))},
        "fit_query_weighting":fit["query_weighting"],"test_opened":true,"test_used_for_selection":false,"fitting_performed":false,"provider_calls":0,
        "native_pangram_confidence":false,"limits":["Frozen CAL mapping applied without refitting or reweighting; same map for every eligible candidate/seed",
            "Pangram text fraction is neither native involvement confidence nor replacement-span ground truth",
            "Paired scores condition on common support; full stage-specific denominators and exclusions are retained",
            "No parity, deployment, new model selection or broad-population qualification inferred"]});
    ensure!(
        sha(selection_path)? == selection_sha
            && sha(Path::new(b::text(&selection["pangram_mapping"], "path")?))? == mapping.file_sha,
        "frozen bindings changed during scoring"
    );
    b::write_new(output, &report)
}

#[cfg(test)]
#[path = "final_test_tests.rs"]
mod tests;
