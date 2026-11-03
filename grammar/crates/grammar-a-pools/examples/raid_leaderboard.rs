//! RAID leaderboard contract for A: turns the public RAID test CSV into A
//! requests and A predictions into the leaderboard's `predictions.json`.
//! It never runs A, never contacts a service and never submits anything.
//!
//! ```text
//! raid_leaderboard requests    --test test.csv.zst --expect-sha256 SHA --out DIR
//! raid_leaderboard predictions --test test.csv.zst --expect-sha256 SHA \
//!     --a-predictions RUN/predictions.jsonl --detector-name NAME --out DIR
//! ```
//!
//! Contract (RAID `run_detection` / `evaluate.py`, MIT, github.com/liamdugan/raid):
//! - input: `test.csv` with columns `id,generation`; every row is scored;
//! - A request: one JSON line `{"id", "text"}` per row, `text` byte-identical
//!   to `generation` (no trimming or normalisation), in file order;
//! - A output: the full-A Rust runner's `predictions.jsonl` rows
//!   (`id`, `text_sha256`, `status`, `prediction.ai_confidence_percent`);
//! - leaderboard output: `predictions.json`, a JSON array of
//!   `{"id": <RAID id>, "score": <float>}` in test order, where a higher score
//!   means more likely machine-generated. The score is A's calibrated
//!   involvement confidence (`ai_confidence_percent / 100`) unless
//!   `--score share` asks for the share estimate.
//! - `metadata.json` follows `leaderboard/template-metadata.json`; contact
//!   fields stay as placeholders for the operator to fill.
//!
//! Rows A cannot score (`unsupported_input`, e.g. over the token limit) fail
//! the run unless `--fill-unsupported X` is given; fills are counted in
//! `run.json`. RAID's evaluator drops a whole cell when any score in it is
//! null, so a missing score is never written as null.
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use grammar_a_pools::{io as pio, raid::CsvStream, text::sha_hex};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(name = "raid_leaderboard", about = "RAID test requests for A and leaderboard predictions from A outputs (no model, network or submission).")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write A requests (`{"id","text"}` JSONL) for every RAID test row.
    Requests {
        #[arg(long)]
        test: PathBuf,
        /// SHA-256 of the decompressed test CSV (the published file hash).
        #[arg(long)]
        expect_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Join A predictions to the test rows and write predictions.json and metadata.json.
    Predictions {
        #[arg(long)]
        test: PathBuf,
        #[arg(long)]
        expect_sha256: String,
        /// The A runner's predictions.jsonl for the requests of this test file.
        #[arg(long)]
        a_predictions: PathBuf,
        #[arg(long)]
        detector_name: String,
        /// confidence (default) or share.
        #[arg(long, default_value = "confidence")]
        score: String,
        /// Score written for rows A reports as unsupported (otherwise an error).
        #[arg(long)]
        fill_unsupported: Option<f64>,
        #[arg(long)]
        date_released: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
}

/// RAID test rows (id, generation) in file order, with the decompressed SHA-256.
fn test_rows(path: &Path, expect: &str) -> Result<(Vec<(String, String)>, String)> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let dec: Box<dyn Read> = if path.extension().is_some_and(|e| e == "zst") { Box::new(zstd::Decoder::new(f)?) } else { Box::new(f) };
    let mut hr = pio::HashingReader::new(dec);
    let mut rows = Vec::new();
    {
        let mut csv = CsvStream::new(BufReader::with_capacity(1 << 22, &mut hr));
        let header = csv.next_record()?.context("empty test CSV")?;
        let (ci, cg) = (header.iter().position(|h| h == "id"), header.iter().position(|h| h == "generation"));
        let (Some(ci), Some(cg)) = (ci, cg) else { bail!("test CSV needs id and generation columns, has {header:?}") };
        let mut seen = std::collections::HashSet::new();
        while let Some(r) = csv.next_record()? {
            ensure!(r.len() == header.len(), "record {} has {} fields", csv.records, r.len());
            ensure!(seen.insert(r[ci].clone()), "duplicate test id {}", r[ci]);
            rows.push((r[ci].clone(), r[cg].clone()));
        }
    }
    let (sha, _) = hr.finish();
    ensure!(sha == expect, "decompressed test SHA-256 {sha} differs from the expected {expect}");
    Ok((rows, sha))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<String> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(sha_hex(bytes))
}

fn requests(test: &Path, expect: &str, out: &Path) -> Result<Value> {
    let (rows, sha) = test_rows(test, expect)?;
    std::fs::create_dir_all(out)?;
    let mut buf = Vec::new();
    let mut empty = 0u64;
    for (id, text) in &rows {
        if text.trim().is_empty() {
            empty += 1;
        }
        serde_json::to_writer(&mut buf, &json!({"id": id, "text": text}))?;
        buf.push(b'\n');
    }
    let req_sha = write_new(&out.join("requests.jsonl"), &buf)?;
    let m = json!({"schema": "slop_ninja_raid_leaderboard_requests_v1", "test_path": test, "test_csv_sha256": sha, "rows": rows.len(),
                   "empty_texts": empty, "requests_sha256": req_sha, "text_rule": "byte-identical to the RAID generation field; file order"});
    write_new(&out.join("requests-manifest.json"), &serde_json::to_vec_pretty(&m)?)?;
    Ok(m)
}

/// Leaderboard score of one A prediction row.
fn score_of(row: &Value, field: &str) -> Option<f64> {
    let p = &row["prediction"];
    let pct = match field {
        "confidence" => p["ai_confidence_percent"].as_f64()?,
        "share" => p["ai_share_percent"].as_f64()?,
        _ => return None,
    };
    (pct.is_finite() && (0.0..=100.0).contains(&pct)).then_some(pct / 100.0)
}

struct Joined {
    scores: Vec<(String, f64)>,
    filled: u64,
    unsupported_reasons: HashMap<String, u64>,
}

fn join(rows: &[(String, String)], a_rows: &[Value], field: &str, fill: Option<f64>) -> Result<Joined> {
    ensure!(field == "confidence" || field == "share", "--score must be confidence or share");
    if let Some(f) = fill {
        ensure!(f.is_finite() && (0.0..=1.0).contains(&f), "--fill-unsupported must lie in [0, 1]");
    }
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    for r in a_rows {
        let id = r["id"].as_str().context("A prediction without id")?;
        ensure!(by_id.insert(id, r).is_none(), "duplicate A prediction for {id}");
    }
    ensure!(by_id.len() == rows.len(), "A predictions cover {} ids, the test has {}", by_id.len(), rows.len());
    let mut scores = Vec::with_capacity(rows.len());
    let (mut filled, mut reasons) = (0u64, HashMap::new());
    for (id, text) in rows {
        let r = by_id.get(id.as_str()).with_context(|| format!("no A prediction for test id {id}"))?;
        ensure!(r["text_sha256"] == sha_hex(text.as_bytes()), "A prediction for {id} was made on different text");
        let s = match r["status"].as_str() {
            Some("ok") => score_of(r, field).with_context(|| format!("A prediction for {id} lacks a valid {field} percentage"))?,
            Some("unsupported_input") => {
                *reasons.entry(r["reason"].as_str().unwrap_or("unknown").to_owned()).or_default() += 1;
                filled += 1;
                fill.with_context(|| format!("A could not score {id} ({}); pass --fill-unsupported to fill", r["reason"]))?
            }
            other => bail!("unexpected A status {other:?} for {id}"),
        };
        scores.push((id.clone(), s));
    }
    Ok(Joined { scores, filled, unsupported_reasons: reasons })
}

#[allow(clippy::too_many_arguments)]
fn predictions(test: &Path, expect: &str, a: &Path, name: &str, field: &str, fill: Option<f64>, date: Option<String>, out: &Path) -> Result<Value> {
    ensure!(!name.trim().is_empty() && !name.contains('/'), "detector name must be a non-empty directory name");
    let (rows, sha) = test_rows(test, expect)?;
    let (a_sha, _) = pio::file_sha(a)?;
    let a_rows: Vec<Value> = pio::jsonl_rows(a)?.collect::<Result<_>>()?;
    let j = join(&rows, &a_rows, field, fill)?;
    let dir = out.join(name);
    std::fs::create_dir_all(&dir)?;
    let preds: Vec<Value> = j.scores.iter().map(|(id, s)| json!({"id": id, "score": s})).collect();
    let p_sha = write_new(&dir.join("predictions.json"), &serde_json::to_vec(&preds)?)?;
    let meta = json!({
        "date_released": date.unwrap_or_else(|| pio::now_utc()[..10].to_owned()),
        "detector_name": name,
        "website": "Link to Homepage (Optional)",
        "paper_link": "Link to Paper (Optional)",
        "huggingface_link": "Link to HF model (Optional)",
        "github_link": "Link to Github (Optional)",
        "contact_info": "to be filled by the operator before any submission",
    });
    let m_sha = write_new(&dir.join("metadata.json"), &serde_json::to_vec_pretty(&meta)?)?;
    let run = json!({"schema": "slop_ninja_raid_leaderboard_predictions_v1", "test_csv_sha256": sha, "rows": rows.len(),
        "a_predictions": a, "a_predictions_sha256": a_sha, "score_field": field,
        "score_rule": if field == "confidence" { "prediction.ai_confidence_percent / 100 (calibrated involvement confidence)" } else { "prediction.ai_share_percent / 100" },
        "unsupported_filled": j.filled, "fill_value": fill, "unsupported_reasons": j.unsupported_reasons,
        "predictions_sha256": p_sha, "metadata_sha256": m_sha, "submitted": false,
        "submission": "not performed by this tool; a leaderboard pull request publishes scores and needs the operator's go-ahead"});
    write_new(&dir.join("run.json"), &serde_json::to_vec_pretty(&run)?)?;
    Ok(run)
}

fn main() -> Result<()> {
    let v = match Args::parse().cmd {
        Cmd::Requests { test, expect_sha256, out } => requests(&test, &expect_sha256, &out)?,
        Cmd::Predictions { test, expect_sha256, a_predictions, detector_name, score, fill_unsupported, date_released, out } => {
            predictions(&test, &expect_sha256, &a_predictions, &detector_name, &score, fill_unsupported, date_released, &out)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_row(id: &str, text: &str, status: &str, pct: f64) -> Value {
        json!({"id": id, "text_sha256": sha_hex(text.as_bytes()), "status": status,
               "reason": if status == "ok" { Value::Null } else { json!("unsupported_length") },
               "prediction": if status == "ok" { json!({"ai_confidence_percent": pct, "ai_share_percent": pct / 2.0}) } else { Value::Null }})
    }

    #[test]
    fn end_to_end_on_a_synthetic_test_file() {
        let d = tempfile::tempdir().unwrap();
        let csv = "id,generation\nu1,\"  A synthetic \"\"line\"\".\nSecond line.\n\"\nu2,plain text\n";
        let p = d.path().join("test.csv");
        std::fs::write(&p, csv).unwrap();
        let sha = sha_hex(csv.as_bytes());
        assert!(requests(&p, &"0".repeat(64), &d.path().join("bad")).is_err());
        let m = requests(&p, &sha, &d.path().join("req")).unwrap();
        assert_eq!(m["rows"], 2);
        let req = std::fs::read_to_string(d.path().join("req/requests.jsonl")).unwrap();
        let first: Value = serde_json::from_str(req.lines().next().unwrap()).unwrap();
        assert_eq!(first["text"], "  A synthetic \"line\".\nSecond line.\n");
        let t1 = "  A synthetic \"line\".\nSecond line.\n";
        let a = d.path().join("a.jsonl");
        let lines = [a_row("u2", "plain text", "ok", 12.5), a_row("u1", t1, "ok", 97.0)];
        std::fs::write(&a, lines.iter().map(|v| v.to_string() + "\n").collect::<String>()).unwrap();
        let run = predictions(&p, &sha, &a, "slop-ninja-a", "confidence", None, Some("2026-10-28".into()), &d.path().join("sub")).unwrap();
        assert_eq!(run["submitted"], false);
        let preds: Value = serde_json::from_slice(&std::fs::read(d.path().join("sub/slop-ninja-a/predictions.json")).unwrap()).unwrap();
        assert_eq!(preds, json!([{"id": "u1", "score": 0.97}, {"id": "u2", "score": 0.125}]));
    }

    #[test]
    fn join_refuses_gaps_wrong_text_and_unfilled_unsupported() {
        let rows = vec![("a".to_owned(), "x".to_owned()), ("b".to_owned(), "y".to_owned())];
        assert!(join(&rows, &[a_row("a", "x", "ok", 50.0)], "confidence", None).is_err());
        assert!(join(&rows, &[a_row("a", "x", "ok", 50.0), a_row("b", "z", "ok", 50.0)], "confidence", None).is_err());
        let un = [a_row("a", "x", "ok", 50.0), a_row("b", "y", "unsupported_input", 0.0)];
        assert!(join(&rows, &un, "confidence", None).is_err());
        let j = join(&rows, &un, "confidence", Some(0.5)).unwrap();
        assert_eq!((j.filled, j.scores[1].1), (1, 0.5));
        assert!(join(&rows, &[a_row("a", "x", "ok", 140.0), a_row("b", "y", "ok", 1.0)], "confidence", None).is_err());
        let s = join(&rows, &[a_row("a", "x", "ok", 40.0), a_row("b", "y", "ok", 10.0)], "share", None).unwrap();
        assert_eq!(s.scores[0].1, 0.2);
    }
}
