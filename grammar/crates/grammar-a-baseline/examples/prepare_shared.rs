//! Frozen numeric-only research data for the shared A trainer. No prose enters x.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn write(p: &Path, v: &Value) -> Result<()> {
    ensure!(!p.exists(), "output exists");
    fs::write(p, serde_json::to_vec_pretty(v)?)?;
    Ok(())
}
fn str_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn measurement(row: &Value, key: &str) -> Result<(f64, f64)> {
    let family = key.split(':').next().context("coordinate")?;
    let denom = row["structural"]["opportunities"][family]
        .as_f64()
        .context("denominator")?;
    let count = row["structural"]["counts"][key].as_f64().unwrap_or(0.);
    ensure!(
        denom >= 0. && count >= 0. && count.is_finite(),
        "invalid structural measurement"
    );
    if denom == 0. {
        ensure!(count == 0., "count without opportunities");
        Ok((0., 0.))
    } else {
        Ok(((count / denom).ln_1p(), 1.))
    }
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 2,
        "prepare_shared PREPARED_DIR NEW_OUTPUT_DIR"
    );
    let input = Path::new(&args[0]);
    let out = Path::new(&args[1]);
    ensure!(!out.exists(), "output must be new");
    let manifest = read(&input.join("manifest.json"))?;
    ensure!(
        manifest["matched_cache_binding"]["profile"] == "default_campaign",
        "first fit requires one matched parser profile"
    );
    let bytes = fs::read(input.join("rows.jsonl"))?;
    ensure!(
        sha(&bytes) == manifest["rows_sha256"],
        "row digest mismatch"
    );
    let rows = std::str::from_utf8(&bytes)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    let mut families = rows
        .iter()
        .map(|r| str_at(&r["metadata"], "source_group").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    families.sort_by_key(|g| sha(format!("slop_ninja_a_exposed_split_v1:{g}").as_bytes()));
    ensure!(
        families.len() >= 12,
        "insufficient families for bounded four-way diagnostic"
    );
    let n = families.len();
    let n_train = (n * 55) / 100;
    let n_dev = (n * 15) / 100;
    let n_cal = (n * 15) / 100;
    let split = families
        .iter()
        .enumerate()
        .map(|(i, g)| {
            (
                g.clone(),
                if i < n_train {
                    "train"
                } else if i < n_train + n_dev {
                    "development"
                } else if i < n_train + n_dev + n_cal {
                    "calibration"
                } else {
                    "test"
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let available = rows
        .iter()
        .filter(|r| r["structural_available"] == true)
        .collect::<Vec<_>>();
    let train = available
        .iter()
        .filter(|r| split[str_at(&r["metadata"], "source_group").unwrap()] == "train")
        .copied()
        .collect::<Vec<_>>();
    let mut frequency = BTreeMap::<String, usize>::new();
    for row in &train {
        for key in row["structural"]["counts"]
            .as_object()
            .context("counts")?
            .keys()
        {
            *frequency.entry(key.clone()).or_default() += 1;
        }
    }
    let keys = frequency
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(k, _)| k)
        .collect::<Vec<_>>();
    ensure!(!keys.is_empty(), "no training coordinates");
    let mut family_counts = BTreeMap::<String, usize>::new();
    for row in &train {
        *family_counts
            .entry(str_at(&row["metadata"], "source_group")?.into())
            .or_default() += 1;
    }
    let mut means = Vec::new();
    let mut scales = Vec::new();
    for key in &keys {
        let mut sum = 0.;
        let mut sum2 = 0.;
        let mut total = 0.;
        for row in &train {
            let (value, mask) = measurement(row, key)?;
            let weight = mask / family_counts[str_at(&row["metadata"], "source_group")?] as f64;
            sum += weight * value;
            sum2 += weight * value * value;
            total += weight;
        }
        let mean = if total > 0. { sum / total } else { 0. };
        means.push(mean);
        scales.push(if total > 0. {
            (sum2 / total - mean * mean).max(0.).sqrt().max(0.01)
        } else {
            1.
        });
    }
    let contract = json!({"schema":"slop_ninja_shared_a_numeric_contract_v1","evidence":"native_cfg_selected_tree_structural_counts","exposure":"all_requested_families_grammar_exposed;four_way_research_resplit_is_not_clean_holdout","source_export_sha256":sha(&fs::read(input.join("manifest.json"))?),"source_rows_sha256":manifest["rows_sha256"],"parser":manifest["matched_cache_binding"],"projection":manifest["projection_identity"],"resolution":"retained_semantically_selected_tree_or_unique_complete_tree;stale_or_missing_selection_unavailable","aggregation":"sum_counts_and_opportunities_before_ratio","coordinates":keys,"normalization":{"transform":"log1p(count/opportunities)","fit":"train_only_equal_source_family_weight","mean":means,"scale":scales,"scale_floor":0.01},"layout":"standardized_values_then_explicit_availability_bits","input_dimension":keys.len()*2,"split_assignments":split,"human_profiles":[],"ai_profiles":[],"supervision_limits":["human_proxy_not_documented_human_only","mixed_share_masked_without_span_provenance","no_independent_human_gallery","single_generator_does_not_qualify_attribution"],"origin_order":["human_only","model_only","mixed"],"labels_not_inputs":true});
    let contract_hash = sha(&serde_json::to_vec(&contract)?);
    let mut output = BTreeMap::<String, Vec<Value>>::new();
    let mut population = Vec::new();
    for row in &rows {
        let group = str_at(&row["metadata"], "source_group")?;
        let part = split[group];
        let id = str_at(row, "campaign_document_id")?;
        let eligible = row["structural_available"] == true;
        population.push(json!({"id":id,"group":group,"split":part,"available":eligible,"metadata":row["metadata"],"candidate_receipt_sha256":row["candidate_receipt_sha256"]}));
        if !eligible {
            continue;
        }
        let mut values = Vec::new();
        let mut masks = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let (value, mask) = measurement(row, key)?;
            values.push(if mask == 1. {
                (value - means[i]) / scales[i]
            } else {
                0.
            });
            masks.push(mask);
        }
        values.extend(masks);
        let origin = match str_at(&row["metadata"], "origin")? {
            "human_only" => 0,
            "model_only" => 1,
            "mixed" => 2,
            _ => anyhow::bail!("origin"),
        };
        let share = match origin {
            0 => json!(0.0),
            1 => json!(1.0),
            _ => Value::Null,
        };
        output.entry(part.into()).or_default().push(json!({"id":id,"source_group":group,"input":{"contract_sha256":contract_hash,"x":values},"targets":{"origin":origin,"ai_involvement":origin!=0,"share":share,"human":null,"ai":null},"origin_evidence":row["metadata"]["evidence"],"exposed_to_grammar":true}));
    }
    fs::create_dir(out)?;
    write(&out.join("contract.json"), &contract)?;
    write(
        &out.join("population.json"),
        &json!({"requested":rows.len(),"available":available.len(),"rows":population,"excluded":manifest["unavailable"]}),
    )?;
    let mut bindings = Vec::new();
    for part in ["train", "development", "calibration", "test"] {
        let subset = output.get(part).context("empty split")?;
        let mut bytes = Vec::new();
        for row in subset {
            serde_json::to_writer(&mut bytes, row)?;
            bytes.push(b'\n');
        }
        fs::write(out.join(format!("{part}.jsonl")), &bytes)?;
        bindings.push(json!({"split":part,"rows":subset.len(),"sha256":sha(&bytes),"families":subset.iter().map(|r|r["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len()}));
    }
    write(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_shared_a_dataset_v1","contract_sha256":contract_hash,"contract_file_sha256":sha(&fs::read(out.join("contract.json"))?),"splits":bindings,"requested":rows.len(),"available":available.len(),"clean_holdout":false,"feature_dimension":keys.len()*2,"implementation_sha256":sha(include_bytes!("prepare_shared.rs"))}),
    )?;
    println!(
        "{}",
        serde_json::to_string(&read(&out.join("manifest.json"))?)?
    );
    Ok(())
}
