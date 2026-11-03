//! Fit a fresh Train-only normalization of the frozen V6 h over a shared-contract
//! bundle's Train rows (`train_query_only_equal_source_family_weight`).
//!
//! requests --bundle BUNDLE --out NEW_DIR [--shards N] [--sample-keep K --sample-modulus M]
//!     Writes `requests-NNN.jsonl` ({id, text}: exact Train text, one shard per
//!     extraction job) and `plan.json` binding the bundle digests, the sample
//!     rule and every planned row (id, text digest, source group, shard).
//!     Rows: `split` train with a positive cohort weight. Sampling keeps whole
//!     source groups: SHA-256("slop_ninja_v6_h_normalization_v1\0" + group)
//!     prefix mod M < K.
//! fit --plan NEW_DIR/plan.json --run RUN_DIR... --out NEW_NORMALIZATION.json
//!     Each RUN_DIR is a complete `v6_embeddings.py` run over one planned shard
//!     (its request snapshot must equal the shard). Every planned row must be
//!     present once with status ok, the planned text digest and one
//!     representation contract across runs. Mean and variance use equal
//!     source-family weights (1 / (groups x rows in the group)), two passes in
//!     FP64; scale = max(sqrt(variance), 0.01). No label, Dev, CAL or Test row
//!     is read.
//!
//! The output is accepted by `prepare_full_a_contract --normalization`.
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use grammar_a_baseline::mining::{bundle_digests, canonical_split, sha256_file, sha256_hex};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const DIM: usize = 768;
const PLAN_SCHEMA: &str = "slop_ninja_full_a_v6_h_normalization_plan_v1";
const NORMALIZATION_SCHEMA: &str = "slop_ninja_v6_train_family_normalization_v1";
const FIT: &str = "train_query_only_equal_source_family_weight";
const SAMPLE_SEED: &str = "slop_ninja_v6_h_normalization_v1";
const SCALE_FLOOR: f64 = 0.01;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Requests {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        shards: usize,
        #[arg(long, default_value_t = 0)]
        sample_keep: u64,
        #[arg(long, default_value_t = 0)]
        sample_modulus: u64,
    },
    Fit {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long = "run", required = true)]
        runs: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn group_sampled(group: &str, keep: u64, modulus: u64) -> bool {
    if modulus == 0 {
        return true;
    }
    let h = sha256_hex(format!("{SAMPLE_SEED}\0{group}").as_bytes());
    u64::from_str_radix(&h[..16], 16).unwrap() % modulus < keep
}

/// Exact text of a row: inline `text` or `text_ref` {path, offset, length}.
struct Texts {
    root: PathBuf,
    files: HashMap<PathBuf, File>,
}

impl Texts {
    fn text(&mut self, v: &Value) -> Result<String> {
        let id = v["id"].as_str().unwrap_or("?");
        let bytes = if let Some(t) = v.get("text") {
            t.as_str().with_context(|| format!("row {id}: text"))?.as_bytes().to_vec()
        } else {
            let r = &v["text_ref"];
            let rel = PathBuf::from(r["path"].as_str().with_context(|| format!("row {id}: text_ref"))?);
            let path = if rel.is_absolute() { rel } else { self.root.join(rel) };
            let offset = r["offset"].as_u64().context("text_ref offset")?;
            let length = r["length"].as_u64().context("text_ref length")? as usize;
            ensure!(length > 0, "row {id}: empty text_ref");
            let f = match self.files.entry(path.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => e.insert(File::open(&path)?),
            };
            f.seek(SeekFrom::Start(offset))?;
            let mut b = vec![0u8; length];
            f.read_exact(&mut b).with_context(|| format!("row {id}: text_ref beyond file end"))?;
            b
        };
        ensure!(Some(sha256_hex(&bytes).as_str()) == v["text_sha256"].as_str(), "row {id}: exact text binding differs");
        Ok(String::from_utf8(bytes)?)
    }
}

fn requests(bundle: &Path, out: &Path, shards: usize, keep: u64, modulus: u64) -> Result<()> {
    ensure!(!out.exists(), "new output directory required");
    ensure!((1..=999).contains(&shards), "1..=999 shards");
    ensure!(modulus == 0 || (0 < keep && keep <= modulus), "sample fraction");
    let digests = bundle_digests(bundle)?;
    fs::create_dir(out)?;
    let mut writers = (0..shards)
        .map(|k| Ok(BufWriter::new(File::create_new(out.join(format!("requests-{k:03}.jsonl")))?)))
        .collect::<Result<Vec<_>>>()?;
    let mut texts = Texts { root: bundle.to_path_buf(), files: HashMap::new() };
    let mut planned = Vec::new();
    let mut skipped_groups = std::collections::BTreeSet::new();
    let reader = BufReader::new(File::open(bundle.join("rows.jsonl"))?);
    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).with_context(|| format!("rows.jsonl line {}", n + 1))?;
        let split = canonical_split(v["split"].as_str().context("split")?)?;
        ensure!(split != "test", "a training bundle must not contain Test rows");
        if split != "train" || v["cohort_weight"].as_f64().context("cohort_weight")? <= 0.0 {
            continue;
        }
        let group = v["source_group"].as_str().context("source_group")?.to_owned();
        if !group_sampled(&group, keep, modulus) {
            skipped_groups.insert(group);
            continue;
        }
        let id = v["id"].as_str().context("id")?.to_owned();
        let shard = planned.len() % shards;
        let text = texts.text(&v)?;
        let w = &mut writers[shard];
        serde_json::to_writer(&mut *w, &json!({"id": id, "text": text}))?;
        w.write_all(b"\n")?;
        planned.push(json!({"id": id, "text_sha256": v["text_sha256"], "source_group": group, "shard": shard}));
    }
    ensure!(!planned.is_empty(), "no Train rows to fit");
    let mut shard_files = Vec::new();
    for (k, mut w) in writers.into_iter().enumerate() {
        w.flush()?;
        drop(w);
        let name = format!("requests-{k:03}.jsonl");
        shard_files.push(json!({"shard": k, "file": name, "sha256": sha256_file(&out.join(&name))?}));
    }
    let groups: std::collections::BTreeSet<&str> = planned.iter().map(|r| r["source_group"].as_str().unwrap()).collect();
    let plan = json!({"schema": PLAN_SCHEMA, "bundle": bundle.display().to_string(), "bundle_sha256": digests,
        "population": "bundle Train rows with a positive cohort weight", "rows": planned.len(),
        "groups": groups.len(), "groups_not_sampled": skipped_groups.len(),
        "sample": (modulus > 0).then(|| json!({"rule": "int(sha256(seed\\0source_group)[:16],16) % modulus < keep",
                                                "seed": SAMPLE_SEED, "keep": keep, "modulus": modulus})),
        "shards": shard_files, "planned": planned, "final_test_opened": false,
        "development_or_calibration_read": false});
    fs::write(out.join("plan.json"), serde_json::to_vec_pretty(&plan)?)?;
    println!("{}", json!({"rows": plan["rows"], "groups": plan["groups"], "shards": shards}));
    Ok(())
}

fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

/// Equal source-family weights over the planned rows: 1 / (G * n_g).
fn weights(planned: &[Value]) -> Result<HashMap<String, (f64, String)>> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for r in planned {
        *counts.entry(r["source_group"].as_str().context("group")?).or_default() += 1;
    }
    let g = counts.len() as f64;
    planned
        .iter()
        .map(|r| {
            let group = r["source_group"].as_str().unwrap();
            Ok((r["id"].as_str().context("id")?.to_owned(),
                (1.0 / (g * counts[group] as f64), r["text_sha256"].as_str().context("digest")?.to_owned())))
        })
        .collect()
}

/// Visit every embedding row of the runs, checked against the plan.
fn each_embedding(runs: &[PathBuf], plan: &HashMap<String, (f64, String)>,
                  mut f: impl FnMut(f64, &[f64])) -> Result<usize> {
    let mut seen = std::collections::HashSet::new();
    for run in runs {
        let reader = BufReader::new(File::open(run.join("rows.jsonl"))?);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line)?;
            let id = v["id"].as_str().context("embedding id")?;
            let (w, digest) = plan.get(id).with_context(|| format!("embedding {id} was not planned"))?;
            ensure!(seen.insert(id.to_owned()), "embedding {id} appears twice");
            ensure!(v["status"] == "ok", "embedding {id}: status {} (Train rows must be supported)", v["status"]);
            ensure!(v["text_sha256"].as_str() == Some(digest.as_str()), "embedding {id}: text binding differs");
            let e = v["embedding"].as_array().context("embedding")?;
            ensure!(e.len() == DIM, "embedding {id}: dimension");
            let x = e.iter().map(|x| x.as_f64().filter(|x| x.is_finite()).context("finite embedding"))
                .collect::<Result<Vec<_>>>()?;
            f(*w, &x);
        }
    }
    ensure!(seen.len() == plan.len(), "runs cover {} of {} planned rows", seen.len(), plan.len());
    Ok(seen.len())
}

fn fit(plan_path: &Path, runs: &[PathBuf], out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new normalization output required");
    let plan = read(plan_path)?;
    ensure!(plan["schema"] == PLAN_SCHEMA && plan["final_test_opened"] == false, "normalization plan required");
    let plan_dir = plan_path.parent().context("plan directory")?;
    let shards: BTreeMap<String, usize> = plan["shards"].as_array().context("shards")?.iter()
        .map(|s| Ok((s["sha256"].as_str().context("shard digest")?.to_owned(), s["shard"].as_u64().context("shard")? as usize)))
        .collect::<Result<_>>()?;
    for s in plan["shards"].as_array().unwrap() {
        ensure!(Some(sha256_file(&plan_dir.join(s["file"].as_str().context("file")?))?.as_str()) == s["sha256"].as_str(),
                "planned shard {} changed", s["shard"]);
    }
    let mut contract = None;
    let mut used = std::collections::BTreeSet::new();
    let mut receipts = Vec::new();
    for run in runs {
        let m = read(&run.join("manifest.json"))?;
        ensure!(m["schema"] == "slop_ninja_v6_embeddings_run_v1" && m["status"] == "complete" &&
                m["dimension"] == DIM as u64, "{} is not a complete V6 embedding run", run.display());
        ensure!(m["rows_sha256"].as_str() == Some(sha256_file(&run.join("rows.jsonl"))?.as_str()) &&
                m["requests_sha256"].as_str() == Some(sha256_file(&run.join("requests.jsonl"))?.as_str()),
                "{}: run files changed since its manifest", run.display());
        let shard = *shards.get(m["requests_sha256"].as_str().unwrap())
            .with_context(|| format!("{}: its requests are not a planned shard", run.display()))?;
        ensure!(used.insert(shard), "shard {shard} given twice");
        ensure!(m["unsupported"] == 0, "{}: unsupported rows", run.display());
        let c = m["representation_contract_sha256"].as_str().context("contract digest")?.to_owned();
        match &contract {
            None => contract = Some((c, m["representation_contract"]["artifact_id"].clone())),
            Some((prev, _)) => ensure!(*prev == c, "runs use different representation contracts"),
        }
        receipts.push(json!({"run": run.display().to_string(), "shard": shard,
                             "manifest_sha256": sha256_file(&run.join("manifest.json"))?,
                             "rows_sha256": m["rows_sha256"], "requests_sha256": m["requests_sha256"]}));
    }
    ensure!(used.len() == shards.len(), "every planned shard needs one run ({} of {})", used.len(), shards.len());
    let planned = plan["planned"].as_array().context("planned rows")?;
    let w = weights(planned)?;
    let mut mean = vec![0.0; DIM];
    let mut total = 0.0;
    let rows = each_embedding(runs, &w, |wt, x| {
        total += wt;
        for (m, v) in mean.iter_mut().zip(x) {
            *m += wt * v;
        }
    })?;
    ensure!((total - 1.0).abs() < 1e-9, "family weights must sum to 1 ({total})");
    let mut variance = vec![0.0; DIM];
    each_embedding(runs, &w, |wt, x| {
        for ((v, m), x) in variance.iter_mut().zip(&mean).zip(x) {
            *v += wt * (x - m).powi(2);
        }
    })?;
    let scale: Vec<f64> = variance.iter().map(|v| v.sqrt().max(SCALE_FLOOR)).collect();
    if !mean.iter().chain(&scale).all(|v| v.is_finite()) {
        bail!("nonfinite normalization");
    }
    let (contract_sha, artifact) = contract.context("no runs")?;
    let groups = planned.iter().map(|r| r["source_group"].as_str().unwrap()).collect::<std::collections::BTreeSet<_>>();
    let value = json!({"schema": NORMALIZATION_SCHEMA, "fit": FIT, "dimension": DIM, "fit_rows": rows,
        "fit_families": groups.len(), "mean": mean, "variance": variance, "scale": scale, "scale_floor": SCALE_FLOOR,
        "development_used_for_fit": false, "calibration_used_for_fit": false, "reference_used_for_fit": false,
        "final_test_opened": false, "labels_used_for_fit": false,
        "population": plan["population"], "sample": plan["sample"], "bundle_sha256": plan["bundle_sha256"],
        "plan_sha256": sha256_file(plan_path)?, "representation_contract_sha256": contract_sha,
        "v6_artifact_id": artifact, "runs": receipts,
        "implementation_sha256": sha256_hex(include_bytes!("fit_full_a_normalization.rs"))});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out)?;
    serde_json::to_writer_pretty(&mut f, &value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"fit_rows": rows, "fit_families": groups.len(), "out": out}));
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Requests { bundle, out, shards, sample_keep, sample_modulus } => {
            requests(&bundle, &out, shards, sample_keep, sample_modulus)
        }
        Command::Fit { plan, runs, out } => fit(&plan, &runs, &out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, split: &str, text: &str, group: &str, weight: f64) -> Value {
        json!({"id": id, "split": split, "text": text, "text_sha256": sha256_hex(text.as_bytes()),
               "source_group": group, "cohort_weight": weight})
    }

    /// A fake complete embedding run over one shard; embedding = f(id).
    fn fake_run(dir: &Path, requests: &Path, f: impl Fn(&str) -> Vec<f64>) {
        fs::create_dir_all(dir).unwrap();
        fs::copy(requests, dir.join("requests.jsonl")).unwrap();
        let mut rows = String::new();
        for line in fs::read_to_string(requests).unwrap().lines() {
            let r: Value = serde_json::from_str(line).unwrap();
            let id = r["id"].as_str().unwrap();
            rows.push_str(&format!("{}\n", json!({"id": id, "status": "ok", "embedding": f(id),
                "text_sha256": sha256_hex(r["text"].as_str().unwrap().as_bytes())})));
        }
        fs::write(dir.join("rows.jsonl"), rows).unwrap();
        fs::write(dir.join("manifest.json"), json!({"schema": "slop_ninja_v6_embeddings_run_v1",
            "status": "complete", "dimension": DIM, "unsupported": 0,
            "rows_sha256": sha256_file(&dir.join("rows.jsonl")).unwrap(),
            "requests_sha256": sha256_file(&dir.join("requests.jsonl")).unwrap(),
            "representation_contract_sha256": "c", "representation_contract": {"artifact_id": "v6"}}).to_string())
            .unwrap();
    }

    #[test]
    fn family_weighted_fit_over_train_rows_only() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let bundle = d.join("bundle");
        fs::create_dir(&bundle).unwrap();
        // Group a has 3 rows (value 0), group b has 1 row (value 4): family mean 2.
        let rows = [row("a1", "train", "x one", "a", 1.0), row("a2", "train", "x two", "a", 1.0),
                    row("a3", "train", "x three", "a", 1.0), row("b1", "train", "y one", "b", 0.5),
                    row("z", "train", "zero weight", "c", 0.0), row("d1", "dev_iter", "dev text", "d", 1.0),
                    row("c1", "cal", "cal text", "e", 1.0)];
        fs::write(bundle.join("rows.jsonl"), rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
        fs::write(bundle.join("weights.json"), "{}").unwrap();
        fs::write(bundle.join("MANIFEST.json"), "{}").unwrap();
        let out = d.join("req");
        requests(&bundle, &out, 2, 0, 0).unwrap();
        let plan = read(&out.join("plan.json")).unwrap();
        assert_eq!(plan["rows"], 4);
        let all: String = (0..2).map(|k| fs::read_to_string(out.join(format!("requests-{k:03}.jsonl"))).unwrap()).collect();
        assert!(!all.contains("dev text") && !all.contains("cal text") && !all.contains("zero weight"));
        let value = |id: &str| vec![if id.starts_with('b') { 4.0 } else { 0.0 }; DIM];
        fake_run(&d.join("r0"), &out.join("requests-000.jsonl"), value);
        fake_run(&d.join("r1"), &out.join("requests-001.jsonl"), value);
        let norm = d.join("norm.json");
        fit(&out.join("plan.json"), &[d.join("r0"), d.join("r1")], &norm).unwrap();
        let n = read(&norm).unwrap();
        assert_eq!((n["fit_rows"].as_u64(), n["fit_families"].as_u64()), (Some(4), Some(2)));
        assert!((n["mean"][0].as_f64().unwrap() - 2.0).abs() < 1e-12);
        assert!((n["scale"][5].as_f64().unwrap() - 2.0).abs() < 1e-12);
        assert_eq!(n["development_used_for_fit"], false);
        // A missing shard, a repeated shard and a changed row are refused.
        assert!(fit(&out.join("plan.json"), &[d.join("r0")], &d.join("n2")).is_err());
        assert!(fit(&out.join("plan.json"), &[d.join("r0"), d.join("r0")], &d.join("n3")).is_err());
        fs::write(d.join("r1/rows.jsonl"), "").unwrap();
        assert!(fit(&out.join("plan.json"), &[d.join("r0"), d.join("r1")], &d.join("n4")).is_err());
        // Test rows anywhere refuse the bundle.
        let mut bad = fs::read_to_string(bundle.join("rows.jsonl")).unwrap();
        bad.push_str(&format!("{}\n", row("t", "test", "test text", "t", 1.0)));
        fs::write(bundle.join("rows.jsonl"), bad).unwrap();
        assert!(requests(&bundle, &d.join("req2"), 1, 0, 0).is_err());
    }

    #[test]
    fn group_sampling_is_deterministic_and_keeps_whole_groups() {
        let kept: Vec<bool> = (0..200).map(|g| group_sampled(&format!("g{g}"), 1, 4)).collect();
        assert_eq!(kept, (0..200).map(|g| group_sampled(&format!("g{g}"), 1, 4)).collect::<Vec<_>>());
        let n = kept.iter().filter(|k| **k).count();
        assert!(20 < n && n < 80, "{n}");
        assert!(group_sampled("anything", 0, 0));
    }
}
