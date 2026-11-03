//! New receipt route; legacy evaluator and its metric implementations unchanged.
use super::test_binding as b;
use super::*;
#[path = "gallery.rs"]
mod gallery;
#[cfg(test)]
#[path = "evaluate_tests.rs"]
mod tests;

fn score(
    eval: &[Value],
    predictions: &BTreeMap<String, Value>,
    weights: &Value,
    contract: &Value,
    fit: &Value,
    train_prior: Option<[f64; 3]>,
) -> Result<Value> {
    let mut attribution = BTreeMap::new();
    for role in ROLES {
        let profiles = roster(contract, role)?;
        ensure!(
            weights["profiles"][role] == json!(profiles),
            "frozen profile roster"
        );
        let threshold = number(&weights["thresholds"][role])?;
        ensure!((0.0..=1.).contains(&threshold), "frozen threshold range");
        let mut m = attribution_metrics(eval, predictions, role, &profiles, threshold)?;
        m["training_status"] = fit["training_status"][role].clone();
        m["calibration"] = fit["conditional_named_calibration"][role].clone();
        attribution.insert(role, m);
    }
    Ok(
        json!({"model":origin_share_metrics(eval,predictions,None)?,"attribution":attribution,
        "family_origin_balanced":balanced_origin_metrics(eval,predictions,None)?,
        "uniform_origin_prior_balanced":balanced_origin_metrics(eval,predictions,Some([1./3.;3]))?,
        "train_family_prior":train_prior,"prior_baseline":train_prior.map(|p|origin_share_metrics(eval,predictions,Some(p))).transpose()?}),
    )
}
pub(super) fn run(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 5 || a.len() == 6,
        "--frozen-test NUMERIC FIT PREDICTION_RECEIPT SELECTION NEW_REPORT [TEST_GALLERY]"
    );
    let (numeric, fit, receipt, selection, out) = (
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        Path::new(&a[4]),
    );
    let admitted = prediction_binding::admit(numeric, fit, selection)?;
    prediction_binding::new_output(out, &admitted.selection, numeric)?;
    let pred = b::predictions(receipt, &admitted.data, &admitted.run)?;
    let eval = admitted
        .data
        .numeric
        .iter()
        .filter(|(id, _)| pred.values.contains_key(*id))
        .map(|(_, r)| r.clone())
        .collect::<Vec<_>>();
    ensure!(
        eval.len() == pred.values.len(),
        "supported query roster differs"
    );
    // The diagnostic prior comes from the frozen original Train numeric shard,
    // never from the appended Test labels or a re-fitted classifier.
    let parent = Path::new(b::text(&admitted.candidate, "numeric_path")?);
    let parent_manifest = read(&parent.join("manifest.json"))?;
    let trains = b::array(&parent_manifest, "splits")?
        .iter()
        .filter(|s| s["split"] == "train")
        .collect::<Vec<_>>();
    ensure!(
        trains.len() == 1
            && trains[0]["sha256"] == b::bound(&admitted.selection, &parent.join("train.jsonl"))?,
        "original Train binding differs"
    );
    let train = query_rows(&parent.join("train.jsonl"))?;
    ensure!(
        train.iter().all(|r| r["frozen_split"] == "train"
            && r["input"]["contract_sha256"] == admitted.data.contract_sha),
        "prior uses only frozen Train queries"
    );
    let prior = prior(&train)?;
    let mut result = score(
        &eval,
        &pred.values,
        &admitted.weights,
        &admitted.contract,
        &admitted.fit,
        prior,
    )?;
    let reference_matching = if let Some(path) = a.get(5) {
        let (g, receipt) = gallery::load(Path::new(path), numeric, &admitted)?;
        json!({"available":true,"contract":receipt,"metrics":retrieval_metrics(&eval,&pred.values,&g,&admitted.contract)?})
    } else {
        json!({"available":false,"reason":"separate_frozen_Test_gallery_not_supplied","classification_attribution_still_scored":true})
    };
    let additions = json!({"schema":"slop_ninja_shared_a_frozen_test_report_v1","complete":true,"split":"test",
        "candidate":admitted.candidate["candidate"],"seed":admitted.run["seed"],"requested":admitted.data.rows.len(),"supported":eval.len(),
        "support":{"upstream":admitted.data.upstream,"assembled_queries":admitted.data.rows.len(),"numeric_supported":admitted.data.numeric.len(),
            "model_supported":eval.len(),"projection_excluded":admitted.data.projection_excluded,"inference_excluded":pred.excluded,
            "all_inference_failed":!admitted.data.numeric.is_empty()&&eval.is_empty()},
        "weighting":"equal_source_family_within_each_metric_eligible_rows; separately reported family_origin_balanced",
        "uncertainty":"existing source-family bootstrap; conditional on fixed model, grammar, seed, and supported rows",
        "reference_matching":reference_matching,"training_status":admitted.fit["training_status"],"fit":admitted.fit,
        "bindings":{"selection_receipt_sha256":admitted.data.selection_sha,"numeric_manifest_sha256":admitted.data.manifest_sha,
            "query_sha256":admitted.data.query_sha,"test_population_sha256":admitted.data.population_sha,"test_dataset_manifest_sha256":admitted.data.dataset_sha,
            "contract_sha256":admitted.data.contract_sha,"weights_sha256":admitted.run["weights_sha256"],"fit_sha256":admitted.run["fit_sha256"],
            "prediction_receipt_sha256":pred.receipt_sha,"predictions_sha256":pred.file_sha,"prior_train_sha256":trains[0]["sha256"]},
        "test_opened":true,"test_used_for_selection":false,"fit_performed":false,"model_calls":0,"provider_calls":0,
        "evaluation_source_sha256":b::digest(include_bytes!("../evaluate_supervised.rs")),"bridge_source_sha256":b::digest(include_bytes!("evaluate.rs")),
        "limits":["All metrics use existing evaluator helpers on explicitly supported rows; no failure is an attribution fallback",
            "All-failed support yields null estimates, not success or zero error","Conditional attribution is scored for documented roles without a predicted-presence cutoff",
            "Pooled reference cosine ranks remain separate diagnostics, not identity authentication","No Test calibration, model selection, refitting, or deployment qualification"]});
    result
        .as_object_mut()
        .context("report object")?
        .extend(additions.as_object().unwrap().clone());
    prediction_binding::unchanged(&admitted, numeric, fit, selection)?;
    ensure!(
        sha(receipt)? == pred.receipt_sha,
        "prediction receipt changed during scoring"
    );
    let receipt_value = read(receipt)?;
    ensure!(
        sha(&b::member(
            receipt.parent().context("receipt parent")?,
            b::text(&receipt_value, "predictions_file")?
        )?)? == pred.file_sha,
        "predictions changed during scoring"
    );
    b::write_new(out, &result)
}
