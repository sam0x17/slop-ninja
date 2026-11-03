//! Build a shared-contract A trainer bundle from the published, redistributable
//! dataset snapshots named by a public recipe (`slop_ninja_a_public_bundle_v1`).
//!
//! public_a_bundle --recipe RECIPE.json --repo REPO_ROOT --out NEW_DIR
//!
//! RECIPE is `grammar/crates/grammar-eval/python/public_a/recipe.json`. Its
//! `bundle.inputs` name decoded snapshot files (restore them first with
//! `tools/dataset-storage`), each with its SHA-256 and the trainer split it
//! feeds (`train`, `dev_iter` or `cal`). Test and reference files are never
//! named, so they are never opened. Rule `slop_ninja_a_public_bundle_v1`:
//!
//! - Each line is a `slop_ninja_a_supervised_record_v1` row whose
//!   `frozen_split` is the input's split (`split_field`). The row's exact
//!   text must hash to its `text_sha256`.
//! - Rows whose `purpose` is not `query` are excluded (`not_a_query`).
//! - A row needs a measured `share_target` (`share_unknown`) and, when AI is
//!   involved, one generator identity (`no_single_generator_identity`).
//! - The generator label is the first `generator_labels` entry whose
//!   `identity_contains` occurs in the identity (`unmapped_generator`
//!   otherwise). A Train label must be in the fixed AI roster
//!   (`train_label_outside_roster`); evaluation splits keep other labels, so
//!   CAL can measure rejection of generators outside the roster.
//! - The first copy of an exact text (input order) is kept; later copies are
//!   excluded (`duplicate_text`). A text or source group in two splits is
//!   refused.
//! - Every row has cohort `public:<snapshot>` and cohort weight 1; the
//!   protocol's `family_origin_balanced` weighting balances source groups
//!   and origin cells. `weights.json` declares that factor per cohort and
//!   label.
//!
//! Writes `rows.jsonl` (sorted by id), `weights.json`, `MANIFEST.json`
//! (inputs, counts, exclusions by reason, recipe and roster digests) and
//! `exclusions.jsonl` (id, input, reason). No text is printed.
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use grammar_a_baseline::mining::{sha256_file, sha256_hex};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const RULE: &str = "slop_ninja_a_public_bundle_v1";
const RECIPE_SCHEMA: &str = "slop_ninja_a_public_recipe_v1";
const RECORD_SCHEMA: &str = "slop_ninja_a_supervised_record_v1";
const SPLITS: [&str; 3] = ["train", "dev_iter", "cal"];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    recipe: PathBuf,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

struct Rule {
    contains: String,
    label: String,
}

struct Recipe {
    sha256: String,
    roster: BTreeSet<String>,
    roster_sha256: String,
    split_field: BTreeMap<String, String>,
    labels: Vec<Rule>,
    inputs: Vec<(String, String, String)>,
}

fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

fn recipe(path: &Path) -> Result<Recipe> {
    let r = read(path)?;
    ensure!(r["schema"] == RECIPE_SCHEMA && r["bundle"]["rule"] == RULE, "a {RECIPE_SCHEMA} recipe with rule {RULE}");
    let p = &r["pinned"];
    let roster_path = path.parent().context("recipe directory")?.join(p["ai_roster_file"].as_str().context("roster file")?);
    let names: Vec<String> = serde_json::from_value(read(&roster_path)?).context("roster names")?;
    ensure!(names.windows(2).all(|w| w[0] < w[1]), "roster must be sorted and unique");
    let roster_sha256 = sha256_hex(&serde_json::to_vec(&names)?);
    ensure!(Some(roster_sha256.as_str()) == p["ai_roster_sha256"].as_str(), "roster file differs from the recipe pin");
    let b = &r["bundle"];
    let split_field: BTreeMap<String, String> = serde_json::from_value(b["split_field"].clone()).context("split_field")?;
    ensure!(split_field.keys().map(String::as_str).eq(SPLITS.iter().copied().collect::<BTreeSet<_>>()),
            "split_field must name train, dev_iter and cal");
    let labels = b["generator_labels"].as_array().context("generator_labels")?.iter()
        .map(|g| Ok(Rule { contains: g["identity_contains"].as_str().context("identity_contains")?.to_owned(),
                           label: g["label"].as_str().context("label")?.to_owned() }))
        .collect::<Result<Vec<_>>>()?;
    ensure!(labels.iter().all(|g| !g.contains.is_empty() && !g.label.is_empty() && !g.label.starts_with("other_")),
            "generator label rule");
    let inputs = b["inputs"].as_array().context("inputs")?.iter()
        .map(|i| {
            let split = i["split"].as_str().context("input split")?.to_owned();
            ensure!(SPLITS.contains(&split.as_str()), "input split {split}");
            Ok((i["file"].as_str().context("input file")?.to_owned(),
                i["sha256"].as_str().context("input sha256")?.to_owned(), split))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!inputs.is_empty(), "no inputs");
    Ok(Recipe { sha256: sha256_file(path)?, roster: names.into_iter().collect(), roster_sha256, split_field, labels,
                inputs })
}

/// `datasets/<snapshot>/...` → `<snapshot>`.
fn snapshot(file: &str) -> Result<String> {
    let mut parts = file.split('/');
    ensure!(parts.next() == Some("datasets"), "input {file} is not under datasets/");
    Ok(parts.next().filter(|s| !s.is_empty()).with_context(|| format!("input {file}"))?.to_owned())
}

fn label_for(rules: &[Rule], identity: &str) -> Option<String> {
    rules.iter().find(|g| identity.contains(&g.contains)).map(|g| g.label.clone())
}

/// One bundle row, or the exclusion reason.
fn bundle_row(r: &Recipe, v: &Value, split: &str, cohort: &str, provenance: String)
              -> Result<std::result::Result<Value, &'static str>> {
    let id = v["id"].as_str().filter(|s| !s.is_empty()).context("row id")?;
    ensure!(v["schema"] == RECORD_SCHEMA, "row {id}: schema");
    ensure!(v["frozen_split"].as_str() == r.split_field.get(split).map(String::as_str),
            "row {id}: frozen_split {} in a {split} input", v["frozen_split"]);
    let text = v["text"].as_str().with_context(|| format!("row {id}: text"))?;
    ensure!(Some(sha256_hex(text.as_bytes()).as_str()) == v["text_sha256"].as_str(), "row {id}: text digest differs");
    ensure!(!text.trim().is_empty(), "row {id}: empty text");
    if v["purpose"] != "query" {
        return Ok(Err("not_a_query"));
    }
    let origin = v["origin"].as_str().with_context(|| format!("row {id}: origin"))?;
    ensure!(matches!(origin, "human_only" | "model_only" | "mixed"), "row {id}: origin {origin}");
    let Some(share) = v["share_target"].as_f64() else { return Ok(Err("share_unknown")) };
    let share_ok = match origin {
        "human_only" => share == 0.0,
        "model_only" => share == 1.0,
        _ => share > 0.0 && share <= 1.0,
    };
    ensure!(share_ok, "row {id}: share {share} for {origin}");
    let label = if origin == "human_only" {
        ensure!(v["ai_identity"].is_null(), "row {id}: human row with an AI identity");
        Value::Null
    } else {
        let Some(identity) = v["ai_identity"].as_str() else { return Ok(Err("no_single_generator_identity")) };
        let Some(label) = label_for(&r.labels, identity) else { return Ok(Err("unmapped_generator")) };
        if split == "train" && !r.roster.contains(&label) {
            return Ok(Err("train_label_outside_roster"));
        }
        Value::String(label)
    };
    let group = v["source_group"].as_str().filter(|s| !s.is_empty()).with_context(|| format!("row {id}: source_group"))?;
    let domain = v["metadata"]["source_collection"].as_str()
        .or_else(|| v["source"]["kind"].as_str())
        .unwrap_or_else(|| group.split(':').next().unwrap_or(group));
    Ok(Ok(json!({"id": id, "split": split, "text": text, "text_sha256": v["text_sha256"], "origin": origin,
        "share": share, "ai_byte_spans": null, "generator_label": label, "cohort_id": cohort, "cohort_weight": 1.0,
        "source_group": group, "domain": domain, "genre": null, "voice_id": null, "voice_cluster_id": null,
        "voice_origin": null, "style_axes": null, "operation": null, "held_out_style": false,
        "provenance_sha256": provenance})))
}

fn build(args: &Args) -> Result<Value> {
    ensure!(!args.out.exists(), "{} exists; the bundle needs a new directory", args.out.display());
    let r = recipe(&args.recipe)?;
    let mut rows: BTreeMap<String, Value> = BTreeMap::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    let mut groups: HashMap<String, String> = HashMap::new();
    let mut exclusions = Vec::new();
    let mut inputs = Vec::new();
    for (file, expected, split) in &r.inputs {
        let path = args.repo.join(file);
        let digest = sha256_file(&path).with_context(|| format!("{file}: restore the snapshot with tools/dataset-storage"))?;
        ensure!(&digest == expected, "{file}: SHA-256 {digest} differs from the recipe pin");
        let cohort = format!("public:{}", snapshot(file)?);
        let (mut kept, mut lines) = (0usize, 0usize);
        for line in BufReader::new(File::open(&path)?).split(b'\n') {
            let line = line?;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            lines += 1;
            let v: Value = serde_json::from_slice(&line).with_context(|| format!("{file} line {lines}"))?;
            let provenance = sha256_hex(&line);
            let row = match bundle_row(&r, &v, split, &cohort, provenance)? {
                Ok(row) => row,
                Err(reason) => {
                    exclusions.push(json!({"id": v["id"], "input": file, "reason": reason}));
                    continue;
                }
            };
            let id = row["id"].as_str().unwrap().to_owned();
            let digest = row["text_sha256"].as_str().unwrap().to_owned();
            if let Some(prev) = texts.get(&digest) {
                ensure!(prev == split, "row {id}: the same text is in {prev} and {split}");
                exclusions.push(json!({"id": id, "input": file, "reason": "duplicate_text"}));
                continue;
            }
            let group = row["source_group"].as_str().unwrap().to_owned();
            if let Some(prev) = groups.insert(group.clone(), split.clone()) {
                ensure!(&prev == split, "source group {group} is in {prev} and {split}");
            }
            ensure!(!rows.contains_key(&id), "duplicate row id {id}");
            texts.insert(digest, split.clone());
            rows.insert(id, row);
            kept += 1;
        }
        inputs.push(json!({"file": file, "sha256": digest, "split": split, "cohort_id": cohort, "rows": lines,
                           "kept": kept}));
    }
    let train_labels: BTreeSet<&str> = rows.values()
        .filter(|v| v["split"] == "train").filter_map(|v| v["generator_label"].as_str()).collect();
    ensure!(train_labels.len() >= 2, "Train needs at least two roster generator labels");
    for split in SPLITS {
        for origin in ["human_only", "model_only", "mixed"] {
            ensure!(rows.values().any(|v| v["split"] == split && v["origin"] == origin), "no {origin} rows in {split}");
        }
    }
    fs::create_dir_all(&args.out)?;
    let mut w = BufWriter::new(File::create_new(args.out.join("rows.jsonl"))?);
    for v in rows.values() {
        serde_json::to_writer(&mut w, v)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    drop(w);
    let mut w = BufWriter::new(File::create_new(args.out.join("exclusions.jsonl"))?);
    for v in &exclusions {
        serde_json::to_writer(&mut w, v)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    drop(w);
    // weights.json: one declared factor (1) per Train cohort and label.
    let mut cohorts: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for v in rows.values().filter(|v| v["split"] == "train") {
        let label = v["generator_label"].as_str().unwrap_or("human").to_owned();
        *cohorts.entry(v["cohort_id"].as_str().unwrap().to_owned()).or_default().entry(label).or_default() += 1;
    }
    let weights = json!({"schema": "slop_ninja_a_bundle_weights_v1", "base_weighting": "family_origin_balanced",
        "cohorts": cohorts.iter().map(|(c, labels)| (c.clone(), json!({"policy": "uniform_v1", "default": true,
            "labels": labels.iter().map(|(l, n)| (l.clone(), json!({"records": n, "weight": 1.0})))
                .collect::<serde_json::Map<_, _>>()}))).collect::<serde_json::Map<_, _>>()});
    fs::write(args.out.join("weights.json"), serde_json::to_vec_pretty(&weights)?)?;
    let mut counts: BTreeMap<(String, String, String, String), u64> = BTreeMap::new();
    for v in rows.values() {
        let key = (v["split"].as_str().unwrap().to_owned(), v["domain"].as_str().unwrap().to_owned(),
                   v["origin"].as_str().unwrap().to_owned(), v["generator_label"].as_str().unwrap_or("").to_owned());
        *counts.entry(key).or_default() += 1;
    }
    let mut reasons: BTreeMap<&str, u64> = BTreeMap::new();
    for e in &exclusions {
        *reasons.entry(e["reason"].as_str().unwrap()).or_default() += 1;
    }
    let mut by_split: BTreeMap<&str, u64> = BTreeMap::new();
    for v in rows.values() {
        *by_split.entry(v["split"].as_str().unwrap()).or_default() += 1;
    }
    let manifest = json!({"schema": "slop_ninja_a_public_bundle_manifest_v1", "rule": RULE,
        "recipe_sha256": r.sha256, "ai_roster_sha256": r.roster_sha256,
        "train_labels": train_labels, "rows": rows.len(), "rows_by_split": by_split,
        "source_groups": groups.len(), "inputs": inputs,
        "counts": counts.iter().map(|((s, d, o, g), n)| json!({"split": s, "domain": d, "origin": o,
            "generator_label": if g.is_empty() { Value::Null } else { json!(g) }, "rows": n})).collect::<Vec<_>>(),
        "exclusions": reasons, "exclusions_sha256": sha256_file(&args.out.join("exclusions.jsonl"))?,
        "test_or_reference_read": false, "weighting": "cohort weight 1; protocol family_origin_balanced",
        "implementation_sha256": sha256_hex(include_bytes!("public_a_bundle.rs"))});
    fs::write(args.out.join("MANIFEST.json"), serde_json::to_vec_pretty(&manifest)?)?;
    Ok(json!({"rows": rows.len(), "rows_by_split": by_split, "train_labels": manifest["train_labels"],
              "exclusions": manifest["exclusions"],
              "manifest_sha256": sha256_file(&args.out.join("MANIFEST.json"))?}))
}

fn main() -> Result<()> {
    let args = Args::parse();
    match build(&args) {
        Ok(summary) => {
            println!("{summary}");
            Ok(())
        }
        Err(e) => bail!("{e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe_for(roster: &[&str]) -> Recipe {
        Recipe { sha256: String::new(), roster: roster.iter().map(|s| s.to_string()).collect(),
                 roster_sha256: String::new(),
                 split_field: [("train", "train"), ("dev_iter", "development"), ("cal", "calibration")]
                     .iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
                 labels: vec![Rule { contains: "Gen-A-7B".into(), label: "gen-a".into() },
                              Rule { contains: "Gen-B".into(), label: "gen-b".into() }],
                 inputs: vec![] }
    }

    fn record(id: &str, split: &str, origin: &str, share: Value, identity: Value) -> Value {
        let text = format!("Synthetic passage {id}.");
        json!({"schema": RECORD_SCHEMA, "id": id, "frozen_split": split, "purpose": "query", "origin": origin,
               "share_target": share, "ai_identity": identity, "source_group": "work:1", "text": text,
               "text_sha256": sha256_hex(text.as_bytes()), "metadata": {"source_collection": "synthetic"}})
    }

    #[test]
    fn rows_follow_the_rule_and_exclusions_name_their_reason() {
        let r = recipe_for(&["gen-a", "gen-z"]);
        let row = |v: &Value, split: &str| bundle_row(&r, v, split, "public:x", "p".into()).unwrap();
        let ok = row(&record("h", "train", "human_only", json!(0.0), Value::Null), "train").unwrap();
        assert_eq!((ok["share"].as_f64(), ok["generator_label"].clone(), ok["domain"].as_str()),
                   (Some(0.0), Value::Null, Some("synthetic")));
        let ok = row(&record("m", "train", "mixed", json!(0.4), json!("org/Gen-A-7B:abc")), "train").unwrap();
        assert_eq!(ok["generator_label"], "gen-a");
        assert_eq!(row(&record("b", "train", "model_only", json!(1.0), json!("Gen-B")), "train"),
                   Err("train_label_outside_roster"));
        assert_eq!(row(&record("b", "calibration", "model_only", json!(1.0), json!("Gen-B")), "cal").unwrap()
                   ["generator_label"], "gen-b");
        assert_eq!(row(&record("u", "train", "mixed", Value::Null, json!("Gen-A-7B")), "train"), Err("share_unknown"));
        assert_eq!(row(&record("n", "train", "model_only", json!(1.0), Value::Null), "train"),
                   Err("no_single_generator_identity"));
        assert_eq!(row(&record("q", "train", "model_only", json!(1.0), json!("Other")), "train"),
                   Err("unmapped_generator"));
        let mut reference = record("r", "train", "human_only", json!(0.0), Value::Null);
        reference["purpose"] = json!("reference");
        assert_eq!(row(&reference, "train"), Err("not_a_query"));
        // Refusals: wrong split, a changed text, an impossible share.
        assert!(bundle_row(&r, &record("s", "development", "human_only", json!(0.0), Value::Null), "train", "c",
                           "p".into()).is_err());
        let mut changed = record("t", "train", "human_only", json!(0.0), Value::Null);
        changed["text"] = json!("Another passage.");
        assert!(bundle_row(&r, &changed, "train", "c", "p".into()).is_err());
        assert!(bundle_row(&r, &record("v", "train", "model_only", json!(0.5), json!("Gen-A-7B")), "train", "c",
                           "p".into()).is_err());
    }
}
