//! Hash-bound later-date inputs, separate from fixed TRAIN profile estimation.
use crate::{
    edit_choice_observation, edit_preference_model as model,
    preference_chronology_evaluation as evaluation,
};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::syntax::Document;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn checked(path: &Path, sha: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    ensure!(hash(&bytes) == sha, "Hash mismatch {}", path.display());
    Ok(bytes)
}
fn bound(repo: &Path, v: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(
        &repo.join(text(v, "path")?),
        text(v, "sha256")?,
    )?)?)
}
fn fresh(path: &Path, v: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(v)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(hash(&bytes))
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a fresh output directory");
    let protocol: Value =
        serde_json::from_slice(&checked(&args.protocol, &args.expected_protocol_sha256)?)?;
    ensure!(
        protocol["schema"] == "slopninja-preference-chronology-protocol-v1"
            && protocol["expected_train_posts"] == 3889
            && protocol["expected_authors"] == 300
            && protocol["expected_query_posts"] == 1080
            && protocol["primary_scope"] == json!(["dev", "test"])
            && protocol["query_updates"] == false
            && protocol["author_prior_effective_dates"] == 4,
        "Chronology protocol differs"
    );
    for (name, sha) in protocol["source_files"]
        .as_object()
        .context("Source bindings")?
    {
        checked(&args.repo.join(name), sha.as_str().context("Source hash")?)?;
    }
    fs::create_dir_all(&args.out)?;
    for name in protocol["source_files"].as_object().unwrap().keys() {
        let to = args.out.join("executed-source").join(name);
        fs::create_dir_all(to.parent().unwrap())?;
        fs::copy(args.repo.join(name), to)?;
    }
    fresh(&args.out.join("protocol.json"), &protocol)?;
    let result = run(&args, &protocol);
    if let Err(error) = &result {
        fresh(
            &args.out.join("failure.json"),
            &json!({"status":"failed; retained attempt","error":format!("{error:#}")}),
        )?;
    }
    result
}

fn run(args: &Args, p: &Value) -> Result<()> {
    let started = Instant::now();
    let receipt = bound(&args.repo, &p["training_receipt"])?;
    ensure!(
        receipt["artifacts_sha256"]["model-input.json"] == p["training_input"]["sha256"],
        "TRAIN input receipt differs"
    );
    let training: Vec<model::Post> =
        serde_json::from_value(bound(&args.repo, &p["training_input"])?)?;
    ensure!(
        training.len() == 3889
            && training
                .iter()
                .map(|post| &post.author)
                .collect::<BTreeSet<_>>()
                .len()
                == 300,
        "TRAIN scope differs"
    );
    let metadata = bound(&args.repo, &p["later_posts"])?;
    let bindings = metadata.as_array().context("Later metadata array")?;
    ensure!(bindings.len() == 1080, "Later-post scope differs");
    let training_binding = bound(&args.repo, &p["training_metadata"])?;
    let training_rows = training_binding
        .as_array()
        .context("TRAIN metadata array")?;
    let mut train_ids = BTreeMap::new();
    let mut train_texts = BTreeSet::new();
    let mut train_groups = BTreeSet::new();
    for row in training_rows {
        let post = &row["post"];
        ensure!(
            post["split"] == "train"
                && train_ids
                    .insert(text(post, "id")?.to_owned(), row)
                    .is_none()
                && train_texts.insert(text(post, "text_sha256")?.to_owned()),
            "Repeated/non-TRAIN metadata"
        );
        train_groups.insert(text(post, "source_group")?.to_owned());
    }
    ensure!(
        train_ids.len() == training.len(),
        "TRAIN metadata count differs"
    );
    for post in &training {
        let expected = train_ids
            .get(&post.id)
            .context("Unbound TRAIN model input")?;
        ensure!(
            post.author == expected["post"]["author"]
                && post.date == expected["post"]["date"]
                && post.panel == expected["panel"],
            "TRAIN model/metadata differs"
        );
    }
    let mut seen = BTreeSet::new();
    let mut texts = BTreeSet::new();
    let mut groups = BTreeMap::<String, (String, String, String)>::new();
    let mut query_posts = Vec::new();
    let mut split_posts = BTreeMap::<String, Vec<model::Post>>::new();
    let mut panel_split_counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut coverage = BTreeMap::<String, u64>::new();
    let mut output = BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.out.join("observations.jsonl"))?,
    );
    for binding in bindings {
        let post = &binding["post"];
        let id = text(post, "id")?;
        let author = text(post, "author")?;
        let date = text(post, "date")?;
        let split = text(post, "split")?;
        let group = text(post, "source_group")?;
        let text_sha = text(post, "text_sha256")?;
        let panel = text(binding, "panel")?;
        ensure!(
            ["dev", "test"].contains(&split)
                && seen.insert(id.to_owned())
                && !train_ids.contains_key(id)
                && texts.insert(text_sha.to_owned())
                && !train_texts.contains(text_sha),
            "Repeated or cross-split source"
        );
        ensure!(
            group == format!("blog:{author}:date:{date}") && !train_groups.contains(group),
            "Source-group leakage"
        );
        if let Some(previous) =
            groups.insert(group.into(), (author.into(), date.into(), split.into()))
        {
            ensure!(
                previous == (author.into(), date.into(), split.into()),
                "Date group crosses split"
            );
        }
        let doc: Document = serde_json::from_slice(&checked(
            &args.repo.join(text(binding, "annotation_path")?),
            text(binding, "annotation_sha256")?,
        )?)?;
        ensure!(
            doc.parser_identity == p["parser_identity"] && hash(doc.text.as_bytes()) == text_sha,
            "Query annotation identity differs"
        );
        let observed = edit_choice_observation::observe(&doc)?;
        let observations = observed
            .opportunities
            .iter()
            .map(|choice| model::Observation {
                conditioning_key: choice.conditioning_key.clone(),
                contracted: choice.observed_form
                    == edit_choice_observation::ObservedForm::Contracted,
            })
            .collect();
        let model_post = model::Post {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            panel: panel.into(),
            observations,
        };
        query_posts.push(model_post.clone());
        split_posts
            .entry(split.into())
            .or_default()
            .push(model_post);
        *panel_split_counts
            .entry(panel.into())
            .or_default()
            .entry(split.into())
            .or_default() += 1;
        let value = serde_json::to_value(&observed.coverage)?;
        for (name, count) in value.as_object().unwrap() {
            if let Some(count) = count.as_u64() {
                *coverage.entry(name.clone()).or_default() += count;
            }
        }
        serde_json::to_writer(
            &mut output,
            &json!({"panel":panel,"post":post,"annotation_sha256":binding["annotation_sha256"],"observation":observed}),
        )?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    ensure!(
        query_posts.len() == 1080
            && serde_json::to_value(&panel_split_counts)? == p["expected_panel_split_posts"],
        "Query panel/split scope differs"
    );
    fresh(
        &args.out.join("query-input.json"),
        &serde_json::to_value(&query_posts)?,
    )?;
    let combined = evaluation::evaluate(training.clone(), query_posts)?;
    fresh(&args.out.join("evaluation.json"), &combined)?;
    let mut splits = BTreeMap::new();
    for (split, queries) in split_posts {
        let result = evaluation::evaluate(training.clone(), queries)?;
        fresh(&args.out.join(format!("evaluation-{split}.json")), &result)?;
        splits.insert(split,json!({"coverage":result["coverage"],"pooled":result["pooled"],"panels":result["panels"]}));
    }
    let report = json!({"schema":"slopninja-preference-chronology-study-v1","status":"complete","protocol_sha256":args.expected_protocol_sha256,"training_posts":training.len(),"query_posts":1080,"panel_split_posts":panel_split_counts,"opportunity_coverage":coverage,"coverage":combined["coverage"],"pooled":combined["pooled"],"panels":combined["panels"],"bootstrap":combined["paired_author_bootstrap"],"prediction_coverage":combined["opportunity_prediction_coverage"],"split_summaries_descriptive":splits,"elapsed_seconds":started.elapsed().as_secs_f64(),"new_author_allocations":0,"llm_calls":0,"detector_calls":0,"new_parser_calls":0,"optimizer_steps":0,"query_updates":false,
        "interpretation":"Later DEV+TEST writing from known authors using fixed TRAIN profiles. Within-author chronology, not a global calendar-time cutoff; prior feature development used these authors. No semantic/readability/tone/detector judgment."});
    fresh(&args.out.join("report.json"), &report)?;
    let mut artifacts = BTreeMap::new();
    for name in [
        "observations.jsonl",
        "query-input.json",
        "evaluation.json",
        "evaluation-dev.json",
        "evaluation-test.json",
        "report.json",
    ] {
        artifacts.insert(name, hash(&fs::read(args.out.join(name))?));
    }
    fresh(
        &args.out.join("receipt.json"),
        &json!({"schema":"slopninja-preference-chronology-receipt-v1","protocol_sha256":args.expected_protocol_sha256,"sources":p["source_files"],"artifacts_sha256":artifacts,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?)}),
    )?;
    println!(
        "Completed fixed later-date preferences on 1080 posts; {}",
        args.out.display()
    );
    Ok(())
}
