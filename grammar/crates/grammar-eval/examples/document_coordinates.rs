//! Export individual documents in the existing frozen coordinate space.
//! No corpus fitting, reparsing, detector calls or reference pooling.
#[allow(dead_code)]
#[path = "../src/coordinate_inference.rs"]
mod coordinate_inference;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use coordinate_inference::Metric;
use grammar_core::features::Features;
use grammar_eval::{document_values, validate_date};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const MODEL: &str = "c19c5015dbf2b091d3ec774a7b108a6f41bc88e93213bc4ac45e787a35e2b916";
const CATALOG: &str = "170b7f3572c910110752930c8e2d067cbb420f720ebba8fa3ac455cd6fe03b79";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    model: PathBuf,
    #[arg(long)]
    source: PathBuf,
    /// SHA-256 of the Blog coordinate manifest or Global Voices result JSON.
    #[arg(long)]
    expected_source_sha256: String,
    #[arg(long, value_parser=["train", "dev", "test", "transfer"])]
    role: String,
    #[arg(long)]
    prepared: Option<PathBuf>,
    #[arg(long)]
    cache: Option<PathBuf>,
    #[arg(long)]
    out: PathBuf,
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("missing {key}"))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        sha(&bytes) == expected,
        "binding changed: {}",
        path.display()
    );
    Ok(bytes)
}
fn json_bound(path: &Path, expected: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(path, expected)?)?)
}
fn save(path: &Path, value: &Value) -> Result<Value> {
    let bytes = serde_json::to_vec(value)?;
    fs::write(path, &bytes)?;
    Ok(
        json!({"path":path.file_name().and_then(|name| name.to_str()).context("UTF-8 output filename")?,"sha256":sha(&bytes)}),
    )
}

struct Export {
    rows: Vec<Value>,
    coordinates: Vec<f64>,
    authors: Vec<String>,
    provenance: Value,
}
fn feature_coordinates(
    bytes: &[u8],
    enveloped: bool,
    hash: &str,
    metric: &Metric,
) -> Result<Vec<f64>> {
    let value: Value = serde_json::from_slice(bytes)?;
    let features: Features = serde_json::from_value(if enveloped {
        value["features"].clone()
    } else {
        value
    })?;
    ensure!(
        features.source_sha256 == hash
            && features.parser_identity == metric.artifact().parser_identity
            && features.schema == metric.artifact().feature_schema,
        "feature identity differs"
    );
    metric.transform_profile(&document_values(&features)?)
}

fn blog(args: &Args, metric: &Metric) -> Result<Export> {
    let prepared = args
        .prepared
        .as_ref()
        .context("Blog export requires --prepared")?;
    let cache = args
        .cache
        .as_ref()
        .context("Blog export requires --cache")?;
    let manifest = json_bound(
        &args.source.join("manifest.json"),
        &args.expected_source_sha256,
    )?;
    ensure!(
        manifest["schema"] == "slopninja-coordinate-frontier-export-v1"
            && manifest["split"] == args.role
            && manifest["catalog_sha256"] == CATALOG,
        "source coordinate identity differs"
    );
    let audit = json_bound(
        &args.source.join("audit.json"),
        string(&manifest, "audit_sha256")?,
    )?;
    let catalog = json_bound(&args.source.join("catalog.json"), CATALOG)?;
    let indices: Vec<usize> =
        serde_json::from_value(catalog["subsets"]["all_m2"]["max_column_indices"].clone())?;
    ensure!(indices.len() == 3557, "fixed subset width differs");
    for (index, coordinate) in indices.iter().zip(&metric.artifact().coordinates) {
        let original = &catalog["coordinates"][*index];
        ensure!(
            original["family"] == coordinate.family && original["feature"] == coordinate.feature,
            "coordinate order differs"
        );
    }
    let authors: Vec<String> = serde_json::from_value(manifest["authors"].clone())?;
    let author_set: BTreeSet<_> = authors.iter().map(String::as_str).collect();
    let audit_path = prepared.join("annotation-audit.json");
    let expected = audit["input_bindings"]
        .as_object()
        .context("missing source bindings")?
        .iter()
        .filter(|(path, _)| {
            Path::new(path)
                .file_name()
                .is_some_and(|name| name == "annotation-audit.json")
        })
        .map(|(_, value)| value.as_str().context("invalid audit hash"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(expected.len() == 1, "ambiguous annotation audit binding");
    let metadata = json_bound(&audit_path, expected[0])?;
    let mut cache_bindings = BTreeMap::new();
    for binding in audit["cache_bindings"]
        .as_array()
        .context("missing cache bindings")?
    {
        ensure!(
            cache_bindings
                .insert(string(binding, "id")?, binding)
                .is_none(),
            "duplicate cache binding"
        );
    }
    let mut documents = Vec::new();
    for row in metadata.as_array().context("invalid annotation audit")? {
        let p = &row["post"];
        let author = string(p, "author_id")?;
        let split = string(p, "split")?;
        if row["status"] != "eligible"
            || !author_set.contains(author)
            || !(split == "train" || split == args.role)
        {
            continue;
        }
        let id = string(p, "id")?;
        let hash = string(p, "text_sha256")?;
        let binding = cache_bindings
            .get(id)
            .context("unbound document feature cache")?;
        ensure!(binding["text_sha256"] == hash, "cache source hash differs");
        let path = cache.join(format!("{hash}.features.json"));
        let bytes = checked(&path, string(binding, "feature_cache_sha256")?)?;
        let vector = feature_coordinates(&bytes, true, hash, metric)?;
        let date = string(&p["provenance"], "supplied_composition_date")?;
        ensure!(
            p["source_group"] == format!("blog:{author}:date:{date}"),
            "source/date binding differs"
        );
        let usage = if args.role == "train" {
            "train"
        } else if split == "train" {
            "reference"
        } else {
            "query"
        };
        documents.push((json!({"id":id,"author":author,"date":date,"source_group":p["source_group"],"text_sha256":hash,
            "split":usage,"source_split":split,"feature_cache_sha256":sha(&bytes)}), vector));
    }
    documents.sort_by(|a, b| a.0["id"].as_str().cmp(&b.0["id"].as_str()));
    // Independently compare every query with the previously exported coordinates.
    let query_rows: Vec<Value> = serde_json::from_value(json_bound(
        &args.source.join("queries.json"),
        string(&manifest, "queries_sha256")?,
    )?)?;
    let array = &manifest["arrays"]["query_coordinates"];
    let binary = checked(
        &args.source.join(string(array, "path")?),
        string(array, "sha256")?,
    )?;
    ensure!(
        array["shape"] == json!([query_rows.len(), 7353])
            && binary.len() == query_rows.len() * 7353 * 8,
        "source array shape differs"
    );
    let queries: Vec<_> = documents
        .iter()
        .filter(|(row, _)| row["split"] == "query" || row["split"] == "train")
        .collect();
    ensure!(
        queries.len() == query_rows.len(),
        "source query coverage changed"
    );
    let mut maximum_difference = 0.0_f64;
    for (i, ((row, vector), original)) in queries.iter().zip(&query_rows).enumerate() {
        ensure!(
            row["id"] == original["id"]
                && row["author"] == original["author"]
                && row["date"] == original["date"]
                && row["text_sha256"] == original["text_sha256"],
            "source query identity differs"
        );
        for (j, value) in vector.iter().enumerate() {
            let start = (i * 7353 + indices[j]) * 8;
            let old = f64::from_le_bytes(binary[start..start + 8].try_into()?);
            let difference = (old - value).abs();
            ensure!(
                old.is_finite() && difference <= 1e-12,
                "document coordinates differ from frozen export"
            );
            maximum_difference = maximum_difference.max(difference);
        }
    }
    let query_count = queries.len();
    Ok(Export {
        coordinates: documents
            .iter()
            .flat_map(|(_, vector)| vector.iter().copied())
            .collect(),
        rows: documents.into_iter().map(|(row, _)| row).collect(),
        authors,
        provenance: json!({"source_manifest_sha256":args.expected_source_sha256,"source":args.source.canonicalize()?,
            "source_audit_sha256":manifest["audit_sha256"],"annotation_audit_sha256":expected[0],
            "queries_compared":query_count,"maximum_coordinate_difference":maximum_difference,
            "raw_profile_author_coordinates_used":false,"new_parser_calls":0}),
    })
}

fn transfer(args: &Args, metric: &Metric) -> Result<Export> {
    let report = json_bound(
        &args.source.join("report.json"),
        &args.expected_source_sha256,
    )?;
    ensure!(
        report["schema"] == "slopninja-author-transfer-result-v1",
        "unknown transfer result"
    );
    let plan = json_bound(
        &args.source.join("plan.json"),
        string(&report["plan"], "sha256")?,
    )?;
    ensure!(
        plan["models"]
            .as_array()
            .context("missing models")?
            .iter()
            .any(|m| m["artifact"]["sha256"] == MODEL),
        "transfer geometry not bound"
    );
    let articles = json_bound(
        &args.source.join("articles.json"),
        string(&report["artifacts"]["articles"], "sha256")?,
    )?;
    let mut rows = Vec::new();
    let mut coordinates = Vec::new();
    for article in articles.as_array().context("invalid transfer articles")? {
        ensure!(
            article["status"] == "available",
            "unavailable transfer article; no silent attrition"
        );
        let hash = string(article, "text_sha256")?;
        let bytes = checked(
            Path::new(string(&article["features"], "path")?),
            string(&article["features"], "sha256")?,
        )?;
        coordinates.extend(feature_coordinates(&bytes, false, hash, metric)?);
        rows.push(json!({"id":article["id"],"author":article["author_id"],"date":article["date"],"source_group":article["source_group"],
            "text_sha256":hash,"split":article["split"],"source_split":article["split"],"feature_cache_sha256":sha(&bytes)}));
    }
    let authors: Vec<_> = rows
        .iter()
        .map(|r| r["author"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ensure!(
        report["coverage"]["requested_articles"] == rows.len()
            && report["coverage"]["requested_authors"] == authors.len(),
        "transfer coverage differs"
    );
    Ok(Export {
        rows,
        coordinates,
        authors,
        provenance: json!({"source_report_sha256":args.expected_source_sha256,
        "source":args.source.canonicalize()?,"source_plan_sha256":report["plan"]["sha256"],"new_parser_calls":0}),
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "output exists; choose a fresh directory"
    );
    let metric = Metric::new(serde_json::from_slice(&checked(&args.model, MODEL)?)?)?;
    let export = if args.role == "transfer" {
        transfer(&args, &metric)?
    } else {
        blog(&args, &metric)?
    };
    ensure!(
        export.authors.len() >= 2 && export.authors.windows(2).all(|w| w[0] < w[1]),
        "invalid author catalog"
    );
    let mut ids = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut dates = BTreeMap::new();
    for row in &export.rows {
        let author = string(row, "author")?;
        let date = string(row, "date")?;
        let split = string(row, "split")?;
        validate_date(date)?;
        ensure!(
            ids.insert(string(row, "id")?) && hashes.insert(string(row, "text_sha256")?),
            "duplicate document"
        );
        ensure!(
            export.authors.binary_search(&author.to_owned()).is_ok(),
            "author outside catalog"
        );
        if let Some(old) = groups.insert(string(row, "source_group")?, (author, split)) {
            ensure!(
                old == (author, split),
                "source group crosses authors/splits"
            );
        }
        if let Some(old) = dates.insert((author, date), split) {
            ensure!(old == split, "date crosses splits");
        }
    }
    for author in &export.authors {
        let mine: Vec<_> = export
            .rows
            .iter()
            .filter(|r| r["author"] == *author)
            .collect();
        if args.role == "train" {
            ensure!(
                mine.iter().all(|r| r["split"] == "train")
                    && mine
                        .iter()
                        .map(|r| r["date"].as_str().unwrap())
                        .collect::<BTreeSet<_>>()
                        .len()
                        >= 2,
                "insufficient training dates"
            );
        } else {
            let reference_max = mine
                .iter()
                .filter(|r| r["split"] == "reference")
                .map(|r| r["date"].as_str().unwrap())
                .max()
                .context("no references")?;
            let query_min = mine
                .iter()
                .filter(|r| r["split"] == "query")
                .map(|r| r["date"].as_str().unwrap())
                .min()
                .context("no queries")?;
            ensure!(
                reference_max < query_min,
                "reference/query chronology overlaps"
            );
        }
    }
    ensure!(
        export.coordinates.len() == export.rows.len() * 3557
            && export.coordinates.iter().all(|v| v.is_finite()),
        "invalid coordinate matrix"
    );
    fs::create_dir_all(&args.out)?;
    let docs = save(&args.out.join("documents.json"), &json!(export.rows))?;
    let bytes: Vec<_> = export
        .coordinates
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    fs::write(args.out.join("coordinates.f64"), &bytes)?;
    let keys: Vec<_> = metric
        .artifact()
        .coordinates
        .iter()
        .map(|c| (&c.family, &c.feature))
        .collect();
    let manifest = json!({"schema":"slopninja-document-coordinates-v1","role":args.role,"authors":export.authors,"dimension":3557,
        "documents":docs,"coordinates":{"path":"coordinates.f64","sha256":sha(&bytes),"shape":[export.rows.len(),3557],"dtype":"float64-le"},
        "geometry":{"source_space_id":metric.artifact().source_space_id,"feature_schema":metric.artifact().feature_schema,"parser_identity":metric.artifact().parser_identity,
            "coordinate_keys_sha256":sha(&serde_json::to_vec(&keys)?),"model_sha256":MODEL},
        "provenance":export.provenance,"compiled_source_sha256":sha(include_bytes!("document_coordinates.rs")),
        "coordinate_inference_source_sha256":sha(include_bytes!("../src/coordinate_inference.rs")),"executed_binary_sha256":sha(&fs::read(std::env::current_exe()?)?)});
    save(&args.out.join("manifest.json"), &manifest)?;
    println!(
        "Exported {} documents for {} authors ({})",
        export.rows.len(),
        export.authors.len(),
        args.role
    );
    Ok(())
}
