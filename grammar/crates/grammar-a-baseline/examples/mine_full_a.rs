//! Hard-negative mining rounds for full A (selection, request queue, next-round
//! bundle, stop rule). Scoring is `mine_full_a.py`; generation is the local
//! generator's job and reads the queue later.
//!
//! select --scores SCORES_DIR --pool TRAIN_MINING_DOCUMENTS.jsonl --bundle BUNDLE
//!        --splits SPLITS_DIR [--evaluation-manifest EXTRA.jsonl...] --round K --top N
//!        [--per-group-cap C] [--min-p P (default 0.5)] [--seed S] [--cohort-weight W]
//!        --generators a,b,c [--kinds mirror,mixed] --queue QUEUE_DIR --out NEW_ROUND_DIR
//! extend --bundle BUNDLE --add ROWS.jsonl... --out NEW_BUNDLE
//! stop   --round-fits FIT,FIT,FIT --round-bundle BUNDLE [--round-fits ... --round-bundle ...]
//!        (one pair per round, round 0 first) --out NEW_DECISION.json
//!
//! `--splits` names the current splits directory; every evaluation manifest
//! (all parts), diagnostic manifest and reserved group list is derived from
//! its SHA256SUMS and required. The scores must come from a fit on BUNDLE.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_baseline::mining::{self, Exclusions, PoolFacts, Score, SelectConfig};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Select {
        #[arg(long)]
        scores: PathBuf,
        #[arg(long)]
        pool: PathBuf,
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        splits: PathBuf,
        #[arg(long = "evaluation-manifest")]
        evaluation_manifests: Vec<PathBuf>,
        #[arg(long)]
        round: usize,
        #[arg(long)]
        top: usize,
        #[arg(long, default_value_t = 4)]
        per_group_cap: usize,
        #[arg(long, default_value_t = mining::DEFAULT_MIN_P)]
        min_p: f64,
        #[arg(long, default_value = "slop_ninja_a_mining_v1")]
        seed: String,
        #[arg(long, default_value_t = 1.0)]
        cohort_weight: f64,
        #[arg(long, value_delimiter = ',', required = true)]
        generators: Vec<String>,
        #[arg(long, value_delimiter = ',', default_value = "mirror,mixed")]
        kinds: Vec<String>,
        #[arg(long)]
        queue: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Extend {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long = "add", required = true)]
        add: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    Stop {
        /// One per round, round 0 (the pre-mining arm) first; seed fit dirs comma separated.
        #[arg(long = "round-fits", required = true)]
        round_fits: Vec<String>,
        /// The bundle each round's fits were trained on, in the same order.
        #[arg(long = "round-bundle", required = true)]
        round_bundles: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn write_jsonl(path: &Path, rows: &[Value]) -> Result<()> {
    let mut w = BufWriter::new(File::create_new(path).with_context(|| format!("create {}", path.display()))?);
    for r in rows {
        w.write_all(serde_json::to_string(r)?.as_bytes())?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn select(scores: &Path, pool: &Path, bundle: &Path, splits: &Path, manifests: &[PathBuf], cfg: SelectConfig,
          cohort_weight: f64, generators: &[String], kinds: &[String], queue: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new round directory required");
    let score_manifest: Value = serde_json::from_slice(&fs::read(scores.join("manifest.json"))?)?;
    ensure!(score_manifest["schema"] == mining::SCORE_SCHEMA && score_manifest["complete"] == true &&
            score_manifest["pool_split"] == mining::POOL_SPLIT && score_manifest["final_test_opened"] == false,
            "complete Train-mining score file required");
    // The scores must come from a fit on the bundle this round extends.
    let binding = &score_manifest["binding"];
    ensure!(binding["bundle_sha256"] == mining::bundle_digests(bundle)? &&
            binding["fit_sha256"] == score_manifest["fit_sha256"] &&
            binding["pool_sha256"] == score_manifest["pool_sha256"] &&
            score_manifest["binding_sha256"].as_str() == Some(mining::sha256_file(&scores.join("binding.json"))?.as_str()),
            "scores were not produced by a fit on this bundle (score binding differs)");
    let scores_file = scores.join("scores.jsonl");
    ensure!(mining::sha256_file(&scores_file)? == score_manifest["scores_sha256"].as_str().unwrap_or(""),
            "scores changed since their manifest");
    let pool_sha = mining::sha256_file(pool)?;
    ensure!(score_manifest["pool_sha256"].as_str() == Some(pool_sha.as_str()), "scores are for another pool file");
    let mut all = Vec::new();
    mining::for_each_jsonl(&scores_file, |_, v| {
        all.push(Score::from_json(&v)?);
        Ok(())
    })?;
    let wanted: HashSet<&str> = all.iter().map(|s| s.id.as_str()).collect();
    // Pass 1 over the pool: split check on every row; facts for scored rows (text-free).
    let mut facts = HashMap::new();
    mining::for_each_jsonl(pool, |n, v| {
        let split = v["split_assignment"]["split"].as_str();
        ensure!(v["frozen_split"] == mining::POOL_SPLIT && split == Some(mining::POOL_SPLIT),
                "pool row {n}: only Train-mining rows may be mined");
        ensure!(v["origin"] == "human_only", "pool row {n}: human-only rows only");
        let id = v["id"].as_str().context("id")?;
        if wanted.contains(id) {
            facts.insert(id.to_owned(), PoolFacts {
                text_sha256: v["text_sha256"].as_str().context("text_sha256")?.to_owned(),
                source_group: v["source_group"].as_str().context("source_group")?.to_owned(),
                writer_key: v["writer_key"].as_str().map(str::to_owned),
            });
        }
        Ok(())
    })?;
    let mut excl = Exclusions::default();
    excl.add_bundle(&bundle.join("rows.jsonl"))?;
    let mut manifest_rows = excl.add_splits_dir(splits)?;
    for m in manifests {
        let n = excl.add_evaluation_manifest(m)?;
        manifest_rows.push(json!({"path": m.display().to_string(), "sha256": mining::sha256_file(m)?, "rows": n}));
    }
    let (chosen, reasons) = mining::select(&all, &facts, &excl, &cfg)?;
    let rank: HashMap<&str, usize> = chosen.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
    // Pass 2: exact text for the chosen rows only.
    let mut rows: Vec<Option<Value>> = vec![None; chosen.len()];
    let mut requests: Vec<Option<Vec<Value>>> = vec![None; chosen.len()];
    let scores_sha = mining::sha256_file(&scores_file)?;
    mining::for_each_jsonl(pool, |_, v| {
        let id = v["id"].as_str().context("id")?;
        if let Some(&r) = rank.get(id) {
            let provenance = json!({"pool_id": id, "pool_text_sha256": v["text_sha256"], "pool_sha256": pool_sha,
                                    "scores_sha256": scores_sha, "round": cfg.round, "selection_rank": r,
                                    "pool_collection": v.get("collection"), "pool_rights": v.get("rights")});
            rows[r] = Some(mining::mined_row(&v, cfg.round, cohort_weight, &provenance)?);
            requests[r] = Some(mining::requests_for(&v, r, cfg.round, kinds, generators)?);
        }
        Ok(())
    })?;
    let rows: Vec<Value> = rows.into_iter().collect::<Option<_>>().context("chosen row missing from pool")?;
    let requests: Vec<Value> = requests.into_iter().flatten().flatten().collect();
    fs::create_dir_all(out)?;
    write_jsonl(&out.join("mined-rows.jsonl"), &rows)?;
    fs::create_dir_all(queue)?;
    let queue_file = queue.join(format!("round-{}-requests.jsonl", cfg.round));
    write_jsonl(&queue_file, &requests)?;
    let queue_index = queue.join("QUEUE.json");
    let mut index: Value = if queue_index.exists() {
        serde_json::from_slice(&fs::read(&queue_index)?)?
    } else {
        json!({"schema": mining::QUEUE_SCHEMA, "rounds": [],
               "consumer": "local positive generator: render each pending request with its own templates over the \
                            parent passage, write shared-contract Train rows in cohort mined_matched_positives_round_K, \
                            then `mine_full_a extend` appends them"})
    };
    index["rounds"].as_array_mut().context("queue rounds")?.push(json!({
        "round": cfg.round, "file": queue_file.file_name().unwrap().to_string_lossy(),
        "sha256": mining::sha256_file(&queue_file)?, "requests": requests.len(), "status": "pending"}));
    let tmp = queue.join("QUEUE.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&index)?)?;
    fs::rename(&tmp, &queue_index)?;
    let p: Vec<f64> = chosen.iter().map(|s| s.p_any_ai).collect();
    let q = |f: f64| p.get(((p.len() as f64 - 1.0) * f).round() as usize).copied();
    fs::write(out.join("round.json"), serde_json::to_vec_pretty(&json!({
        "schema": mining::ROUND_SCHEMA, "round": cfg.round, "complete": true, "final_test_opened": false,
        "scores": {"manifest": score_manifest, "sha256": scores_sha}, "pool": {"path": pool.display().to_string(),
        "sha256": pool_sha}, "bundle_rows_sha256": mining::sha256_file(&bundle.join("rows.jsonl"))?,
        "evaluation_manifests": manifest_rows, "scored": all.len(), "selected": chosen.len(),
        "exclusions": reasons, "config": {"top": cfg.top, "per_group_cap": cfg.per_group_cap, "min_p": cfg.min_p,
        "seed": cfg.seed, "cohort_weight": cohort_weight, "generators": generators, "kinds": kinds},
        "selected_p_any_ai": {"max": q(0.0), "median": q(0.5), "min": q(1.0)},
        "mined_rows_sha256": mining::sha256_file(&out.join("mined-rows.jsonl"))?,
        "queue_file": queue_file.display().to_string(), "requests": requests.len(),
        "splits": splits.display().to_string(),
        "rule": "highest uncalibrated P(any AI) Train-mining humans at or above min_p, not in the bundle and not \
                 sharing a document, source family or writer with CAL, Dev-iter, Dev-select, Test, a diagnostic \
                 slice or a reserved evaluation group; ties by SHA-256(seed, id)",
    }))?)?;
    println!("{}", json!({"selected": chosen.len(), "requests": requests.len()}));
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Select { scores, pool, bundle, splits, evaluation_manifests, round, top, per_group_cap, min_p,
                          seed, cohort_weight, generators, kinds, queue, out } => {
            ensure!(cohort_weight.is_finite() && cohort_weight > 0.0, "cohort weight");
            ensure!(min_p.is_finite() && (0.0..=1.0).contains(&min_p), "min_p");
            select(&scores, &pool, &bundle, &splits, &evaluation_manifests,
                   SelectConfig { round, top, per_group_cap, min_p, seed }, cohort_weight, &generators, &kinds,
                   &queue, &out)
        }
        Command::Extend { bundle, add, out } => {
            let adds: Vec<&Path> = add.iter().map(PathBuf::as_path).collect();
            let m = mining::extend_bundle(&bundle, &adds, &out)?;
            println!("{}", json!({"rows_sha256": m["rows_sha256"], "appended_cohorts": m["appended_cohorts"]}));
            Ok(())
        }
        Command::Stop { round_fits, round_bundles, out } => {
            ensure!(!out.exists(), "new decision file required");
            ensure!(round_fits.len() == round_bundles.len(), "one --round-bundle per --round-fits");
            let mut fits = Vec::new();
            for group in &round_fits {
                fits.push(group.split(',').map(|p| mining::RoundFit::read(Path::new(p))).collect::<Result<Vec<_>>>()?);
            }
            let bundles: Vec<&Path> = round_bundles.iter().map(PathBuf::as_path).collect();
            let (means, rounds) = mining::comparable_rounds(&fits, &bundles)?;
            let d = mining::stop_rule(&means)?;
            let v = json!({"schema": mining::STOP_SCHEMA, "rounds": rounds, "improvements": d.improvements,
                           "stop": d.stop, "next_round": d.next_round, "reason": d.reason,
                           "comparability": "same seed set, arm, arm definition, protocol and Dev-iter rows in \
                                             every round; round k's bundle extends round k-1's with round-k \
                                             mining cohorts only",
                           "rule": "stop after the first round whose mean Dev-iter balanced Brier falls by less \
                                    than 0.001, and after three rounds at most"});
            fs::write(&out, serde_json::to_vec_pretty(&v)?)?;
            println!("{v}");
            Ok(())
        }
    }
}
