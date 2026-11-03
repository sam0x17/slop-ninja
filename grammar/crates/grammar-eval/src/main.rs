use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{
    features::{self, Features},
    space::{self, Config, Reference},
    syntax::{self, Document},
};
use grammar_eval::weights::{FrozenSelection, paired_author_difference};
use grammar_eval::{
    DATE_POLICY, DistanceGeometry, Metrics, PROTOCOL, Profile, Values, ablations, cosine_distance,
    document_values, family_distances, fit_idf, grouped_profile, normalize, query_metrics, rank,
    summarize, tfidf, validate_splits, weighted_distance,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Evaluate frozen grammar and word author profiles on chronological blog splits")]
struct Args {
    /// Directory produced by unslop-corpus export-authors.
    export_dir: PathBuf,
    /// New or resumable output directory; corpus-derived artifacts stay local.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "python3")]
    python: String,
    #[arg(long, default_value_t = 32)]
    batch_size: usize,
    /// Optional shared annotation cache. Identity and exact text are revalidated.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Freeze old numerical scales and extend only its categorical vocabulary.
    #[arg(long, requires = "selection")]
    reference_space: Option<PathBuf>,
    /// Development-selected weights; evaluation authors must be disjoint.
    #[arg(long, requires = "reference_space")]
    selection: Option<PathBuf>,
    /// Exclude whole authors lacking valid split/date support, with an audit.
    #[arg(long)]
    allow_author_exclusions: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Post {
    id: String,
    author_id: String,
    source_group: String,
    split: String,
    path: String,
    text_sha256: String,
    provenance: Value,
}

#[derive(Debug)]
struct Input {
    post: Post,
    text: String,
}

fn unsupported_authors<'a>(
    posts: impl Iterator<Item = &'a Post>,
    authors: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let posts: Vec<_> = posts.collect();
    let mut out = BTreeMap::new();
    for author in authors {
        let mine: Vec<_> = posts
            .iter()
            .filter(|post| &post.author_id == author)
            .collect();
        let mut reasons = Vec::new();
        for split in ["train", "dev", "test"] {
            if !mine.iter().any(|post| post.split == split) {
                reasons.push(format!("no eligible {split} posts"));
            }
        }
        let dates: BTreeSet<_> = mine
            .iter()
            .filter(|post| post.split == "train")
            .map(|post| &post.source_group)
            .collect();
        if dates.len() < 2 {
            reasons.push(format!(
                "{} eligible training dates; need at least two",
                dates.len()
            ));
        }
        if !reasons.is_empty() {
            out.insert(author.clone(), reasons);
        }
    }
    out
}

#[derive(Serialize, Deserialize)]
struct CachedFeatures {
    features: Features,
    content_lemma_counts: BTreeMap<String, u64>,
}

struct Parsed {
    input: Input,
    features: Features,
    content: BTreeMap<String, u64>,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(
        File::open(path).with_context(|| format!("open {}", path.display()))?,
    ))
    .with_context(|| format!("read {}", path.display()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut writer = BufWriter::new(File::create(&temporary)?);
    serde_json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    drop(writer);
    fs::rename(temporary, path)?;
    Ok(())
}

fn str_field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .with_context(|| format!("missing string field {name}"))
}

fn count(value: &Value, name: &str) -> Result<u64> {
    value[name]
        .as_u64()
        .with_context(|| format!("missing count field {name}"))
}

fn within(root: &Path, base: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(!Path::new(relative).is_absolute(), "absolute corpus path");
    let path = base.join(relative).canonicalize()?;
    ensure!(
        path.starts_with(root),
        "corpus path escapes export directory"
    );
    Ok(path)
}

/// Check hashes, complete counts, global duplicates, date groups and chronology
/// before any annotation or fitting. Do not silently accept damaged manifests.
fn load_export(root: &Path) -> Result<(Value, Vec<Input>, Value)> {
    let summary_path = root.join("summary.json");
    let summary_bytes = fs::read(&summary_path)?;
    let summary: Value = serde_json::from_slice(&summary_bytes)?;
    ensure!(
        str_field(&summary, "schema")? == "unslop-blog-author-export-v1",
        "unsupported export schema"
    );
    let authors = summary["authors"].as_array().context("missing authors")?;
    ensure!(
        authors.len() as u64 == count(&summary, "selected_authors")? && authors.len() >= 2,
        "invalid selected author count"
    );
    let mut seen_authors = BTreeSet::new();
    let mut exact_hashes = BTreeSet::new();
    let mut duplicate_hashes = BTreeSet::new();
    let mut post_ids = BTreeSet::new();
    let mut inputs = Vec::new();
    let mut manifests = Vec::new();
    let mut all_words = 0;
    for author in authors {
        let id = str_field(author, "author_id")?;
        ensure!(seen_authors.insert(id), "duplicate author ID");
        let path = within(root, root, str_field(author, "manifest")?)?;
        let bytes = fs::read(&path)?;
        manifests.push(json!({ "path": str_field(author, "manifest")?, "sha256": hash(&bytes) }));
        let start = inputs.len();
        let mut author_words = 0;
        let mut split_posts = BTreeMap::<String, u64>::new();
        let mut split_words = BTreeMap::<String, u64>::new();
        for (line_number, line) in BufReader::new(bytes.as_slice()).lines().enumerate() {
            let line = line?;
            ensure!(!line.trim().is_empty(), "blank manifest line");
            let post: Post = serde_json::from_str(&line)
                .with_context(|| format!("{}:{}", path.display(), line_number + 1))?;
            ensure!(post.author_id == id, "manifest author mismatch");
            ensure!(post_ids.insert(post.id.clone()), "duplicate post ID");
            let date = str_field(&post.provenance, "supplied_composition_date")?;
            ensure!(
                post.source_group == format!("blog:{id}:date:{date}"),
                "source date-group mismatch"
            );
            ensure!(
                str_field(&post.provenance, "archive_sha256")?
                    == str_field(&summary, "archive_sha256")?,
                "archive identity mismatch"
            );
            let text_path = within(
                root,
                path.parent().context("manifest has no parent")?,
                &post.path,
            )?;
            let text = fs::read_to_string(&text_path)?;
            let actual_hash = hash(text.as_bytes());
            ensure!(
                actual_hash == post.text_sha256
                    && str_field(&post.provenance, "text_sha256")? == actual_hash
                    && str_field(&post.provenance, "excerpt_sha256")? == actual_hash,
                "post text hash mismatch"
            );
            ensure!(
                exact_hashes.insert(actual_hash),
                "duplicate exact text across selected posts/splits"
            );
            let normalized = hash(
                text.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .as_bytes(),
            );
            ensure!(
                str_field(&post.provenance, "normalized_duplicate_sha256")? == normalized,
                "normalized hash mismatch"
            );
            ensure!(
                duplicate_hashes.insert(normalized),
                "duplicate normalized text across selected posts/splits"
            );
            let words = features::words(&text).len() as u64;
            ensure!(
                count(&post.provenance, "word_count")? == words,
                "post word count mismatch"
            );
            author_words += words;
            *split_posts.entry(post.split.clone()).or_default() += 1;
            *split_words.entry(post.split.clone()).or_default() += words;
            inputs.push(Input { post, text });
        }
        ensure!(
            (inputs.len() - start) as u64 == count(author, "selected_posts")?,
            "author post count mismatch"
        );
        ensure!(
            author_words == count(author, "selected_words")?,
            "author word count mismatch"
        );
        ensure!(
            serde_json::to_value(&split_posts)? == author["split_posts"]
                && serde_json::to_value(&split_words)? == author["split_words"],
            "author split counts mismatch"
        );
        let dates: Vec<_> = inputs[start..]
            .iter()
            .map(|input| {
                Ok((
                    str_field(&input.post.provenance, "supplied_composition_date")?,
                    input.post.split.as_str(),
                ))
            })
            .collect::<Result<_>>()?;
        validate_splits(&dates)?;
        all_words += author_words;
    }
    ensure!(
        inputs.len() as u64 == count(&summary, "selected_posts")?,
        "total selected posts mismatch"
    );
    ensure!(
        all_words == count(&summary, "selected_words")?,
        "total selected words mismatch"
    );
    inputs.sort_by(|a, b| a.post.id.cmp(&b.post.id));
    let bindings = json!({"summary_sha256": hash(&summary_bytes), "manifests": manifests});
    Ok((summary, inputs, bindings))
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

fn prepare(document: &Document, input: &Input, identity: &str) -> Result<CachedFeatures> {
    ensure!(
        document.text == input.text,
        "cached annotation text differs from input"
    );
    ensure!(
        document.parser_identity == identity,
        "parser identity changed or stale annotation cache"
    );
    syntax::validate(document)?;
    let features = features::extract(document)?;
    ensure!(
        features.source_sha256 == input.post.text_sha256,
        "feature/source mismatch"
    );
    Ok(CachedFeatures {
        features,
        content_lemma_counts: content_counts(document),
    })
}

fn annotate(
    inputs: Vec<Input>,
    args: &Args,
    parser_identity: &str,
    cache: &Path,
) -> Result<(Vec<Parsed>, Vec<Value>)> {
    fs::create_dir_all(cache)?;
    let mut parsed = Vec::new();
    let mut audit = Vec::new();
    let mut inputs = inputs.into_iter();
    let total = inputs.len();
    loop {
        let batch: Vec<_> = inputs.by_ref().take(args.batch_size).collect();
        if batch.is_empty() {
            break;
        }
        let mut documents: Vec<Option<std::result::Result<Document, String>>> =
            (0..batch.len()).map(|_| None).collect();
        let mut missing = Vec::new();
        for (index, input) in batch.iter().enumerate() {
            let path = cache.join(format!("{}.annotation.json", input.post.text_sha256));
            if path.exists() {
                documents[index] =
                    Some(read_json(&path).map_err(|e| format!("invalid annotation cache: {e:#}")));
            } else {
                missing.push(index);
            }
        }
        if !missing.is_empty() {
            let texts: Vec<_> = missing
                .iter()
                .map(|&index| batch[index].text.as_str())
                .collect();
            let results = grammar_spacy::parse_batch(&texts, &args.python);
            match results {
                Ok(results) => {
                    ensure!(
                        results.len() == missing.len(),
                        "batch parser result count mismatch"
                    );
                    for (index, result) in missing.into_iter().zip(results) {
                        documents[index] = Some(result);
                    }
                }
                Err(error) => {
                    for index in missing {
                        documents[index] = Some(Err(format!("batch transport failure: {error:#}")));
                    }
                }
            }
        }
        for (input, result) in batch.into_iter().zip(documents) {
            let result = result.context("missing parser result")?;
            let prepared = result.map_err(anyhow::Error::msg).and_then(|document| {
                let prepared = prepare(&document, &input, parser_identity)?;
                let annotation_path =
                    cache.join(format!("{}.annotation.json", input.post.text_sha256));
                let features_path = cache.join(format!("{}.features.json", input.post.text_sha256));
                if !annotation_path.exists() {
                    write_json(&annotation_path, &document)?;
                }
                if features_path.exists() {
                    let cached: CachedFeatures = read_json(&features_path)?;
                    ensure!(
                        cached.features == prepared.features
                            && cached.content_lemma_counts == prepared.content_lemma_counts,
                        "cached features differ from annotation re-extraction"
                    );
                } else {
                    write_json(&features_path, &prepared)?;
                }
                // Preserve valid annotations and raw features even when a
                // family is unavailable. Such posts are excluded everywhere.
                document_values(&prepared.features)?;
                Ok(prepared)
            });
            match prepared {
                Ok(prepared) => {
                    audit.push(json!({"post": input.post, "status": "eligible", "annotation": format!("{}.annotation.json", input.post.text_sha256), "features": format!("{}.features.json", input.post.text_sha256)}));
                    parsed.push(Parsed { input, features: prepared.features, content: prepared.content_lemma_counts });
                }
                Err(error) => audit.push(json!({"post": input.post, "status": "excluded_all_variants", "error": format!("{error:#}")})),
            }
        }
        write_json(&args.out.join("annotation-audit.json"), &audit)?;
        eprintln!(
            "Annotated {}/{} posts; {} eligible",
            audit.len(),
            total,
            parsed.len()
        );
    }
    Ok((parsed, audit))
}

fn content_profiles(posts: &[Parsed], idf: &Values) -> BTreeMap<String, Values> {
    let mut groups: BTreeMap<&str, BTreeMap<&str, Vec<Values>>> = BTreeMap::new();
    for post in posts.iter().filter(|post| post.input.post.split == "train") {
        groups
            .entry(&post.input.post.author_id)
            .or_default()
            .entry(&post.input.post.source_group)
            .or_default()
            .push(tfidf(&post.content, idf));
    }
    groups
        .into_iter()
        .map(|(author, dates)| {
            let mut profile = Values::new();
            for documents in dates.values() {
                for document in documents {
                    for (key, value) in document {
                        *profile.entry(key.clone()).or_default() +=
                            value / dates.len() as f64 / documents.len() as f64;
                    }
                }
            }
            normalize(&mut profile);
            (author.to_owned(), profile)
        })
        .collect()
}

fn evaluate_split(
    split: &str,
    posts: &[Parsed],
    profiles: &BTreeMap<String, Profile>,
    geometry: &DistanceGeometry,
    variants: &BTreeMap<String, Values>,
    content_model: (&Values, &BTreeMap<String, Values>),
    out: &Path,
) -> Result<Value> {
    let (idf, content_profiles) = content_model;
    let mut score_file = BufWriter::new(File::create(out.join(format!("{split}-queries.jsonl")))?);
    let mut rows: BTreeMap<String, Vec<(String, Metrics)>> = BTreeMap::new();
    let mut missing_content_queries = 0;
    let mut query_count = 0;
    for post in posts.iter().filter(|post| post.input.post.split == split) {
        let content_query = tfidf(&post.content, idf);
        if content_query.is_empty() {
            missing_content_queries += 1;
        }
        let content_ranking = rank(
            content_profiles
                .iter()
                .map(|(author, profile)| (author.clone(), cosine_distance(&content_query, profile)))
                .collect(),
        )?;
        let impostor = content_ranking
            .iter()
            .find(|row| row.author_id != post.input.post.author_id)
            .context("no candidate impostor")?
            .author_id
            .clone();
        let impostor_distance = content_ranking
            .iter()
            .find(|row| row.author_id == impostor)
            .unwrap()
            .distance;
        let tied_impostors: Vec<_> = content_ranking
            .iter()
            .filter(|row| {
                row.author_id != post.input.post.author_id && row.distance == impostor_distance
            })
            .map(|row| &row.author_id)
            .collect();
        let query = document_values(&post.features)?;
        let family_scores: BTreeMap<_, _> = profiles
            .iter()
            .map(|(author, profile)| {
                Ok((author.clone(), family_distances(geometry, &query, profile)?))
            })
            .collect::<Result<_>>()?;
        let mut rankings = BTreeMap::new();
        let content_metrics =
            query_metrics(&content_ranking, &post.input.post.author_id, &impostor)?;
        rows.entry("content_lemma_tfidf".into())
            .or_default()
            .push((post.input.post.author_id.clone(), content_metrics));
        rankings.insert(
            "content_lemma_tfidf".to_owned(),
            json!({"metrics": content_metrics, "ranked_authors": content_ranking}),
        );
        for (variant, weights) in variants {
            let ranking = rank(
                family_scores
                    .iter()
                    .map(|(author, distances)| {
                        (author.clone(), weighted_distance(distances, weights))
                    })
                    .collect(),
            )?;
            let metrics = query_metrics(&ranking, &post.input.post.author_id, &impostor)?;
            rows.entry(variant.clone())
                .or_default()
                .push((post.input.post.author_id.clone(), metrics));
            rankings.insert(
                variant.clone(),
                json!({"metrics": metrics, "ranked_authors": ranking}),
            );
        }
        let output = json!({"post": post.input.post, "content_proxy_available": !content_query.is_empty(), "content_matched_impostor": impostor, "equally_close_content_impostors": tied_impostors, "rankings": rankings, "unweighted_squared_family_distances": family_scores});
        serde_json::to_writer(&mut score_file, &output)?;
        score_file.write_all(b"\n")?;
        query_count += 1;
        if query_count % 25 == 0 {
            eprintln!("Scored {query_count} {split} queries");
        }
    }
    score_file.flush()?;
    let summaries: BTreeMap<_, _> = rows
        .into_iter()
        .map(|(variant, values)| Ok((variant, summarize(&values)?)))
        .collect::<Result<_>>()?;
    let mut paired = BTreeMap::new();
    if let Some(selected) = summaries.get("selected") {
        for baseline in ["word", "combined", "selected_lexical"] {
            let Some(reference) = summaries.get(baseline) else {
                continue;
            };
            paired.insert(
                format!("selected_minus_{baseline}"),
                paired_author_difference(&selected.per_author, &reference.per_author)?,
            );
        }
    }
    Ok(
        json!({"query_count": query_count, "content_proxy_unavailable_queries": missing_content_queries, "variants": summaries, "paired_differences":paired}),
    )
}

fn run(args: Args) -> Result<()> {
    ensure!(args.batch_size > 0, "batch size must be positive");
    let root = args.export_dir.canonicalize()?;
    let (summary, inputs, bindings) = load_export(&root)?;
    let mut author_ids: BTreeSet<String> = summary["authors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|author| author["author_id"].as_str().unwrap().to_owned())
        .collect();
    let frozen: Option<(space::Space, FrozenSelection, Value)> = match (
        &args.reference_space,
        &args.selection,
    ) {
        (Some(path), Some(selection_path)) => {
            let reference: space::Space = read_json(path)?;
            reference.validate()?;
            let selection: FrozenSelection = read_json(selection_path)?;
            selection.validate(
                &reference.family_schemas.keys().cloned().collect(),
                &author_ids,
            )?;
            selection.validate_reference_authors(&reference)?;
            ensure!(
                selection.source_space_id == reference.id
                    && selection.feature_schema == reference.feature_schema
                    && selection.parser_identity == reference.parser_identity,
                "frozen selection and reference space identity differ"
            );
            let binding = json!({"source_space_id":reference.id,"source_space_sha256":hash(&fs::read(path)?),"selection_sha256":hash(&fs::read(selection_path)?),"selected_name":selection.selected_name,"weights":selection.weights,"selection_author_ids":selection.selection_author_ids});
            Some((reference, selection, binding))
        }
        (None, None) => None,
        _ => anyhow::bail!("reference space and selection must be supplied together"),
    };
    eprintln!(
        "Validated {} posts from {} authors before annotation",
        inputs.len(),
        summary["selected_authors"]
    );
    fs::create_dir_all(&args.out)?;
    let probe = grammar_spacy::parse(
        "A careful reader compares the writing samples.",
        &args.python,
    )?;
    let parser_identity = probe.parser_identity;
    let cache = args.cache.clone().unwrap_or_else(|| args.out.join("cache"));
    let mut protocol = json!({
        "protocol": PROTOCOL,
        "feature_schema": features::FEATURE_VERSION,
        "geometry": space::GEOMETRY,
        "parser_identity": parser_identity,
        "input_bindings": bindings,
        "fitting": "one shared space; only train date-group means fit target and numerical scales; author means use equal date-group then equal within-date post weights",
        "query_basis": "vocabulary-only transductive basis: dev and test feature keys enumerate categorical axes; no query frequencies or labels fit targets, IDF, scales or weights",
        "variants": "fixed original weights: word+bigram, other twelve grammar families, original combined; retained weights renormalize within ablation without refitting",
        "selection": "all successfully annotated posts with all fourteen families available; same eligible posts for every variant; errors audited; no author may lose every train/dev/test post or fewer than two train date groups",
        "evaluation": "dev scored separately before test, all weights and protocol fixed before either; nearest training author, no classifier training",
        "ties": "exact equal scores: author-ID display order, expected top-k and reciprocal rank over tied orders; content impostor ties use author-ID order and all tied IDs are retained",
        "content_proxy": "NOUN/PROPN/VERB/ADJ/ADV lowercased NFC lemma counts; raw TF times train-document smoothed IDF ln((1+N)/(1+df))+1 then L2 normalize; group-mean author vectors L2 normalize; missing train-vocabulary content reported explicitly; this is a topic proxy, not topic labels or a cross-topic benchmark",
        "uncertainty": "macro-author percentile bootstrap 2000 replicates, xorshift seed 0x6a09e667f3bcc909, 2.5/97.5 percent nearest ranks; sampling uncertainty within this selected corpus, not generalization assurance",
        "date_policy": {"version": DATE_POLICY, "minimum_inclusive": null, "validation": "valid supplied calendar dates required; no age floor; whole date groups remain in one chronological split", "provenance": "supplied corpus dates are not independently verified; existing exports retain the eligibility policy used by their original import"},
        "limits": ["corpus quotes, copied passages and near duplicates not fully assessed", "accounts are author labels, not verified identities", "random 100-author local pilot with relatively few chronological held-out posts per author", "retrieval does not establish readability, meaning preservation, detector performance or Bittensor viability"],
        "external_model_calls": 0,
        "detector_calls": 0,
    });
    if let Some((reference, _, binding)) = &frozen {
        ensure!(
            reference.parser_identity == parser_identity
                && reference.feature_schema == features::FEATURE_VERSION,
            "frozen space incompatible with current extractor/parser"
        );
        protocol["frozen_selection"] = binding.clone();
        protocol["fitting"] = json!(
            "original shared numerical scales frozen; categorical basis only extends; fresh author means fit fresh train date groups; fresh content baseline IDF fits fresh training only"
        );
        protocol["variants"] = json!(
            "word, grammar, original combined and content baseline plus one weight vector frozen from disjoint development authors"
        );
        protocol["selected_lexical_diagnostic"] = json!(
            "renormalize selected word and word_bigram weights without training or selection; absent if selected lexical weight is zero"
        );
    }
    let protocol_path = args.out.join("protocol.json");
    if args.allow_author_exclusions {
        protocol["author_support_policy"] = json!(
            "exclude whole authors missing any eligible train/dev/test split or having fewer than two eligible training dates; preserve all reasons and apply uniformly to all representations; other validation failures retain existing behavior"
        );
    }
    if protocol_path.exists() {
        ensure!(
            read_json::<Value>(&protocol_path)? == protocol,
            "output protocol/input identity differs; use a fresh output directory"
        );
    } else {
        write_json(&protocol_path, &protocol)?;
    }
    write_json(&args.out.join("source-summary.json"), &summary)?;
    let (mut posts, mut audit) = annotate(inputs, &args, &parser_identity, &cache)?;
    let exclusions = unsupported_authors(posts.iter().map(|post| &post.input.post), &author_ids);
    ensure!(
        args.allow_author_exclusions || exclusions.is_empty(),
        "authors lack required eligible split/date support: {exclusions:?}; inspect audit"
    );
    if !exclusions.is_empty() {
        posts.retain(|post| !exclusions.contains_key(&post.input.post.author_id));
        author_ids.retain(|author| !exclusions.contains_key(author));
        ensure!(
            author_ids.len() >= 2,
            "fewer than two eligible candidate authors remain"
        );
        for record in &mut audit {
            if let Some(reasons) = exclusions.get(record["post"]["author_id"].as_str().unwrap()) {
                record["author_exclusion_reasons"] = json!(reasons);
                if record["status"] == "eligible" {
                    record["status"] = json!("excluded_all_variants");
                    record["error"] = json!("author lacks required eligible split/date support");
                }
            }
        }
        write_json(&args.out.join("annotation-audit.json"), &audit)?;
    }
    write_json(
        &args.out.join("author-exclusions.json"),
        &json!({"input_authors":summary["selected_authors"],"candidate_authors":author_ids.len(),"excluded_authors":exclusions,"eligible_author_ids":author_ids}),
    )?;
    let mut grouped: BTreeMap<String, Vec<(&str, &Features)>> = BTreeMap::new();
    let mut references = Vec::new();
    let mut basis = Vec::new();
    for post in &posts {
        if post.input.post.split == "train" {
            grouped
                .entry(post.input.post.author_id.clone())
                .or_default()
                .push((&post.input.post.source_group, &post.features));
            references.push(Reference {
                source_group: post.input.post.source_group.clone(),
                features: post.features.clone(),
            });
        } else {
            basis.push(post.features.clone());
        }
    }
    if frozen.is_some() {
        eprintln!(
            "Extending the original vocabulary with numerical scales frozen; {} new training posts fit author means",
            references.len()
        );
    } else {
        eprintln!(
            "Fitting one shared frozen space from {} training posts",
            references.len()
        );
    }
    let space = if let Some((reference, _, _)) = &frozen {
        let all: Vec<_> = posts.iter().map(|post| post.features.clone()).collect();
        reference.extend_basis(&all)?
    } else {
        space::fit(&references, &basis, &Config::default())?
    };
    let train_count = references.len();
    drop(references);
    drop(basis);
    let geometry = DistanceGeometry::new(&space);
    let profiles: BTreeMap<_, _> = grouped
        .into_iter()
        .map(|(author, docs)| Ok((author, grouped_profile(&docs)?)))
        .collect::<Result<_>>()?;
    let mut variants = ablations(&space.family_weights);
    if let Some((_, selection, _)) = &frozen {
        variants.insert("selected".into(), selection.weights.clone());
        let mut lexical: Values = selection
            .weights
            .iter()
            .filter(|(name, _)| matches!(name.as_str(), "word" | "word_bigram"))
            .map(|(name, weight)| (name.clone(), *weight))
            .collect();
        let total = lexical.values().sum::<f64>();
        if total > 0.0 {
            for weight in lexical.values_mut() {
                *weight /= total;
            }
            variants.insert("selected_lexical".into(), lexical);
        }
        fs::copy(
            args.selection.as_ref().unwrap(),
            args.out.join("frozen-selection.json"),
        )?;
    }
    write_json(&args.out.join("space.json"), &space)?;
    let bindings: Vec<_> = posts
        .iter()
        .filter(|post| post.input.post.split == "train")
        .map(|post| &post.input.post)
        .collect();
    write_json(
        &args.out.join("author-targets.json"),
        &json!({"space_id": space.id, "representation": "sparse raw per-date-group mean probabilities and metrics; distributions transform sqrt(p)*axis.multiplier and numerical means transform (mean-axis.center)/axis.scale*axis.multiplier", "training_bindings": bindings, "profiles": profiles}),
    )?;
    write_json(&args.out.join("ablation-weights.json"), &variants)?;
    let train_content: Vec<_> = posts
        .iter()
        .filter(|post| post.input.post.split == "train")
        .map(|post| &post.content)
        .collect();
    let idf = fit_idf(&train_content);
    let content_profiles = content_profiles(&posts, &idf);
    ensure!(
        content_profiles.values().all(|profile| !profile.is_empty()),
        "author content profile has no observed content lemmas"
    );
    write_json(
        &args.out.join("content-proxy.json"),
        &json!({"idf": idf, "profiles": content_profiles, "training_document_count": train_content.len()}),
    )?;
    eprintln!(
        "Frozen {} coordinates; evaluating dev then test",
        space.axes.len()
    );
    let dev = evaluate_split(
        "dev",
        &posts,
        &profiles,
        &geometry,
        &variants,
        (&idf, &content_profiles),
        &args.out,
    )?;
    write_json(&args.out.join("dev-summary.json"), &dev)?;
    let test = evaluate_split(
        "test",
        &posts,
        &profiles,
        &geometry,
        &variants,
        (&idf, &content_profiles),
        &args.out,
    )?;
    write_json(&args.out.join("test-summary.json"), &test)?;
    let mut axes_by_family = BTreeMap::<String, usize>::new();
    for axis in &space.axes {
        *axes_by_family.entry(axis.family.clone()).or_default() += 1;
    }
    let report = json!({"protocol": PROTOCOL, "protocol_sha256": hash(&fs::read(protocol_path)?), "space_id": space.id, "parser_identity": parser_identity, "candidate_authors": author_ids.len(), "train_posts": train_count, "input_posts": audit.len(), "eligible_posts": posts.len(), "excluded_posts": audit.len()-posts.len(), "dimensions": space.axes.len(), "dimensions_by_family": axes_by_family, "annotation_cache": cache, "random_ranking_expectation": {"top1": 1.0/author_ids.len() as f64, "top5": (5_usize.min(author_ids.len())) as f64/author_ids.len() as f64, "mrr": (1..=author_ids.len()).map(|rank| 1.0/rank as f64).sum::<f64>()/author_ids.len() as f64}, "dev": dev, "test": test});
    write_json(&args.out.join("report.json"), &report)?;
    let mut artifact_hashes = BTreeMap::new();
    for entry in fs::read_dir(&args.out)? {
        let path = entry?.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("json" | "jsonl")
            )
            && path.file_name().and_then(|s| s.to_str()) != Some("implementation.json")
        {
            artifact_hashes.insert(
                path.file_name().unwrap().to_string_lossy().into_owned(),
                hash(&fs::read(&path)?),
            );
        }
    }
    write_json(
        &args.out.join("implementation.json"),
        &json!({
            "schema":"unslop-author-evaluation-implementation-v1",
            "executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),
            "package_version":env!("CARGO_PKG_VERSION"),
            "artifacts_sha256":artifact_hashes,
            "external_model_calls":0,
            "detector_calls":0
        }),
    )?;
    eprintln!(
        "Evaluation complete: {}",
        args.out.join("report.json").display()
    );
    Ok(())
}

fn main() -> Result<()> {
    run(Args::parse())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_support_counts_dates_and_preserves_missing_split_reasons() {
        let post = |author: &str, split: &str, date: &str| Post {
            id: format!("{author}-{split}-{date}"),
            author_id: author.into(),
            source_group: format!("blog:{author}:date:{date}"),
            split: split.into(),
            path: String::new(),
            text_sha256: String::new(),
            provenance: json!({}),
        };
        let mut posts = vec![
            post("a", "train", "2004-01-01"),
            post("a", "train", "2004-01-01"),
            post("a", "dev", "2004-01-02"),
            post("a", "test", "2004-01-03"),
            post("b", "train", "2004-01-01"),
            post("b", "train", "2004-01-02"),
            post("b", "dev", "2004-01-03"),
        ];
        let authors = BTreeSet::from(["a".into(), "b".into()]);
        let bad = unsupported_authors(posts.iter(), &authors);
        assert_eq!(
            bad["a"],
            vec!["1 eligible training dates; need at least two"]
        );
        assert_eq!(bad["b"], vec!["no eligible test posts"]);
        posts.push(post("a", "train", "2003-12-31"));
        assert!(!unsupported_authors(posts.iter(), &authors).contains_key("a"));
    }

    fn export_fixture(root: &Path) {
        let mut authors = Vec::new();
        let mut total_words = 0;
        for author in ["alpha", "beta"] {
            let directory = root.join("authors").join(author);
            fs::create_dir_all(directory.join("texts")).unwrap();
            let mut manifest = File::create(directory.join("references.jsonl")).unwrap();
            let mut split_words = BTreeMap::new();
            let mut selected_words = 0;
            for (index, split) in ["train", "dev", "test"].into_iter().enumerate() {
                let text = format!("{author} wrote a synthetic example number {index}.");
                let sha = hash(text.as_bytes());
                let words = features::words(&text).len();
                let date = format!("2004-01-0{}", index + 1);
                let post = json!({
                    "id": format!("{author}-{index}"), "author_id": author,
                    "source_group": format!("blog:{author}:date:{date}"),
                    "split": split, "path": format!("texts/{index}.txt"),
                    "text_sha256": sha,
                    "provenance": { "supplied_composition_date": date,
                        "archive_sha256": "synthetic-archive",
                        "text_sha256": sha, "excerpt_sha256": sha,
                        "normalized_duplicate_sha256": sha, "word_count": words }
                });
                writeln!(manifest, "{post}").unwrap();
                fs::write(directory.join(format!("texts/{index}.txt")), text).unwrap();
                split_words.insert(split, words);
                selected_words += words;
            }
            authors.push(json!({"author_id": author, "manifest": format!("authors/{author}/references.jsonl"), "selected_posts": 3, "selected_words": selected_words, "split_posts": {"train": 1, "dev": 1, "test": 1}, "split_words": split_words}));
            total_words += selected_words;
        }
        write_json(&root.join("summary.json"), &json!({"schema": "unslop-blog-author-export-v1", "archive_sha256": "synthetic-archive", "selected_authors": 2, "selected_posts": 6, "selected_words": total_words, "authors": authors})).unwrap();
    }

    #[test]
    fn export_validation_checks_every_manifest_and_source_hash_before_fitting() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        export_fixture(&root);
        let (_, inputs, bindings) = load_export(&root).unwrap();
        assert_eq!(inputs.len(), 6);
        assert_eq!(bindings["manifests"].as_array().unwrap().len(), 2);
        fs::write(root.join("authors/alpha/texts/0.txt"), "Tampered source.").unwrap();
        assert!(
            load_export(&root)
                .unwrap_err()
                .to_string()
                .contains("hash mismatch")
        );
    }

    #[test]
    fn rehashed_cross_author_and_cross_split_duplicates_still_fail() {
        for whitespace_variant in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            export_fixture(&root);
            let source = fs::read_to_string(root.join("authors/alpha/texts/0.txt")).unwrap();
            let replacement = if whitespace_variant {
                source.replace(' ', "  ")
            } else {
                source
            };
            fs::write(root.join("authors/beta/texts/2.txt"), &replacement).unwrap();
            let manifest = root.join("authors/beta/references.jsonl");
            let mut rows: Vec<Value> = fs::read_to_string(&manifest)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let sha = hash(replacement.as_bytes());
            rows[2]["text_sha256"] = json!(sha);
            rows[2]["provenance"]["text_sha256"] = json!(sha);
            rows[2]["provenance"]["excerpt_sha256"] = json!(sha);
            rows[2]["provenance"]["normalized_duplicate_sha256"] = json!(hash(
                replacement
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .as_bytes()
            ));
            fs::write(
                &manifest,
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
            let error = load_export(&root).unwrap_err().to_string();
            assert!(
                error.contains(if whitespace_variant {
                    "duplicate normalized"
                } else {
                    "duplicate exact"
                }),
                "{error}"
            );
        }
    }
}
