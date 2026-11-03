//! Score frozen V6's existing origin outputs without inventing share or attribution.
//! evaluate_frozen_v6 NUMERIC EMBEDDINGS SPLIT NEW_REPORT [MATCHED_FIT_MODALITY_ROOT]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};
#[path = "evaluate_frozen_v6/final_test.rs"]
mod final_test;
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[allow(dead_code)]
#[path = "evaluate_supervised.rs"]
mod supervised;
#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod test_binding;
const V6: &str = "506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f";
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn indexed(values: Vec<Value>) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for row in values {
        ensure!(
            out.insert(s(&row, "id")?.to_owned(), row).is_none(),
            "duplicate id"
        );
    }
    Ok(out)
}
fn prediction(v: &Value) -> Result<Value> {
    ensure!(
        v["status"] == "ok" && v["artifact_id"] == V6,
        "unsupported or changed V6 output"
    );
    ensure!(
        v["labels"] == json!(["human_only", "model_only", "mixed"]),
        "V6 label order"
    );
    let p = v["probabilities"].as_array().context("V6 probabilities")?;
    ensure!(p.len() == 3, "V6 probability shape");
    let p = p
        .iter()
        .map(|v| v.as_f64().context("probability"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        p.iter().all(|p| p.is_finite() && (0.0..=1.).contains(p))
            && (p.iter().sum::<f64>() - 1.).abs() < 1e-7,
        "V6 probability mass"
    );
    ensure!(
        (v["model_or_assistance_probability"]
            .as_f64()
            .context("V6 involvement")?
            - p[1]
            - p[2])
            .abs()
            < 1e-7,
        "V6 involvement mismatch"
    );
    Ok(
        json!({"origin":{"human_only":p[0],"model_only":p[1],"mixed":p[2]},"ai_confidence_percent":100.*(p[1]+p[2])}),
    )
}
fn metrics(rows: &[Value], pred: &BTreeMap<String, Value>) -> Result<Value> {
    // No share field exists in the frozen V6 output. Masking happens only in this
    // comparator view; the original labelled dataset remains byte-identical.
    let origin_rows = rows
        .iter()
        .map(|r| {
            let mut v = r.clone();
            v["targets"]["share"] = Value::Null;
            v
        })
        .collect::<Vec<_>>();
    let mut natural = supervised::origin_share_metrics(&origin_rows, pred, None)?;
    natural.as_object_mut().unwrap().remove("share");
    Ok(
        json!({"family":natural,"family_origin_balanced":supervised::balanced_origin_metrics(&origin_rows,pred,None)?}),
    )
}
fn gap_summary(rows: &[Value], gaps: &[f64]) -> Result<Value> {
    ensure!(rows.len() == gaps.len(), "paired gap support differs");
    let mut values = Vec::new();
    let mut cells = BTreeMap::<(String, u64), (f64, usize)>::new();
    for (row, gap) in rows.iter().zip(gaps) {
        ensure!(gap.is_finite(), "nonfinite gap");
        let family = s(row, "source_group")?.to_owned();
        let origin = row["targets"]["origin"]
            .as_u64()
            .context("missing origin target")?;
        ensure!(origin < 3, "invalid origin");
        values.push((family.clone(), *gap));
        let cell = cells.entry((family, origin)).or_default();
        cell.0 += gap;
        cell.1 += 1;
    }
    let balanced = cells
        .into_iter()
        .map(|((g, _), (s, n))| (g, s / n as f64))
        .collect();
    Ok(
        json!({"family":supervised::summary(&values),"family_origin_balanced":supervised::summary(&balanced)}),
    )
}
fn paired_fits(
    root: &Path,
    data: &Path,
    split: &str,
    requested: usize,
    rows: &[Value],
    v6: &BTreeMap<String, Value>,
) -> Result<Value> {
    let mut reports = Vec::new();
    let mut averaged = vec![0.; rows.len()];
    for seed in [17, 29, 41] {
        let fit = root.join(format!("joint-{seed}"));
        let binding = read(&fit.join("fit.json"))?;
        ensure!(
            binding["seed"] == seed
                && binding["control"] == "joint"
                && binding["final_test_opened"] == false,
            "not a pre-Test joint fit"
        );
        ensure!(
            binding["data_manifest_sha256"] == sha(&data.join("manifest.json"))?
                && binding["weights_sha256"] == sha(&fit.join("weights.json"))?,
            "fit binding differs"
        );
        let path = fit.join(format!("{split}-predictions.json"));
        let predictions = indexed(
            read(&path)?
                .as_array()
                .context("predictions array")?
                .clone(),
        )?;
        ensure!(
            predictions.len() == requested,
            "paired requested support differs"
        );
        let predictions = predictions
            .into_iter()
            .map(|(id, row)| (id, row["prediction"].clone()))
            .collect::<BTreeMap<_, _>>();
        let score = metrics(rows, &predictions)?;
        let mut gaps = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let id = s(row, "id")?;
            let target = if row["targets"]["origin"] == 0 {
                0.
            } else {
                1.
            };
            let p = predictions[id]["ai_confidence_percent"]
                .as_f64()
                .context("joint confidence")?
                / 100.;
            let q = v6[id]["ai_confidence_percent"]
                .as_f64()
                .context("V6 confidence")?
                / 100.;
            let gap = (p - target).powi(2) - (q - target).powi(2);
            gaps.push(gap);
            averaged[i] += gap / 3.;
        }
        reports.push(json!({"seed":seed,"joint_origin_metrics":score,"joint_minus_frozen_v6_brier":gap_summary(rows,&gaps)?,"weights_sha256":binding["weights_sha256"],"predictions_sha256":sha(&path)?}));
    }
    Ok(
        json!({"runs":reports,"mean_over_fixed_seeds_joint_minus_frozen_v6_brier":gap_summary(rows,&averaged)?,
        "negative_favors_joint":true,"seed_selection":"all three predeclared seeds; never best seed","uncertainty":"1000 whole-source-family bootstrap draws conditional on Development-selected models; not an independent Test result"}),
    )
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|a| a == "--frozen-test") {
        return final_test::run(&args[1..]);
    }
    ensure!(
        args.len() == 4 || args.len() == 5,
        "evaluate_frozen_v6 NUMERIC EMBEDDINGS SPLIT NEW_REPORT [MATCHED_FIT_MODALITY_ROOT]"
    );
    let data = Path::new(&args[0]);
    let embeddings = Path::new(&args[1]);
    let split = &args[2];
    let output = Path::new(&args[3]);
    ensure!(
        matches!(split.as_str(), "development" | "calibration"),
        "only non-Test evaluation is supported"
    );
    ensure!(!output.exists(), "new report required");
    let manifest = read(&data.join("manifest.json"))?;
    let binding = manifest["splits"]
        .as_array()
        .context("split bindings")?
        .iter()
        .find(|b| b["split"] == *split)
        .context("split binding")?;
    let query_path = data.join(format!("{split}.jsonl"));
    ensure!(
        binding["sha256"] == sha(&query_path)?,
        "numeric rows changed"
    );
    ensure!(
        manifest["population_file_sha256"] == sha(&data.join("population.json"))?,
        "population changed"
    );
    let pop = read(&data.join("population.json"))?;
    let population = indexed(pop["rows"].as_array().context("population rows")?.clone())?;
    let em = read(&embeddings.join("manifest.json"))?;
    ensure!(
        em["status"] == "complete" && em["rows_sha256"] == sha(&embeddings.join("rows.jsonl"))?,
        "embedding run incomplete or changed"
    );
    ensure!(
        em["representation_contract"]["artifact_id"] == V6,
        "frozen runtime identity mismatch"
    );
    let all = indexed(rows(&embeddings.join("rows.jsonl"))?)?;
    ensure!(
        all.len() == em["requested"].as_u64().context("requested")? as usize,
        "embedding population differs"
    );
    let query = rows(&query_path)?;
    let mut supported = Vec::new();
    let mut pred = BTreeMap::new();
    let mut excluded = Vec::new();
    for row in &query {
        ensure!(row["purpose"] == "query", "reference in query split");
        let id = s(row, "id")?;
        let raw = population.get(id).context("missing population binding")?;
        ensure!(
            raw["metadata"]["source_group"] == row["source_group"],
            "family binding mismatch"
        );
        let v = all.get(id).context("missing V6 row")?;
        ensure!(
            v["text_sha256"] == raw["metadata"]["text_sha256"],
            "V6 exact text mismatch"
        );
        if v["status"] != "ok" {
            excluded.push(json!({"id":id,"status":v["status"],"reason":v["reason"],"token_count":v["token_count"]}));
            continue;
        }
        pred.insert(id.to_owned(), prediction(v)?);
        supported.push(row.clone());
    }
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
    let paired = args
        .get(4)
        .map(|p| paired_fits(Path::new(p), data, split, query.len(), &supported, &pred))
        .transpose()?;
    let report = json!({"schema":"slop_ninja_frozen_v6_origin_report_v1","split":split,"requested":query.len(),"supported":supported.len(),"excluded":excluded,
        "all_origins":metrics(&supported,&pred)?,"human_model_only":metrics(&pure,&pred)?,"mixed_only":metrics(&mixed,&pred)?,
        "paired_shared_model":paired,
        "artifact_id":V6,"recalibrated":false,"temperature":em["representation_contract"]["model"]["temperature"],
        "bindings":{"numeric_manifest_sha256":sha(&data.join("manifest.json"))?,"embedding_manifest_sha256":sha(&embeddings.join("manifest.json"))?,"embedding_rows_sha256":em["rows_sha256"],"query_sha256":sha(&query_path)?,"source_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_frozen_v6.rs")))},
        "unsupported_outputs":["fractional_ai_share","human_attribution","generator_attribution"],"test_opened":false,
        "limits":["Historical V6 95.1% accuracy and 0.03264 Brier exclude mixed and describe a different population","Frozen historical temperature is unchanged; this comparator is not recalibrated on the new cohort","Historical human proxies are not documented contemporary human-only workflows","Audited family absence does not establish pretraining or grammar-development nonexposure"]});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    f.write_all(&serde_json::to_vec_pretty(&report)?)?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"requested":query.len(),"supported":supported.len(),"family_brier":report["all_origins"]["family"]["involvement_brier"],"balanced_brier":report["all_origins"]["family_origin_balanced"]["involvement_brier"]})
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_only_metrics_never_invent_share() {
        let v = json!({"status":"ok","artifact_id":V6,"labels":["human_only","model_only","mixed"],"probabilities":[0.1,0.2,0.7],"model_or_assistance_probability":0.9});
        let p = prediction(&v).unwrap();
        assert!(p.get("ai_share_percent").is_none());
        let r = json!({"id":"synthetic","source_group":"work","targets":{"origin":2,"share":0.2}});
        let result = metrics(&[r], &BTreeMap::from([("synthetic".into(), p)])).unwrap();
        assert!(result["family"].get("share").is_none());
        assert!((result["family"]["involvement_brier"].as_f64().unwrap() - 0.01).abs() < 1e-9);
    }
    #[test]
    fn paired_summary_preserves_origin_balance_and_family_units() {
        let mut rows = vec![json!({"source_group":"a","targets":{"origin":0}})];
        let mut gaps = vec![1.];
        for _ in 0..9 {
            rows.push(json!({"source_group":"a","targets":{"origin":2}}));
            gaps.push(-1.);
        }
        rows.push(json!({"source_group":"b","targets":{"origin":1}}));
        gaps.push(0.);
        let result = gap_summary(&rows, &gaps).unwrap();
        assert_eq!(result["family_origin_balanced"]["estimate"], 0.);
        assert_eq!(result["family"]["estimate"], -0.4);
    }
}
