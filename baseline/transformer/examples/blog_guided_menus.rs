//! Prepare a fixed menu under several Blog TRAIN reference profiles.
//! Run on the Studio. Corpus text and resulting inputs remain private research data.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::words;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    /// Fresh-root B input with the exact source, brief, claims and chosen menu policy.
    #[arg(long)]
    template: PathBuf,
    /// Original unslop-corpus export; only selected TRAIN text is read.
    #[arg(long)]
    export: PathBuf,
    #[arg(long, required = true)]
    author: Vec<String>,
    /// At most one complete post per supplied date; never concatenate posts.
    #[arg(long, default_value_t = 6)]
    references: usize,
    #[arg(long, default_value = "b-blog-guidance-v1")]
    seed: String,
    #[arg(long)]
    out: PathBuf,
}

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .with_context(|| format!("Missing string {name}"))
}
fn within(root: &Path, base: &Path, path: &str) -> Result<PathBuf> {
    ensure!(!Path::new(path).is_absolute(), "Absolute export path");
    let path = base.join(path).canonicalize()?;
    ensure!(path.starts_with(root), "Path escapes author export");
    Ok(path)
}
fn write(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "Keep prior attempts; use a new output directory"
    );
    ensure!(
        (2..=20).contains(&args.references),
        "Use 2..20 independent dates"
    );
    ensure!(!args.seed.is_empty(), "Empty selection seed");
    let template_bytes = fs::read(&args.template)?;
    let template: Value = serde_json::from_slice(&template_bytes)?;
    ensure!(
        template["schema"] == "slop_ninja_b_edit_menu_input_v1"
            && template["parent_selection_sha256"].is_null(),
        "Expected a fresh-root menu input"
    );
    let source_hash = hash(field(&template["source"], "text")?);
    let root = args.export.canonicalize()?;
    let summary_bytes = fs::read(root.join("summary.json"))?;
    let summary: Value = serde_json::from_slice(&summary_bytes)?;
    ensure!(
        summary["schema"] == "unslop-blog-author-export-v1",
        "Wrong export schema"
    );
    let authors = summary["authors"].as_array().context("Author catalog")?;
    let mut selected_authors = BTreeSet::new();
    let mut inputs = vec![];
    let mut reports = vec![];
    for author in &args.author {
        ensure!(
            !author.is_empty()
                && author.bytes().all(|b| b.is_ascii_digit())
                && selected_authors.insert(author),
            "Invalid or repeated author ID"
        );
        let entries: Vec<_> = authors
            .iter()
            .filter(|r| r["author_id"] == *author)
            .collect();
        ensure!(entries.len() == 1, "Author missing or repeated in export");
        let manifest = within(&root, &root, field(entries[0], "manifest")?)?;
        let manifest_bytes = fs::read(&manifest)?;
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut group_splits = BTreeMap::new();
        let mut ids = BTreeSet::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for line in std::str::from_utf8(&manifest_bytes)?.lines() {
            let post: Value = serde_json::from_str(line)?;
            ensure!(
                post["author_id"] == *author
                    && post["authorship"] == "human"
                    && ids.insert(field(&post, "id")?.to_owned()),
                "Invalid post attribution or repeated ID"
            );
            ensure!(
                post["provenance"]["archive_sha256"] == summary["archive_sha256"],
                "Archive mismatch"
            );
            let split = field(&post, "split")?;
            ensure!(["train", "dev", "test"].contains(&split), "Unknown split");
            *counts.entry(split.to_owned()).or_default() += 1;
            let group = field(&post, "source_group")?.to_owned();
            let date = field(&post["provenance"], "supplied_composition_date")?;
            ensure!(
                group == format!("blog:{author}:date:{date}"),
                "Date-group mismatch"
            );
            if let Some(prior) = group_splits.insert(group.clone(), split.to_owned()) {
                ensure!(prior == split, "A date crosses splits");
            }
            if split == "train" {
                ensure!(
                    post["provenance"]["has_placeholder"] == false
                        && post["provenance"]["has_c1_controls"] == false,
                    "Flagged training post"
                );
                groups.entry(group).or_default().push(post);
            }
        }
        ensure!(
            json!(counts) == entries[0]["split_posts"],
            "Export split counts differ"
        );
        let mut dates: Vec<_> = groups.into_iter().collect();
        dates.sort_by_key(|(date, _)| hash(format!("{}|{author}|{date}", args.seed)));
        let mut references = vec![];
        let mut seen_texts = BTreeSet::from([source_hash.clone()]);
        let mut seen_normalized = BTreeSet::new();
        let mut selected = vec![];
        for (date, mut posts) in dates.into_iter().take(args.references) {
            posts.sort_by_key(|p| hash(format!("{}|{}", args.seed, p["id"])));
            let post = &posts[0];
            let path = within(&root, manifest.parent().unwrap(), field(post, "path")?)?;
            let content = fs::read_to_string(path)?;
            let digest = hash(&content);
            ensure!(
                digest == post["text_sha256"] && seen_texts.insert(digest.clone()),
                "Changed or duplicate reference text"
            );
            let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
            ensure!(
                seen_normalized.insert(hash(normalized)),
                "Duplicate reference after whitespace normalization"
            );
            let count = words(&content).len();
            ensure!(
                count >= 40 && post["provenance"]["word_count"] == count,
                "Word count differs or reference too short"
            );
            references.push(json!({"id":date,"text":content,"provenance":post}));
            selected.push(json!({"post_id":post["id"],"source_group":date,"text_sha256":digest,"words":count}));
        }
        ensure!(
            references.len() >= 2,
            "Insufficient independent reference dates"
        );
        let mut input = template.clone();
        input["references"] = json!(references);
        inputs.push((format!("author-{author}.json"), input));
        reports.push(json!({"author_id":author,"manifest_sha256":hash(&manifest_bytes),"selected":selected,"split_counts":counts}));
    }
    fs::create_dir_all(&args.out)?;
    write(&args.out.join("control.json"), &template)?;
    for (name, input) in inputs {
        write(&args.out.join(name), &input)?;
    }
    write(
        &args.out.join("protocol.json"),
        &json!({
            "schema":"slop_ninja_blog_guided_menus_v1","template_sha256":hash(template_bytes),
            "export_summary_sha256":hash(summary_bytes),"seed":args.seed,"maximum_references":args.references,
            "authors":reports,"root_source_sha256":source_hash,
            "selection":"seeded date rank, then seeded post rank; one whole TRAIN post per date",
            "primary_objective":"whole-document A and Pangram reduction subject to meaning, intended tone and quality constraints",
            "profile_role":"secondary steering signal; source, brief, claims and candidate policy remain identical",
            "scope":"existing development authors; metadata attribution and dates are not independently verified; no generalization claim",
            "held_out_text_opened":false,"training_license":"local noncommercial experiment only",
            "research_only":true,"semantic_preservation_certified":false
        }),
    )
}
