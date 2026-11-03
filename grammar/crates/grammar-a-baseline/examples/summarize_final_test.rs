//! Frozen, aggregate-only final Test evidence. No inference, fitting or selection.
//! summarize_final_test PLAN.json NEW_PUBLIC_JSON
#[path = "summarize_final_test/evidence.rs"]
mod evidence;
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[path = "summarize_final_test/metrics.rs"]
mod metrics;
#[allow(dead_code)]
#[path = "predict_final_test/binding.rs"]
mod prediction_binding;
#[path = "summarize_final_test/strata.rs"]
mod strata;
#[allow(dead_code)]
#[path = "evaluate_supervised.rs"]
mod supervised;
#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod test_binding;
#[cfg(test)]
#[path = "summarize_final_test/tests.rs"]
mod tests;
#[allow(dead_code)]
#[path = "summarize_warm_start.rs"]
mod warm;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use test_binding as b;
use warm::fusion::{balance_origins, difference, family_summary, number};
const SEEDS: [u64; 3] = [17, 29, 41];
type Pred = BTreeMap<String, Value>;

struct Run {
    candidate: String,
    seed: u64,
    pred: Pred,
    pg: Pred,
    report: Value,
    pg_report: Value,
    support: Value,
    evidence: Value,
}

fn run(plan_path: &Path, output: &Path) -> Result<()> {
    ensure!(!output.exists(), "new aggregate report required");
    let mut pins = evidence::Pins::default();
    let plan_sha = b::sha(plan_path)?;
    pins.add(plan_path.to_owned(), &plan_sha)?;
    let plan = b::read(plan_path)?;
    ensure!(
        plan["schema"] == "slop_ninja_final_test_summary_plan_v1",
        "summary plan schema"
    );
    evidence::roster(b::array(&plan, "runs")?)?;
    let selection_path = pins.file(&plan["selection"])?;
    let selection = final_test_binding::validate_selection_receipt(&selection_path)?;
    let selection_sha = b::sha(&selection_path)?;
    let dataset_path = pins.file(&plan["dataset_manifest"])?;
    let dataset = b::read(&dataset_path)?;
    let mapping_path = Path::new(b::text(&selection["pangram_mapping"], "path")?);
    let mapping_sha = b::bound(&selection, mapping_path)?;
    let mapping = b::read(mapping_path)?;
    ensure!(
        selection["pangram_mapping"]["sha256"] == mapping_sha
            && selection["pangram_mapping"]["method"] == "isotonic_v1"
            && selection["pangram_mapping"]["fit_split"] == "calibration"
            && mapping["calibration"]["method"] == "isotonic_v1"
            && mapping["calibration"]["fit_split"] == "calibration",
        "frozen CAL mapping"
    );
    let mut runs = BTreeMap::<String, Vec<Run>>::new();
    let mut canonical: Option<b::TestData> = None;
    let mut selected_metadata = Pred::new();
    let mut selected_contract = Value::Null;
    let mut pg_observations = evidence::ProviderEvidence::default();
    for item in b::array(&plan, "runs")? {
        let manifest_path = pins.file(&item["numeric_manifest"])?;
        let numeric = manifest_path.parent().context("numeric parent")?;
        ensure!(
            manifest_path
                .file_name()
                .is_some_and(|s| s == "manifest.json"),
            "numeric manifest basename"
        );
        prediction_binding::new_output(output, &selection, numeric)?;
        let data = b::load_data(numeric, &selection, &selection_sha)?;
        ensure!(
            data.dataset_sha == b::text(&plan["dataset_manifest"], "sha256")?,
            "dataset pin differs"
        );
        if let Some(parent) = &canonical {
            ensure!(
                data.rows == parent.rows
                    && data.hashes == parent.hashes
                    && data.upstream == parent.upstream,
                "candidate query labels, text, family or upstream support differ"
            );
        }
        let candidate = b::text(item, "candidate")?;
        let frozen_candidate = b::array(&selection, "candidates")?
            .iter()
            .find(|c| c["candidate"] == candidate)
            .context("frozen candidate")?;
        let frozen_run = b::array(frozen_candidate, "runs")?
            .iter()
            .find(|r| r["seed"] == item["seed"])
            .context("frozen seed")?;
        let fit_path = Path::new(b::text(frozen_run, "model_path")?);
        let (c, r, fit) = b::fit_binding(&selection, &data, fit_path)?;
        ensure!(
            c["candidate"] == candidate && r["seed"] == item["seed"],
            "candidate/seed binding"
        );
        let contract = b::read(&numeric.join("contract.json"))?;
        let weights = b::read(&fit_path.join("weights.json"))?;
        prediction_binding::all_heads(&weights, &contract)?;
        let population_path = b::member(
            numeric,
            b::text(&data.manifest["test_transform"], "audit_file")?,
        )?;
        let population = b::indexed(b::array(&b::read(&population_path)?, "rows")?)?;
        for (id, row) in &data.numeric {
            prediction_binding::roles(row, &population[id]["metadata"], &contract)?;
        }
        if selection["selected_candidate"] == candidate {
            selected_metadata = population;
            selected_contract = contract.clone();
        }
        let receipt = pins.file(&item["prediction_receipt"])?;
        let pred = b::predictions(&receipt, &data, &r)?;
        let evaluation = pins.file(&item["evaluation"])?;
        let report = b::read(&evaluation)?;
        evidence::evaluation(&report, &data, &pred, &c, &r, &fit)?;
        let pg_path = pins.file(&item["pangram_comparison"])?;
        let pg_report = b::read(&pg_path)?;
        let pg = evidence::pangram(
            &pg_report,
            &data,
            &pred,
            &c,
            &r,
            &mapping,
            &mapping_sha,
            &mut pg_observations,
        )?;
        let rr = b::read(&receipt)?;
        pins.add(
            b::member(
                receipt.parent().context("receipt parent")?,
                b::text(&rr, "predictions_file")?,
            )?,
            &pred.file_sha,
        )?;
        for (path, hash) in [
            (numeric.join("test.jsonl"), data.query_sha.clone()),
            (
                numeric.join("contract.json"),
                b::hash_text(&data.manifest, "contract_file_sha256")?,
            ),
            (population_path, data.population_sha.clone()),
            (
                numeric.join("test_dataset_manifest.json"),
                data.dataset_sha.clone(),
            ),
        ] {
            pins.add(path, &hash)?;
        }
        let support = json!({"assembled_queries":data.rows.len(),"numeric_supported":data.numeric.len(),
            "projection_excluded":data.projection_excluded.len(),"model_supported":pred.values.len(),
            "inference_excluded":pred.excluded.len(),"pangram_supported":pg_report["support"]["pangram_supported"],
            "model_pangram_paired":pg.len(),"model_pangram_unpaired":data.rows.len()-pg.len(),
            "all_inference_failed":!data.numeric.is_empty()&&pred.values.is_empty()});
        let evidence = json!({"numeric_manifest_sha256":data.manifest_sha,"query_sha256":data.query_sha,
            "contract_sha256":data.contract_sha,"weights_sha256":r["weights_sha256"],"fit_sha256":r["fit_sha256"],
            "prediction_receipt_sha256":pred.receipt_sha,"predictions_sha256":pred.file_sha,
            "evaluation_sha256":item["evaluation"]["sha256"],"pangram_comparison_sha256":item["pangram_comparison"]["sha256"]});
        runs.entry(candidate.into()).or_default().push(Run {
            candidate: candidate.into(),
            seed: b::count(item, "seed")? as u64,
            pred: pred.values,
            pg,
            report,
            pg_report,
            support,
            evidence,
        });
        if canonical.is_none() {
            canonical = Some(data);
        }
    }
    for values in runs.values_mut() {
        values.sort_by_key(|r| r.seed);
    }
    let data = canonical.context("empty roster")?;
    ensure!(
        b::upstream_accounting(&dataset, &data.rows.keys().cloned().collect())? == data.upstream,
        "dataset accounting changed"
    );
    let (v6, v6_report) = evidence::v6(&plan["v6"], &selection, &selection_sha, &data, &mut pins)?;
    let chosen = b::text(&selection, "selected_candidate")?;
    let chosen_runs = &runs[chosen];
    let left = chosen_runs.iter().map(|r| &r.pred).collect::<Vec<_>>();
    let pg = chosen_runs.iter().map(|r| &r.pg).collect::<Vec<_>>();
    let vv = vec![&v6; 3];
    let trio_ids = metrics::common(
        &data.rows,
        &left
            .iter()
            .chain(pg.iter())
            .chain(vv.iter())
            .copied()
            .collect::<Vec<_>>(),
    );
    let trio_rows = metrics::rows(&data.rows, &trio_ids);
    let mut comparisons = vec![
        metrics::comparison(chosen, "frozen_v6", &trio_rows, &left, &vv)?,
        metrics::comparison(chosen, "CAL_mapped_pangram_proxy", &trio_rows, &left, &pg)?,
        metrics::comparison(
            "frozen_v6",
            "CAL_mapped_pangram_proxy",
            &trio_rows,
            &vv,
            &pg,
        )?,
    ];
    let all_candidate_ids = metrics::common(
        &data.rows,
        &runs
            .values()
            .flat_map(|r| r.iter().map(|r| &r.pred))
            .collect::<Vec<_>>(),
    );
    let all_candidate_rows = metrics::rows(&data.rows, &all_candidate_ids);
    for name in final_test_binding::CANDIDATE_NAMES {
        if name == chosen {
            continue;
        }
        let right = runs[name].iter().map(|r| &r.pred).collect::<Vec<_>>();
        comparisons.push(metrics::comparison(
            chosen,
            name,
            &all_candidate_rows,
            &left,
            &right,
        )?);
    }
    // Fixed matched feature contrasts; these never change the selected model.
    for (left_name, right_name) in [
        ("warm_combined", "warm_text"),
        ("combined", "text"),
        ("scale_all_combined", "scale_all_text"),
        ("ordered_cfg", "global_cfg_control"),
    ] {
        let left = runs[left_name].iter().map(|r| &r.pred).collect::<Vec<_>>();
        let right = runs[right_name].iter().map(|r| &r.pred).collect::<Vec<_>>();
        comparisons.push(metrics::comparison(
            left_name, right_name, &all_candidate_rows, &left, &right,
        )?);
    }
    let table = final_test_binding::CANDIDATE_NAMES
        .iter()
        .map(|name| metrics::table(&runs[*name]))
        .collect::<Result<Vec<_>>>()?;
    let report = json!({"schema":"slop_ninja_final_test_aggregate_v1","complete":true,"split":"test",
        "selected_candidate":chosen,"evaluation_seeds":SEEDS,"candidate_count":16,"fit_count":48,
        "test_used_for_selection":false,"selection_changed":false,"fit_performed":false,"model_calls":0,"provider_calls":0,
        "support":evidence::public_upstream(&data.upstream)?,"candidates":table,
        "selected_v6_pangram_common_support":{"assembled_queries":data.rows.len(),"documents":trio_rows.len(),
            "excluded_from_common_support":data.rows.len()-trio_rows.len(),
            "source_families":metrics::families(&trio_rows)},
        "all_candidates_common_support":{"assembled_queries":data.rows.len(),"documents":all_candidate_rows.len(),
            "excluded_from_common_support":data.rows.len()-all_candidate_rows.len(),"source_families":metrics::families(&all_candidate_rows)},
        "paired_comparisons":comparisons,
        "strata":strata::report(&trio_rows,&selected_metadata,&selected_contract,&left,&vv,&pg)?,
        "frozen_v6":metrics::v6_public(&v6_report)?,
        "pangram":{"native_confidence":false,"mapping":"isotonic_v1","fit_split":"calibration",
            "mapping_applied_without_refit":true,"frozen_mapping_sha256":mapping_sha,
            "meaning":"CAL-mapped fraction proxy for any AI involvement; neither native Pangram confidence nor measured replacement-span share",
            "unsupported_outputs":["three_way_origin","human_attribution","generator_attribution"],
            "calibration_bins_available":false},
        "historical_v6_binary_evidence":{"included_in_fresh_metrics":false,
            "meaning":"Archived binary metrics have a different population and label task; this report scores fresh human/model/mixed Test queries only"},
        "bindings":{"plan_sha256":plan_sha,"selection_receipt_sha256":selection_sha,
            "dataset_manifest_sha256":plan["dataset_manifest"]["sha256"],"dataset_records_sha256":dataset["records_sha256"],
            "v6_report_sha256":plan["v6"]["report"]["sha256"],"v6_embedding_rows_sha256":plan["v6"]["embedding_rows"]["sha256"],
            "implementation_sha256":b::digest(include_bytes!("summarize_final_test.rs")),
            "evidence_source_sha256":b::digest(include_bytes!("summarize_final_test/evidence.rs")),
            "metrics_source_sha256":b::digest(include_bytes!("summarize_final_test/metrics.rs")),
            "strata_source_sha256":b::digest(include_bytes!("summarize_final_test/strata.rs")),
            "family_bootstrap_source_sha256":b::digest(include_bytes!("summarize_v6_fusion.rs")),
            "fixed_seed_helper_source_sha256":b::digest(include_bytes!("summarize_warm_start.rs")),
            "evaluation_helper_source_sha256":b::digest(include_bytes!("evaluate_supervised.rs")),
            "admission_helper_source_sha256":b::digest(include_bytes!("compare_supervised_pangram/test_binding.rs"))},
        "uncertainty":"1000 existing deterministic source-family bootstrap replicates; average paired differences across all three fixed seeds before resampling families; seed min/max are descriptive, not independent-sample confidence intervals",
        "limits":["Full-support per-seed metrics and common-support paired estimates have different denominators",
            "Candidate comparisons are descriptive frozen comparisons, not Test reselection; no multiple-comparison guarantee",
            "Strata report involvement gaps only on the selected/V6/Pangram intersection across all seeds; conditional attribution/open-set remain in the all-head table",
            "Unknown identities and missing diagnostics are counted, never imputed; historical source proxies do not authenticate authorship",
            "No deployment, broad generalization, or native Pangram confidence qualification"]});
    warm::public_boundary(&report)?;
    pins.unchanged()?;
    ensure!(
        final_test_binding::validate_selection_receipt(&selection_path)? == selection,
        "selection changed during summary"
    );
    b::write_new(output, &report)
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 2,
        "summarize_final_test PLAN.json NEW_PUBLIC_JSON"
    );
    run(Path::new(&args[0]), Path::new(&args[1]))
}
