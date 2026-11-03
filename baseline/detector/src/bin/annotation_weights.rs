//! Prepare a Train-only weighting manifest while retaining every annotation.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, OriginRecord, Split, sha256};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long, required = true)]
    corpus: Vec<PathBuf>,
    #[arg(long, required = true)]
    annotations: Vec<PathBuf>,
    #[arg(long)]
    output_dir: PathBuf,
}

fn bin(score: f64) -> &'static str {
    if (score - 1.0).abs() <= 0.00001 {
        "100"
    } else if score < 0.6 {
        "below-60"
    } else if score < 0.7 {
        "60-70"
    } else if score < 0.8 {
        "70-80"
    } else if score < 0.9 {
        "80-90"
    } else {
        "90-below-100"
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let mut records: Vec<OriginRecord> = Vec::new();
    let mut bindings = Vec::new();
    for path in &args.corpus {
        let before = fs::read(path)?;
        records.extend(dataset::read_records(path)?);
        ensure!(before == fs::read(path)?, "Corpus changed during loading");
        bindings.push(json!({"kind":"corpus","path":path,"sha256":sha256(before)}));
    }
    dataset::validate_records(&records)?;
    ensure!(
        records.iter().all(|r| r.split == Some(Split::Train)),
        "Weighting view accepts Train only"
    );
    let index: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    ensure!(index.len() == records.len(), "Duplicate record IDs");
    let mut observed = BTreeMap::<String, Value>::new();
    let mut counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for path in &args.annotations {
        let bytes = fs::read(path)?;
        for line in std::str::from_utf8(&bytes)?.lines() {
            let annotation: Value = serde_json::from_str(line)?;
            ensure!(
                annotation["schema"] == "slop_ninja_pangram_corpus_annotation_v1"
                    && annotation["observation"]["stage"] == "STAGE_SUCCESS",
                "Require successful, archived collector observations"
            );
            let result = &annotation["observation"]["result"];
            let fractions: Vec<f64> = ["fraction_ai", "fraction_ai_assisted", "fraction_human"]
                .iter()
                .map(|key| result[key].as_f64().context("Missing numeric fraction"))
                .collect::<Result<_>>()?;
            ensure!(
                fractions
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    && (fractions.iter().sum::<f64>() - 1.0).abs() <= 0.00001,
                "Invalid provider fractions"
            );
            let score = fractions[0] + fractions[1];
            let score_bin = bin(score);
            for member in annotation["member"]["records"]
                .as_array()
                .context("Missing member records")?
            {
                let id = member["record_id"].as_str().context("Missing record ID")?;
                let row = index
                    .get(id)
                    .context("Annotation outside supplied corpus")?;
                ensure!(
                    annotation["submitted_text_sha256"] == row.text_sha256
                        && member["source_group"] == row.source_group
                        && member["origin"] == serde_json::to_value(row.origin)?
                        && member["split"] == serde_json::to_value(Split::Train)?,
                    "Annotation provenance mismatch"
                );
                let origin = member["origin"].as_str().context("Missing origin")?;
                let value = json!({
                    "record_id":id,"source_group":row.source_group,"text_sha256":row.text_sha256,
                    "origin":origin,"split":"train","score_bin":score_bin,
                    "fraction_ai":fractions[0],"fraction_ai_assisted":fractions[1],
                    "fraction_human":fractions[2],"combined_fraction":score,
                    "provider_version":result["version"],"echo_match":annotation["echo_match"],
                    "generator_attribution":slop_ninja_detector::attribution::describe(row, &index)?
                });
                ensure!(
                    observed.insert(id.to_owned(), value).is_none(),
                    "Repeated observation; declare repeat aggregation separately"
                );
                *counts
                    .entry(origin.to_owned())
                    .or_default()
                    .entry(score_bin.to_owned())
                    .or_default() += 1;
            }
        }
        bindings.push(json!({"kind":"annotations","path":path,"sha256":sha256(bytes)}));
    }
    ensure!(
        observed.len() == records.len() && !observed.is_empty(),
        "Incomplete annotation coverage"
    );
    let total = observed.len() as f64;
    let origins = counts.len() as f64;
    let mut rows = Vec::new();
    let mut weight_sum = 0.0;
    let mut squares = 0.0;
    let mut maximum: f64 = 0.0;
    for row in observed.values_mut() {
        let cells = &counts[row["origin"].as_str().unwrap()];
        let cell_count = cells[row["score_bin"].as_str().unwrap()];
        let weight = total / (origins * cells.len() as f64 * cell_count as f64);
        row["sample_weight"] = json!(weight);
        weight_sum += weight;
        squares += weight * weight;
        maximum = maximum.max(weight);
        serde_json::to_writer(&mut rows, row)?;
        rows.push(b'\n');
    }
    ensure!(
        (weight_sum - total).abs() < 1e-8 * total,
        "Weight normalization failed"
    );
    let summary = json!({
        "schema":"slop_ninja_annotation_weights_v1","status":"prepared_not_fitted",
        "records":observed.len(),"cells":counts,"input_bindings":bindings,
        "policy":"Equal total weight per observed origin, then equal weight per occupied score bin within origin. Mean sample weight one. Retain every record, raw fraction and independent origin label. Keep source families together in any later split.",
        "limits":"Adaptive Train view, not a benchmark or prevalence estimate. Reweighting does not create independent examples; rare bins may overfit. No change to frozen v6 fitting. Any teacher-supervised fit needs its own declared protocol.",
        "weight_sum":weight_sum,"max_weight":maximum,
        "weight_effective_sample_size":weight_sum * weight_sum / squares,
        "weights_sha256":sha256(&rows),
        "source_sha256":sha256(include_bytes!("annotation_weights.rs")),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?)
    });
    fs::create_dir(&args.output_dir)?;
    for (name, bytes) in [
        ("weights.jsonl", rows),
        ("summary.json", serde_json::to_vec_pretty(&summary)?),
    ] {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.output_dir.join(name))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"records":observed.len(),"max_weight":maximum,"weight_effective_sample_size":weight_sum * weight_sum / squares})
        )?
    );
    Ok(())
}
