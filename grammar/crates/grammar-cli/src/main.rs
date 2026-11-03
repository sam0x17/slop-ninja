use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    features::{self, Features},
    rules,
    space::{self, Config, Reference, Space},
    syntax::Document,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    about = "Grammar and word occurrence: explicit spaces and deterministic edit neighborhoods"
)]
struct Cli {
    #[arg(long, global = true)]
    python: Option<String>,
    #[arg(long, global = true, default_value = grammar_spacy::DEFAULT_MODEL)]
    parser_model: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse exact UTF-8 text with the local spaCy adapter.
    Annotate {
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Extract counts and grammatical features; --annotated needs no Python.
    Features {
        input: PathBuf,
        #[arg(long)]
        annotated: bool,
        #[arg(long)]
        out: PathBuf,
    },
    /// Fit a frozen space from a JSON array of typed Reference objects.
    Fit {
        references: PathBuf,
        #[arg(long)]
        basis: Vec<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Project saved features into an existing space; unknown axes are rejected.
    Project {
        features: PathBuf,
        #[arg(long)]
        space: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Generate, reparse and rank one-edit possibilities using raw reference samples.
    Neighborhood {
        input: PathBuf,
        #[arg(long)]
        references: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 32)]
        max_candidates: usize,
        #[arg(long, value_delimiter = ',')]
        rules: Vec<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Print the built-in, versioned rule catalog.
    Rules,
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("decode {}", path.display()))
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}; outputs are never overwritten", path.display()))?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn config(path: Option<&Path>) -> Result<Config> {
    path.map(read).transpose().map(Option::unwrap_or_default)
}

fn event(out: &Path, stage: &str, status: &str, detail: Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out.join("events.jsonl"))?;
    serde_json::to_writer(
        &mut file,
        &json!({"stage":stage,"status":status,"detail":detail}),
    )?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn snapshot(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

#[derive(Deserialize)]
struct Sample {
    id: String,
    path: PathBuf,
    source_group: String,
    split: String,
    authorship: String,
    author_id: String,
    #[serde(default)]
    provenance: Value,
}

fn load_references(
    manifest: &Path,
    parser: &grammar_spacy::Parser<'_>,
    out: &Path,
) -> Result<(Vec<Reference>, Value)> {
    let manifest_bytes = fs::read(manifest)?;
    snapshot(&out.join("references.manifest.jsonl"), &manifest_bytes)?;
    let text = std::str::from_utf8(&manifest_bytes)?;
    let rows: Vec<Sample> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line).with_context(|| format!("reference manifest line {}", i + 1))
        })
        .collect::<Result<_>>()?;
    ensure!(!rows.is_empty(), "empty reference manifest");
    let mut ids = BTreeSet::new();
    let mut splits = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut author = None;
    let mut references = Vec::new();
    let mut audit = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        ensure!(
            !row.id.trim().is_empty() && ids.insert(&row.id),
            "empty or duplicate sample id"
        );
        ensure!(!row.source_group.trim().is_empty(), "empty source group");
        ensure!(
            matches!(row.split.as_str(), "train" | "dev" | "test"),
            "explicit train/dev/test split required"
        );
        if let Some(old) = splits.insert(&row.source_group, &row.split) {
            ensure!(old == &row.split, "source group crosses splits");
        }
        let path = manifest.parent().unwrap_or(Path::new(".")).join(&row.path);
        let source =
            fs::read_to_string(&path).with_context(|| format!("read sample {}", row.id))?;
        let hash = hex::encode(Sha256::digest(source.as_bytes()));
        if let Some(expected) = row.provenance.get("excerpt_sha256") {
            ensure!(
                expected.as_str() == Some(hash.as_str()),
                "sample {} differs from its pinned excerpt hash",
                row.id
            );
        }
        ensure!(
            hashes.insert(hash.clone(), row.split.clone()).is_none(),
            "duplicate sample text across reference manifest"
        );
        let selected = row.split == "train";
        audit.push(json!({"id":row.id,"source_group":row.source_group,"split":row.split,"authorship":row.authorship,"author_id":row.author_id,"source_sha256":hash,"used_for_fit":selected,"provenance":row.provenance}));
        write(
            &out.join(format!("reference-{i:03}.audit.json")),
            audit.last().unwrap(),
        )?;
        if !selected {
            continue;
        }
        ensure!(
            matches!(
                row.authorship.as_str(),
                "user_declared_human" | "human" | "synthetic_fixture"
            ),
            "training reference needs explicit human provenance or synthetic-fixture label"
        );
        ensure!(!row.author_id.trim().is_empty(), "missing author id");
        if let Some(old) = author {
            ensure!(old == &row.author_id, "training reference mixes authors");
        }
        author = Some(&row.author_id);
        eprintln!("reference {}: parsing", i + 1);
        let document = parser.parse(&source)?;
        let features = features::extract(&document)?;
        write(
            &out.join(format!("reference-{i:03}.features.json")),
            &features,
        )?;
        references.push(Reference {
            source_group: row.source_group.clone(),
            features,
        });
    }
    let audit = json!({"manifest_sha256":hex::encode(Sha256::digest(&manifest_bytes)),"author_id":author,"samples":audit});
    Ok((references, audit))
}

fn neighborhood(
    input: &Path,
    manifest: &Path,
    out: &Path,
    max_candidates: usize,
    selected_rules: &[String],
    config: Config,
    parser: &grammar_spacy::Parser<'_>,
) -> Result<()> {
    ensure!(
        (1..=rules::MAX_CANDIDATES).contains(&max_candidates),
        "candidate limit must be between 1 and {}",
        rules::MAX_CANDIDATES
    );
    let selected: BTreeSet<_> = selected_rules.iter().map(String::as_str).collect();
    ensure!(
        selected.len() == selected_rules.len()
            && selected
                .iter()
                .all(|rule| rules::BUILTIN_RULES.contains(rule)),
        "unknown or duplicate rewrite rule selection"
    );
    ensure!(
        !out.exists(),
        "output directory already exists; choose a new run directory"
    );
    fs::create_dir_all(out)?;
    event(
        out,
        "run",
        "started",
        json!({"input":input,"references":manifest,"catalog_version":rules::CATALOG_VERSION,"max_candidates":max_candidates,"selected_rules":selected_rules,"config":config}),
    )?;
    let result = (|| -> Result<()> {
        let text = fs::read_to_string(input)?;
        snapshot(&out.join("source.txt"), text.as_bytes())?;
        event(
            out,
            "source",
            "parsing",
            json!({"source_sha256":hex::encode(Sha256::digest(text.as_bytes()))}),
        )?;
        let source_doc = parser.parse(&text)?;
        let source_features = features::extract(&source_doc)?;
        write(&out.join("source.syntax.json"), &source_doc)?;
        write(&out.join("source.features.json"), &source_features)?;
        let (references, reference_audit) = load_references(manifest, parser, out)?;
        write(&out.join("references.audit.json"), &reference_audit)?;
        // An exact reference copy is not an independent source to move toward it.
        ensure!(
            !references
                .iter()
                .any(|r| r.features.source_sha256 == source_features.source_sha256),
            "source is an exact training reference"
        );
        let candidates = if selected_rules.is_empty() {
            rules::generate(&source_doc, max_candidates)?
        } else {
            rules::generate_selected(&source_doc, max_candidates, selected_rules)?
        };
        write(&out.join("generated.json"), &candidates)?;
        let mut parsed = Vec::new();
        let mut rejected = Vec::new();
        let mut basis = vec![source_features.clone()];
        for (i, candidate) in candidates.into_iter().enumerate() {
            eprintln!("candidate {}: parsing", i + 1);
            event(
                out,
                "candidate",
                "parsing",
                json!({"index":i,"id":candidate.id,"candidate_sha256":candidate.candidate_sha256}),
            )?;
            let result = parser
                .parse(&candidate.text)
                .and_then(|doc| features::extract(&doc));
            match result {
                Ok(features) => {
                    write(
                        &out.join(format!("candidate-{i:03}.features.json")),
                        &features,
                    )?;
                    basis.push(features.clone());
                    parsed.push((candidate, features));
                }
                Err(error) => {
                    let rejection = json!({"candidate":candidate,"reason":format!("{error:#}")});
                    write(
                        &out.join(format!("candidate-{i:03}.failure.json")),
                        &rejection,
                    )?;
                    event(
                        out,
                        "candidate",
                        "failed",
                        json!({"index":i,"error":format!("{error:#}")}),
                    )?;
                    rejected.push(rejection);
                }
            }
        }
        event(
            out,
            "fit",
            "started",
            json!({"references":references.len(),"basis":basis.len()}),
        )?;
        let space = space::fit(&references, &basis, &config)?;
        write(&out.join("space.json"), &space)?;
        let source_point = space.project(&source_features)?;
        let source_distance = space.distance(&source_point)?;
        let mut ranked = Vec::new();
        for (candidate, features) in parsed {
            let point = space.project(&features)?;
            let distance = space.distance(&point)?;
            let improvement = source_distance.euclidean - distance.euclidean;
            let movements = space.movements(&source_point, &point)?;
            ranked.push(json!({"candidate":candidate,"point":point,"distance":distance,"distance_improvement":improvement,"movements":movements,"requires_preservation_review":true}));
        }
        ranked.sort_by(|a, b| {
            b["distance_improvement"]
                .as_f64()
                .unwrap()
                .total_cmp(&a["distance_improvement"].as_f64().unwrap())
                .then_with(|| {
                    a["candidate"]["id"]
                        .as_str()
                        .cmp(&b["candidate"]["id"].as_str())
                })
        });
        let improving = ranked
            .iter()
            .filter(|c| c["distance_improvement"].as_f64().unwrap() > 0.0)
            .count();
        let report = json!({
            "schema":"unslop-grammar-neighborhood-v1", "space_id":space.id,
            "axes":space.axes.len(), "source_point":source_point, "source_distance":source_distance,
            "references":reference_audit, "generator":{"catalog_version":rules::CATALOG_VERSION,"rules":if selected_rules.is_empty(){rules::BUILTIN_RULES.to_vec()}else{selected_rules.iter().map(String::as_str).collect()},"max_candidates":max_candidates,"enumeration":"round-robin across rules, source order within each rule","scope":"one rule application per candidate"},
            "candidates":ranked,"rejected_candidates":rejected,"improving_candidates":improving,
            "recommendation":Value::Null,"meaning_preservation":"unreviewed; geometric improvement alone does not establish fidelity",
            "llm_calls":0,"detector_calls":0
        });
        write(&out.join("report.json"), &report)?;
        println!(
            "{} axes; {} candidates; {} closer to target; source distance {:.6}",
            space.axes.len(),
            report["candidates"].as_array().unwrap().len(),
            improving,
            source_distance.euclidean
        );
        println!("{}", out.join("report.json").display());
        Ok(())
    })();
    match result {
        Ok(()) => event(out, "run", "complete", json!({"report":"report.json"})),
        Err(error) => {
            if let Err(persistence_error) =
                event(out, "run", "failed", json!({"error":format!("{error:#}")}))
            {
                eprintln!("Could not persist failure record: {persistence_error:#}");
            }
            Err(error)
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let python = cli.python.unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.venv/bin/python")
            .to_string_lossy()
            .into_owned()
    });
    let parser = grammar_spacy::Parser {
        python: &python,
        model: &cli.parser_model,
    };
    match cli.command {
        Command::Annotate { input, out } => {
            write(&out, &parser.parse(&fs::read_to_string(input)?)?)
        }
        Command::Features {
            input,
            annotated,
            out,
        } => {
            let document: Document = if annotated {
                read(&input)?
            } else {
                parser.parse(&fs::read_to_string(input)?)?
            };
            write(&out, &features::extract(&document)?)
        }
        Command::Fit {
            references,
            basis,
            config: config_path,
            out,
        } => {
            let references: Vec<Reference> = read(&references)?;
            let basis: Vec<Features> =
                basis.iter().map(|path| read(path)).collect::<Result<_>>()?;
            write(
                &out,
                &space::fit(&references, &basis, &config(config_path.as_deref())?)?,
            )
        }
        Command::Project {
            features,
            space,
            out,
        } => {
            let space: Space = read(&space)?;
            let point = space.project(&read(&features)?)?;
            write(
                &out,
                &json!({"distance":space.distance(&point)?,"point":point}),
            )
        }
        Command::Neighborhood {
            input,
            references,
            out,
            max_candidates,
            rules,
            config: config_path,
        } => neighborhood(
            &input,
            &references,
            &out,
            max_candidates,
            &rules,
            config(config_path.as_deref())?,
            &parser,
        ),
        Command::Rules => {
            let catalog = json!({
                "schema":"unslop-rule-catalog-v1",
                "catalog_version":rules::CATALOG_VERSION,
                "rules":rules::CATALOG.iter().map(|rule| json!({
                    "id":rule.id,"version":rule.version,"preconditions":rule.preconditions,"risks":rule.risks
                })).collect::<Vec<_>>()
            });
            println!("{}", serde_json::to_string_pretty(&catalog)?);
            Ok(())
        }
    }
}
