//! New-corpus retrieval with the three frozen Blog author metrics; no fitting.
//! Run from grammar/: cargo run --release -p grammar-eval --example author_transfer --
//! --input INPUT.jsonl --models MODEL0 --models MODEL17 --models MODEL29 --out OUT
#[path = "../src/coordinate_inference.rs"]
mod coordinate_inference;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use coordinate_inference::{Artifact, Metric};
use grammar_core::{
    features::{self, Features},
    syntax::{self, Document},
};
use grammar_eval::{Profile, RankedAuthor, document_values, family_values, grouped_profile, rank};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

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
const BATCH_SIZE: usize = 32;
const VARIANTS: [&str; 3] = ["full", "lexical_family_only", "grammar_family_only"];
const DECOMPOSITION_TOLERANCE: f64 = 1e-10;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    /// Repeat for the three exact frozen seed-0/17/29 portable JSON files.
    #[arg(long, required = true, action = clap::ArgAction::Append)]
    models: Vec<PathBuf>,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = default_python())]
    python: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Input {
    id: String,
    author_id: String,
    source_group: String,
    date: String,
    text: String,
    text_sha256: String,
    split: String,
    provenance: Value,
}

#[derive(Clone, Copy, Serialize)]
struct Measures {
    top1: f64,
    top5: f64,
    mrr: f64,
}

fn default_python() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.venv/bin/python")
        .to_string_lossy()
        .into_owned()
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn binding(path: &Path) -> Result<Value> {
    Ok(json!({"path": path.canonicalize()?, "sha256": sha(&fs::read(path)?)}))
}

// Existing files may be reused after an interrupted run, never silently replaced.
fn save(path: &Path, value: &impl Serialize) -> Result<Value> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if path.exists() {
        ensure!(
            fs::read(path)? == bytes,
            "existing artifact differs: {}",
            path.display()
        );
    } else {
        fs::create_dir_all(path.parent().context("artifact has no parent")?)?;
        fs::write(path, &bytes)?;
    }
    Ok(json!({"path": path.canonicalize()?, "sha256": sha(&bytes)}))
}

fn load_input(bytes: &[u8]) -> Result<Vec<Input>> {
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    let mut exact = BTreeSet::new();
    let mut normalized = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut dates = BTreeMap::new();
    for (line, text) in std::str::from_utf8(bytes)?.lines().enumerate() {
        ensure!(!text.trim().is_empty(), "empty input row {}", line + 1);
        let row: Input =
            serde_json::from_str(text).with_context(|| format!("input row {}", line + 1))?;
        ensure!(
            !row.id.is_empty() && !row.author_id.is_empty() && !row.source_group.is_empty(),
            "missing input identity at row {}",
            line + 1
        );
        ensure!(
            !row.text.trim().is_empty()
                && row.provenance.is_object()
                && row.provenance.as_object().is_some_and(|p| !p.is_empty()),
            "missing text/provenance at row {}",
            line + 1
        );
        ensure!(
            matches!(row.split.as_str(), "reference" | "query"),
            "invalid split at row {}",
            line + 1
        );
        grammar_eval::validate_date(&row.date)?;
        ensure!(
            row.text_sha256 == sha(row.text.as_bytes()),
            "text hash mismatch at row {}",
            line + 1
        );
        ensure!(ids.insert(row.id.clone()), "duplicate input id");
        ensure!(
            exact.insert(row.text_sha256.clone()),
            "duplicate exact source text"
        );
        let collapsed = row.text.split_whitespace().collect::<Vec<_>>().join(" ");
        ensure!(
            normalized.insert(sha(collapsed.as_bytes())),
            "duplicate whitespace-normalized source text"
        );
        let group = (row.author_id.clone(), row.split.clone());
        if let Some(old) = groups.insert(row.source_group.clone(), group.clone()) {
            ensure!(old == group, "source group crosses authors or splits");
        }
        if let Some(old) =
            dates.insert((row.author_id.clone(), row.date.clone()), row.split.clone())
        {
            ensure!(
                old == row.split,
                "author date crosses reference/query split"
            );
        }
        rows.push(row);
    }
    ensure!(!rows.is_empty(), "empty input");
    let authors: BTreeSet<_> = rows.iter().map(|row| &row.author_id).collect();
    ensure!(
        authors.len() >= 2,
        "retrieval requires at least two authors"
    );
    for author in authors {
        let reference_max = rows
            .iter()
            .filter(|r| &r.author_id == author && r.split == "reference")
            .map(|r| &r.date)
            .max()
            .context("author has no reference articles")?;
        let query_min = rows
            .iter()
            .filter(|r| &r.author_id == author && r.split == "query")
            .map(|r| &r.date)
            .min()
            .context("author has no query articles")?;
        ensure!(
            reference_max < query_min,
            "reference/query chronology is not strictly separated for an author"
        );
    }
    Ok(rows)
}

fn validate_document(doc: &Document, row: &Input, artifact: &Artifact) -> Result<()> {
    syntax::validate(doc)?;
    ensure!(
        doc.text == row.text && sha(doc.text.as_bytes()) == row.text_sha256,
        "annotation changed input bytes"
    );
    ensure!(
        doc.parser_identity == artifact.parser_identity,
        "parser identity differs from frozen metric"
    );
    Ok(())
}

fn references(
    rows: &[Input],
    extracted: &[Option<Features>],
    author: &str,
    word_only: bool,
) -> Result<Profile> {
    let mut selected = Vec::new();
    for (row, feature) in rows.iter().zip(extracted) {
        if row.author_id == author && row.split == "reference" {
            let mut feature = feature
                .as_ref()
                .context("reference annotation/extraction unavailable")?
                .clone();
            if word_only {
                feature.families.retain(|name, _| name == "word");
            }
            selected.push((row.date.as_str(), feature));
        }
    }
    let borrowed: Vec<_> = selected
        .iter()
        .map(|(date, feature)| (*date, feature))
        .collect();
    grouped_profile(&borrowed)
}

fn measures(ranking: &[RankedAuthor], author: &str) -> Result<Measures> {
    let own = ranking
        .iter()
        .find(|r| r.author_id == author)
        .context("true author absent from complete gallery")?;
    let n = (own.rank_max - own.rank_min + 1) as f64;
    let top = |k: usize| (k.min(own.rank_max) + 1).saturating_sub(own.rank_min) as f64 / n;
    Ok(Measures {
        top1: top(1),
        top5: top(5),
        mrr: (own.rank_min..=own.rank_max)
            .map(|r| 1.0 / r as f64)
            .sum::<f64>()
            / n,
    })
}

// Same arithmetic as the private family_ablation::family_terms helper. Keeping
// this small copy avoids importing its corpus-specific experiment runner.
fn family_terms(artifact: &Artifact, distances: &[f64]) -> Result<(Vec<f64>, [f64; 2])> {
    ensure!(
        distances.len() == artifact.families.len(),
        "family distance width differs"
    );
    let mut terms = Vec::new();
    let mut lexical = 0.0;
    let mut grammar = 0.0;
    for (i, distance) in distances.iter().enumerate() {
        ensure!(
            distance.is_finite() && *distance >= 0.0,
            "invalid adjusted distance"
        );
        let interval = artifact.knots.partition_point(|k| *k < *distance);
        let mut area = 0.0;
        let mut left = 0.0;
        for j in 0..interval {
            area += artifact.slopes[j] * (artifact.knots[j] - left);
            left = artifact.knots[j];
        }
        let calibrated = area + artifact.slopes[interval] * (*distance - left);
        let weighted = calibrated * artifact.family_weights[i];
        if ["word", "word_bigram"].contains(&artifact.families[i].name.as_str()) {
            lexical += weighted;
        } else {
            grammar += weighted;
        }
        terms.push(-weighted / artifact.temperature);
    }
    ensure!(
        terms.iter().all(|x| x.is_finite() && *x <= 0.0),
        "invalid signed family term"
    );
    Ok((
        terms,
        [
            -lexical / artifact.temperature,
            -grammar / artifact.temperature,
        ],
    ))
}

fn mean(rows: &[Measures]) -> Option<Measures> {
    (!rows.is_empty()).then(|| Measures {
        top1: rows.iter().map(|r| r.top1).sum::<f64>() / rows.len() as f64,
        top5: rows.iter().map(|r| r.top5).sum::<f64>() / rows.len() as f64,
        mrr: rows.iter().map(|r| r.mrr).sum::<f64>() / rows.len() as f64,
    })
}

fn aggregate(
    authors: &[String],
    rows: &[Input],
    observations: &[(String, Measures)],
) -> (Value, Option<Measures>) {
    let mut macro_rows = Vec::new();
    let author_rows: Vec<_> = authors.iter().map(|author| {
        let supported: Vec<_> = observations.iter().filter(|(id, _)| id == author).map(|(_, m)| *m).collect();
        let average = mean(&supported);
        if let Some(value) = average { macro_rows.push(value); }
        json!({"author_id": author, "requested_queries": rows.iter().filter(|r| &r.author_id == author && r.split == "query").count(), "scored_queries": supported.len(), "metrics": average})
    }).collect();
    let macro_mean = mean(&macro_rows);
    (
        json!({"macro_author": macro_mean, "micro_query": mean(&observations.iter().map(|(_, m)| *m).collect::<Vec<_>>()), "scored_authors": macro_rows.len(), "scored_queries": observations.len(), "authors": author_rows}),
        macro_mean,
    )
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.join("report.json").exists(),
        "completed output exists; choose a new output directory"
    );
    ensure!(
        args.models.len() == 3,
        "supply the three frozen model files"
    );
    let input_bytes = fs::read(&args.input)?;
    let rows = load_input(&input_bytes)?;
    let authors: Vec<_> = rows
        .iter()
        .map(|r| r.author_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut models = BTreeMap::new();
    let mut model_bindings = Vec::new();
    for path in &args.models {
        let bytes = fs::read(path)?;
        let digest = sha(&bytes);
        let seed = MODELS
            .iter()
            .find(|(_, expected)| *expected == digest)
            .context("model is not one of the three frozen treatment artifacts")?
            .0;
        ensure!(!models.contains_key(&seed), "duplicate model seed");
        let metric = Metric::new(serde_json::from_slice(&bytes)?)?;
        model_bindings.push(json!({"seed": seed, "artifact": binding(path)?, "source_space_id": metric.artifact().source_space_id, "feature_schema": metric.artifact().feature_schema, "parser_identity": metric.artifact().parser_identity, "provenance_sha256": metric.artifact().provenance_sha256}));
        models.insert(seed, metric);
    }
    let artifact = models[&0].artifact();
    for metric in models.values() {
        ensure!(
            metric.artifact().source_space_id == artifact.source_space_id
                && metric.artifact().parser_identity == artifact.parser_identity
                && metric.artifact().feature_schema == artifact.feature_schema,
            "model identities differ"
        );
    }
    let plan = json!({
        "schema": "slopninja-author-transfer-plan-v1", "input": {"path": args.input.canonicalize()?, "sha256": sha(&input_bytes)}, "models": model_bindings,
        "python": {"command": args.python, "executable": binding(Path::new(&args.python))?}, "binary": binding(&std::env::current_exe()?)?,
        "compiled_sources_sha256": {
            "example": sha(include_bytes!("author_transfer.rs")),
            "coordinate_inference": sha(include_bytes!("../src/coordinate_inference.rs")),
            "family_ablation_arithmetic_reference": sha(include_bytes!("../src/family_ablation.rs")),
            "grammar_eval": sha(include_bytes!("../src/lib.rs")),
            "features": sha(include_bytes!("../../grammar-core/src/features.rs")),
            "spacy_bridge": sha(include_bytes!("../../grammar-spacy/python/syntax_bridge.py")),
            "spacy_rust": sha(include_bytes!("../../grammar-spacy/src/lib.rs")),
            "cargo_lock": sha(include_bytes!("../../../Cargo.lock"))
        },
        "policy": {"requested_authors": authors.len(), "requested_articles": rows.len(), "batch_size": BATCH_SIZE,
            "reference_pooling": "all reference articles; equal dates then equal articles within date; raw family values averaged before frozen transforms",
            "missing_reference": "any unavailable reference disables that author profile; any unavailable author disables complete-gallery scoring for that variant",
            "missing_query": "retain null metrics and report conditional-on-scorable coverage; never zero-fill",
            "annotation_cache": "retain both successful annotations and per-input parser errors; interrupted resumes reuse either under the identical plan and reject conflicting cache entries",
            "chronology": "each author's maximum reference date is strictly earlier than its minimum query date; whole dates and source groups never cross splits",
            "aggregation": "per-query exact-tie metrics, mean queries within author, mean supported authors, then mean the three seed summaries",
            "frozen_variants": VARIANTS,
            "family_order": artifact.families.iter().map(|f| &f.name).collect::<Vec<_>>(),
            "ablation": "full uses the existing score; lexical_family_only keeps word and word_bigram calibrated terms; grammar_family_only keeps the remaining 12. Original weights, adjusted distances, spline and temperature remain fixed, without renormalization or refitting. All three require the same complete original14 profiles and gallery.",
            "decomposition_check": {"absolute_tolerance": DECOMPOSITION_TOLERANCE, "per_query_target_seed": "sum of both block logits and separately sum of all signed family terms must each equal the original full logit within tolerance"},
            "word_control": "unigram word family only; raw probabilities, same date-balanced references, squared Hellinger distance; separate coverage",
            "no_fitting": true, "gallery_comparable_to_prior_100_author_benchmark": false}
    });
    let plan_binding = save(&args.out.join("plan.json"), &plan)?;
    let cache = args.out.join("cache");
    fs::create_dir_all(&cache)?;
    let mut docs: Vec<Option<Document>> = vec![None; rows.len()];
    let mut parse_errors: BTreeMap<usize, String> = BTreeMap::new();
    let mut missing = Vec::new();
    let mut cache_hits = 0;
    let mut cached_error_hits = 0;
    for (i, row) in rows.iter().enumerate() {
        let path = cache.join(format!("{}.annotation.json", row.text_sha256));
        let error_path = cache.join(format!("{}.error.json", row.text_sha256));
        ensure!(
            !(path.exists() && error_path.exists()),
            "both annotation and error cached for one input"
        );
        if path.exists() {
            let doc: Document = serde_json::from_slice(&fs::read(&path)?)?;
            validate_document(&doc, row, artifact)?;
            docs[i] = Some(doc);
            cache_hits += 1;
        } else if error_path.exists() {
            let error: Value = serde_json::from_slice(&fs::read(&error_path)?)?;
            ensure!(
                error["schema"] == "slopninja-author-transfer-parse-error-v1"
                    && error["text_sha256"] == row.text_sha256
                    && error["parser_identity"] == artifact.parser_identity,
                "cached parser error identity differs"
            );
            parse_errors.insert(
                i,
                error["error"]
                    .as_str()
                    .context("cached parser error missing message")?
                    .to_owned(),
            );
            cached_error_hits += 1;
        } else {
            missing.push(i);
        }
    }
    for indices in missing.chunks(BATCH_SIZE) {
        let texts: Vec<_> = indices.iter().map(|i| rows[*i].text.as_str()).collect();
        let batch = grammar_spacy::parse_batch(&texts, &args.python)?;
        ensure!(
            batch.len() == indices.len(),
            "parser response count mismatch"
        );
        for (i, result) in indices.iter().zip(batch) {
            match result {
                Ok(doc) => {
                    validate_document(&doc, &rows[*i], artifact)?;
                    save(
                        &cache.join(format!("{}.annotation.json", rows[*i].text_sha256)),
                        &doc,
                    )?;
                    docs[*i] = Some(doc);
                }
                Err(error) => {
                    save(
                        &cache.join(format!("{}.error.json", rows[*i].text_sha256)),
                        &json!({"schema": "slopninja-author-transfer-parse-error-v1", "text_sha256": rows[*i].text_sha256, "parser_identity": artifact.parser_identity, "error": error}),
                    )?;
                    parse_errors.insert(*i, error);
                }
            }
        }
    }
    let mut extracted = vec![None; rows.len()];
    let mut annotation_rows = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut evidence = json!({"id": row.id, "author_id": row.author_id, "source_group": row.source_group, "date": row.date, "split": row.split, "text_sha256": row.text_sha256, "provenance": row.provenance});
        if let Some(doc) = &docs[i] {
            evidence["annotation"] =
                binding(&cache.join(format!("{}.annotation.json", row.text_sha256)))?;
            evidence["annotated_text_sha256"] = json!(sha(doc.text.as_bytes()));
            match features::extract(doc) {
                Ok(feature) => {
                    ensure!(
                        feature.schema == artifact.feature_schema
                            && feature.parser_identity == artifact.parser_identity
                            && feature.source_sha256 == row.text_sha256,
                        "extracted feature identity differs"
                    );
                    evidence["features"] = save(
                        &cache.join(format!("{}.features.json", row.text_sha256)),
                        &feature,
                    )?;
                    let unavailable: Vec<_> = feature
                        .families
                        .iter()
                        .filter_map(|(name, family)| family_values(family).err().map(|_| name))
                        .collect();
                    evidence["status"] = json!(if unavailable.is_empty() {
                        "available"
                    } else {
                        "missing_families"
                    });
                    evidence["unavailable_families"] = json!(unavailable);
                    extracted[i] = Some(feature);
                }
                Err(error) => {
                    evidence["status"] = json!("extraction_error");
                    evidence["error"] = json!(format!("{error:#}"));
                }
            }
        } else {
            evidence["status"] = json!("parse_error");
            evidence["error"] = json!(parse_errors.get(&i));
            evidence["error_artifact"] =
                binding(&cache.join(format!("{}.error.json", row.text_sha256)))?;
        }
        annotation_rows.push(evidence);
    }
    let annotations_binding = save(&args.out.join("articles.json"), &annotation_rows)?;
    let mut full_targets = BTreeMap::new();
    let mut word_targets = BTreeMap::new();
    let mut target_rows = Vec::new();
    for author in &authors {
        let full = references(&rows, &extracted, author, false);
        let word = references(&rows, &extracted, author, true);
        target_rows.push(json!({"author_id": author,
            "reference_articles": rows.iter().filter(|r| &r.author_id == author && r.split == "reference").count(),
            "reference_dates": rows.iter().filter(|r| &r.author_id == author && r.split == "reference").map(|r| &r.date).collect::<BTreeSet<_>>(),
            "full_error": full.as_ref().err().map(|e| format!("{e:#}")), "word_error": word.as_ref().err().map(|e| format!("{e:#}"))}));
        if let Ok(profile) = full {
            full_targets.insert(author.clone(), profile);
        }
        if let Ok(profile) = word {
            word_targets.insert(author.clone(), profile);
        }
    }
    let targets_binding = save(
        &args.out.join("reference-profiles.json"),
        &json!({"authors": target_rows, "full": full_targets, "word": word_targets}),
    )?;
    let full_gallery = full_targets.len() == authors.len();
    let word_gallery = word_targets.len() == authors.len();
    let mut observations: BTreeMap<_, BTreeMap<_, Vec<(String, Measures)>>> = VARIANTS
        .iter()
        .map(|variant| {
            (
                *variant,
                MODELS.iter().map(|(seed, _)| (*seed, Vec::new())).collect(),
            )
        })
        .collect();
    let mut maximum_decomposition_error = 0.0_f64;
    let mut decomposed_pairs = 0_usize;
    let mut word_observations = Vec::new();
    let mut query_rows = Vec::new();
    for (i, row) in rows.iter().enumerate().filter(|(_, r)| r.split == "query") {
        let mut output = json!({"id": row.id, "author_id": row.author_id, "date": row.date, "source_group": row.source_group, "text_sha256": row.text_sha256,
            "requested_gallery_size": authors.len(), "full": {"status": "unavailable"}, "word_only": {"status": "unavailable"}});
        for variant in VARIANTS {
            output[variant] = json!({"status": "unavailable"});
        }
        let profile = extracted[i]
            .as_ref()
            .context("query annotation/extraction unavailable")
            .and_then(document_values);
        if full_gallery {
            match &profile {
                Ok(profile) => {
                    let mut seed_rows: [Vec<Value>; 3] = std::array::from_fn(|_| Vec::new());
                    let mut query_means: [Vec<Measures>; 3] = std::array::from_fn(|_| Vec::new());
                    for (seed, metric) in &models {
                        let mut scores: [Vec<Value>; 3] = std::array::from_fn(|_| Vec::new());
                        let mut distances: [Vec<(String, f64)>; 3] =
                            std::array::from_fn(|_| Vec::new());
                        for (author, target) in &full_targets {
                            let score = metric.score_profiles(profile, target)?;
                            let (terms, blocks) =
                                family_terms(metric.artifact(), &score.adjusted_family_distances)?;
                            let error = (score.logit - blocks.iter().sum::<f64>())
                                .abs()
                                .max((score.logit - terms.iter().sum::<f64>()).abs());
                            ensure!(
                                error <= DECOMPOSITION_TOLERANCE,
                                "full family decomposition changed score"
                            );
                            maximum_decomposition_error = maximum_decomposition_error.max(error);
                            decomposed_pairs += 1;
                            for (index, logit) in
                                [score.logit, blocks[0], blocks[1]].into_iter().enumerate()
                            {
                                distances[index].push((author.clone(), -logit));
                                if index == 0 {
                                    scores[index].push(json!({"author_id": author, "score": score, "signed_family_contributions": terms}));
                                } else {
                                    scores[index]
                                        .push(json!({"author_id": author, "logit": logit}));
                                }
                            }
                        }
                        for (index, variant) in VARIANTS.iter().enumerate() {
                            let ranking = rank(std::mem::take(&mut distances[index]))?;
                            let metrics = measures(&ranking, &row.author_id)?;
                            observations
                                .get_mut(variant)
                                .context("unknown variant")?
                                .get_mut(seed)
                                .context("unknown model seed")?
                                .push((row.author_id.clone(), metrics));
                            query_means[index].push(metrics);
                            seed_rows[index].push(json!({"seed": seed, "scores": scores[index], "ranking_by_negative_logit": ranking, "metrics": metrics}));
                        }
                    }
                    for (index, variant) in VARIANTS.iter().enumerate() {
                        output[*variant] = json!({"status": "scored", "gallery_size": authors.len(), "seeds": seed_rows[index], "seed_mean_metrics": mean(&query_means[index])});
                    }
                }
                Err(error) => {
                    for variant in VARIANTS {
                        output[variant]["reason"] = json!(format!("{error:#}"));
                    }
                }
            }
        } else {
            for variant in VARIANTS {
                output[variant]["reason"] = json!("incomplete_reference_gallery");
            }
        }
        if word_gallery {
            let word = extracted[i]
                .as_ref()
                .and_then(|f| f.families.get("word"))
                .context("query word family unavailable")
                .and_then(family_values);
            match word {
                Ok(word) => {
                    let mut scores = Vec::new();
                    for (author, target) in &word_targets {
                        let p = &target["word"];
                        let overlap = word
                            .iter()
                            .map(|(key, q)| (q * p.get(key).copied().unwrap_or(0.0)).sqrt())
                            .sum::<f64>();
                        ensure!(
                            overlap.is_finite() && overlap <= 1.0 + 1e-9,
                            "invalid word Hellinger overlap"
                        );
                        scores.push((author.clone(), (1.0 - overlap).max(0.0)));
                    }
                    let ranking = rank(scores)?;
                    let metrics = measures(&ranking, &row.author_id)?;
                    word_observations.push((row.author_id.clone(), metrics));
                    output["word_only"] = json!({"status": "scored", "gallery_size": authors.len(), "ranking_by_squared_hellinger": ranking, "metrics": metrics});
                }
                Err(error) => {
                    output["word_only"]["reason"] = json!(format!("{error:#}"));
                }
            }
        } else {
            output["word_only"]["reason"] = json!("incomplete_reference_gallery");
        }
        query_rows.push(output);
    }
    let queries_binding = save(&args.out.join("queries.json"), &query_rows)?;
    let mut report = json!({
        "schema": "slopninja-author-transfer-result-v1", "plan": plan_binding,
        "artifacts": {"articles": annotations_binding, "reference_profiles": targets_binding, "queries": queries_binding},
        "coverage": {"requested_authors": authors.len(), "requested_articles": rows.len(), "requested_references": rows.iter().filter(|r| r.split == "reference").count(), "requested_queries": query_rows.len(), "annotated_articles": docs.iter().flatten().count(), "parse_errors": parse_errors.len(), "complete_full_reference_authors": full_targets.len(), "complete_word_reference_authors": word_targets.len(), "cache_hits": cache_hits, "cached_error_hits": cached_error_hits, "newly_requested_annotations": missing.len(), "new_parser_processes": missing.len().div_ceil(BATCH_SIZE)},
        "decomposition": {"checked_query_target_seed_pairs": decomposed_pairs, "maximum_absolute_error": maximum_decomposition_error, "absolute_tolerance": DECOMPOSITION_TOLERANCE},
        "word_only": {"gallery_available": word_gallery, "summary": aggregate(&authors, &rows, &word_observations).0},
        "limits": ["Frozen Blog-trained weights transferred without fitting; this is a separate corpus and gallery.", "Input provenance/bylines and cross-work near-duplicate control require external collection review; only exact and whitespace-normalized duplicates are checked here.", "Query metrics condition on available queries; null author rows and every requested input are retained.", "No human authorship, AI-origin, meaning preservation, or writing quality conclusion follows from retrieval.", "Unigram-only Hellinger is an untrained control; its available-query support must be compared explicitly with full-model support."],
        "optimizer_steps": 0
    });
    for (variant, by_seed) in &observations {
        let mut seeds = Vec::new();
        let mut seed_means = Vec::new();
        for (seed, values) in by_seed {
            let (summary, average) = aggregate(&authors, &rows, values);
            if let Some(value) = average {
                seed_means.push(value);
            }
            seeds.push(json!({"seed": seed, "summary": summary}));
        }
        report[*variant] = json!({"gallery_available": full_gallery, "seeds": seeds, "macro_author_seed_mean": if seed_means.len() == 3 { mean(&seed_means) } else { None }});
    }
    save(&args.out.join("report.json"), &report)?;
    println!(
        "Wrote {} ({} authors, {} query articles)",
        args.out.join("report.json").display(),
        authors.len(),
        query_rows.len()
    );
    Ok(())
}
