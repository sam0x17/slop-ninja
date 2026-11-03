use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{
    features::{self, Features},
    space::Space,
    syntax::{self, Document},
};
use grammar_eval::{DistanceGeometry, Profile, document_values, family_distances, validate_date};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[path = "../fixed_coordinate_panel.rs"]
mod panels;
// Reuse the frozen date-balanced target implementation without changing it.
#[allow(dead_code)]
#[path = "../neural.rs"]
mod neural;
use panels::{load_catalog, validate_panel_authors};

#[derive(Parser)]
#[command(about = "Export additional author panels using the fixed slopninja coordinate catalog")]
struct Args {
    evaluation: PathBuf,
    #[arg(long)]
    export: PathBuf,
    #[arg(long)]
    cache: PathBuf,
    /// Original frozen Space whose numerical transforms and vocabulary define the catalog.
    #[arg(long)]
    space: PathBuf,
    #[arg(long,value_parser=["train","test"])]
    split: String,
    /// Exact maximal catalog from the completed coordinate frontier.
    #[arg(long)]
    catalog: PathBuf,
    /// Frozen training-author-scale protocol; read and hash-bound before source loading.
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
    metadata: Value,
    id: String,
    author: String,
    date: String,
    group: String,
    split: String,
    profile: Profile,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read(path: &Path, bindings: &mut Value) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    bindings[path.display().to_string()] = json!(hash(&bytes));
    Ok(serde_json::from_slice(&bytes)?)
}
fn write(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))
}
fn within(root: &Path, base: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(!Path::new(relative).is_absolute(), "absolute corpus path");
    let path = base.join(relative).canonicalize()?;
    ensure!(path.starts_with(root), "corpus path escapes export");
    Ok(path)
}

fn load(args: &Args, space: &Space, bindings: &mut Value) -> Result<(Vec<Post>, Value)> {
    let root = args.export.canonicalize()?;
    let protocol = read(&args.evaluation.join("protocol.json"), bindings)?;
    let implementation = read(&args.evaluation.join("implementation.json"), bindings)?;
    ensure!(
        protocol["protocol"] == grammar_eval::PROTOCOL
            && protocol["parser_identity"] == space.parser_identity,
        "evaluation protocol/parser mismatch"
    );
    let summary_path = root.join("summary.json");
    let summary = read(&summary_path, bindings)?;
    ensure!(
        summary["schema"] == "unslop-blog-author-export-v1"
            && protocol["input_bindings"]["summary_sha256"]
                == bindings[summary_path.display().to_string()],
        "export summary identity mismatch"
    );
    let mut stored = BTreeMap::new();
    for filename in [
        "protocol.json",
        "annotation-audit.json",
        "author-targets.json",
    ] {
        let path = args.evaluation.join(filename);
        let value = read(&path, bindings)?;
        ensure!(
            implementation["artifacts_sha256"][filename] == bindings[path.display().to_string()],
            "evaluation artifact changed: {filename}"
        );
        stored.insert(filename, value);
    }
    let mut statuses = BTreeMap::new();
    let requested = |split: &Value| split == "train" || split == &args.split;
    for row in stored["annotation-audit.json"]
        .as_array()
        .context("invalid audit")?
        .iter()
        .filter(|row| requested(&row["post"]["split"]))
    {
        ensure!(
            statuses
                .insert(text(&row["post"], "id")?.to_owned(), row)
                .is_none(),
            "duplicate audited post"
        );
    }
    let mut seen = BTreeSet::new();
    let mut source_hashes = BTreeSet::new();
    let mut normalized_hashes = BTreeSet::new();
    let mut seen_authors = BTreeSet::new();
    let mut posts = Vec::new();
    let mut caches = Vec::new();
    let mut excluded = Vec::new();
    for author in summary["authors"].as_array().context("missing authors")? {
        let author_id = text(author, "author_id")?;
        ensure!(
            seen_authors.insert(author_id.to_owned()),
            "duplicate author"
        );
        let relative = text(author, "manifest")?;
        let manifest = within(&root, &root, relative)?;
        let bytes = fs::read(&manifest)?;
        let digest = hash(&bytes);
        let expected = protocol["input_bindings"]["manifests"]
            .as_array()
            .context("missing manifest bindings")?
            .iter()
            .find(|row| row["path"] == relative)
            .context("unbound manifest")?;
        ensure!(expected["sha256"] == digest, "manifest changed");
        bindings[manifest.display().to_string()] = json!(digest);
        for line in std::str::from_utf8(&bytes)?.lines() {
            let metadata: Value = serde_json::from_str(line)?;
            if !requested(&metadata["split"]) {
                continue;
            }
            let id = text(&metadata, "id")?.to_owned();
            ensure!(
                seen.insert(id.clone()) && metadata["author_id"] == author_id,
                "duplicate or mismatched post"
            );
            let status = statuses
                .get(&id)
                .context("post absent from audited split")?;
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
                    "audited metadata changed: {key}"
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
                manifest.parent().context("manifest parent")?,
                text(&metadata, "path")?,
            )?)?;
            let sha = hash(source.as_bytes());
            ensure!(
                metadata["text_sha256"] == sha
                    && provenance["text_sha256"] == sha
                    && provenance["excerpt_sha256"] == sha,
                "source hash mismatch"
            );
            let normalized = hash(
                source
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .as_bytes(),
            );
            ensure!(
                source_hashes.insert(sha.clone())
                    && normalized_hashes.insert(normalized.clone())
                    && provenance["normalized_duplicate_sha256"] == normalized,
                "duplicate or stale normalized source"
            );
            ensure!(
                provenance["word_count"].as_u64() == Some(features::words(&source).len() as u64),
                "word count mismatch"
            );
            let annotation_bytes = fs::read(args.cache.join(format!("{sha}.annotation.json")))?;
            let feature_bytes = fs::read(args.cache.join(format!("{sha}.features.json")))?;
            let document: Document = serde_json::from_slice(&annotation_bytes)?;
            ensure!(
                document.text == source && document.parser_identity == space.parser_identity,
                "annotation identity mismatch"
            );
            syntax::validate(&document)?;
            let extracted = features::extract(&document)?;
            let cached: CachedFeatures = serde_json::from_slice(&feature_bytes)?;
            ensure!(
                cached.features == extracted
                    && extracted.source_sha256 == sha
                    && extracted.schema == space.feature_schema,
                "cached features mismatch"
            );
            let profile = document_values(&extracted)?;
            ensure!(
                profile.keys().eq(space.family_schemas.keys()),
                "feature family mismatch"
            );
            caches.push(json!({"id":id,"text_sha256":sha,"annotation_sha256":hash(&annotation_bytes),"feature_cache_sha256":hash(&feature_bytes)}));
            posts.push(Post {
                id,
                author: author_id.into(),
                date,
                group,
                split: text(&metadata, "split")?.into(),
                profile,
                metadata,
            });
        }
        if seen_authors.len() % 10 == 0 {
            eprintln!(
                "Validated {} authors / {} train and query posts",
                seen_authors.len(),
                posts.len()
            );
        }
    }
    ensure!(
        seen.len() == statuses.len()
            && seen_authors.len() as u64
                == summary["selected_authors"]
                    .as_u64()
                    .context("author count")?,
        "audit/export count mismatch"
    );
    let training: Vec<_> = posts.iter().filter(|post| post.split == "train").collect();
    let bindings_actual: BTreeSet<_> = training
        .iter()
        .map(|post| {
            (
                post.group.clone(),
                text(&post.metadata, "text_sha256").unwrap().to_owned(),
            )
        })
        .collect();
    let target_bindings = stored["author-targets.json"]["training_bindings"]
        .as_array()
        .context("missing target bindings")?;
    let bindings_expected: BTreeSet<_> = target_bindings
        .iter()
        .map(|post| {
            Ok((
                text(post, "source_group")?.to_owned(),
                text(post, "text_sha256")?.to_owned(),
            ))
        })
        .collect::<Result<_>>()?;
    ensure!(
        bindings_actual == bindings_expected && training.len() == target_bindings.len(),
        "training target bindings mismatch"
    );
    // These sources apply an existing catalog. Their exact prepared target bindings
    // are verified above; they must not be used to fit catalog support or scales.
    posts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((
        posts,
        json!({"cache_bindings":caches,"excluded_requested_posts":excluded,
        "loaded_splits":if args.split=="train" {vec!["train"]} else {vec!["train",&args.split]},
        "source_author_ids":seen_authors,"input_bindings":bindings}),
    ))
}

struct Binary {
    writer: BufWriter<File>,
    digest: Sha256,
    elements: usize,
}
impl Binary {
    fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            writer: BufWriter::new(File::create(path)?),
            digest: Sha256::new(),
            elements: 0,
        })
    }
    fn append(&mut self, values: &[f64]) -> Result<()> {
        for value in values {
            ensure!(value.is_finite(), "nonfinite export array");
            let bytes = value.to_le_bytes();
            self.writer.write_all(&bytes)?;
            self.digest.update(bytes);
            self.elements += 1;
        }
        Ok(())
    }
    fn finish(mut self, path: &str, shape: &[usize]) -> Result<Value> {
        ensure!(
            shape.iter().try_fold(1_usize, |n, d| n.checked_mul(*d)) == Some(self.elements),
            "array shape mismatch"
        );
        self.writer.flush()?;
        Ok(
            json!({"path":path,"shape":shape,"dtype":"float64-le","sha256":hex::encode(self.digest.finalize()),"bytes":self.elements*8}),
        )
    }
}

fn run(args: Args) -> Result<()> {
    ensure!(!args.out.exists(), "use a fresh output directory");
    let mut bindings = json!({});
    let value = read(&args.space, &mut bindings)?;
    let space: Space = serde_json::from_value(value)?;
    space.validate()?;
    read(&args.protocol, &mut bindings)?;
    let (posts, audit) = load(&args, &space, &mut bindings)?;
    let training: Vec<_> = posts.iter().filter(|post| post.split == "train").collect();
    read(&args.catalog, &mut bindings)?;
    let catalog_bytes = fs::read(&args.catalog)?;
    let catalog = load_catalog(&space, &catalog_bytes)?;
    let mut grouped = BTreeMap::<String, Vec<(&str, &Profile)>>::new();
    for post in &training {
        grouped
            .entry(post.author.clone())
            .or_default()
            .push((&post.date, &post.profile));
    }
    let authors: Vec<_> = grouped.keys().cloned().collect();
    validate_panel_authors(&authors, &catalog.training_authors, args.split == "train")?;
    let targets: Vec<_> = authors
        .iter()
        .map(|author| neural::target_profile(&grouped[author], None))
        .collect::<Result<_>>()?;
    let target_coords: Vec<_> = targets
        .iter()
        .map(|profile| catalog.transform(profile))
        .collect::<Result<_>>()?;
    let queries: Vec<_> = posts
        .iter()
        .filter(|post| post.split == args.split)
        .collect();
    ensure!(
        !queries.is_empty() && authors.len() >= 2,
        "no eligible queries or gallery"
    );
    let loss_weights = neural::balanced_weights(
        &queries
            .iter()
            .map(|post| (post.author.as_str(), post.date.as_str()))
            .collect::<Vec<_>>(),
    )?;
    fs::create_dir_all(&args.out)?;
    fs::write(args.out.join("catalog.json"), &catalog_bytes)?;
    write(&args.out.join("audit.json"), &audit)?;
    let mut query_file = Binary::new(&args.out.join("query-coordinates.f64"))?;
    let mut author_file = Binary::new(&args.out.join("author-coordinates.f64"))?;
    let mut distance_file = Binary::new(&args.out.join("family-distances.f64"))?;
    let mut held_file = if args.split == "train" {
        Some(Binary::new(&args.out.join("own-heldout-coordinates.f64"))?)
    } else {
        None
    };
    for coordinates in &target_coords {
        author_file.append(coordinates)?;
    }
    let geometry = DistanceGeometry::new(&space);
    let mut metadata = Vec::new();
    let mut minimum_residual = f64::INFINITY;
    for (index, (post, loss_weight)) in queries.iter().zip(loss_weights).enumerate() {
        let own = authors
            .binary_search(&post.author)
            .map_err(|_| anyhow::anyhow!("query author lacks profile"))?;
        let query = catalog.transform(&post.profile)?;
        query_file.append(&query)?;
        let held = if args.split == "train" {
            Some(neural::target_profile(
                &grouped[&post.author],
                Some(&post.date),
            )?)
        } else {
            None
        };
        let held_coords = held
            .as_ref()
            .map(|profile| catalog.transform(profile))
            .transpose()?;
        if let Some(file) = &mut held_file {
            file.append(held_coords.as_ref().unwrap())?;
        }
        for (candidate, target) in targets.iter().enumerate() {
            let (target, coordinates) =
                match (candidate == own, held.as_ref(), held_coords.as_ref()) {
                    (true, Some(held), Some(coords)) => (held, coords),
                    _ => (target, &target_coords[candidate]),
                };
            let full = family_distances(&geometry, &post.profile, target)?;
            for residual in catalog.residual(&query, coordinates, &full)? {
                minimum_residual = minimum_residual.min(residual);
            }
            distance_file.append(
                &catalog
                    .families
                    .iter()
                    .map(|family| full[family])
                    .collect::<Vec<_>>(),
            )?;
        }
        metadata.push(json!({"id":post.id,"author":post.author,"source_group":post.group,"date":post.date,"split":post.split,"own":own,"loss_weight":loss_weight,"text_sha256":post.metadata["text_sha256"]}));
        if (index + 1) % 100 == 0 {
            eprintln!(
                "Exported {} / {} coordinate queries",
                index + 1,
                queries.len()
            );
        }
    }
    let q = queries.len();
    let a = authors.len();
    let k = catalog.coordinates.len();
    let f = catalog.families.len();
    let mut arrays = json!({});
    arrays["query_coordinates"] = query_file.finish("query-coordinates.f64", &[q, k])?;
    arrays["author_coordinates"] = author_file.finish("author-coordinates.f64", &[a, k])?;
    arrays["family_distances"] = distance_file.finish("family-distances.f64", &[q, a, f])?;
    if let Some(file) = held_file {
        arrays["own_heldout_coordinates"] = file.finish("own-heldout-coordinates.f64", &[q, k])?;
    }
    write(&args.out.join("queries.json"), &metadata)?;
    write(
        &args.out.join("manifest.json"),
        &json!({"schema":"slopninja-coordinate-frontier-export-v1","split":args.split,
        "source_space_id":space.id,"feature_schema":space.feature_schema,"parser_identity":space.parser_identity,
        "families":catalog.families,"authors":authors,"coordinate_count":k,"query_count":q,"arrays":arrays,
        "catalog_sha256":hash(&fs::read(args.out.join("catalog.json"))?),"queries_sha256":hash(&fs::read(args.out.join("queries.json"))?),
        "input_bindings":bindings,"reference_reproduction":null,
        "role":if args.split=="train" {"training_panel"} else {"test_gallery"},
        "training_catalog_policy":panels::TRAINING_CATALOG_POLICY,
        "fixed_subset_name":panels::SUBSET_NAME,"fixed_subset":catalog.subsets[panels::SUBSET_NAME],
        "catalog_fit_performed":false,"audit_sha256":hash(&fs::read(args.out.join("audit.json"))?),
        "minimum_residual_after_roundoff_clamp":minimum_residual,
        "residual_tolerance":"reject residual < -1e-9*(1+abs(full_family_distance)); residual diagnostic clamps tolerated roundoff to zero; raw family distances remain unchanged",
        "geometry":"categorical sqrt(p/2); numerical value/(original_scale*sqrt(full_original_family_axis_count)); no family weights; complete raw family distances preserve unselected/unseen coordinates",
        "training_positive":"entire query date excluded before author/date probability means; only true-author row replaced; dev/test use full training author profiles",
        "executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "source_sha256":{"fixed_coordinate_panel.rs":hash(include_bytes!("../fixed_coordinate_panel.rs")),"coordinate_frontier.rs":hash(include_bytes!("../coordinate_frontier.rs")),"coordinates.rs":hash(include_bytes!("../coordinates.rs")),"exporter.rs":hash(include_bytes!("unslop-export-fixed-panels.rs")),"neural.rs":hash(include_bytes!("../neural.rs"))},
        "external_model_calls":0,"detector_calls":0}),
    )?;
    eprintln!(
        "Coordinate export complete: {} queries / {} authors / {} coordinates",
        q, a, k
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
    fn panel_loader_preserves_frozen_source_checks_except_original_fitting_sources() {
        fn section(source: &str) -> &str {
            &source[source.find("fn load(").unwrap()..source.find("    posts.sort_by").unwrap()]
        }
        let original = section(include_str!("unslop-export-coordinate-frontier.rs"));
        let panel = section(include_str!("unslop-export-fixed-panels.rs"));
        let old_boundary = original
            .find("    if args.split == \"train\" {\n        let fitted:")
            .unwrap();
        let new_boundary = panel
            .find("    // These sources apply an existing catalog.")
            .unwrap();
        assert_eq!(&original[..old_boundary], &panel[..new_boundary]);
    }

    #[test]
    fn binary_arrays_are_little_endian_hash_bound_and_shape_checked() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("array.f64");
        let mut array = Binary::new(&path).unwrap();
        array.append(&[1.25, -3.0, 0.0, 2.5]).unwrap();
        let metadata = array.finish("array.f64", &[2, 2]).unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(metadata["sha256"], hash(&bytes));
        let values: Vec<_> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| f64::from_le_bytes(*chunk))
            .collect();
        assert_eq!(values, vec![1.25, -3.0, 0.0, 2.5]);
        let array = Binary::new(&temp.path().join("wrong.f64")).unwrap();
        assert!(array.finish("wrong.f64", &[1]).is_err());
        let mut array = Binary::new(&temp.path().join("nan.f64")).unwrap();
        assert!(array.append(&[f64::NAN]).is_err());
    }
}
