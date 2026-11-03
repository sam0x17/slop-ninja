use anyhow::Result;
use clap::{Parser, Subcommand};
use grammar_a_assemble::{bundle, consolidate, styles, successor, supervision, test_split, trainer};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "slop_ninja_a_assemble")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the configured split bundles under ROOT/build-<id> and point ROOT/CURRENT.json at it.
    Build {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Source commit recorded in every manifest.
        #[arg(long)]
        commit: String,
        /// The written seal (required to build the Test split).
        #[arg(long, requires = "seal_sha256")]
        seal: Option<PathBuf>,
        /// The seal's sha256, stated by the caller.
        #[arg(long, requires = "seal")]
        seal_sha256: Option<String>,
    },
    /// Project one evaluation split of a build into the A scorers' row fields
    /// (test after the seal; cal only as the mechanical scoring-path check).
    ScoringRows {
        #[arg(long)]
        build: PathBuf,
        #[arg(long)]
        split: String,
        #[arg(long, requires = "seal_sha256")]
        seal: Option<PathBuf>,
        #[arg(long, requires = "seal")]
        seal_sha256: Option<String>,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write the Pangram collection allow-list of a Test bundle.
    PublicCollections {
        #[arg(long)]
        rows: PathBuf,
        /// Test human documents (repeatable).
        #[arg(long = "humans", required = true)]
        humans: Vec<PathBuf>,
        #[arg(long)]
        seal: PathBuf,
        #[arg(long)]
        seal_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write the candidate files of the overlap audit after Test acquisition.
    AuditCandidates {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long = "humans", required = true)]
        humans: Vec<PathBuf>,
        /// Positive record files whose request style texts are checked (repeatable).
        #[arg(long = "records")]
        records: Vec<PathBuf>,
        #[arg(long)]
        seal: PathBuf,
        #[arg(long)]
        seal_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write the overlap receipt and the source-group exclusions of a Test audit scan.
    AuditReceipt {
        #[arg(long)]
        candidates: PathBuf,
        #[arg(long)]
        scan: PathBuf,
        #[arg(long)]
        seal: PathBuf,
        #[arg(long)]
        seal_sha256: String,
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        exclusions: PathBuf,
    },
    /// Merge evaluation-human directories (parts in order) into a new directory.
    MergeEvalHumans {
        /// A part directory; repeat the option, at least twice.
        #[arg(long, required = true)]
        part: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Check that every line of BASE occurs byte for byte in SUCCESSOR.
    VerifyLines {
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        successor: PathBuf,
    },
    /// Write the pools index from a layer specification.
    PoolsIndex {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Project split directories of one build into a new trainer bundle directory.
    TrainerBundle {
        /// Build directory (ROOT/build-<id>).
        #[arg(long)]
        build: PathBuf,
        /// A split to include (train is required); repeat the option.
        #[arg(long = "split", required = true)]
        splits: Vec<String>,
        /// Cap rows of a split and source kind: SPLIT:KIND=N (repeatable).
        #[arg(long = "take")]
        take: Vec<String>,
        /// Seed of the take and carve hashes.
        #[arg(long, default_value = "0")]
        seed: String,
        /// Smoke only: move Train source groups with hash mod M < K into Dev-iter (K/M).
        #[arg(long)]
        smoke_carve_dev_iter: Option<String>,
        /// Open a sealed split for its protocol use, once: final_selection (dev_select) or
        /// calibration_fit (cal). Recorded in the build as UNSEALED-<purpose>.json.
        #[arg(long = "unseal-for")]
        unseal_for: Vec<String>,
        /// Train weights in the bundle: final (the build's row weights, default) or
        /// cohort-factor (declared factors; the trainer recomputes family/origin balance).
        #[arg(long, default_value = "final")]
        weighting: String,
        /// Source commit recorded in the manifest.
        #[arg(long)]
        commit: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write a splits successor (Train-side OCR screen and cap, private-use screen, ELLIPSE half split).
    SplitsSuccessor {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        commit: String,
    },
    /// Text-free quantiles of the OCR measures per domain (Train and Train-mining).
    OcrProfile {
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        lexicon: PathBuf,
        #[arg(long = "domain", required = true)]
        domains: Vec<String>,
    },
    /// Write the frozen style partition file at a fixed library snapshot.
    FreezeStyles {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        commit: String,
    },
    /// Check a v2 frontier export against the v1 export and sidecar it replaces.
    VerifyFrontier {
        #[arg(long)]
        old: PathBuf,
        #[arg(long)]
        new: PathBuf,
    },
    /// Write the r2_domain_supervision_v1 targets (S0-8) of a build's Train split, once.
    Supervision {
        /// The Train split directory of a build (BUILD_DIR/train).
        #[arg(long)]
        train: PathBuf,
        /// The covered domains, evaluation masses, generators and plans (schema slop_ninja_a_r2_supervision_config_v1).
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Report the Kish ESS of a build's Train split under the trainer's weighting (V24; a report, no refusal).
    Ess {
        /// The Train split directory of a build (BUILD_DIR/train).
        #[arg(long)]
        train: PathBuf,
        #[arg(long)]
        commit: String,
    },
}

fn code(commit: &str) -> Result<Value> {
    let exe = std::env::current_exe()?;
    Ok(json!({"crate": "grammar-a-assemble", "version": env!("CARGO_PKG_VERSION"),
        "commit": commit, "assembler_sha256": bundle::file_sha(&exe)?}))
}

fn self_sha() -> Result<String> {
    bundle::file_sha(&std::env::current_exe()?)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let out: Value = match cli.cmd {
        Cmd::Build { config, out, commit, seal, seal_sha256 } => {
            let exe = std::env::current_exe()?;
            let code = json!({"crate": "grammar-a-assemble", "version": env!("CARGO_PKG_VERSION"),
                "commit": commit, "assembler_sha256": bundle::file_sha(&exe)?});
            let seal = seal.zip(seal_sha256).map(|(path, sha256)| bundle::SealCheck { path, sha256 });
            bundle::build_sealed(&config, &out, &code, seal.as_ref())?
        }
        Cmd::ScoringRows { build, split, seal, seal_sha256, commit, out } => {
            let seal = seal.zip(seal_sha256).map(|(path, sha256)| bundle::SealCheck { path, sha256 });
            test_split::scoring_rows(&build, &split, seal.as_ref(), &code(&commit)?, &out)?
        }
        Cmd::PublicCollections { rows, humans, seal, seal_sha256, out } => {
            let s = bundle::SealCheck { path: seal, sha256: seal_sha256 };
            test_split::public_collections(&rows, &humans, &s, &self_sha()?, &out)?
        }
        Cmd::AuditCandidates { rows, humans, records, seal, seal_sha256, out } => {
            let s = bundle::SealCheck { path: seal, sha256: seal_sha256 };
            test_split::audit_candidates(&rows, &humans, &records, &s, &self_sha()?, &out)?
        }
        Cmd::AuditReceipt { candidates, scan, seal, seal_sha256, receipt, exclusions } => {
            let s = bundle::SealCheck { path: seal, sha256: seal_sha256 };
            test_split::audit_receipt(&candidates, &scan, &s, &self_sha()?, &receipt, &exclusions)?
        }
        Cmd::MergeEvalHumans { part, out } => {
            let parts: Vec<&std::path::Path> = part.iter().map(|p| p.as_path()).collect();
            consolidate::merge_eval_humans(&parts, &out)?
        }
        Cmd::VerifyLines { base, successor } => consolidate::verify_lines(&base, &successor)?,
        Cmd::PoolsIndex { spec, out } => {
            let s: Value = serde_json::from_slice(&std::fs::read(&spec)?)?;
            let idx = consolidate::pools_index(&s)?;
            let mut b = serde_json::to_vec_pretty(&idx)?;
            b.push(b'\n');
            std::fs::write(&out, &b)?;
            json!({"out": out, "layers": idx["layers"].as_array().map(|a| a.len())})
        }
        Cmd::TrainerBundle { build, splits, take, seed, smoke_carve_dev_iter, unseal_for, weighting, commit, out } => {
            let code = code(&commit)?;
            let weighting = match weighting.as_str() {
                "final" => trainer::TrainerWeighting::Final,
                "cohort-factor" => trainer::TrainerWeighting::CohortFactor,
                w => anyhow::bail!("unknown --weighting {w}"),
            };
            let mut sel = trainer::Selection { seed, unseal_for, weighting, ..Default::default() };
            for t in &take {
                sel.parse_take(t)?;
            }
            if let Some(c) = &smoke_carve_dev_iter {
                sel.parse_carve(c)?;
            }
            trainer::trainer_bundle(&build, &splits, &sel, &out, &code)?
        }
        Cmd::VerifyFrontier { old, new } => consolidate::verify_frontier(&old, &new)?,
        Cmd::SplitsSuccessor { config, out, commit } => successor::successor(&config, &out, &code(&commit)?)?,
        Cmd::OcrProfile { base, lexicon, domains } => successor::ocr_profile(&base, &lexicon, &domains)?,
        Cmd::FreezeStyles { config, out, commit } => styles::freeze_styles(&config, &out, &code(&commit)?)?,
        Cmd::Supervision { train, config, commit, out } => supervision::run(&train, &config, &out, &code(&commit)?)?,
        Cmd::Ess { train, commit } => supervision::train_ess(&train, &code(&commit)?)?,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
