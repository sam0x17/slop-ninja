use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use unslop::{
    collection, evaluation, experiments, features, human_pilot,
    provider_ids::ProviderIds,
    statistics, store,
    util::{digest, read, read_jsonl, write_json},
    workflows,
};

#[derive(Parser)]
#[command(
    version,
    about = "Corpus profiling and controlled prose revision experiments"
)]
struct Cli {
    #[arg(long, default_value = "data/unslop.sqlite3")]
    db: PathBuf,
    #[arg(long, default_value = ".venv/bin/python")]
    python: String,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Args)]
struct ProfileArgs {
    #[arg(long, default_value = "word")]
    family: String,
    #[arg(long, default_value = "train")]
    split: String,
    #[arg(long)]
    domain: Option<String>,
    #[arg(long)]
    register: Option<String>,
    #[arg(long)]
    extractor: Option<String>,
}
#[derive(Subcommand)]
enum Commands {
    Init,
    Status,
    Ingest {
        jsonl: PathBuf,
        #[arg(long)]
        grammar: bool,
    },
    ImportPangram {
        directory: PathBuf,
        #[arg(long, default_value = "preface-experiments")]
        corpus: String,
        #[arg(long, default_value = "cit-preface")]
        group_id: String,
        #[arg(long)]
        grammar: bool,
    },
    Profile {
        #[arg(long)]
        corpus: String,
        #[command(flatten)]
        filter: ProfileArgs,
    },
    Compare {
        #[arg(long)]
        left: String,
        #[arg(long)]
        right: String,
        #[command(flatten)]
        filter: ProfileArgs,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long, default_value_t = 5)]
        min_count: u64,
        #[arg(long, default_value_t = 3)]
        min_documents: u64,
    },
    Analyze {
        text: PathBuf,
        #[arg(long)]
        reference: String,
        #[arg(long)]
        group_id: Option<String>,
        #[arg(long)]
        grammar: bool,
        #[command(flatten)]
        filter: ProfileArgs,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    InspectPair {
        source: PathBuf,
        candidate: PathBuf,
        #[arg(long)]
        grammar: bool,
    },
    Perturb {
        source: PathBuf,
        #[arg(long)]
        old: String,
        #[arg(long)]
        new: String,
        #[arg(long, default_value_t = 1)]
        occurrence: usize,
        #[arg(long)]
        out: PathBuf,
    },
    Runs {
        #[arg(long)]
        corpus: Option<String>,
        #[arg(long, default_value_t = 0.1)]
        threshold: f64,
    },
    Study {
        baseline: PathBuf,
        candidate: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 3)]
        repeats: usize,
        #[arg(long)]
        max_requests: usize,
        #[arg(long, default_value = "pangram-4")]
        model: String,
        #[arg(long, default_value_t = 600.0)]
        timeout: f64,
        #[arg(long, default_value_t = 0.1)]
        threshold: f64,
        #[arg(long, required_unless_present = "section", conflicts_with = "section")]
        full_document: bool,
        #[arg(long)]
        section: bool,
        #[arg(long)]
        audit: Option<PathBuf>,
    },
    Score {
        text: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "pangram-4")]
        model: String,
        #[arg(long)]
        max_requests: usize,
    },
    Collect {
        prompts: PathBuf,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        corpus: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        max_requests: usize,
        #[arg(long, default_value_t = 2048)]
        max_output_tokens: u64,
    },
    HumanPilot {
        #[arg(long, default_value = "data/matched-pilot-v1")]
        out: PathBuf,
        #[arg(
            long,
            default_value = "experiments/matched-pilot-v1/human-source-manifest.json"
        )]
        manifest: PathBuf,
    },
    RewritePrompts {
        human: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    EditPrompts {
        #[arg(required = true, num_args = 1..)]
        sources: Vec<PathBuf>,
        #[arg(long)]
        instructions: PathBuf,
        #[arg(long)]
        split: String,
        #[arg(long)]
        out: PathBuf,
    },
    FinewebSample {
        #[arg(long, default_value = "data/public-datasets/fineweb-2021-43-pilot")]
        out: PathBuf,
        #[arg(long, default_value_t = 100)]
        rows: usize,
    },
    ValidateSubmission {
        challenge: PathBuf,
        submission: PathBuf,
    },
    Reward {
        challenge: PathBuf,
        submission: PathBuf,
        records: PathBuf,
        audit: PathBuf,
    },
    AttachScores {
        #[arg(long)]
        corpus: String,
        directory: PathBuf,
    },
    PilotReport {
        #[arg(long, default_value = "human-plos-abstracts-v1")]
        human: String,
        #[arg(long,num_args=1..)]
        models: Vec<String>,
        #[arg(
            long,
            default_value = "experiments/matched-pilot-v1/profile-report.json"
        )]
        out: PathBuf,
    },
    ScoreCorpus {
        jsonl: PathBuf,
        #[arg(long, default_value = "train")]
        split: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "pangram-4")]
        model: String,
        #[arg(long)]
        max_requests: usize,
    },
    Features {
        text: PathBuf,
        #[arg(long)]
        grammar: bool,
    },
    /// Fit an author and register profile from declared writing samples.
    StyleFit {
        samples: PathBuf,
        #[arg(long)]
        author: String,
        #[arg(long)]
        register: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        grammar: bool,
    },
    /// Measure a text against a frozen author profile and optional controls.
    StyleCompare {
        profile: PathBuf,
        text: PathBuf,
        #[arg(long)]
        target: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Set explicit accessibility proxies and rhetorical editing instructions.
    StyleControls {
        profile: PathBuf,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        accessibility: f64,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        rhetorical_strength: f64,
        #[arg(long)]
        lexicon_weight: Option<f64>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Prepare bounded LLM proposals without making model calls.
    StylePlan {
        profile: PathBuf,
        source: PathBuf,
        #[arg(long)]
        target: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 3)]
        candidates: usize,
        #[arg(long, default_value_t = 0.15)]
        max_edit_ratio: f64,
    },
    /// Remeasure proposals; recommend only improvements with an exact review.
    StyleRank {
        profile: PathBuf,
        source: PathBuf,
        candidates: PathBuf,
        #[arg(long)]
        target: Option<PathBuf>,
        #[arg(long)]
        reviews: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 0.15)]
        max_edit_ratio: f64,
    },
}
fn prof(
    db: &rusqlite::Connection,
    corpus: &str,
    f: &ProfileArgs,
    excluded: &[String],
) -> Result<Value> {
    store::profile(
        db,
        corpus,
        &f.family,
        &f.split,
        f.domain.as_deref(),
        f.register.as_deref(),
        f.extractor.as_deref(),
        excluded,
    )
}
fn execute(c: Cli) -> Result<Value> {
    match c.command {
        Commands::StyleFit {
            samples,
            author,
            register,
            out,
            grammar,
        } => {
            return unslop::style_workflows::fit(
                &samples, &author, &register, &out, grammar, &c.python,
            );
        }
        Commands::StyleCompare {
            profile,
            text,
            target,
            out,
        } => {
            let report =
                unslop::style_workflows::compare(&profile, &text, target.as_deref(), &c.python)?;
            if let Some(path) = out {
                unslop::style_workflows::save_report(&path, &report)?;
            }
            return Ok(report);
        }
        Commands::StyleControls {
            profile,
            accessibility,
            rhetorical_strength,
            lexicon_weight,
            out,
        } => {
            return unslop::style_workflows::controls(
                &profile,
                accessibility,
                rhetorical_strength,
                lexicon_weight,
                &out,
            );
        }
        Commands::StylePlan {
            profile,
            source,
            target,
            out,
            candidates,
            max_edit_ratio,
        } => {
            return unslop::style_workflows::plan(
                &profile,
                &source,
                target.as_deref(),
                &out,
                candidates,
                max_edit_ratio,
                &c.python,
            );
        }
        Commands::StyleRank {
            profile,
            source,
            candidates,
            target,
            reviews,
            out,
            max_edit_ratio,
        } => {
            return unslop::style_workflows::rank(unslop::style_workflows::RankOptions {
                profile: &profile,
                source: &source,
                candidates: &candidates,
                target: target.as_deref(),
                reviews: reviews.as_deref(),
                out: &out,
                max_edit_ratio,
                python: &c.python,
            });
        }
        Commands::Collect {
            prompts,
            provider,
            model,
            corpus,
            out,
            max_requests,
            max_output_tokens,
        } => {
            return collection::collect_batch(
                &prompts,
                &provider,
                &model,
                &corpus,
                &out,
                max_requests,
                max_output_tokens,
            );
        }
        Commands::HumanPilot { out, manifest } => return human_pilot::build(&out, &manifest),
        Commands::FinewebSample { out, rows } => {
            return unslop::public_datasets::fineweb_sample(&out, rows);
        }
        Commands::ValidateSubmission {
            challenge,
            submission,
        } => {
            return unslop::subnet::validate_submission(
                &serde_json::from_str(&read(&challenge)?)?,
                &serde_json::from_str(&read(&submission)?)?,
            );
        }
        Commands::Reward {
            challenge,
            submission,
            records,
            audit,
        } => {
            let ids = ProviderIds::load()?;
            let report = unslop::subnet::reward(
                &serde_json::from_str(&read(&challenge)?)?,
                &serde_json::from_str(&read(&submission)?)?,
                &serde_json::from_str::<Vec<Value>>(&read(&records)?)?,
                &serde_json::from_str(&read(&audit)?)?,
                &ids,
            )?;
            ids.record("unslop reward (standard output)")?;
            return Ok(report);
        }
        Commands::RewritePrompts { human, out } => {
            return collection::make_rewrite_prompts(&human, &out);
        }
        Commands::EditPrompts {
            sources,
            instructions,
            split,
            out,
        } => {
            return collection::make_edit_prompts(&sources, &instructions, &split, &out);
        }
        Commands::ScoreCorpus {
            jsonl,
            split,
            out,
            model,
            max_requests,
        } => return workflows::score_corpus(&jsonl, &split, &out, &model, max_requests),
        Commands::Features { text, grammar } => {
            return features::extract(&read(&text)?, grammar, &c.python);
        }
        Commands::InspectPair {
            source,
            candidate,
            grammar,
        } => {
            let a = read(&source)?;
            let b = read(&candidate)?;
            return experiments::compare_features(
                &a,
                &b,
                &features::extract(&a, grammar, &c.python)?,
                &features::extract(&b, grammar, &c.python)?,
            );
        }
        Commands::Perturb {
            source,
            old,
            new,
            occurrence,
            out,
        } => {
            let (text, m) = experiments::substitute(&read(&source)?, &old, &new, occurrence)?;
            let mp = PathBuf::from(format!("{}.json", out.display()));
            ensure!(
                !out.exists() && !mp.exists(),
                "Candidate output already exists"
            );
            fs::create_dir_all(out.parent().unwrap_or(std::path::Path::new(".")))?;
            fs::write(&out, text)?;
            write_json(&mp, &m)?;
            return Ok(m);
        }
        Commands::Study {
            baseline,
            candidate,
            out,
            repeats,
            max_requests,
            model,
            timeout,
            threshold,
            full_document,
            section: _,
            audit,
        } => {
            return workflows::study(workflows::StudyOptions {
                baseline: &baseline,
                candidate: &candidate,
                out: &out,
                model: &model,
                repeats,
                max_requests,
                timeout,
                threshold,
                full_document,
                audit: audit.as_deref(),
            });
        }
        Commands::Score {
            text,
            out,
            model,
            max_requests,
        } => return workflows::score_file(&text, &out, &model, max_requests),
        _ => {}
    }
    let mut db = store::connect(&c.db)?;
    match c.command {
        Commands::Init | Commands::Status => store::status(&db),
        Commands::AttachScores { corpus, directory } => {
            let mut paths = fs::read_dir(&directory)?
                .map(|r| r.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.retain(|p| {
                p.extension().is_some_and(|e| e == "json")
                    && p.file_name().is_some_and(|n| n != "summary.json")
            });
            paths.sort();
            ensure!(!paths.is_empty(), "No score records found");
            let tx = db.transaction()?;
            let mut added = 0;
            for path in paths {
                let r: Value = serde_json::from_str(&read(&path)?)?;
                evaluation::validate_run(&r)?;
                let id: i64 = tx
                    .query_row(
                        "SELECT id FROM documents WHERE corpus=? AND sha256=?",
                        rusqlite::params![
                            corpus,
                            r["sha256"].as_str().context("Missing score hash")?
                        ],
                        |row| row.get(0),
                    )
                    .context("Import the attributed corpus before attaching scores")?;
                added += usize::from(store::add_run(&tx, id, &r)?);
            }
            tx.commit()?;
            Ok(json!({"corpus":corpus,"attached_runs":added}))
        }
        Commands::PilotReport { human, models, out } => {
            unslop::pilot::report(&db, &human, &models, &out)
        }
        Commands::Ingest { jsonl, grammar } => {
            let docs = read_jsonl(&jsonl)?;
            let tx = db.transaction()?;
            let mut added = 0;
            for d in &docs {
                let e = features::extract(
                    d["text"].as_str().context("Document missing text")?,
                    grammar,
                    &c.python,
                )?;
                added += usize::from(store::add_document(&tx, d, &e)?.1);
            }
            tx.commit()?;
            Ok(json!({"records":docs.len(),"imported_extractions":added}))
        }
        Commands::ImportPangram {
            directory,
            corpus,
            group_id,
            grammar,
        } => {
            let mut paths = fs::read_dir(&directory)?
                .map(|r| r.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.retain(|p| p.extension().is_some_and(|e| e == "json"));
            paths.sort();
            ensure!(!paths.is_empty(), "No JSON records found");
            let tx = db.transaction()?;
            let mut added = 0;
            let mut skipped = 0;
            for path in paths {
                let r: Value = serde_json::from_str(&read(&path)?)?;
                if r.get("submitted_text").is_none() {
                    if ["manifest.json", "summary.json"]
                        .contains(&path.file_name().unwrap().to_str().unwrap_or(""))
                    {
                        skipped += 1;
                        continue;
                    }
                    bail!("Not a detector record: {}", path.display());
                }
                evaluation::validate_run(&r)?;
                let d = json!({"text":r["submitted_text"],"corpus":corpus,"source_kind":"experimental","domain":"philosophy","register":"book-preface","group_id":group_id,"split":"exploratory","metadata":{"first_import_path":path,"generator_provenance":"unverified"}});
                let e = features::extract(d["text"].as_str().unwrap(), grammar, &c.python)?;
                let (id, _) = store::add_document(&tx, &d, &e)?;
                added += usize::from(store::add_run(&tx, id, &r)?);
            }
            tx.commit()?;
            Ok(
                json!({"imported_runs":added,"non_run_files_skipped":skipped,"status":store::status(&db)?}),
            )
        }
        Commands::Profile { corpus, filter } => prof(&db, &corpus, &filter, &[]),
        Commands::Compare {
            left,
            right,
            filter,
            limit,
            min_count,
            min_documents,
        } => {
            let a = prof(&db, &left, &filter, &[])?;
            let b = prof(&db, &right, &filter, &[])?;
            let ranks = statistics::contrast(&a, &b, min_count, min_documents)?;
            Ok(
                json!({"left":a,"right":b,"same_stratum_names":a["strata"]==b["strata"],"features":ranks.as_array().context("Contrast must be an array")?.iter().take(limit).collect::<Vec<_>>(),"interpretation":"Exploratory feature ranks; match topic/register and validate on unseen source groups."}),
            )
        }
        Commands::Analyze {
            text,
            reference,
            group_id,
            grammar,
            filter,
            limit,
        } => {
            let t = read(&text)?;
            let e = features::extract(&t, grammar, &c.python)?;
            let mut q = db.prepare("SELECT DISTINCT group_id FROM documents WHERE sha256=?")?;
            let mut excluded = q
                .query_map([digest(&t)], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if let Some(g) = group_id {
                excluded.push(g);
            }
            let p = prof(&db, &reference, &filter, &excluded)?;
            let ranks =
                statistics::contrast(&statistics::document_profile(&e, &filter.family)?, &p, 5, 3)?;
            Ok(
                json!({"sha256":digest(&t),"reference_total":p["total"],"reference_groups":p["groups"],"excluded_groups":excluded,"metrics":e["metrics"],"vector":statistics::vector(&e,&filter.family)?,"anomalies":ranks.as_array().unwrap().iter().take(limit).collect::<Vec<_>>()}),
            )
        }
        Commands::Runs { corpus, threshold } => {
            let sql =
                "SELECT r.record_json FROM detector_runs r JOIN documents d ON d.id=r.document_id";
            let mut q = db.prepare(&format!(
                "{sql}{}",
                if corpus.is_some() {
                    " WHERE d.corpus=?"
                } else {
                    ""
                }
            ))?;
            let params: Vec<_> = corpus.iter().collect();
            let rows = q.query_map(rusqlite::params_from_iter(params), |r| {
                r.get::<_, String>(0)
            })?;
            let records = rows
                .map(|r| Ok(serde_json::from_str::<Value>(&r?)?))
                .collect::<Result<Vec<_>>>()?;
            let ids = ProviderIds::load()?;
            let summary = evaluation::summarize_runs(&records, threshold, 3, &ids)?;
            ids.record("unslop runs (standard output)")?;
            Ok(summary)
        }
        _ => unreachable!(),
    }
}
fn main() {
    match execute(Cli::parse()) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
        Err(error) => {
            eprintln!("unslop: {error:#}");
            std::process::exit(1);
        }
    }
}
