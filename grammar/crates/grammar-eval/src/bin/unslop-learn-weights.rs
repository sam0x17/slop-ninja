use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{
    features::{self, Features},
    space::Space,
    syntax::{self, Document},
};
use grammar_eval::{
    DistanceGeometry, Profile, Values, document_values, family_distances, query_metrics, rank,
    summarize, validate_date, weighted_distance,
    weights::{FrozenSelection, SELECTION_SCHEMA},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[path = "../neural.rs"]
mod neural;
use neural::{Adam, Example, Model, balanced_weights, select_checkpoint, target_profile};

#[derive(Parser)]
#[command(about = "Learn positive grammar-family weights on original training authors only")]
struct Args {
    evaluation: PathBuf,
    #[arg(long)]
    export: PathBuf,
    #[arg(long)]
    cache: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Deserialize)]
struct CachedFeatures {
    features: Features,
}
struct Post {
    id: String,
    author: String,
    date: String,
    group: String,
    profile: Profile,
}
struct Development {
    own: String,
    impostor: String,
    distances: BTreeMap<String, Values>,
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read(path: &Path, bindings: &mut Value) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    bindings[path.display().to_string()] = json!(digest(&bytes));
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
fn within(root: &Path, base: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(!Path::new(relative).is_absolute(), "absolute corpus path");
    let result = base.join(relative).canonicalize()?;
    ensure!(result.starts_with(root), "corpus path escapes export");
    Ok(result)
}

fn load_training(
    args: &Args,
    space: &Space,
    bindings: &mut Value,
) -> Result<(Vec<Post>, Value, BTreeSet<String>)> {
    let root = args.export.canonicalize()?;
    let old = read(&args.evaluation.join("protocol.json"), bindings)?;
    ensure!(
        old["protocol"] == grammar_eval::PROTOCOL
            && old["parser_identity"] == space.parser_identity,
        "original protocol mismatch"
    );
    let summary_path = root.join("summary.json");
    let summary = read(&summary_path, bindings)?;
    ensure!(
        summary["schema"] == "unslop-blog-author-export-v1",
        "unsupported export"
    );
    ensure!(
        bindings[summary_path.display().to_string()] == old["input_bindings"]["summary_sha256"],
        "original summary changed"
    );
    let audit = read(&args.evaluation.join("annotation-audit.json"), bindings)?;
    let mut statuses = BTreeMap::new();
    for row in audit
        .as_array()
        .context("invalid original audit")?
        .iter()
        .filter(|row| row["post"]["split"] == "train")
    {
        ensure!(
            statuses
                .insert(text(&row["post"], "id")?.to_owned(), row)
                .is_none(),
            "duplicate audited training ID"
        );
    }
    let mut seen = BTreeSet::new();
    let mut authors = BTreeSet::new();
    let mut posts = Vec::new();
    let mut caches = Vec::new();
    let mut excluded = Vec::new();
    let mut train_bindings = BTreeSet::new();
    for author in summary["authors"].as_array().context("missing authors")? {
        let author_id = text(author, "author_id")?;
        ensure!(authors.insert(author_id.to_owned()), "duplicate author");
        let relative = text(author, "manifest")?;
        let manifest_path = within(&root, &root, relative)?;
        let manifest_bytes = fs::read(&manifest_path)?;
        let hash = digest(&manifest_bytes);
        let bound = old["input_bindings"]["manifests"]
            .as_array()
            .context("missing bound manifests")?
            .iter()
            .find(|row| row["path"] == relative)
            .context("unbound source manifest")?;
        ensure!(bound["sha256"] == hash, "original manifest changed");
        bindings[manifest_path.display().to_string()] = json!(hash);
        for line in std::str::from_utf8(&manifest_bytes)?.lines() {
            let metadata: Value = serde_json::from_str(line)?;
            if metadata["split"] != "train" {
                continue;
            }
            let id = text(&metadata, "id")?.to_owned();
            ensure!(
                seen.insert(id.clone()) && metadata["author_id"] == author_id,
                "duplicate or mismatched training post"
            );
            let status = statuses
                .get(&id)
                .context("missing original training audit")?;
            for key in [
                "id",
                "author_id",
                "source_group",
                "split",
                "path",
                "text_sha256",
                "provenance",
            ] {
                ensure!(
                    status["post"][key] == metadata[key],
                    "original training metadata differs: {key}"
                );
            }
            if status["status"] != "eligible" {
                excluded.push((*status).clone());
                continue;
            }
            let provenance = &metadata["provenance"];
            let date = text(provenance, "supplied_composition_date")?.to_owned();
            validate_date(&date)?;
            let group = format!("blog:{author_id}:date:{date}");
            ensure!(
                metadata["source_group"] == group
                    && provenance["archive_sha256"] == summary["archive_sha256"],
                "source provenance mismatch"
            );
            let source = fs::read_to_string(within(
                &root,
                manifest_path.parent().context("manifest parent")?,
                text(&metadata, "path")?,
            )?)?;
            let hash = digest(source.as_bytes());
            ensure!(
                metadata["text_sha256"] == hash
                    && provenance["text_sha256"] == hash
                    && provenance["excerpt_sha256"] == hash,
                "source text digest mismatch"
            );
            ensure!(
                provenance["word_count"].as_u64() == Some(features::words(&source).len() as u64),
                "word count mismatch"
            );
            let annotation_bytes = fs::read(args.cache.join(format!("{hash}.annotation.json")))?;
            let feature_bytes = fs::read(args.cache.join(format!("{hash}.features.json")))?;
            let annotation: Document = serde_json::from_slice(&annotation_bytes)?;
            ensure!(
                annotation.text == source && annotation.parser_identity == space.parser_identity,
                "annotation identity mismatch"
            );
            syntax::validate(&annotation)?;
            let extracted = features::extract(&annotation)?;
            let cached: CachedFeatures = serde_json::from_slice(&feature_bytes)?;
            ensure!(
                cached.features == extracted
                    && extracted.source_sha256 == hash
                    && extracted.schema == space.feature_schema,
                "cached feature mismatch"
            );
            let profile = document_values(&extracted)?;
            ensure!(
                profile.keys().eq(space.family_schemas.keys()),
                "training feature catalog differs"
            );
            train_bindings.insert((group.clone(), hash.clone()));
            caches.push(json!({"id":id, "text_sha256":hash, "annotation_sha256":digest(&annotation_bytes), "feature_cache_sha256":digest(&feature_bytes)}));
            posts.push(Post {
                id,
                author: author_id.into(),
                date,
                group,
                profile,
            });
        }
        if authors.len() % 10 == 0 {
            eprintln!(
                "Validated {} authors / {} training posts",
                authors.len(),
                posts.len()
            );
        }
    }
    ensure!(
        seen.len() == statuses.len()
            && authors.len() as u64
                == summary["selected_authors"]
                    .as_u64()
                    .context("missing author count")?,
        "training audit or author count mismatch"
    );
    let fitted: BTreeSet<_> = space
        .references
        .iter()
        .map(|row| (row.source_group.clone(), row.source_sha256.clone()))
        .collect();
    ensure!(
        posts.len() == space.references.len() && train_bindings == fitted,
        "frozen space training sources differ"
    );
    ensure!(
        posts
            .iter()
            .map(|post| post.author.clone())
            .collect::<BTreeSet<_>>()
            == authors,
        "author has no eligible training rows"
    );
    posts.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((
        posts,
        json!({"cache_bindings":caches, "original_annotation_exclusions":excluded}),
        authors,
    ))
}

fn training_tensor(
    posts: &[Post],
    space: &Space,
    authors: &[String],
    families: &[String],
) -> Result<Vec<Example>> {
    let geometry = DistanceGeometry::new(space);
    let mut grouped = BTreeMap::<&str, Vec<(&str, &Profile)>>::new();
    for post in posts {
        grouped
            .entry(&post.author)
            .or_default()
            .push((&post.date, &post.profile));
    }
    let targets: BTreeMap<_, _> = grouped
        .iter()
        .map(|(author, rows)| Ok((*author, target_profile(rows, None)?)))
        .collect::<Result<_>>()?;
    let weights = balanced_weights(
        &posts
            .iter()
            .map(|post| (post.author.as_str(), post.date.as_str()))
            .collect::<Vec<_>>(),
    )?;
    let mut output = Vec::new();
    for (index, (post, loss_weight)) in posts.iter().zip(weights).enumerate() {
        let held_date_target = target_profile(&grouped[post.author.as_str()], Some(&post.date))?;
        let mut distances = Vec::new();
        for author in authors {
            let target = if author == &post.author {
                &held_date_target
            } else {
                &targets[author.as_str()]
            };
            let values = family_distances(&geometry, &post.profile, target)?;
            distances.push(families.iter().map(|name| values[name]).collect());
        }
        output.push(Example {
            id: post.id.clone(),
            author: post.author.clone(),
            source_group: post.group.clone(),
            own: authors
                .binary_search(&post.author)
                .map_err(|_| anyhow::anyhow!("unknown training author"))?,
            loss_weight,
            distances,
        });
        if (index + 1) % 100 == 0 {
            eprintln!(
                "Materialized {} / {} training distance rows",
                index + 1,
                posts.len()
            );
        }
    }
    ensure!(
        (output.iter().map(|row| row.loss_weight).sum::<f64>() - 1.0).abs() < 1e-12,
        "training query weight mass"
    );
    Ok(output)
}

fn load_development(
    args: &Args,
    space: &Space,
    authors: &BTreeSet<String>,
    bindings: &mut Value,
) -> Result<Vec<Development>> {
    let implementation = read(&args.evaluation.join("implementation.json"), bindings)?;
    for filename in [
        "space.json",
        "source-summary.json",
        "dev-queries.jsonl",
        "annotation-audit.json",
    ] {
        let bytes = fs::read(args.evaluation.join(filename))?;
        ensure!(
            implementation["artifacts_sha256"][filename] == digest(&bytes),
            "original evaluation artifact changed: {filename}"
        );
        bindings[args.evaluation.join(filename).display().to_string()] = json!(digest(&bytes));
    }
    let source = read(&args.evaluation.join("source-summary.json"), bindings)?;
    let source_authors: BTreeSet<_> = source["authors"]
        .as_array()
        .context("missing source authors")?
        .iter()
        .map(|row| text(row, "author_id").map(str::to_owned))
        .collect::<Result<_>>()?;
    ensure!(&source_authors == authors, "source author catalog differs");
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let mut observed_authors = BTreeSet::new();
    for line in fs::read_to_string(args.evaluation.join("dev-queries.jsonl"))?.lines() {
        let query: Value = serde_json::from_str(line)?;
        ensure!(
            query["post"]["split"] == "dev",
            "nondevelopment input rejected"
        );
        ensure!(
            seen.insert(text(&query["post"], "id")?.to_owned()),
            "duplicate development query"
        );
        let own = text(&query["post"], "author_id")?.to_owned();
        let impostor = text(&query, "content_matched_impostor")?.to_owned();
        ensure!(
            authors.contains(&own) && authors.contains(&impostor) && own != impostor,
            "development author mismatch"
        );
        observed_authors.insert(own.clone());
        let distances: BTreeMap<String, Values> =
            serde_json::from_value(query["unweighted_squared_family_distances"].clone())?;
        ensure!(
            distances.keys().cloned().collect::<BTreeSet<_>>() == *authors,
            "development candidate catalog differs"
        );
        for row in distances.values() {
            ensure!(
                row.keys().eq(space.family_schemas.keys())
                    && row.values().all(|d| d.is_finite() && *d >= 0.0),
                "invalid development distance"
            );
        }
        output.push(Development {
            own,
            impostor,
            distances,
        });
    }
    ensure!(
        observed_authors == *authors,
        "development author coverage differs"
    );
    Ok(output)
}

fn development_summary(
    rows: &[Development],
    weights: &Values,
) -> Result<grammar_eval::SummaryMetrics> {
    let metrics = rows
        .iter()
        .map(|row| {
            let ranking = rank(
                row.distances
                    .iter()
                    .map(|(author, distances)| {
                        (author.clone(), weighted_distance(distances, weights))
                    })
                    .collect(),
            )?;
            Ok((
                row.own.clone(),
                query_metrics(&ranking, &row.own, &row.impostor)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    summarize(&metrics)
}

fn validate_protocol(protocol: &Value, space: &Space) -> Result<()> {
    ensure!(
        protocol["schema"] == "unslop-neural-family-weight-transfer-protocol-v1",
        "unsupported neural protocol"
    );
    ensure!(
        protocol["source_space_id"] == space.id,
        "neural protocol space mismatch"
    );
    ensure!(
        protocol["model"]["family_count"] == 14
            && protocol["model"]["parameter_count"] == 15
            && protocol["training"]["initial_temperature"] == 0.1,
        "model initialization mismatch"
    );
    ensure!(
        protocol["training"]["epochs"] == neural::EPOCHS
            && protocol["training"]["learning_rate"] == neural::LEARNING_RATE
            && protocol["training"]["beta1"] == neural::BETA1
            && protocol["training"]["beta2"] == neural::BETA2
            && protocol["training"]["epsilon"] == neural::EPSILON
            && protocol["training"]["log_temperature_bounds"] == json!(neural::LOG_TAU_BOUNDS),
        "optimizer protocol mismatch"
    );
    ensure!(
        protocol["training"]["optimizer"] == "full-batch Adam"
            && protocol["training"]["loss_weighting"]
                == "equal authors, then dates, then posts within date"
            && protocol["training"]["positive_profile"]
                == "same author training mean excluding ALL posts in the query date group"
            && protocol["training"]["checkpoint_selection"]
                == "highest original-development macro-author top1, then highest MRR, then earliest epoch; include epoch0"
            && protocol["training"]["hyperparameter_search"] == false,
        "training or selection protocol mismatch"
    );
    Ok(())
}

fn run(args: Args) -> Result<()> {
    ensure!(!args.out.exists(), "neural output already exists");
    let mut bindings = json!({});
    let protocol = read(&args.protocol, &mut bindings)?;
    let space: Space =
        serde_json::from_value(read(&args.evaluation.join("space.json"), &mut bindings)?)?;
    space.validate()?;
    ensure!(
        space.feature_schema == features::FEATURE_VERSION && space.family_schemas.len() == 14,
        "feature definition changed"
    );
    validate_protocol(&protocol, &space)?;
    fs::create_dir_all(&args.out)?;
    write(&args.out.join("protocol.json"), &protocol)?;
    let (posts, audit, author_set) = load_training(&args, &space, &mut bindings)?;
    let dev = load_development(&args, &space, &author_set, &mut bindings)?;
    let authors: Vec<_> = author_set.iter().cloned().collect();
    let families: Vec<_> = space.family_schemas.keys().cloned().collect();
    write(
        &args.out.join("input-validation.json"),
        &json!({"input_sha256":bindings, "training_audit":audit, "training_post_count":posts.len(), "development_query_count":dev.len(), "author_ids":authors}),
    )?;
    let tensor = training_tensor(&posts, &space, &authors, &families)?;
    write(
        &args.out.join("training-tensor.json"),
        &json!({"schema":"unslop-neural-distance-tensor-v1", "source_space_id":space.id, "feature_schema":space.feature_schema, "parser_identity":space.parser_identity, "families":families, "authors":authors, "input_sha256":bindings, "training_audit":audit, "rows":tensor}),
    )?;
    let initial: Vec<_> = families
        .iter()
        .map(|name| space.family_weights[name])
        .collect();
    for (name, weight) in families.iter().zip(&initial) {
        let expected = if matches!(name.as_str(), "word" | "word_bigram") {
            0.25
        } else {
            0.5 / 12.0
        };
        ensure!(
            (weight - expected).abs() < 1e-12,
            "original combined initialization differs"
        );
    }
    let mut model = Model::initial(&initial)?;
    let mut adam = Adam::new(families.len() + 1);
    let mut history = BufWriter::new(File::create(args.out.join("epochs.jsonl"))?);
    let mut selected = None;
    let mut best = None;
    for epoch in 0..=neural::EPOCHS {
        let (loss, gradient) = model.loss_gradient(&tensor)?;
        let weights = model.named_weights(&families);
        let development = development_summary(&dev, &weights)?;
        let score = (development.macro_author.top1, development.macro_author.mrr);
        let record = json!({"epoch":epoch, "training_cross_entropy":loss, "development":development,
            "model":model, "temperature":model.log_tau.exp(), "weights":weights, "optimizer":adam,
            "gradient_l2":gradient.iter().map(|g| g * g).sum::<f64>().sqrt()});
        serde_json::to_writer(&mut history, &record)?;
        history.write_all(b"\n")?;
        history.flush()?;
        if select_checkpoint(&mut best, epoch, &model, score)? {
            selected = Some(record);
        }
        if epoch % 20 == 0 {
            eprintln!(
                "Epoch {epoch}: train loss {loss:.6}, dev macro top1 {:.6}, MRR {:.6}",
                score.0, score.1
            );
        }
        if epoch < neural::EPOCHS {
            adam.update(&mut model, &gradient)?;
        }
    }
    let selected = selected.context("no checkpoint")?;
    let best = best.context("missing selected model")?;
    let weights: Values = serde_json::from_value(selected["weights"].clone())?;
    ensure!(
        weights == best.model.named_weights(&families) && selected["epoch"] == best.epoch,
        "selected checkpoint replay differs"
    );
    let selection = FrozenSelection {
        schema: SELECTION_SCHEMA.into(),
        selected_name: format!(
            "neural_epoch_{:03}",
            selected["epoch"].as_u64().context("missing epoch")?
        ),
        weights,
        selection_author_ids: author_set,
        source_space_id: space.id.clone(),
        feature_schema: space.feature_schema.clone(),
        parser_identity: space.parser_identity.clone(),
    };
    selection.validate_reference_authors(&space)?;
    let mut output = serde_json::to_value(selection)?;
    output["model_schema"] = json!("unslop-positive-family-network-v1");
    output["selected_checkpoint"] = selected;
    output["selection_rule"] = json!(
        "original-development macro-author top1, then MRR, then earliest epoch including epoch 0; no original-test or fresh-author inputs"
    );
    output["input_sha256"] = bindings;
    output["training_post_count"] = json!(tensor.len());
    output["development_query_count"] = json!(dev.len());
    output["training_tensor_sha256"] =
        json!(digest(&fs::read(args.out.join("training-tensor.json"))?));
    output["epochs_sha256"] = json!(digest(&fs::read(args.out.join("epochs.jsonl"))?));
    output["executed_binary_sha256"] = json!(digest(&fs::read(std::env::current_exe()?)?));
    output["source_sha256"] = json!({
        "neural.rs":digest(include_bytes!("../neural.rs")),
        "unslop-learn-weights.rs":digest(include_bytes!("unslop-learn-weights.rs"))
    });
    write(&args.out.join("selection.json"), &output)?;
    println!(
        "Selected {}: dev macro top1 {:.6}, MRR {:.6}",
        output["selected_name"], best.score.0, best.score.1
    );
    Ok(())
}

fn main() -> Result<()> {
    run(Args::parse())
}
