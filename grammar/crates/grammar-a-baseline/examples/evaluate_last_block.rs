//! Bound non-Test evaluation of the calibrated final-block comparison.
//! FIT_BUNDLE RAW_RUN SEALED_SELECTION NEW_OUTPUT_DIRECTORY
//! No fitting, provider calls, raw prose, or Test acquisition.
#[allow(dead_code, unused_imports)]
#[path = "evaluate_supervised.rs"]
pub(crate) mod evaluation;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
type Pred = BTreeMap<String, Value>;
type Gallery = BTreeMap<String, BTreeMap<String, Vec<f64>>>;
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite(), "nonfinite scalar");
    Ok(n)
}
fn vector(v: &Value) -> Result<Vec<f64>> {
    v.as_array()
        .context("numeric array")?
        .iter()
        .map(number)
        .collect()
}
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
fn profiles(c: &Value, role: &str) -> Result<Vec<String>> {
    c[format!("{role}_profiles")]
        .as_array()
        .context("profiles")?
        .iter()
        .map(|v| Ok(v.as_str().context("profile")?.to_owned()))
        .collect()
}
fn softmax(v: &Value, n: usize, t: f64) -> Result<Vec<f64>> {
    ensure!(t.is_finite() && t > 0., "positive finite temperature");
    let values = vector(v)?;
    ensure!(values.len() == n && n > 1, "logit shape");
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exp = values
        .iter()
        .map(|v| ((v - max) / t).exp())
        .collect::<Vec<_>>();
    let sum = exp.iter().sum::<f64>();
    Ok(exp.into_iter().map(|v| v / sum).collect())
}
pub(crate) fn prediction(raw: &Value, c: &Value, cal: &Value) -> Result<Value> {
    let p = softmax(&raw["origin_logits"], 3, number(&cal["temperature"])?)?;
    let q = number(&raw["conditional_mixed_share"])?;
    ensure!((0.0..=1.).contains(&q), "mixed share bound");
    let mut attr = BTreeMap::new();
    for role in ["human", "ai"] {
        let names = profiles(c, role)?;
        let scores = softmax(&raw[format!("{role}_logits")], names.len(), 1.)?;
        let winner = (0..scores.len())
            .max_by(|a, b| scores[*a].total_cmp(&scores[*b]).then_with(|| b.cmp(a)))
            .unwrap();
        ensure!(
            cal["roles"][role]["qualified"] == true,
            "unqualified conditional threshold"
        );
        let threshold = number(&cal["roles"][role]["threshold"])?;
        ensure!((0.0..=1.).contains(&threshold), "threshold bound");
        let profile = if scores[winner] >= threshold {
            names[winner].clone()
        } else {
            format!("other_{role}")
        };
        attr.insert(role,json!({"profile":profile,"involvement_probability":if role=="human"{p[0]+p[2]}else{p[1]+p[2]},
            "conditional_named_scores":names.into_iter().zip(scores).collect::<BTreeMap<_,_>>(),"fallback_asserts_presence":false}));
    }
    let z = vector(&raw["encoding"])?;
    ensure!(z.len() == 768, "shared encoding dimension");
    Ok(
        json!({"origin":{"human_only":p[0],"model_only":p[1],"mixed":p[2]},
        "ai_confidence_percent":100.*(p[1]+p[2]),"ai_share_percent":100.*(p[1]+p[2]*q),
        "attribution":attr,"encoding":z,"calibrated":true}),
    )
}
pub(crate) fn load_part(
    data: &Path,
    run: &Path,
    selection: &Path,
    cal: &Value,
    part: &str,
) -> Result<(Vec<Value>, Pred, Value)> {
    ensure!(
        ["development", "calibration", "reference"].contains(&part),
        "non-Test only"
    );
    let dm = read(&data.join("manifest.json"))?;
    let rm = read(&run.join(part).join("manifest.json"))?;
    let name = format!("{part}.jsonl");
    ensure!(
        rm["schema"] == "slop_ninja_last_block_raw_predictions_v1"
            && rm["complete"] == true
            && rm["partition"] == part
            && rm["final_test_opened"] == false
            && rm["selection_sha256"] == sha(selection)?
            && rm["contract_sha256"] == dm["contract_sha256"]
            && rm["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?
            && rm["partition_sha256"] == dm["files_sha256"][&name]
            && rm["partition_sha256"] == sha(&data.join(&name))?
            && rm["predictions_sha256"] == sha(&run.join(part).join("predictions.jsonl"))?
            && rm["origin_logits_semantics"]
                == "original_classifier_linear_divided_by_base_temperature; no_CAL_temperature",
        "raw data/prediction binding mismatch"
    );
    for key in ["condition", "seed", "selected_checkpoint_sha256"] {
        ensure!(rm[key] == cal[key], "mixed checkpoints");
    }
    let data = rows(&data.join(name))?;
    let mut truth = BTreeMap::new();
    for r in &data {
        ensure!(
            (if part == "reference" {
                r["purpose"] == "reference"
                    && ["train", "development", "calibration"].contains(&string(r, "frozen_split")?)
            } else {
                r["purpose"] == "query" && r["frozen_split"] == part
            }) && truth.insert(string(r, "id")?, r).is_none(),
            "wrong/duplicate population"
        );
    }
    let mut pred = Pred::new();
    for r in rows(&run.join(part).join("predictions.jsonl"))? {
        let id = string(&r, "id")?.to_owned();
        let t = truth.get(id.as_str()).context("extra prediction")?;
        ensure!(
            r["prefix_sha256"] == t["prefix"]["sha256"] && pred.insert(id, r).is_none(),
            "wrong prefix/duplicate prediction"
        );
    }
    ensure!(
        !data.is_empty()
            && data.len() == pred.len()
            && rm["requested"] == data.len()
            && rm["supported"] == data.len()
            && rm["unsupported"] == 0,
        "incomplete predictions"
    );
    Ok((data, pred, rm))
}
pub(crate) fn gallery(refs: &[Value], raw: &Pred, queries: &[Value], part: &str) -> Result<(Gallery, Value)> {
    let query_groups = queries
        .iter()
        .map(|r| string(r, "source_group"))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut samples = BTreeMap::<String, Vec<Vec<f64>>>::new();
    let mut groups = BTreeSet::new();
    let mut n = 0;
    for r in refs {
        if !["train", part].contains(&string(r, "frozen_split")?) {
            continue;
        }
        ensure!(
            !query_groups.contains(string(r, "source_group")?),
            "reference/query source overlap"
        );
        ensure!(
            r["roles"]["human"]["present"] == true && r["roles"]["ai"]["present"] == false,
            "reference role scope"
        );
        let id = string(r, "id")?;
        let identity = string(&r["roles"]["human"], "identity")?;
        samples
            .entry(identity.into())
            .or_default()
            .push(vector(&raw[id]["encoding"])?);
        groups.insert(string(r, "source_group")?.to_owned());
        n += 1;
    }
    let mut human = BTreeMap::new();
    for (name, samples) in samples {
        human.insert(
            name,
            evaluation::pool(&samples)?.context("zero reference profile")?,
        );
    }
    Ok((
        BTreeMap::from([("human".into(), human)]),
        json!({"reference_excerpts":n,"reference_works":groups.len(),
        "partitions":["train",part],"pooling":"normalize(mean(raw shared encodings)); equal reference weight",
        "fit_or_normalization":false,"generator_reference_gallery_available":false,
        "purpose":"independent-work cosine ranking diagnostic; no named threshold and no identity authentication"}),
    ))
}
pub(crate) fn metrics(data: &[Value], pred: &Pred, c: &Value, cal: &Value) -> Result<Value> {
    let mut attribution = BTreeMap::new();
    for role in ["human", "ai"] {
        attribution.insert(
            role,
            evaluation::attribution_metrics(
                data,
                pred,
                role,
                &profiles(c, role)?,
                number(&cal["roles"][role]["threshold"])?,
            )?,
        );
    }
    let mut strata = BTreeMap::<String, Vec<Value>>::new();
    for r in data {
        strata
            .entry(string(r, "acquisition_stratum")?.into())
            .or_default()
            .push(r.clone());
    }
    let by_stratum = strata
        .iter()
        .map(|(k, v)| {
            Ok((
                k,
                json!({"family":evaluation::origin_share_metrics(v,pred,None)?,
        "family_origin_balanced":evaluation::balanced_origin_metrics(v,pred,None)?}),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(
        json!({"family":evaluation::origin_share_metrics(data,pred,None)?,
        "family_origin_balanced":evaluation::balanced_origin_metrics(data,pred,None)?,"attribution":attribution,"by_acquisition_stratum":by_stratum}),
    )
}
fn run(data: &Path, run: &Path, selection: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new evaluation output required");
    let dm = read(&data.join("manifest.json"))?;
    let c = read(&data.join("contract.json"))?;
    let selected = read(selection)?;
    let cal = read(&run.join("calibration.json"))?;
    ensure!(
        dm["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && dm["complete"] == true
            && dm["final_test_opened"] == false
            && dm["contract_sha256"] == sha(&data.join("contract.json"))?,
        "non-Test bundle required"
    );
    ensure!(
        selected["schema"] == "slop_ninja_last_block_condition_selection_v1"
            && selected["complete"] == true
            && selected["selection_split"] == "development"
            && selected["final_test_opened"] == false
            && selected["calibration_read_for_selection"] == false
            && selected["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?,
        "sealed Development selection required"
    );
    ensure!(
        cal["schema"] == "slop_ninja_last_block_calibration_v1"
            && cal["complete"] == true
            && cal["fit_split"] == "calibration"
            && cal["final_test_opened"] == false
            && cal["origin_calibrated"] == true
            && cal["selection_sha256"] == sha(selection)?
            && cal["dataset_manifest_sha256"] == sha(&data.join("manifest.json"))?
            && cal["contract_sha256"] == dm["contract_sha256"]
            && cal["raw_manifest_sha256"] == sha(&run.join("calibration/manifest.json"))?,
        "CAL binding mismatch"
    );
    let admitted = selected["runs"]
        .as_array()
        .context("runs")?
        .iter()
        .filter(|r| r["condition"] == cal["condition"] && r["seed"] == cal["seed"])
        .collect::<Vec<_>>();
    ensure!(
        admitted.len() == 1
            && admitted[0]["selected_checkpoint_sha256"] == cal["selected_checkpoint_sha256"],
        "checkpoint not selected"
    );
    let (refs, ref_raw, ref_manifest) = load_part(data, run, selection, &cal, "reference")?;
    fs::create_dir(out)?;
    let mut reports = BTreeMap::new();
    for part in ["development", "calibration"] {
        let (data, raw, manifest) = load_part(data, run, selection, &cal, part)?;
        let mut pred = Pred::new();
        for (id, r) in raw {
            pred.insert(id, prediction(&r, &c, &cal)?);
        }
        let (gallery, scope) = gallery(&refs, &ref_raw, &data, part)?;
        let report = json!({"partition":part,"condition":cal["condition"],"seed":cal["seed"],"requested":data.len(),"supported":pred.len(),"unsupported":0,
            "metrics":metrics(&data,&pred,&c,&cal)?,"reference_retrieval":evaluation::retrieval_metrics(&data,&pred,&gallery,&c)?,
            "reference_scope":scope,"raw_predictions_manifest":manifest,"raw_reference_manifest":ref_manifest,
            "calibration_fitted_on_this_population":part=="calibration"});
        put(&out.join(format!("{part}-predictions.json")), &json!(pred))?;
        put(&out.join(format!("{part}.json")), &report)?;
        reports.insert(part,json!({"report_sha256":sha(&out.join(format!("{part}.json")))?,"predictions_sha256":sha(&out.join(format!("{part}-predictions.json")))?}));
    }
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_evaluation_v1","complete":true,"condition":cal["condition"],"seed":cal["seed"],
        "dataset_manifest_sha256":sha(&data.join("manifest.json"))?,"selection_sha256":sha(selection)?,"calibration_sha256":sha(&run.join("calibration.json"))?,
        "reports":reports,"final_test_opened":false,"fitting_performed":false,"rust_encoder_numerical_qualification":"separate; evaluation does not waive failed replay gates",
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_last_block.rs"))),
        "metrics_implementation_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_supervised.rs")))}),
    )?;
    println!(
        "{}",
        json!({"complete":true,"condition":cal["condition"],"seed":cal["seed"]})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "FIT_BUNDLE RAW_RUN SEALED_SELECTION NEW_OUTPUT_DIRECTORY"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn low_involvement_does_not_hide_roles_and_share_is_bounded() {
        let raw = json!({"origin_logits":[10.,-10.,-10.],"conditional_mixed_share":0.2,
            "human_logits":[1.,0.],"ai_logits":[2.,0.],"encoding":vec![0.;768]});
        let c = json!({"human_profiles":["human-a","human-b"],"ai_profiles":["ai-a","ai-b"]});
        let cal = json!({"temperature":2.,"roles":{"human":{"qualified":true,"threshold":0.99},"ai":{"qualified":true,"threshold":0.5}}});
        let p = prediction(&raw, &c, &cal).unwrap();
        assert_eq!(p["attribution"]["human"]["profile"], "other_human");
        assert_eq!(p["attribution"]["ai"]["profile"], "ai-a");
        assert!(number(&p["attribution"]["ai"]["involvement_probability"]).unwrap() < 0.001);
        let share = number(&p["ai_share_percent"]).unwrap();
        assert!(share >= 100. * number(&p["origin"]["model_only"]).unwrap());
        assert!(share <= number(&p["ai_confidence_percent"]).unwrap());
        let mut bad = raw;
        bad["conditional_mixed_share"] = json!(1.1);
        assert!(prediction(&bad, &c, &cal).is_err());
    }
    #[test]
    fn reference_query_family_overlap_is_rejected() {
        let refs = vec![json!({"frozen_split":"train","source_group":"same"})];
        let queries = vec![json!({"source_group":"same"})];
        assert!(gallery(&refs, &Pred::new(), &queries, "development").is_err());
    }
}
