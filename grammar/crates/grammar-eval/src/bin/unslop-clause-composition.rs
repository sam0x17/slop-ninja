use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    features::{Family, FeatureExtractor, Features},
    syntax::Document,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[path = "../clause_composition.rs"]
mod clause_composition;
use clause_composition::{CompositionalExtractor, FAMILY, project_legacy};

#[derive(Parser)]
#[command(
    about = "Add the versioned clause-child family without changing the fourteen legacy families"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Extract from a validated syntax Document JSON. No parser is loaded.
    Document {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        expected_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Project a hash-bound v1 feature-cache wrapper into the new Features schema.
    Cache {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        expected_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Verify direct/document and encoded/cache extraction on a frozen TRAIN export.
    VerifyTrain {
        #[arg(long)]
        export: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        expected_manifest_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read_bound(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        hash(&bytes) == expected,
        "Input SHA mismatch: {}",
        path.display()
    );
    Ok(bytes)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn write_fresh(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&bytes)?;
    Ok(())
}
fn cached(bytes: &[u8]) -> Result<Features> {
    let value: Value = serde_json::from_slice(bytes)?;
    serde_json::from_value(
        value
            .get("features")
            .context("Expected a v1 feature-cache wrapper")?
            .clone(),
    )
    .map_err(Into::into)
}

fn verify_train(export: &Path, cache: &Path, expected: &str, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a fresh verification receipt path");
    let manifest: Value =
        serde_json::from_slice(&read_bound(&export.join("manifest.json"), expected)?)?;
    ensure!(
        manifest["schema"] == "slopninja-coordinate-frontier-export-v1"
            && manifest["split"] == "train",
        "Expected a frozen coordinate frontier TRAIN export"
    );
    let queries: Value = serde_json::from_slice(&read_bound(
        &export.join("queries.json"),
        text(&manifest, "queries_sha256")?,
    )?)?;
    let audit: Value = serde_json::from_slice(&read_bound(
        &export.join("audit.json"),
        text(&manifest, "audit_sha256")?,
    )?)?;
    ensure!(
        audit["loaded_splits"] == json!(["train"]),
        "Export audit includes another split"
    );
    let rows = queries.as_array().context("Expected TRAIN query array")?;
    let audit_rows = audit["cache_bindings"]
        .as_array()
        .context("Missing cache bindings")?;
    let mut bindings = BTreeMap::new();
    for row in audit_rows {
        ensure!(
            bindings.insert(text(row, "id")?, row).is_none(),
            "Duplicate audit query ID"
        );
    }
    ensure!(
        rows.len()
            == manifest["query_count"]
                .as_u64()
                .context("Missing query count")? as usize
            && bindings.len() == rows.len(),
        "TRAIN query count differs"
    );
    let mut seen = BTreeSet::new();
    let mut authors = BTreeSet::new();
    let mut opportunities = 0_u64;
    let mut old_opportunities = 0_u64;
    let mut vocabulary = BTreeSet::new();
    let mut sources = Vec::new();
    let mut unavailable = 0_u64;
    for row in rows {
        ensure!(row["split"] == "train", "Non-TRAIN query encountered");
        let id = text(row, "id")?;
        ensure!(seen.insert(id), "Repeated TRAIN query");
        authors.insert(text(row, "author")?);
        let binding = bindings.get(id).context("Query lacks audit binding")?;
        let source = text(row, "text_sha256")?;
        ensure!(
            source.len() == 64
                && source.bytes().all(|c| c.is_ascii_hexdigit())
                && text(binding, "text_sha256")? == source,
            "Source binding differs"
        );
        let features = cached(&read_bound(
            &cache.join(format!("{source}.features.json")),
            text(binding, "feature_cache_sha256")?,
        )?)?;
        let doc: Document = serde_json::from_slice(&read_bound(
            &cache.join(format!("{source}.annotation.json")),
            text(binding, "annotation_sha256")?,
        )?)?;
        ensure!(
            features.source_sha256 == source
                && features.schema == text(&manifest, "feature_schema")?
                && features.parser_identity == text(&manifest, "parser_identity")?,
            "Cached feature identity differs"
        );
        let projected = project_legacy(&features)?;
        let direct = CompositionalExtractor.extract(&doc)?;
        ensure!(
            direct == projected,
            "Document/cache extension differs for {id}"
        );
        for (name, old) in &features.families {
            ensure!(
                serde_json::to_vec(old)? == serde_json::to_vec(&direct.families[name])?,
                "Legacy family changed for {id}/{name}"
            );
        }
        let Family::Distribution {
            counts,
            opportunities: n,
        } = &direct.families[FAMILY]
        else {
            anyhow::bail!("New family is not a distribution")
        };
        if *n == 0 {
            unavailable += 1;
        }
        opportunities = opportunities
            .checked_add(*n)
            .context("Opportunity overflow")?;
        vocabulary.extend(counts.keys().cloned());
        let Family::Distribution {
            opportunities: old_n,
            ..
        } = &features.families["clause_head_frame"]
        else {
            anyhow::bail!("Old family is not a distribution")
        };
        old_opportunities = old_opportunities
            .checked_add(*old_n)
            .context("Old opportunity overflow")?;
        sources.push(json!({"id":id,"annotation_sha256":binding["annotation_sha256"],"feature_cache_sha256":binding["feature_cache_sha256"]}));
    }
    let expected_authors: BTreeSet<&str> = manifest["authors"]
        .as_array()
        .context("Missing author catalog")?
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    ensure!(authors == expected_authors, "TRAIN author catalog differs");
    write_fresh(
        out,
        &json!({"schema":"slopninja-clause-composition-verification-v1","feature_schema":CompositionalExtractor.identity(),
        "legacy_feature_schema":manifest["feature_schema"],"parser_identity":manifest["parser_identity"],"export_manifest_sha256":expected,
        "queries_sha256":manifest["queries_sha256"],"audit_sha256":manifest["audit_sha256"],"training_posts":rows.len(),"training_authors":authors.len(),
        "exact_document_cache_matches":rows.len(),"legacy_families_unchanged":true,"new_family":FAMILY,"new_family_types":vocabulary.len(),
        "new_family_opportunities":opportunities,"old_clause_head_opportunities":old_opportunities,"unavailable_posts":unavailable,
        "verified_sources":sources,"training_calls":0,"parser_calls":0,"validation_or_test_reads":0}),
    )
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Document {
            input,
            expected_sha256,
            out,
        } => {
            let doc: Document = serde_json::from_slice(&read_bound(&input, &expected_sha256)?)?;
            write_fresh(&out, &CompositionalExtractor.extract(&doc)?)
        }
        Command::Cache {
            input,
            expected_sha256,
            out,
        } => write_fresh(
            &out,
            &project_legacy(&cached(&read_bound(&input, &expected_sha256)?)?)?,
        ),
        Command::VerifyTrain {
            export,
            cache,
            expected_manifest_sha256,
            out,
        } => verify_train(&export, &cache, &expected_manifest_sha256, &out),
    }
}
