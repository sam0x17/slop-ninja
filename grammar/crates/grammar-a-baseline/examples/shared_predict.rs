//! Streaming Rust numeric inference, with no reference predictions or Python.
//! Each input line is {id, input:{contract_sha256,x}}; other audit fields ignored.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::shared::SharedInput;
use grammar_a_baseline::warm_shared::NumericSharedModel as SharedModel;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    time::Instant,
};
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 3,
        "shared_predict WEIGHTS.json NUMERIC_ROWS.jsonl NEW_PREDICTIONS.jsonl"
    );
    ensure!(!Path::new(&a[2]).exists(), "new output required");
    let start = Instant::now();
    let model = SharedModel::from_json(&fs::read(&a[0])?)?;
    let load_us = start.elapsed().as_micros();
    let mut output = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&a[2])?,
    );
    let mut times = Vec::new();
    for line in BufReader::new(fs::File::open(&a[1])?).lines() {
        let line = line?;
        let row: Value = serde_json::from_str(&line)?;
        let id = row["id"].as_str().context("row id")?;
        let input: SharedInput = serde_json::from_value(row["input"].clone())?;
        let clock = Instant::now();
        let prediction = model.predict(&input)?;
        times.push(clock.elapsed().as_nanos());
        serde_json::to_writer(&mut output, &json!({"id":id,"prediction":prediction}))?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    output.get_ref().sync_all()?;
    ensure!(!times.is_empty(), "empty request");
    times.sort_unstable();
    eprintln!(
        "{}",
        json!({"documents":times.len(),"model_load_micros":load_us,"numeric_inference_p50_ns":times[(times.len()-1)/2],"numeric_inference_p95_ns":times[(times.len()-1)*95/100],"numeric_weight_bytes":model.numeric_weight_bytes(),"scope":"numeric_inference_only;CFG_extraction_and_normalization_separate"})
    );
    Ok(())
}
