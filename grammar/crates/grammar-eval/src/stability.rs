//! Matched, training-only grammar diagnostics. Content overlap is a proxy, not
//! a topic annotation. All control selection precedes grammar comparisons.
use anyhow::{Context, Result, ensure};
use grammar_core::{
    features::{self, Features},
    space::Space,
    syntax::{self, Document},
};
use grammar_eval::{
    DistanceGeometry, Profile, Values, cosine_distance, document_values, family_distances, fit_idf,
    tfidf, validate_date,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

const PROTOCOL: &str = "unslop-grammar-stability-v1";
const SEED: &str = "unslop-grammar-stability-pairs-v1";
const MAX_PAIRS: usize = 32;
const REPLICATES: usize = 2000;

#[derive(Deserialize)]
struct CachedFeatures {
    features: Features,
    content_lemma_counts: BTreeMap<String, u64>,
}

struct Post {
    metadata: Value,
    id: String,
    author: String,
    date: String,
    words: u64,
    grammar: Profile,
    content_counts: BTreeMap<String, u64>,
    content: Values,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Estimate {
    pub pairs: usize,
    pub authors: usize,
    pub micro: f64,
    pub macro_author: f64,
    pub macro_author_bootstrap_95: [f64; 2],
    pub per_author: BTreeMap<String, f64>,
}

#[derive(Serialize)]
struct Pair {
    anchor: String,
    positive: String,
    anchor_author: String,
    same_content_distance: f64,
    same_length_ratio: f64,
    content_distance_bin: usize,
    length_ratio_bin: usize,
    overlap_group: String,
    candidate_control_count: usize,
    control: Option<String>,
    control_author: Option<String>,
    control_content_distance: Option<f64>,
    control_length_ratio: Option<f64>,
    same_squared_family_distances: Option<Values>,
    control_squared_family_distances: Option<Values>,
    family_concordance: Option<Values>,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn keyed_hash(parts: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in std::iter::once(SEED).chain(parts.iter().copied()) {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    digest.finalize().into()
}

fn read_bound(path: &Path, bindings: &mut Value) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    bindings[path.display().to_string()] = json!(hash(&bytes));
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}

fn count(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .with_context(|| format!("missing count {key}"))
}

fn within(root: &Path, base: &Path, relative: &str) -> Result<std::path::PathBuf> {
    ensure!(!Path::new(relative).is_absolute(), "absolute corpus path");
    let path = base.join(relative).canonicalize()?;
    ensure!(path.starts_with(root), "corpus path escapes export");
    Ok(path)
}

fn content_counts(document: &Document) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for token in &document.tokens {
        if matches!(
            token.pos.as_str(),
            "NOUN" | "PROPN" | "VERB" | "ADJ" | "ADV"
        ) && !token.is_punct
            && !token.is_space
        {
            for lemma in features::words(&token.lemma) {
                *counts.entry(lemma).or_default() += 1;
            }
        }
    }
    counts
}

/// Re-read only training text and caches. Nontraining manifest metadata is read
/// solely to verify the original manifest's hash; its text/cache is never opened.
fn load_training(
    root: &Path,
    evaluation: &Path,
    cache: &Path,
    bindings: &mut Value,
) -> Result<(Vec<Post>, Space, Value)> {
    let old_protocol = read_bound(&evaluation.join("protocol.json"), bindings)?;
    ensure!(
        old_protocol["protocol"] == grammar_eval::PROTOCOL,
        "unsupported prior evaluation"
    );
    let summary = read_bound(&root.join("summary.json"), bindings)?;
    ensure!(
        summary["schema"] == "unslop-blog-author-export-v1",
        "unsupported export"
    );
    ensure!(
        bindings[root.join("summary.json").display().to_string()]
            == old_protocol["input_bindings"]["summary_sha256"],
        "original summary hash changed"
    );
    let space_value = read_bound(&evaluation.join("space.json"), bindings)?;
    let space: Space = serde_json::from_value(space_value)?;
    space.validate()?;
    ensure!(
        space.feature_schema == features::FEATURE_VERSION,
        "feature schema changed"
    );
    ensure!(
        old_protocol["parser_identity"] == space.parser_identity,
        "parser identity differs"
    );
    let content_model = read_bound(&evaluation.join("content-proxy.json"), bindings)?;
    let idf: Values = serde_json::from_value(content_model["idf"].clone())?;
    let audit = read_bound(&evaluation.join("annotation-audit.json"), bindings)?;
    let audit_rows = audit.as_array().context("invalid annotation audit")?;
    let mut statuses = BTreeMap::new();
    for row in audit_rows
        .iter()
        .filter(|row| row["post"]["split"] == "train")
    {
        ensure!(
            statuses
                .insert(string(&row["post"], "id")?.to_owned(), row)
                .is_none(),
            "duplicate training audit ID"
        );
    }
    let mut posts = Vec::new();
    let mut cache_bindings = Vec::new();
    let mut training_rows = 0;
    let mut audit_exclusions = Vec::new();
    let mut observed = BTreeSet::new();
    let mut observed_authors = BTreeSet::new();
    for author in summary["authors"].as_array().context("invalid authors")? {
        let author_id = string(author, "author_id")?;
        ensure!(observed_authors.insert(author_id), "duplicate author ID");
        let relative_manifest = string(author, "manifest")?;
        let manifest = within(root, root, relative_manifest)?;
        let manifest_bytes = fs::read(&manifest)?;
        let manifest_hash = hash(&manifest_bytes);
        let bound = old_protocol["input_bindings"]["manifests"]
            .as_array()
            .context("missing original manifest bindings")?
            .iter()
            .find(|binding| binding["path"] == relative_manifest)
            .context("manifest not bound to original evaluation")?;
        ensure!(
            bound["sha256"] == manifest_hash,
            "original manifest hash changed"
        );
        bindings[manifest.display().to_string()] = json!(manifest_hash);
        for line in std::str::from_utf8(&manifest_bytes)?.lines() {
            let metadata: Value = serde_json::from_str(line)?;
            if metadata["split"] != "train" {
                continue;
            }
            training_rows += 1;
            let id = string(&metadata, "id")?.to_owned();
            ensure!(observed.insert(id.clone()), "duplicate training post ID");
            ensure!(metadata["author_id"] == author_id, "author mismatch");
            let audit = statuses
                .get(&id)
                .context("training post missing from original audit")?;
            // Original Post serialization intentionally drops unused top-level fields.
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
                    audit["post"][key] == metadata[key],
                    "original audit/manifest mismatch: {key}"
                );
            }
            if audit["status"] != "eligible" {
                audit_exclusions.push((*audit).clone());
                continue;
            }
            let provenance = &metadata["provenance"];
            let date = string(provenance, "supplied_composition_date")?.to_owned();
            validate_date(&date)?;
            ensure!(
                metadata["source_group"] == format!("blog:{author_id}:date:{date}"),
                "date group mismatch"
            );
            ensure!(
                provenance["archive_sha256"] == summary["archive_sha256"],
                "archive mismatch"
            );
            let text_path = within(
                root,
                manifest.parent().context("manifest parent")?,
                string(&metadata, "path")?,
            )?;
            let text = fs::read_to_string(text_path)?;
            let source_hash = hash(text.as_bytes());
            ensure!(
                metadata["text_sha256"] == source_hash
                    && provenance["text_sha256"] == source_hash
                    && provenance["excerpt_sha256"] == source_hash,
                "training text hash mismatch"
            );
            let word_count = features::words(&text).len() as u64;
            ensure!(
                count(provenance, "word_count")? == word_count && word_count > 0,
                "word count mismatch"
            );
            let annotation_path = cache.join(format!("{source_hash}.annotation.json"));
            let feature_path = cache.join(format!("{source_hash}.features.json"));
            let annotation_bytes = fs::read(&annotation_path)?;
            let feature_bytes = fs::read(&feature_path)?;
            let annotation: Document = serde_json::from_slice(&annotation_bytes)?;
            ensure!(
                annotation.text == text && annotation.parser_identity == space.parser_identity,
                "annotation identity mismatch"
            );
            syntax::validate(&annotation)?;
            let extracted = features::extract(&annotation)?;
            let cached: CachedFeatures = serde_json::from_slice(&feature_bytes)?;
            ensure!(
                extracted == cached.features && extracted.source_sha256 == source_hash,
                "feature cache mismatch"
            );
            ensure!(
                content_counts(&annotation) == cached.content_lemma_counts,
                "content lemma cache mismatch"
            );
            let mut grammar = document_values(&extracted)?;
            grammar.remove("word");
            grammar.remove("word_bigram");
            cache_bindings.push(json!({"id": id, "text_sha256": source_hash,
                "annotation_sha256": hash(&annotation_bytes), "feature_cache_sha256": hash(&feature_bytes)}));
            let content = tfidf(&cached.content_lemma_counts, &idf);
            posts.push(Post {
                metadata,
                id,
                author: author_id.to_owned(),
                date,
                words: word_count,
                grammar,
                content_counts: cached.content_lemma_counts,
                content,
            });
        }
    }
    ensure!(
        observed.len() == statuses.len(),
        "unmatched original training audit rows"
    );
    ensure!(
        observed_authors.len() as u64 == count(&summary, "selected_authors")?,
        "author count mismatch"
    );
    let train_bindings: BTreeSet<_> = posts
        .iter()
        .map(|p| {
            (
                p.metadata["source_group"].as_str().unwrap().to_owned(),
                p.metadata["text_sha256"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let space_bindings: BTreeSet<_> = space
        .references
        .iter()
        .map(|binding| (binding.source_group.clone(), binding.source_sha256.clone()))
        .collect();
    ensure!(
        space.references.len() == posts.len() && space_bindings == train_bindings,
        "shared space was not fitted to these exact eligible training posts"
    );
    let counts: Vec<_> = posts.iter().map(|p| &p.content_counts).collect();
    ensure!(
        count(&content_model, "training_document_count")? == posts.len() as u64
            && fit_idf(&counts) == idf,
        "saved IDF does not match training-only recomputation"
    );
    posts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((
        posts,
        space,
        json!({"training_manifest_posts": training_rows,
        "original_annotation_exclusions": audit_exclusions, "cache_bindings": cache_bindings}),
    ))
}

fn content_bin(distance: f64) -> usize {
    ((distance.clamp(0.0, 1.0) * 20.0).floor() as usize).min(19)
}

fn length_ratio(left: u64, right: u64) -> f64 {
    left.max(right) as f64 / left.min(right) as f64
}

fn length_bin(ratio: f64) -> usize {
    [1.25, 1.5, 2.0, 3.0]
        .iter()
        .position(|edge| ratio < *edge)
        .unwrap_or(4)
}

fn overlap_group(distance: f64) -> &'static str {
    if distance >= 0.9 {
        "lower_content_overlap"
    } else {
        "higher_content_overlap"
    }
}

fn concordance(same: f64, control: f64) -> f64 {
    if same < control {
        1.0
    } else if same == control {
        0.5
    } else {
        0.0
    }
}

fn distribution(values: &[f64]) -> Value {
    if values.is_empty() {
        return json!({"count": 0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| sorted[((p * sorted.len() as f64).ceil() as usize).saturating_sub(1)];
    json!({"count": values.len(), "mean": values.iter().sum::<f64>() / values.len() as f64,
        "minimum": sorted[0], "p05": percentile(0.05), "median": percentile(0.5),
        "p95": percentile(0.95), "maximum": sorted[sorted.len()-1]})
}

pub fn estimate(rows: &[(String, f64)]) -> Result<Estimate> {
    ensure!(!rows.is_empty(), "cannot summarize zero pairs");
    let mut grouped = BTreeMap::<String, Vec<f64>>::new();
    for (author, value) in rows {
        ensure!(
            value.is_finite() && (0.0..=1.0).contains(value),
            "invalid concordance"
        );
        grouped.entry(author.clone()).or_default().push(*value);
    }
    let per_author: BTreeMap<_, _> = grouped
        .iter()
        .map(|(author, values)| {
            (
                author.clone(),
                values.iter().sum::<f64>() / values.len() as f64,
            )
        })
        .collect();
    let values: Vec<_> = per_author.values().copied().collect();
    let mut state = 0x6a09e667f3bcc909_u64;
    let mut samples = Vec::with_capacity(REPLICATES);
    for _ in 0..REPLICATES {
        let mut sum = 0.0;
        for _ in 0..values.len() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            sum += values[(state % values.len() as u64) as usize];
        }
        samples.push(sum / values.len() as f64);
    }
    samples.sort_by(f64::total_cmp);
    Ok(Estimate {
        pairs: rows.len(),
        authors: values.len(),
        micro: rows.iter().map(|(_, v)| v).sum::<f64>() / rows.len() as f64,
        macro_author: values.iter().sum::<f64>() / values.len() as f64,
        macro_author_bootstrap_95: [samples[49], samples[1949]],
        per_author,
    })
}

pub fn reliability_weights(estimates: &BTreeMap<String, Estimate>) -> Result<(Values, bool)> {
    ensure!(!estimates.is_empty(), "no reliability families");
    let mut weights: Values = estimates
        .iter()
        .map(|(family, value)| (family.clone(), (2.0 * value.macro_author - 1.0).max(0.0)))
        .collect();
    let total = weights.values().sum::<f64>();
    let fallback = total == 0.0;
    let n = weights.len() as f64;
    for value in weights.values_mut() {
        *value = if fallback { 1.0 / n } else { *value / total };
    }
    Ok((weights, fallback))
}

fn protocol() -> Value {
    json!({"protocol": PROTOCOL, "pair_seed": SEED, "max_pairs_per_author": MAX_PAIRS,
        "training_only": "Only original eligible train text, annotations, features, shared Space numerical scales and train-document IDF. Dev/test text, annotation and score files are never opened.",
        "pairs": "All unordered distinct-date pairs within each author; SHA256 length-prefixed seed/author/left-ID/right-ID ordering, first 32. Last digest bit orients anchor and positive.",
        "matching": "Different-author training control shares anchor; same cosine-distance bin and length-ratio bin. Choose minimum SHA256 length-prefixed seed/anchor-ID/positive-ID/control-ID. Controls may be reused. Grammar distances never affect selection.",
        "content_distance_bins": {"width": 0.05, "count": 20, "intervals": "floor(distance*20), last bin includes 1.0"},
        "length_ratio_bins": ["[1,1.25)", "[1.25,1.5)", "[1.5,2)", "[2,3)", "[3,infinity)"],
        "overlap_groups": "lower: cosine similarity <=0.10 (distance >=0.9); higher: similarity >0.10. These are overlap strata, not topic labels.",
        "outcome": "Per-family matched-pair concordance: 1 if same-author squared distance < matched-control squared distance, 0.5 on exact tie, 0 otherwise. Average within anchor author then equally across represented authors. This is AUC-like concordance, not global all-pairs AUC.",
        "uncertainty": {"method": "percentile bootstrap of per-anchor-author averages", "replicates": REPLICATES,
            "seed": "0x6a09e667f3bcc909", "ranks": [50,1950],
            "scope": "Conditional on sampled/matched controls. Reused controls and cross-author dependence are not captured; intervals are descriptive and unadjusted across 12 families."},
        "weights": "r=max(0,2*overall_macro_author_concordance-1), normalize over twelve grammar families; equal weights if all r=0. No selection on strata or uncertainty.",
        "attrition": "Preserve original annotation exclusions; abort on altered bindings, caches or invalid inputs. Exclude content-empty posts before pair selection. Keep every selected matched/unmatched pair and support counts.",
        "limits": ["Content overlap and length matching do not remove all topical, temporal, genre or lexical confounding.",
            "No claim of true cross-topic authorship, semantic fidelity, readability or detector performance."]})
}

pub fn run(export: &Path, evaluation: &Path, cache: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "use a fresh output directory");
    fs::create_dir_all(out)?;
    // Frozen before reading feature values or producing diagnostic outcomes.
    write_json(&out.join("protocol.json"), &protocol())?;
    let root = export.canonicalize()?;
    let mut bindings = json!({});
    let (all_posts, space, training_audit) =
        load_training(&root, evaluation, cache, &mut bindings)?;
    let mut geometry = DistanceGeometry::new(&space);
    geometry.families.remove("word");
    geometry.families.remove("word_bigram");
    ensure!(
        geometry.families.len() == 12,
        "expected current twelve grammar families"
    );
    let content_exclusions: Vec<_> = all_posts
        .iter()
        .filter(|p| p.content.is_empty())
        .map(|p| p.metadata.clone())
        .collect();
    let posts: Vec<_> = all_posts.iter().filter(|p| !p.content.is_empty()).collect();
    let mut grouped = BTreeMap::<String, Vec<usize>>::new();
    for (index, post) in posts.iter().enumerate() {
        grouped.entry(post.author.clone()).or_default().push(index);
    }
    write_json(
        &out.join("training-audit.json"),
        &json!({"original": training_audit,
        "content_empty_exclusions": content_exclusions,
        "eligible_training_bindings": posts.iter().map(|p| &p.metadata).collect::<Vec<_>>()}),
    )?;
    write_json(&out.join("input-bindings.json"), &bindings)?;
    eprintln!(
        "Validated {} training posts from {} authors; selecting matched pairs",
        posts.len(),
        grouped.len()
    );
    let mut selected = Vec::new();
    let mut author_support = BTreeMap::new();
    for (author, indices) in &grouped {
        let mut candidates = Vec::new();
        let mut same_date = 0;
        for (offset, &left) in indices.iter().enumerate() {
            for &right in &indices[offset + 1..] {
                if posts[left].date == posts[right].date {
                    same_date += 1;
                    continue;
                }
                let key = keyed_hash(&[author, &posts[left].id, &posts[right].id]);
                let (anchor, positive) = if key[31] & 1 == 0 {
                    (left, right)
                } else {
                    (right, left)
                };
                candidates.push((key, anchor, positive));
            }
        }
        candidates.sort_by_key(|row| row.0);
        let eligible = candidates.len();
        candidates.truncate(MAX_PAIRS);
        author_support.insert(author.clone(), json!({"training_posts": indices.len(),
            "distinct_dates": indices.iter().map(|&i| &posts[i].date).collect::<BTreeSet<_>>().len(),
            "same_date_pairs_excluded": same_date, "distinct_date_pairs_available": eligible,
            "pairs_selected": candidates.len(), "pairs_matched": 0}));
        selected.extend(candidates.into_iter().map(|(_, a, b)| (a, b)));
    }
    // Cache all content comparisons needed for anchors, once, before grammar scores.
    let anchors: BTreeSet<_> = selected.iter().map(|(a, _)| *a).collect();
    let mut content_distances = BTreeMap::new();
    for anchor in anchors {
        let distances: Vec<_> = posts
            .iter()
            .map(|post| cosine_distance(&posts[anchor].content, &post.content))
            .collect();
        content_distances.insert(anchor, distances);
    }
    let mut matched = Vec::new();
    for (anchor, positive) in selected {
        let a = posts[anchor];
        let p = posts[positive];
        let distance = content_distances[&anchor][positive];
        let ratio = length_ratio(a.words, p.words);
        let cbin = content_bin(distance);
        let lbin = length_bin(ratio);
        let mut controls: Vec<_> = posts
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                c.author != a.author
                    && content_bin(content_distances[&anchor][*i]) == cbin
                    && length_bin(length_ratio(a.words, c.words)) == lbin
            })
            .map(|(i, c)| (keyed_hash(&[&a.id, &p.id, &c.id]), i))
            .collect();
        controls.sort_by_key(|row| row.0);
        let control = controls.first().map(|(_, i)| *i);
        matched.push((
            anchor,
            positive,
            control,
            Pair {
                anchor: a.id.clone(),
                positive: p.id.clone(),
                anchor_author: a.author.clone(),
                same_content_distance: distance,
                same_length_ratio: ratio,
                content_distance_bin: cbin,
                length_ratio_bin: lbin,
                overlap_group: overlap_group(distance).to_owned(),
                candidate_control_count: controls.len(),
                control: control.map(|i| posts[i].id.clone()),
                control_author: control.map(|i| posts[i].author.clone()),
                control_content_distance: control.map(|i| content_distances[&anchor][i]),
                control_length_ratio: control.map(|i| length_ratio(a.words, posts[i].words)),
                same_squared_family_distances: None,
                control_squared_family_distances: None,
                family_concordance: None,
            },
        ));
    }
    write_json(
        &out.join("selected-controls.json"),
        &matched
            .iter()
            .map(|(_, _, _, pair)| pair)
            .collect::<Vec<_>>(),
    )?;
    eprintln!(
        "Selected {} pairs, {} matched; computing grammar concordance",
        matched.len(),
        matched.iter().filter(|(_, _, c, _)| c.is_some()).count()
    );
    let mut rows = BTreeMap::<String, BTreeMap<String, Vec<(String, f64)>>>::new();
    let mut controls_used = BTreeMap::<String, usize>::new();
    let mut bin_support = BTreeMap::<String, Value>::new();
    let mut magnitude_rows = BTreeMap::<String, (Vec<f64>, Vec<f64>)>::new();
    let mut content_residuals = Vec::new();
    let mut length_residuals = Vec::new();
    let mut pairs_file = BufWriter::new(File::create(out.join("pairs.jsonl"))?);
    for (anchor, positive, control, mut pair) in matched {
        let key = format!(
            "content-{:02}_length-{}",
            pair.content_distance_bin, pair.length_ratio_bin
        );
        let support = bin_support
            .entry(key)
            .or_insert(json!({"selected": 0, "matched": 0}));
        support["selected"] = json!(support["selected"].as_u64().unwrap() + 1);
        if let Some(control) = control {
            support["matched"] = json!(support["matched"].as_u64().unwrap() + 1);
            let author_count =
                &mut author_support.get_mut(&pair.anchor_author).unwrap()["pairs_matched"];
            *author_count = json!(author_count.as_u64().unwrap() + 1);
            *controls_used.entry(posts[control].id.clone()).or_default() += 1;
            let same =
                family_distances(&geometry, &posts[anchor].grammar, &posts[positive].grammar)?;
            let different =
                family_distances(&geometry, &posts[anchor].grammar, &posts[control].grammar)?;
            content_residuals
                .push(pair.control_content_distance.unwrap() - pair.same_content_distance);
            length_residuals.push(pair.control_length_ratio.unwrap() - pair.same_length_ratio);
            for (family, distance) in &same {
                let values = magnitude_rows.entry(family.clone()).or_default();
                values.0.push(*distance);
                values.1.push(different[family]);
            }
            let scores: Values = same
                .iter()
                .map(|(family, value)| (family.clone(), concordance(*value, different[family])))
                .collect();
            for group in ["overall", pair.overlap_group.as_str()] {
                for (family, value) in &scores {
                    rows.entry(group.to_owned())
                        .or_default()
                        .entry(family.clone())
                        .or_default()
                        .push((pair.anchor_author.clone(), *value));
                }
            }
            pair.same_squared_family_distances = Some(same);
            pair.control_squared_family_distances = Some(different);
            pair.family_concordance = Some(scores);
        }
        serde_json::to_writer(&mut pairs_file, &pair)?;
        pairs_file.write_all(b"\n")?;
    }
    pairs_file.flush()?;
    let mut estimates = BTreeMap::new();
    for (group, families) in rows {
        let summaries: BTreeMap<String, Estimate> = families
            .iter()
            .map(|(family, rows)| Ok((family.clone(), estimate(rows)?)))
            .collect::<Result<_>>()?;
        estimates.insert(group, summaries);
    }
    let overall = estimates.get("overall").context("no matched pairs")?;
    ensure!(overall.len() == 12, "incomplete grammar families");
    let (weights, equal_fallback) = reliability_weights(overall)?;
    let magnitude_summaries: BTreeMap<_,_> = magnitude_rows.iter().map(|(family,(same,control))|
        (family.clone(),json!({"same_author": distribution(same), "matched_control": distribution(control),
            "units": if matches!(geometry.families[family], grammar_core::space::Kind::Distribution) {
                "squared Hellinger distance, range [0,1]" } else {
                "mean squared difference divided by original training numerical scales, unbounded" }}))).collect();
    let absolute_content: Vec<_> = content_residuals.iter().map(|v| v.abs()).collect();
    let absolute_length: Vec<_> = length_residuals.iter().map(|v| v.abs()).collect();
    write_json(
        &out.join("grammar-reliability-weights.json"),
        &json!({"schema": PROTOCOL,
        "space_id": space.id, "weights": weights, "equal_fallback": equal_fallback,
        "input_bindings_sha256": hash(&fs::read(out.join("input-bindings.json"))?),
        "protocol_sha256": hash(&fs::read(out.join("protocol.json"))?)}),
    )?;
    write_json(
        &out.join("report.json"),
        &json!({"protocol": PROTOCOL, "space_id": space.id, "source_space_id": space.id,
        "training_author_ids": grouped.keys().collect::<Vec<_>>(), "source_author_ids": grouped.keys().collect::<Vec<_>>(),
        "feature_schema": space.feature_schema, "parser_identity": space.parser_identity,
        "eligible_training_posts": posts.len(), "eligible_authors": grouped.len(),
        "content_empty_posts": content_exclusions.len(), "author_support": author_support,
        "fine_bin_support": bin_support, "distinct_control_posts": controls_used.len(),
        "control_post_reuse": controls_used, "estimates": estimates,
        "squared_family_distance_magnitudes": magnitude_summaries,
        "matching_residuals": {"definition": "control minus same-author pair, sharing anchor",
            "content_distance": distribution(&content_residuals), "absolute_content_distance": distribution(&absolute_content),
            "length_ratio": distribution(&length_residuals), "absolute_length_ratio": distribution(&absolute_length)},
        "reliability_weights": weights, "equal_weight_fallback": equal_fallback,
        "input_bindings_sha256": hash(&fs::read(out.join("input-bindings.json"))?)}),
    )?;
    let mut summary = String::from(
        "# Training grammar stability\n\nContent overlap is a topic proxy. Each comparison uses an anchor and a same-author post from another date, with a different-author control in the same content-distance and length-ratio bins. Scores are macro-author matched-pair concordance; 50% represents no ordering advantage. They are not global AUC or proof of independence from topic.\n\n| Family | Concordance | Author bootstrap 95% | Grammar weight |\n|---|---:|---:|---:|\n",
    );
    for (family, estimate) in overall {
        summary.push_str(&format!(
            "| {family} | {:.2}% | {:.2}–{:.2}% | {:.4} |\n",
            100.0 * estimate.macro_author,
            100.0 * estimate.macro_author_bootstrap_95[0],
            100.0 * estimate.macro_author_bootstrap_95[1],
            weights[family]
        ));
    }
    let sample = overall.values().next().unwrap();
    summary.push_str(&format!("\n{} matched pairs from {} authors; {} eligible training posts. Equal-weight fallback: {equal_fallback}. No dev/test text or score files were read. See report.json for strata, attrition, per-author values, and control reuse; pairs.jsonl records every selected comparison.\n\nThe intervals resample anchor authors and are conditional on the sampled controls; reuse of controls and dependence between authors are not captured. They are descriptive and unadjusted for comparisons across twelve families. The reliability formula was fixed before outcomes and does not select features by confidence interval or overlap stratum.\n", sample.pairs, sample.authors, posts.len()));
    fs::write(out.join("README.md"), summary)?;
    eprintln!(
        "Grammar stability complete: {}",
        out.join("report.json").display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_boundaries_and_exact_ties_are_explicit() {
        assert_eq!(content_bin(0.0), 0);
        assert_eq!(content_bin(0.049), 0);
        assert_eq!(content_bin(0.05), 1);
        assert_eq!(content_bin(1.0), 19);
        assert_eq!(length_bin(1.249), 0);
        assert_eq!(length_bin(1.25), 1);
        assert_eq!(length_bin(3.0), 4);
        assert_eq!(overlap_group(0.9), "lower_content_overlap");
        assert_eq!(concordance(1.0, 1.0), 0.5);
        assert_eq!(concordance(0.2, 0.3), 1.0);
        assert_eq!(concordance(0.3, 0.2), 0.0);
    }

    #[test]
    fn author_macro_is_not_pair_weighted_and_bootstrap_is_repeatable() {
        let rows = [
            ("a".into(), 1.0),
            ("a".into(), 1.0),
            ("a".into(), 1.0),
            ("b".into(), 0.0),
        ];
        let summary = estimate(&rows).unwrap();
        assert_eq!(summary.micro, 0.75);
        assert_eq!(summary.macro_author, 0.5);
        assert_eq!(summary, estimate(&rows).unwrap());
        assert_eq!(summary.macro_author_bootstrap_95, [0.0, 1.0]);
    }

    #[test]
    fn reliability_is_nonnegative_and_has_an_equal_fallback() {
        let make = |v| estimate(&[("a".into(), v)]).unwrap();
        let estimates = BTreeMap::from([
            ("a".into(), make(0.4)),
            ("b".into(), make(0.6)),
            ("c".into(), make(0.8)),
        ]);
        let (weights, fallback) = reliability_weights(&estimates).unwrap();
        assert!(!fallback);
        assert_eq!(weights["a"], 0.0);
        assert!((weights["b"] - 0.25).abs() < 1e-12);
        assert!((weights["c"] - 0.75).abs() < 1e-12);
        let (weights, fallback) = reliability_weights(&BTreeMap::from([
            ("a".into(), make(0.4)),
            ("b".into(), make(0.5)),
        ]))
        .unwrap();
        assert!(fallback);
        assert_eq!(
            weights,
            BTreeMap::from([("a".into(), 0.5), ("b".into(), 0.5)])
        );
    }

    #[test]
    fn length_prefixed_hashes_do_not_confuse_partitioned_ids() {
        assert_ne!(keyed_hash(&["ab", "c"]), keyed_hash(&["a", "bc"]));
        assert_eq!(keyed_hash(&["a", "b", "c"]), keyed_hash(&["a", "b", "c"]));
    }
}
