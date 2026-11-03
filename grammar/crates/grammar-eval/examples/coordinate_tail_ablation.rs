//! Conditional removal of unselected coordinate tails from three frozen metrics.
//! No fitting, normalization changes, new profiles, or parser calls.
#[path = "../src/coordinate_inference.rs"]
#[allow(dead_code)] // The frozen module also exposes profile scoring, unused by this array replay.
mod coordinate_inference;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use coordinate_inference::{Artifact, Metric};
use grammar_core::features::Features;
use grammar_eval::{
    DistanceGeometry, Profile, RankedAuthor, document_values, family_distances, rank,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

const BASE: &str = "data/author-corpora/blog-authorship-2004";
const GV: &str = "data/author-corpora/global-voices-v1/transfer-v1";
const MODELS: [(u32, &str); 3] = [
    (
        0,
        "c19c5015dbf2b091d3ec774a7b108a6f41bc88e93213bc4ac45e787a35e2b916",
    ),
    (
        17,
        "c8abdd2a30a770eaebdb16834d9cdb448e44896680cdd24e022660d299516f85",
    ),
    (
        29,
        "d7bbb67714beb04bf445612bafcad6a8ee7d4413934e1bb375e37f59e3a863d1",
    ),
];
const TOL: f64 = 1e-10;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Freeze {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Run {
        #[arg(long)]
        protocol: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .with_context(|| format!("missing {name}"))
}
fn model_path(seed: u32) -> String {
    format!("{BASE}/scale-coordinate-inference-v1/models-treatment-v1/metric-seed-{seed}.json")
}
fn implementation() -> Result<Value> {
    Ok(
        json!({"source_sha256": hash(include_bytes!("coordinate_tail_ablation.rs")),
        "coordinate_inference_sha256": hash(include_bytes!("../src/coordinate_inference.rs")),
        "grammar_eval_sha256": hash(include_bytes!("../src/lib.rs")),
        "binary_sha256": hash(&fs::read(std::env::current_exe()?)?)}),
    )
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("output parent")?)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    Ok(())
}
fn freeze_file(
    repo: &Path,
    path: &Path,
    expected: Option<&str>,
    files: &mut BTreeMap<String, String>,
) -> Result<()> {
    let path = repo.join(path).canonicalize()?;
    ensure!(path.starts_with(repo), "input outside repository");
    let digest = hash(&fs::read(&path)?);
    if let Some(expected) = expected {
        ensure!(
            digest == expected,
            "bound input changed: {}",
            path.display()
        );
    }
    files.insert(path.to_str().context("UTF-8 input path")?.into(), digest);
    Ok(())
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let repo = repo.canonicalize()?;
    let mut files = BTreeMap::new();
    for (seed, digest) in MODELS {
        freeze_file(
            &repo,
            Path::new(&model_path(seed)),
            Some(digest),
            &mut files,
        )?;
    }
    for i in 1..=6 {
        let export = repo.join(format!(
            "{BASE}/training-author-scale-v1/coordinates-test-{i}-v1"
        ));
        freeze_file(&repo, &export.join("manifest.json"), None, &mut files)?;
        let manifest = read_json(&export.join("manifest.json"))?;
        for (name, key) in [
            ("catalog.json", "catalog_sha256"),
            ("queries.json", "queries_sha256"),
        ] {
            freeze_file(
                &repo,
                &export.join(name),
                Some(field(&manifest, key)?),
                &mut files,
            )?;
        }
        for entry in manifest["arrays"].as_object().context("arrays")?.values() {
            freeze_file(
                &repo,
                &export.join(field(entry, "path")?),
                Some(field(entry, "sha256")?),
                &mut files,
            )?;
        }
        for (seed, _) in MODELS {
            let replay = repo.join(format!(
                "{BASE}/scale-coordinate-inference-v1/replay-seed-{seed}-test-{i}-v1"
            ));
            freeze_file(&repo, &replay.join("manifest.json"), None, &mut files)?;
            let receipt = read_json(&replay.join("manifest.json"))?;
            freeze_file(
                &repo,
                &replay.join("queries.json"),
                Some(field(&receipt, "queries_sha256")?),
                &mut files,
            )?;
            freeze_file(
                &repo,
                &replay.join(field(&receipt["logits"], "path")?),
                Some(field(&receipt["logits"], "sha256")?),
                &mut files,
            )?;
        }
    }
    let gv = repo.join(GV);
    freeze_file(&repo, &gv.join("report.json"), None, &mut files)?;
    let report = read_json(&gv.join("report.json"))?;
    for entry in report["artifacts"]
        .as_object()
        .context("GV artifacts")?
        .values()
        .chain(std::iter::once(&report["plan"]))
    {
        freeze_file(
            &repo,
            Path::new(field(entry, "path")?),
            Some(field(entry, "sha256")?),
            &mut files,
        )?;
    }
    let articles = read_json(&gv.join("articles.json"))?;
    for article in articles
        .as_array()
        .context("GV articles")?
        .iter()
        .filter(|a| a["split"] == "query")
    {
        let entry = &article["features"];
        freeze_file(
            &repo,
            Path::new(field(entry, "path")?),
            Some(field(entry, "sha256")?),
            &mut files,
        )?;
    }
    let protocol = json!({"schema":"slopninja-coordinate-tail-ablation-protocol-v1", "status":"frozen", "repo":repo,
        "implementation": implementation()?, "files":files, "seeds":[0,17,29],
        "scope":{"blog_test_galleries":6,"blog_authors":600,"blog_queries":1206,"global_voices_authors":14,"global_voices_queries":58},
        "policy":{"variants":["full","selected_coordinates_only"],"coordinates":3557,
            "selected_raw":"sum exact squared original transformed-coordinate differences within each original family, in artifact order",
            "frozen":"original raw-profile date/post pooling, coordinate multipliers, family weights, spline and temperature; no renormalization",
            "baseline_gate":"all full logits match bound saved baseline within tolerance and all candidate ordering/ties match",
            "absolute_tolerance":TOL,"missingness":"fail closed on any missing input, family, candidate, query, nonfinite score or baseline mismatch; no dropping",
            "metrics":"exact ties averaged; query means within author, equal authors within corpus, then mean three seed summaries; galleries retained separately",
            "interpretation":"post-hoc conditional tail-removal diagnostic; does not isolate prototype pooling, normalization, cosine scoring or projection capacity",
            "training_calls":0,"parser_calls":0}});
    write_json(out, &protocol)?;
    println!(
        "Frozen protocol {} sha256={}",
        out.display(),
        hash(&fs::read(out)?)
    );
    Ok(())
}

struct Frozen {
    repo: PathBuf,
    files: BTreeMap<String, String>,
}
impl Frozen {
    fn bytes(&self, path: impl AsRef<Path>) -> Result<Vec<u8>> {
        let path = self.repo.join(path).canonicalize()?;
        let expected = self
            .files
            .get(path.to_str().context("UTF-8 path")?)
            .context("unbound input")?;
        let bytes = fs::read(path)?;
        ensure!(hash(&bytes) == *expected, "frozen input changed");
        Ok(bytes)
    }
    fn json(&self, path: impl AsRef<Path>) -> Result<Value> {
        Ok(serde_json::from_slice(&self.bytes(path)?)?)
    }
    fn array(&self, directory: &Path, entry: &Value, shape: &[usize]) -> Result<Vec<f64>> {
        ensure!(
            entry["shape"] == json!(shape) && entry["dtype"] == "float64-le",
            "array shape/dtype mismatch"
        );
        let bytes = self.bytes(directory.join(field(entry, "path")?))?;
        ensure!(
            hash(&bytes) == field(entry, "sha256")?
                && bytes.len() == shape.iter().product::<usize>() * 8,
            "array size/hash mismatch"
        );
        let values: Vec<_> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| f64::from_le_bytes(*b))
            .collect();
        ensure!(
            values.iter().all(|v| v.is_finite()),
            "nonfinite input array"
        );
        Ok(values)
    }
}

struct Gallery {
    corpus: &'static str,
    name: String,
    authors: Vec<String>,
    queries: Vec<Value>,
    query: Vec<f64>,
    target: Vec<f64>,
    raw: Vec<f64>,
    baseline: BTreeMap<u32, Vec<f64>>,
}
fn gather(values: &[f64], width: usize, indices: &[usize]) -> Vec<f64> {
    values
        .chunks_exact(width)
        .flat_map(|row| indices.iter().map(move |i| row[*i]))
        .collect()
}
fn blog(frozen: &Frozen, i: usize, metric: &Metric) -> Result<Gallery> {
    let directory = PathBuf::from(format!(
        "{BASE}/training-author-scale-v1/coordinates-test-{i}-v1"
    ));
    let manifest_bytes = frozen.bytes(directory.join("manifest.json"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let artifact = metric.artifact();
    ensure!(
        manifest["schema"] == "slopninja-coordinate-frontier-export-v1"
            && manifest["split"] == "test",
        "Blog export identity"
    );
    for (key, value) in [
        ("source_space_id", &artifact.source_space_id),
        ("feature_schema", &artifact.feature_schema),
        ("parser_identity", &artifact.parser_identity),
    ] {
        ensure!(
            manifest[key] == *value,
            "Blog model identity differs: {key}"
        );
    }
    let catalog_bytes = frozen.bytes(directory.join("catalog.json"))?;
    ensure!(
        manifest["catalog_sha256"] == hash(&catalog_bytes)
            && artifact.provenance_sha256["catalog"] == hash(&catalog_bytes),
        "catalog hash differs"
    );
    let catalog: Value = serde_json::from_slice(&catalog_bytes)?;
    let indices: Vec<usize> =
        serde_json::from_value(catalog["subsets"]["all_m2"]["max_column_indices"].clone())?;
    let coordinates = catalog["coordinates"]
        .as_array()
        .context("maximal coordinates")?;
    ensure!(
        indices.len() == 3557
            && coordinates.len() == 7353
            && indices.windows(2).all(|w| w[0] < w[1]),
        "coordinate subset shape/order"
    );
    for (index, axis) in indices.iter().zip(&artifact.coordinates) {
        let row = coordinates
            .get(*index)
            .context("subset index out of bounds")?;
        let expected = serde_json::to_value(axis)?;
        for key in [
            "family_index",
            "family",
            "feature",
            "kind",
            "scale",
            "original_family_axis_count",
        ] {
            ensure!(
                row[key] == expected[key],
                "selected coordinate geometry differs"
            );
        }
    }
    let authors: Vec<String> = serde_json::from_value(manifest["authors"].clone())?;
    let query_bytes = frozen.bytes(directory.join("queries.json"))?;
    ensure!(
        manifest["queries_sha256"] == hash(&query_bytes),
        "query hash differs"
    );
    let queries: Vec<Value> = serde_json::from_slice(&query_bytes)?;
    let q = queries.len();
    ensure!(
        authors.len() == 100 && manifest["query_count"] == q,
        "Blog coverage differs"
    );
    let query_max = frozen.array(
        &directory,
        &manifest["arrays"]["query_coordinates"],
        &[q, 7353],
    )?;
    let target_max = frozen.array(
        &directory,
        &manifest["arrays"]["author_coordinates"],
        &[100, 7353],
    )?;
    let raw = frozen.array(
        &directory,
        &manifest["arrays"]["family_distances"],
        &[q, 100, 14],
    )?;
    let mut baseline = BTreeMap::new();
    for (seed, model_hash) in MODELS {
        let replay = PathBuf::from(format!(
            "{BASE}/scale-coordinate-inference-v1/replay-seed-{seed}-test-{i}-v1"
        ));
        let receipt = frozen.json(replay.join("manifest.json"))?;
        ensure!(
            receipt["schema"] == "slopninja-subset-coordinate-replay-v1"
                && receipt["model_sha256"] == model_hash
                && receipt["export_manifest_sha256"] == hash(&manifest_bytes)
                && receipt["authors"] == manifest["authors"]
                && receipt["queries_sha256"] == hash(&query_bytes)
                && frozen.bytes(replay.join("queries.json"))? == query_bytes,
            "saved Blog baseline binding differs"
        );
        baseline.insert(seed, frozen.array(&replay, &receipt["logits"], &[q, 100])?);
    }
    Ok(Gallery {
        corpus: "blog",
        name: format!("blog-test-{i}"),
        authors,
        queries,
        query: gather(&query_max, 7353, &indices),
        target: gather(&target_max, 7353, &indices),
        raw,
        baseline,
    })
}

fn global_voices(frozen: &Frozen, metric: &Metric) -> Result<Gallery> {
    let directory = PathBuf::from(GV);
    let report = frozen.json(directory.join("report.json"))?;
    let plan = frozen.json(directory.join("plan.json"))?;
    ensure!(
        report["schema"] == "slopninja-author-transfer-result-v1"
            && report["plan"]["sha256"] == hash(&frozen.bytes(directory.join("plan.json"))?),
        "GV report identity"
    );
    for (seed, digest) in MODELS {
        let models = plan["models"].as_array().context("GV models")?;
        ensure!(
            models
                .iter()
                .filter(|m| m["seed"] == seed && m["artifact"]["sha256"] == digest)
                .count()
                == 1,
            "GV frozen model differs"
        );
    }
    let targets: BTreeMap<String, Profile> = serde_json::from_value(
        frozen.json(directory.join("reference-profiles.json"))?["full"].clone(),
    )?;
    let authors: Vec<_> = targets.keys().cloned().collect();
    let saved: Vec<Value> = serde_json::from_value(frozen.json(directory.join("queries.json"))?)?;
    let articles: Vec<Value> =
        serde_json::from_value(frozen.json(directory.join("articles.json"))?)?;
    ensure!(
        authors.len() == 14 && saved.len() == 58,
        "GV fixed coverage differs"
    );
    let artifact = metric.artifact();
    let geometry = DistanceGeometry {
        families: artifact
            .families
            .iter()
            .map(|f| (f.name.clone(), f.kind.clone()))
            .collect(),
        numerical_scales: artifact
            .families
            .iter()
            .filter(|f| !f.numerical_axes.is_empty())
            .map(|f| {
                (
                    f.name.clone(),
                    f.numerical_axes
                        .iter()
                        .map(|a| (a.feature.clone(), a.scale))
                        .collect(),
                )
            })
            .collect(),
    };
    let mut query = Vec::new();
    let mut target = Vec::new();
    for profile in targets.values() {
        target.extend(metric.transform_profile(profile)?);
    }
    let mut queries = Vec::new();
    let mut raw = Vec::new();
    let mut baseline: BTreeMap<u32, Vec<f64>> =
        MODELS.iter().map(|(s, _)| (*s, Vec::new())).collect();
    for row in &saved {
        ensure!(
            row["full"]["status"] == "scored" && row["full"]["gallery_size"] == 14,
            "unavailable GV baseline"
        );
        let matches: Vec<_> = articles
            .iter()
            .filter(|a| a["id"] == row["id"] && a["split"] == "query")
            .collect();
        ensure!(matches.len() == 1, "GV article identity ambiguity");
        let article = matches[0];
        for key in ["author_id", "date", "source_group", "text_sha256"] {
            ensure!(article[key] == row[key], "GV query metadata differs");
        }
        ensure!(
            article["status"] == "available"
                && article["annotated_text_sha256"] == row["text_sha256"],
            "GV article unavailable"
        );
        let bytes = frozen.bytes(field(&article["features"], "path")?)?;
        ensure!(
            article["features"]["sha256"] == hash(&bytes),
            "GV feature hash differs"
        );
        let features: Features = serde_json::from_slice(&bytes)?;
        ensure!(
            features.schema == artifact.feature_schema
                && features.parser_identity == artifact.parser_identity
                && row["text_sha256"] == features.source_sha256,
            "GV feature identity differs"
        );
        let profile = document_values(&features)?;
        query.extend(metric.transform_profile(&profile)?);
        for target in targets.values() {
            let distances = family_distances(&geometry, &profile, target)?;
            raw.extend(artifact.families.iter().map(|f| distances[&f.name]));
        }
        let own = authors
            .binary_search(&field(row, "author_id")?.to_owned())
            .map_err(|_| anyhow::anyhow!("GV true author missing"))?;
        queries.push(json!({"id":row["id"],"author":row["author_id"],"date":row["date"],"source_group":row["source_group"],"text_sha256":row["text_sha256"],"own":own,"split":"test"}));
        let seeds = row["full"]["seeds"].as_array().context("GV seeds")?;
        ensure!(seeds.len() == 3, "GV seed count differs");
        for (seed, _) in MODELS {
            let matches: Vec<_> = seeds.iter().filter(|s| s["seed"] == seed).collect();
            ensure!(matches.len() == 1, "GV seed missing or duplicated");
            let scores = matches[0]["scores"].as_array().context("GV scores")?;
            ensure!(scores.len() == authors.len(), "GV candidate count differs");
            for author in &authors {
                let matches: Vec<_> = scores
                    .iter()
                    .filter(|s| s["author_id"] == *author)
                    .collect();
                ensure!(matches.len() == 1, "GV candidate missing or duplicated");
                baseline.get_mut(&seed).unwrap().push(
                    matches[0]["score"]["logit"]
                        .as_f64()
                        .context("GV baseline logit")?,
                );
            }
        }
    }
    Ok(Gallery {
        corpus: "global_voices",
        name: "global-voices".into(),
        authors,
        queries,
        query,
        target,
        raw,
        baseline,
    })
}

#[derive(Clone, Copy, Serialize)]
struct Measures {
    top1: f64,
    top5: f64,
    mrr: f64,
}
fn measures(ranking: &[RankedAuthor], own: &str) -> Result<Measures> {
    let r = ranking
        .iter()
        .find(|r| r.author_id == own)
        .context("true author missing")?;
    let ties = (r.rank_max - r.rank_min + 1) as f64;
    let top = |k: usize| (k.min(r.rank_max) + 1).saturating_sub(r.rank_min) as f64 / ties;
    Ok(Measures {
        top1: top(1),
        top5: top(5),
        mrr: (r.rank_min..=r.rank_max)
            .map(|r| 1.0 / r as f64)
            .sum::<f64>()
            / ties,
    })
}
fn mean(rows: &[Measures]) -> Measures {
    Measures {
        top1: rows.iter().map(|r| r.top1).sum::<f64>() / rows.len() as f64,
        top5: rows.iter().map(|r| r.top5).sum::<f64>() / rows.len() as f64,
        mrr: rows.iter().map(|r| r.mrr).sum::<f64>() / rows.len() as f64,
    }
}
fn summarize(rows: &[(String, Measures)]) -> Value {
    let mut authors: BTreeMap<&str, Vec<Measures>> = BTreeMap::new();
    for (author, value) in rows {
        authors.entry(author).or_default().push(*value);
    }
    let means: Vec<_> = authors.values().map(|r| mean(r)).collect();
    json!({"query_count":rows.len(),"author_count":authors.len(),"macro_author":mean(&means),
        "per_author":authors.iter().map(|(a,r)|json!({"author":a,"query_count":r.len(),"metrics":mean(r)})).collect::<Vec<_>>()})
}

fn run(protocol_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "fresh output directory required");
    let protocol_bytes = fs::read(protocol_path)?;
    let protocol: Value = serde_json::from_slice(&protocol_bytes)?;
    ensure!(
        protocol["schema"] == "slopninja-coordinate-tail-ablation-protocol-v1"
            && protocol["status"] == "frozen"
            && protocol["implementation"] == implementation()?,
        "protocol/source/binary identity differs"
    );
    let frozen = Frozen {
        repo: PathBuf::from(field(&protocol, "repo")?),
        files: serde_json::from_value(protocol["files"].clone())?,
    };
    let mut models = BTreeMap::new();
    for (seed, digest) in MODELS {
        let bytes = frozen.bytes(model_path(seed))?;
        ensure!(hash(&bytes) == digest, "frozen model differs");
        let artifact: Artifact = serde_json::from_slice(&bytes)?;
        ensure!(
            artifact.coordinates.len() == 3557 && artifact.families.len() == 14,
            "frozen model shape"
        );
        models.insert(seed, Metric::new(artifact)?);
    }
    let first = models.get(&0).context("seed zero")?;
    for model in models.values() {
        for (a, b) in model
            .artifact()
            .coordinates
            .iter()
            .zip(&first.artifact().coordinates)
        {
            ensure!(
                a.family_index == b.family_index
                    && a.family == b.family
                    && a.feature == b.feature
                    && a.kind == b.kind
                    && a.scale == b.scale
                    && a.original_family_axis_count == b.original_family_axis_count,
                "cross-seed geometry differs"
            );
        }
    }
    let mut galleries = (1..=6)
        .map(|i| blog(&frozen, i, first))
        .collect::<Result<Vec<_>>>()?;
    galleries.push(global_voices(&frozen, first)?);
    ensure!(
        galleries
            .iter()
            .filter(|g| g.corpus == "blog")
            .map(|g| g.queries.len())
            .sum::<usize>()
            == 1206,
        "Blog query scope differs"
    );
    let mut seen_authors: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut seen_ids = BTreeSet::new();
    let mut seen_texts = BTreeSet::new();
    for gallery in &galleries {
        ensure!(
            gallery.authors.windows(2).all(|w| w[0] < w[1]),
            "author ordering differs"
        );
        for author in &gallery.authors {
            ensure!(
                seen_authors
                    .entry(gallery.corpus)
                    .or_default()
                    .insert(author.clone()),
                "author repeated across galleries"
            );
        }
        let mut query_authors = BTreeSet::new();
        for q in &gallery.queries {
            let own = q["own"].as_u64().context("query label")? as usize;
            ensure!(
                own < gallery.authors.len()
                    && q["author"] == gallery.authors[own]
                    && q["split"] == "test",
                "query metadata/label differs"
            );
            ensure!(
                seen_ids.insert(field(q, "id")?.to_owned())
                    && seen_texts.insert(field(q, "text_sha256")?.to_owned()),
                "duplicate query source"
            );
            query_authors.insert(field(q, "author")?.to_owned());
        }
        ensure!(
            query_authors == gallery.authors.iter().cloned().collect(),
            "gallery contains author without query; stop rather than change denominator"
        );
    }
    fs::create_dir_all(out)?;
    let mut gallery_reports = Vec::new();
    let mut pooled: BTreeMap<(String, u32, String), Vec<(String, Measures)>> = BTreeMap::new();
    let mut maximum_error = 0.0_f64;
    let mut comparisons = 0;
    for gallery in &galleries {
        for (seed, metric) in &models {
            let path = out.join(format!("{}-seed-{seed}.jsonl", gallery.name));
            let mut output = BufWriter::new(
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?,
            );
            let mut full_rows = Vec::new();
            let mut selected_rows = Vec::new();
            let mut raw_sum = [0.0; 14];
            let mut selected_sum = [0.0; 14];
            let mut local_error = 0.0_f64;
            let mut ranking_changes = 0;
            let mut local_tail_max = 0.0_f64;
            let a = gallery.authors.len();
            for (i, query) in gallery.queries.iter().enumerate() {
                let q = &gallery.query[i * 3557..(i + 1) * 3557];
                let mut full_scores = Vec::new();
                let mut selected_scores = Vec::new();
                let mut expected_scores = Vec::new();
                for (j, author) in gallery.authors.iter().enumerate() {
                    let p = &gallery.target[j * 3557..(j + 1) * 3557];
                    let raw = &gallery.raw[(i * a + j) * 14..(i * a + j + 1) * 14];
                    let mut selected = [0.0; 14];
                    for (k, coordinate) in metric.artifact().coordinates.iter().enumerate() {
                        selected[coordinate.family_index] += (q[k] - p[k]).powi(2);
                    }
                    let full = metric.score_pair(raw, q, p)?;
                    let truncated = metric.score_pair(&selected, q, p)?;
                    ensure!(
                        truncated.minimum_unselected_tail == 0.0,
                        "selected-only input retained a tail"
                    );
                    let expected = gallery.baseline[seed][i * a + j];
                    ensure!(expected.is_finite(), "nonfinite saved baseline");
                    let error = (full.logit - expected).abs();
                    ensure!(error <= TOL, "full baseline differs beyond tolerance");
                    local_error = local_error.max(error);
                    comparisons += 1;
                    full_scores.push((author.clone(), -full.logit));
                    selected_scores.push((author.clone(), -truncated.logit));
                    expected_scores.push((author.clone(), -expected));
                    for f in 0..14 {
                        raw_sum[f] += raw[f];
                        selected_sum[f] += selected[f];
                        local_tail_max = local_tail_max.max((raw[f] - selected[f]).abs());
                    }
                }
                let full = rank(full_scores)?;
                let selected = rank(selected_scores)?;
                let expected = rank(expected_scores)?;
                let signature = |r: &[RankedAuthor]| {
                    r.iter()
                        .map(|r| (r.author_id.clone(), r.rank_min, r.rank_max))
                        .collect::<Vec<_>>()
                };
                ensure!(
                    signature(&full) == signature(&expected),
                    "full candidate ordering or ties differ from saved baseline"
                );
                ranking_changes += usize::from(signature(&full) != signature(&selected));
                let own = field(query, "author")?;
                let fm = measures(&full, own)?;
                let sm = measures(&selected, own)?;
                full_rows.push((own.to_owned(), fm));
                selected_rows.push((own.to_owned(), sm));
                let row = json!({"query":query,"gallery_size":a,"seed":seed,
                    "full":{"ranking_by_negative_logit":full,"metrics":fm},
                    "selected_coordinates_only":{"ranking_by_negative_logit":selected,"metrics":sm}});
                serde_json::to_writer(&mut output, &row)?;
                output.write_all(b"\n")?;
            }
            output.flush()?;
            maximum_error = maximum_error.max(local_error);
            let report = json!({"corpus":gallery.corpus,"gallery":gallery.name,"seed":seed,"candidate_authors":a,
                "full":summarize(&full_rows),"selected_coordinates_only":summarize(&selected_rows),
                "baseline_maximum_absolute_error":local_error,"changed_query_rankings":ranking_changes,
                "maximum_unselected_family_distance":local_tail_max,
                "family_distance_sums":metric.artifact().families.iter().enumerate().map(|(f,family)|json!({"family":family.name,"full_raw_sum":raw_sum[f],"selected_raw_sum":selected_sum[f],"removed_raw_sum":raw_sum[f]-selected_sum[f],"removed_fraction":if raw_sum[f]>0.0 {Some((raw_sum[f]-selected_sum[f])/raw_sum[f])}else{None}})).collect::<Vec<_>>(),
                "queries":{"path":path.file_name().and_then(|p|p.to_str()).context("UTF-8 output name")?,"sha256":hash(&fs::read(&path)?)}});
            println!(
                "{} seed={seed} full_top1={:.8} selected_top1={:.8} full_error={local_error:.3e}",
                gallery.name,
                report["full"]["macro_author"]["top1"]
                    .as_f64()
                    .context("full macro top1")?,
                report["selected_coordinates_only"]["macro_author"]["top1"]
                    .as_f64()
                    .context("selected macro top1")?
            );
            gallery_reports.push(report);
            pooled
                .entry((gallery.corpus.into(), *seed, "full".into()))
                .or_default()
                .extend(full_rows);
            pooled
                .entry((
                    gallery.corpus.into(),
                    *seed,
                    "selected_coordinates_only".into(),
                ))
                .or_default()
                .extend(selected_rows);
        }
    }
    let mut corpora = Vec::new();
    for corpus in ["blog", "global_voices"] {
        let mut variants = BTreeMap::new();
        for variant in ["full", "selected_coordinates_only"] {
            let mut summaries = Vec::new();
            let mut means = Vec::new();
            for (seed, _) in MODELS {
                let summary = summarize(&pooled[&(corpus.into(), seed, variant.into())]);
                means.push(Measures {
                    top1: summary["macro_author"]["top1"].as_f64().unwrap(),
                    top5: summary["macro_author"]["top5"].as_f64().unwrap(),
                    mrr: summary["macro_author"]["mrr"].as_f64().unwrap(),
                });
                summaries.push(json!({"seed":seed,"summary":summary}));
            }
            variants.insert(
                variant,
                json!({"seeds":summaries,"seed_mean_macro_author":mean(&means)}),
            );
        }
        corpora.push(
            json!({"corpus":corpus,"authors":seen_authors[corpus].len(),"variants":variants}),
        );
    }
    let report = json!({"schema":"slopninja-coordinate-tail-ablation-result-v1","protocol_sha256":hash(&protocol_bytes),"implementation":implementation()?,
        "baseline_checked_candidate_scores":comparisons,"baseline_maximum_absolute_error":maximum_error,"all_full_orderings_and_ties_match":true,
        "corpora":corpora,"galleries":gallery_reports,"missing_or_excluded_queries":0,"training_calls":0,"parser_calls":0,
        "interpretation":"conditional diagnostic of removing unselected feature tails under frozen raw-profile pooling/calibration; no isolation of pooling, cosine normalization, projection architecture, or alternative retraining"});
    write_json(&out.join("report.json"), &report)?;
    println!(
        "Completed {} sha256={}",
        out.join("report.json").display(),
        hash(&fs::read(out.join("report.json"))?)
    );
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::Freeze { repo, out } => freeze(&repo, &out),
        Command::Run { protocol, out } => run(&protocol, &out),
    }
}
