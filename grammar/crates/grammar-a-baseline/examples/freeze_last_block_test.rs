//! Freeze the completed two-condition experiment before acquiring reserved Test.
//! STAGE TEST_PLAN.json NEW_SEAL.json | verify SEAL.json
//! No fitting, provider calls or Test prose reads.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::Path,
};

pub const SCHEMA: &str = "slop_ninja_last_block_final_test_seal_v1";
pub fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
pub fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
pub fn sha(p: &Path) -> Result<String> {
    let mut f = fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
#[derive(Default)]
struct Files(BTreeMap<String, String>);
impl Files {
    fn pin(&mut self, p: &Path) -> Result<Value> {
        let p = p.canonicalize()?;
        let h = sha(&p)?;
        if let Some(old) = self
            .0
            .insert(p.to_str().context("UTF8 path")?.into(), h.clone())
        {
            ensure!(old == h, "file changed while sealing");
        }
        Ok(json!({"path":p,"sha256":h}))
    }
    fn require(&mut self, p: &Path, expected: &Value) -> Result<Value> {
        let binding = self.pin(p)?;
        ensure!(
            binding["sha256"] == *expected,
            "changed binding: {}",
            p.display()
        );
        Ok(binding)
    }
}
fn snapshot(stage: &Path, plan_path: &Path) -> Result<Value> {
    let stage = stage.canonicalize()?;
    let mut files = Files::default();
    let plan = read(plan_path)?;
    ensure!(
        plan["schema"] == "slop_ninja_last_block_test_plan_v1"
            && plan["query_works"] == 20
            && plan["reference_works"] == 2
            && plan["recipe"] == "encoder_stage3_v1"
            && plan["requested_span_percentages"] == json!([10, 40, 80, 100])
            && plan["reselection"] == false
            && plan["threshold_fitting"] == false
            && plan["share_target"] == "realized_final_replacement_span_word_fraction"
            && plan["runtime"] == "frozen_cuda_training_runtime"
            && plan["rust_serving_qualification"] == "separate_synthetic_only_no_Test_tuning",
        "frozen evaluation plan required"
    );
    let plan_binding = files.pin(plan_path)?;
    let bundle = stage.join("fit-bundle-v1");
    let dm = read(&bundle.join("manifest.json"))?;
    ensure!(
        dm["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && dm["complete"] == true
            && dm["final_test_opened"] == false,
        "non-Test fitting dataset required"
    );
    let data = files.pin(&bundle.join("manifest.json"))?;
    files.require(&bundle.join("contract.json"), &dm["contract_sha256"])?;
    let contract = read(&bundle.join("contract.json"))?;
    files.require(&bundle.join("protocol.json"), &dm["protocol_sha256"])?;
    for (name, hash) in dm["files_sha256"].as_object().context("dataset files")? {
        ensure!(
            Path::new(name).components().count() == 1,
            "dataset member path"
        );
        files.require(&bundle.join(name), hash)?;
    }
    let selection_path = stage.join("calibration-run-v1/selection.json");
    let selection = read(&selection_path)?;
    let selection_pin = files.pin(&selection_path)?;
    ensure!(
        selection["schema"] == "slop_ninja_last_block_condition_selection_v1"
            && selection["complete"] == true
            && selection["selection_split"] == "development"
            && selection["calibration_read_for_selection"] == false
            && selection["final_test_opened"] == false
            && selection["dataset_manifest_sha256"] == data["sha256"]
            && selection["contract_sha256"] == dm["contract_sha256"]
            && selection["seeds"] == json!([17, 29, 41])
            && selection["representative_seed"] == 17,
        "sealed Development decision required"
    );
    let conditions = ["frozen_last_block", "adapt_last_block"];
    let means = &selection["mean_selected_development_brier"];
    let scores = conditions.map(|c| {
        means[c]
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=1.).contains(v))
    });
    ensure!(
        scores.iter().all(Option::is_some),
        "missing selection means"
    );
    let chosen = conditions[usize::from(scores[1].unwrap() < scores[0].unwrap())];
    ensure!(
        selection["selected_condition"] == chosen,
        "selection rule changed"
    );
    let sr = selection["runs"].as_array().context("selection runs")?;
    ensure!(sr.len() == 6, "six fixed runs required");
    for condition in conditions {
        let values = sr
            .iter()
            .filter(|r| r["condition"] == condition)
            .map(|r| r["criterion"].as_f64().context("selection criterion"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            values.len() == 3
                && values
                    .iter()
                    .all(|x| x.is_finite() && (0.0..=1.).contains(x))
                && (values.iter().sum::<f64>() / 3. - means[condition].as_f64().unwrap()).abs()
                    < 1e-12,
            "selection mean changed"
        );
    }
    let mut runs = Vec::new();
    for condition in conditions {
        for seed in [17, 29, 41] {
            let selected = sr
                .iter()
                .filter(|r| r["condition"] == condition && r["seed"] == seed)
                .collect::<Vec<_>>();
            ensure!(selected.len() == 1, "missing/duplicate selected run");
            let selected = selected[0];
            let fit = stage.join(format!("fits-v1/{condition}-seed-{seed}"));
            files.require(&fit.join("fit.json"), &selected["fit_manifest_sha256"])?;
            let checkpoint = files.require(
                &fit.join("selected.pt"),
                &selected["selected_checkpoint_sha256"],
            )?;
            files.require(
                &fit.join("last-layer.safetensors"),
                &selected["last_layer_sha256"],
            )?;
            files.require(
                &fit.join("shared-heads.safetensors"),
                &selected["shared_heads_sha256"],
            )?;
            let export = stage.join(format!(
                "calibration-run-v1/runs/{condition}-seed-{seed}/export"
            ));
            let m = read(&export.join("manifest.json"))?;
            ensure!(
                m["schema"] == "slop_ninja_last_block_a_export_v1"
                    && m["complete"] == true
                    && m["calibrated"] == true
                    && m["final_test_opened"] == false
                    && m["condition"] == condition
                    && m["seed"] == seed
                    && m["selected_checkpoint_sha256"] == checkpoint["sha256"]
                    && m["selected_epoch"] == selected["selected_epoch"]
                    && m["contract"]["sha256"] == dm["contract_sha256"],
                "export lineage"
            );
            let export_pin = files.pin(&export.join("manifest.json"))?;
            for (key, name) in [
                ("contract", "contract.json"),
                ("weights", "weights.json"),
                ("last_layer", "last-layer.safetensors"),
                ("calibration", "calibration.json"),
                ("reference_requests", "reference-requests.jsonl"),
                ("reference_predictions", "reference-predictions.json"),
            ] {
                ensure!(m[key]["file"] == name, "export member path");
                files.require(&export.join(name), &m[key]["sha256"])?;
            }
            let cal = read(&export.join("calibration.json"))?;
            ensure!(
                cal["complete"] == true
                    && cal["fit_split"] == "calibration"
                    && cal["condition"] == condition
                    && cal["seed"] == seed
                    && cal["final_test_opened"] == false
                    && cal["selection_sha256"] == selection_pin["sha256"]
                    && cal["selected_checkpoint_sha256"] == checkpoint["sha256"]
                    && cal["contract_sha256"] == dm["contract_sha256"]
                    && cal["dataset_manifest_sha256"] == data["sha256"],
                "calibration lineage"
            );
            runs.push(json!({"condition":condition,"seed":seed,"export":export_pin,"checkpoint":checkpoint}));
        }
    }
    let pm = read(&stage.join("pangram-evaluation-v1/manifest.json"))?;
    ensure!(
        pm["complete"] == true
            && pm["final_test_opened"] == false
            && pm["dataset_manifest_sha256"] == data["sha256"],
        "Pangram comparison incomplete"
    );
    files.pin(&stage.join("pangram-evaluation-v1/manifest.json"))?;
    let mapping = files.require(
        &stage.join("pangram-evaluation-v1/mapping.json"),
        &pm["mapping_sha256"],
    )?;
    let mapping_value = read(Path::new(text(&mapping, "path")?))?;
    ensure!(
        mapping_value["fitting_split"] == "calibration"
            && mapping_value["development_used_for_fit"] == false
            && mapping_value["final_test_opened"] == false,
        "CAL-only Pangram mapping required"
    );
    let fixed = read(&stage.join("fixed-comparators-v1/manifest.json"))?;
    files.require(
        &stage.join("fixed-comparators-v1/manifest.json"),
        &pm["prepared_manifest_sha256"],
    )?;
    files.require(
        &stage.join("fixed-comparators-v1/prior-contract.json"),
        &fixed["files_sha256"]["prior-contract.json"],
    )?;
    files.require(
        Path::new(text(&fixed, "v6_initializer_path")?),
        &fixed["v6_initializer_sha256"],
    )?;
    for r in fixed["fixed_prior_runs"].as_array().context("prior runs")? {
        files.require(Path::new(text(r, "weights_path")?), &r["weights_sha256"])?;
    }
    let sm = read(&stage.join("source-metadata-v2/manifest.json"))?;
    ensure!(
        sm["complete"] == true && sm["source_prose_opened"] == false,
        "metadata reservation required"
    );
    files.pin(&stage.join("source-metadata-v2/manifest.json"))?;
    files.require(
        &stage.join("source-metadata-v2/reservation.json"),
        &sm["reservation_sha256"],
    )?;
    let source_plan = files.require(
        &stage.join("source-metadata-v2/selection.tsv"),
        &sm["selection_sha256"],
    )?;
    let tsv = fs::read_to_string(text(&source_plan, "path")?)?;
    let mut lines = tsv.lines();
    let header = lines
        .next()
        .context("source TSV header")?
        .split('\t')
        .collect::<Vec<_>>();
    let split_col = header
        .iter()
        .position(|x| *x == "split")
        .context("split column")?;
    let parts = lines
        .map(|l| l.split('\t').nth(split_col).context("source row"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        parts.iter().filter(|p| **p == "test").count() == 20
            && parts.iter().filter(|p| **p == "test_reference").count() == 2,
        "reserved source counts changed"
    );
    let parent_path = Path::new(text(&plan, "parent_artifact_path")?);
    let parent = read(&parent_path.join("manifest.json"))?;
    ensure!(
        parent["artifact_id"] == contract["parent_artifact_id"],
        "parent changed"
    );
    files.pin(&parent_path.join("manifest.json"))?;
    for (name, hash) in parent["files"].as_object().context("parent files")? {
        ensure!(
            Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "parent member path"
        );
        files.require(&parent_path.join(name), hash)?;
    }
    let mut generator_ids = BTreeSet::new();
    let generators = plan["generators"].as_array().context("generator plan")?;
    ensure!(
        generators.len() == 3,
        "two known generators and one withheld Test generator required"
    );
    for binding in generators {
        let config_path = Path::new(text(binding, "path")?);
        files.require(config_path, &binding["sha256"])?;
        let c = read(config_path)?;
        ensure!(
            c["schema"] == "slop_ninja_local_generator_v1"
                && c["allowed_splits"] == json!(["test"])
                && c["rights"]["generation_permitted"] == true
                && generator_ids.insert(text(&c, "identity")?.to_owned()),
            "generator scope/identity/rights"
        );
        files.require(Path::new(text(&c, "model_path")?), &c["sha256"])?;
        files.require(Path::new(text(&c, "runtime_path")?), &c["runtime_sha256"])?;
        for lib in c["runtime_libraries"]
            .as_array()
            .context("runtime libraries")?
        {
            files.require(Path::new(text(lib, "path")?), &lib["sha256"])?;
        }
    }
    let known = contract["ai_profiles"]
        .as_array()
        .context("known generators")?
        .iter()
        .map(|v| Ok(v.as_str().context("generator id")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    let held_cal = read(&stage.join("generation-v1/configs/heldout_CAL_only.json"))?;
    files.pin(&stage.join("generation-v1/configs/heldout_CAL_only.json"))?;
    ensure!(
        known.len() == 2
            && known.is_subset(&generator_ids)
            && !generator_ids.contains(text(&held_cal, "identity")?),
        "Test generator enrollment differs"
    );
    Ok(json!({"schema":SCHEMA,"complete":true,"stage_root":stage,
        "final_test_opened":false,"test_used_for_selection":false,"selected_condition":chosen,
        "evaluation_seeds":[17,29,41],"representative_seed":17,"selection":selection_pin,
        "source_plan":source_plan,"test_query_works":20,"test_reference_works":2,
        "test_plan":plan_binding,"dataset":data,"runs":runs,"pangram_mapping":mapping,
        "production_qualified":false,"model_reselection_permitted":false,
        "binding_files":files.0.iter().map(|(p,h)|json!({"path":p,"sha256":h})).collect::<Vec<_>>()}))
}
pub fn validate(seal: &Path) -> Result<Value> {
    let r = read(seal)?;
    ensure!(
        r["schema"] == SCHEMA && r["complete"] == true,
        "Test seal schema"
    );
    let expected = snapshot(
        Path::new(text(&r, "stage_root")?),
        Path::new(text(&r["test_plan"], "path")?),
    )?;
    ensure!(r == expected, "sealed experiment changed");
    Ok(r)
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.len() == 2 && a[0] == "verify" {
        validate(Path::new(&a[1]))?;
        println!("verified frozen Test seal");
        return Ok(());
    }
    ensure!(
        a.len() == 3,
        "STAGE TEST_PLAN.json NEW_SEAL.json | verify SEAL.json"
    );
    ensure!(!Path::new(&a[2]).exists(), "new seal required");
    let r = snapshot(Path::new(&a[0]), Path::new(&a[1]))?;
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&a[2])?;
    serde_json::to_writer_pretty(&mut f, &r)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"complete":true,"selected_condition":r["selected_condition"],"bound_files":r["binding_files"].as_array().unwrap().len(),"final_test_opened":false})
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutation_cannot_be_silently_rebound() {
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("weights");
        fs::write(&p, b"original").unwrap();
        let mut f = Files::default();
        let pin = f.pin(&p).unwrap();
        f.require(&p, &pin["sha256"]).unwrap();
        fs::write(&p, b"modified").unwrap();
        assert!(f.require(&p, &pin["sha256"]).is_err());
    }
    #[test]
    fn incomplete_or_other_experiment_seal_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("seal.json");
        for r in [
            json!({}),
            json!({"schema":SCHEMA,"complete":false}),
            json!({"schema":"other","complete":true}),
        ] {
            fs::write(&p, serde_json::to_vec(&r).unwrap()).unwrap();
            assert!(validate(&p).is_err());
        }
    }
}
