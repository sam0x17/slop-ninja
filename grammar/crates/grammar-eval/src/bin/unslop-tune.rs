use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::space::Space;
use grammar_eval::{
    Values, query_metrics, rank, summarize, weighted_distance,
    weights::{FrozenSelection, SELECTION_SCHEMA, candidates},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Select frozen feature weights from a fixed original-development grid")]
struct Args {
    evaluation: PathBuf,
    #[arg(long)]
    stability: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn read(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path)?).with_context(|| format!("read {}", path.display()))
}
fn digest(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}

fn run(args: Args) -> Result<()> {
    ensure!(!args.out.exists(), "selection output already exists");
    let space: Space = serde_json::from_value(read(&args.evaluation.join("space.json"))?)?;
    space.validate()?;
    let frozen_protocol = read(&args.protocol)?;
    ensure!(
        frozen_protocol["schema"] == "unslop-grammar-weight-transfer-protocol-v1",
        "unsupported experiment protocol"
    );
    ensure!(
        frozen_protocol["source_space_id"] == space.id,
        "experiment source space mismatch"
    );
    ensure!(
        frozen_protocol["candidate_grid"]["grammar_mass"]
            == json!([0.0, 0.1, 0.25, 0.5, 0.75, 1.0])
            && frozen_protocol["candidate_grid"]["grammar_shape"]
                == json!(["equal", "reliability"])
            && frozen_protocol["candidate_grid"]["word_shape"] == "equal word and word_bigram"
            && frozen_protocol["candidate_grid"]["deduplicate_identical_weight_vectors"] == true,
        "declared candidate grid differs from the implemented fixed grid"
    );
    let original_report = read(&args.evaluation.join("report.json"))?;
    ensure!(
        original_report["space_id"] == space.id,
        "source report space differs"
    );
    let implementation = read(&args.evaluation.join("implementation.json"))?;
    for filename in [
        "dev-queries.jsonl",
        "source-summary.json",
        "report.json",
        "space.json",
    ] {
        ensure!(
            implementation["artifacts_sha256"][filename]
                == digest(&args.evaluation.join(filename))?,
            "source evaluation artifact digest differs: {filename}"
        );
    }
    let stability = read(&args.stability)?;
    ensure!(
        stability["source_space_id"] == space.id,
        "stability source space mismatch"
    );
    ensure!(
        stability["feature_schema"] == space.feature_schema
            && stability["parser_identity"] == space.parser_identity,
        "stability feature or parser identity mismatch"
    );
    let source = read(&args.evaluation.join("source-summary.json"))?;
    let authors: BTreeSet<String> = source["authors"]
        .as_array()
        .context("missing source authors")?
        .iter()
        .map(|a| {
            a["author_id"]
                .as_str()
                .context("missing author ID")
                .map(str::to_owned)
        })
        .collect::<Result<_>>()?;
    let stability_authors: BTreeSet<String> =
        serde_json::from_value(stability["source_author_ids"].clone())?;
    ensure!(
        authors == stability_authors,
        "stability author set mismatch"
    );
    let reliability: Values = serde_json::from_value(stability["reliability_weights"].clone())?;
    let grid = candidates(
        &reliability,
        &space.family_schemas.keys().cloned().collect(),
    )?;
    let query_path = args.evaluation.join("dev-queries.jsonl");
    let queries: Vec<Value> = fs::read_to_string(&query_path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    ensure!(!queries.is_empty(), "no development queries");
    ensure!(
        original_report["dev"]["query_count"].as_u64() == Some(queries.len() as u64),
        "development query count differs from source report"
    );
    let mut ids = BTreeSet::new();
    let mut query_authors = BTreeSet::new();
    let mut validated = Vec::new();
    for query in &queries {
        ensure!(
            query["post"]["split"] == "dev",
            "non-development query in selection input"
        );
        let own = query["post"]["author_id"]
            .as_str()
            .context("missing true author")?
            .to_owned();
        let id = query["post"]["id"].as_str().context("missing query ID")?;
        ensure!(
            authors.contains(&own) && ids.insert(id),
            "unknown author or duplicate development query"
        );
        query_authors.insert(own.clone());
        let impostor = query["content_matched_impostor"]
            .as_str()
            .context("missing content impostor")?
            .to_owned();
        ensure!(
            authors.contains(&impostor) && impostor != own,
            "invalid content impostor"
        );
        let distances: BTreeMap<String, Values> =
            serde_json::from_value(query["unweighted_squared_family_distances"].clone())?;
        ensure!(
            distances.keys().cloned().collect::<BTreeSet<_>>() == authors,
            "candidate author set differs"
        );
        for families in distances.values() {
            ensure!(
                families.keys().eq(space.family_schemas.keys()),
                "distance family catalog mismatch"
            );
            ensure!(
                families.values().all(|d| d.is_finite() && *d >= 0.0),
                "invalid saved family distance"
            );
        }
        validated.push((own, impostor, distances));
    }
    ensure!(
        query_authors == authors,
        "development queries missing authors"
    );
    let mut results = Vec::new();
    let mut best = None;
    let mut best_score = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (index, candidate) in grid.iter().enumerate() {
        let rows: Vec<_> = validated
            .iter()
            .map(|(own, impostor, distances)| {
                let ranking = rank(
                    distances
                        .iter()
                        .map(|(author, families)| {
                            (
                                author.clone(),
                                weighted_distance(families, &candidate.weights),
                            )
                        })
                        .collect(),
                )?;
                Ok((own.clone(), query_metrics(&ranking, own, impostor)?))
            })
            .collect::<Result<_>>()?;
        let summary = summarize(&rows)?;
        let score = (summary.macro_author.top1, summary.macro_author.mrr);
        if score > best_score {
            best = Some(index);
            best_score = score;
        }
        results.push(json!({"candidate":candidate,"development":summary}));
    }
    let winner = &grid[best.context("empty candidate grid")?];
    let selection = FrozenSelection {
        schema: SELECTION_SCHEMA.into(),
        selected_name: winner.name.clone(),
        weights: winner.weights.clone(),
        selection_author_ids: authors,
        source_space_id: space.id,
        feature_schema: space.feature_schema,
        parser_identity: space.parser_identity,
    };
    let mut output = serde_json::to_value(selection)?;
    output["candidate_results"] = json!(results);
    output["development_query_count"] = json!(queries.len());
    output["input_sha256"] = json!({"experiment_protocol":digest(&args.protocol)?,"stability":digest(&args.stability)?,"dev_queries":digest(&query_path)?,"source_summary":digest(&args.evaluation.join("source-summary.json"))?});
    output["selection_rule"] = json!(
        "macro-author dev top1, then MRR, then smaller grammar mass, then equal shape, then candidate name; fixed grid; no original-test queries"
    );
    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.out, serde_json::to_vec_pretty(&output)?)?;
    println!(
        "Selected {}: development macro top1 {:.4}, MRR {:.4}; {} candidates",
        winner.name,
        best_score.0,
        best_score.1,
        grid.len()
    );
    Ok(())
}
fn main() -> Result<()> {
    run(Args::parse())
}
