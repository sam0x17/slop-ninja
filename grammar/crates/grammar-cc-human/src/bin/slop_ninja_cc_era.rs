//! `slop_ninja_cc_era`: the pre-mainnet era gates on a Common Crawl-parented
//! mock batch. Steps: `batch` (parents,
//! roles and local generation requests at a versioned launch share table),
//! `generate` (one generator against a local llama-server under a cohort
//! generation profile; the default `cc_cohort_profile_v1` runs the
//! parent-conditioned normaliser, form screen and final-document length
//! window in the acceptance path; `cc_cohort_profile_v2` replaces v1's layout
//! step with the parent-conditioned layout transfer, screens a child's form
//! exactly when its source passes the screen, echo-screens every child and
//! draws conventions from a population x domain prior), `norm-prior` and
//! `renorm` (the normaliser's prior, v1 or per population for v2, and the
//! re-verdict of existing journals under a profile), `assemble`
//! (mock cohort and A request shards, with the profile's every-class step)
//! and `gates` (versioned gate definitions: era-only and layout/typography
//! adversaries, with the probe AUC, character-class and length diagnostics;
//! B0, sigma, delta and w). `normalise` (layout normaliser v0) is superseded
//! by the profile and kept to reproduce era-v1. No hosted model, detector or
//! network call outside the local llama-server.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use grammar_cc_human::{era_batch, era_gates, era_norm, shares::{self, ShareTable}};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "slop_ninja_cc_era")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Draw parents, assign roles and write generation requests.
    Batch {
        #[arg(long)]
        pool: PathBuf,
        /// lookup-v1 frame.json (per-year first-seen unique documents).
        #[arg(long)]
        frame: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "cc-era-gates-20261031")]
        seed: String,
        #[arg(long, default_value_t = 400)]
        human: usize,
        #[arg(long, default_value_t = 1100)]
        mixed: usize,
        #[arg(long, default_value_t = 1000)]
        mirror_dated: usize,
        #[arg(long, default_value_t = 200)]
        mirror_topic: usize,
        #[arg(long, default_value_t = 200)]
        mirror_plain: usize,
        #[arg(long, default_value_t = 500)]
        cohort_model_only: usize,
        #[arg(long, value_delimiter = ',', default_value = "glm4,mistral7b,olmo2_instruct")]
        generators: Vec<String>,
        /// Launch share table id (era-v1: cc_launch_shares_v1).
        #[arg(long, default_value = shares::DEFAULT_ID)]
        shares: String,
    },
    /// Run one generator's requests against a local llama-server (resumable).
    Generate {
        #[arg(long)]
        batch: PathBuf,
        /// Request set directory (a top-up); default: the batch.
        #[arg(long)]
        requests: Option<PathBuf>,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        generator: String,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        tokenizer: PathBuf,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        #[arg(long)]
        limit: Option<usize>,
        /// Normaliser prior (`norm-prior`), required by every normalising profile (a v2 prior for cc_cohort_profile_v2).
        #[arg(long)]
        norm: Option<PathBuf>,
        /// Cohort generation profile: cc_cohort_profile_v1 (default), cc_cohort_profile_v2, cc_era_parent_norm_v1 (format-v1) or raw (era-v1).
        #[arg(long, default_value = era_norm::PROFILE_V1)]
        profile: String,
    },
    /// Write the parent-conditioned normaliser's prior and key seed for a batch.
    /// Under cc_cohort_profile_v2 the batch is one population, named by its
    /// BATCH.json: its prior is tallied population-wide and per parent
    /// domain, with the population's own seed (`norm_prior_seed`, derived
    /// from the population id; no --seed).
    NormPrior {
        #[arg(long)]
        batch: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Key seed (v1 only; default: the format-v1 seed). Profile v2 derives its seed from the population.
        #[arg(long)]
        seed: Option<String>,
        /// Profile the prior serves: cc_cohort_profile_v1 (default; also cc_era_parent_norm_v1) or cc_cohort_profile_v2.
        #[arg(long, default_value = era_norm::PROFILE_V1)]
        profile: String,
        /// The batch's population (profile v2 only; must equal BATCH.json's), e.g. train2.
        #[arg(long)]
        population: Option<String>,
    },
    /// Re-verdict journaled responses with the normaliser in the acceptance path (no model call).
    Renorm {
        #[arg(long)]
        batch: PathBuf,
        #[arg(long)]
        norm: PathBuf,
        /// Journals to re-verdict (`<runs>/<generator>/journal-*.jsonl`).
        #[arg(long)]
        runs: PathBuf,
        /// Request set directory (a top-up); default: the batch.
        #[arg(long)]
        requests: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        tokenizer: PathBuf,
        /// Cohort generation profile (a normalising one).
        #[arg(long, default_value = era_norm::PROFILE_V1)]
        profile: String,
    },
    /// Re-issue exhausted unaccepted requests once to the next generator.
    Topup {
        #[arg(long)]
        batch: PathBuf,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_delimiter = ',', default_value = "glm4,mistral7b,olmo2_instruct")]
        generators: Vec<String>,
    },
    /// Assemble the mock cohort and the A request shards.
    Assemble {
        #[arg(long)]
        batch: PathBuf,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        topup: Option<PathBuf>,
        #[arg(long)]
        topup_runs: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 8)]
        shards: usize,
        /// Normaliser prior: require post-processed children and apply the every-class step.
        #[arg(long)]
        norm: Option<PathBuf>,
        /// Cohort generation profile (see `generate`).
        #[arg(long, default_value = era_norm::PROFILE_V1)]
        profile: String,
    },
    /// Superseded: normalised copy of an assembled cohort (layout normaliser v0, era-v1 only) and its A request shards.
    Normalise {
        #[arg(long)]
        cohort: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 8)]
        shards: usize,
        /// Flat normaliser v0 (all whitespace runs to one space) instead of layout v0.
        #[arg(long)]
        flat: bool,
    },
    /// Era probe AUC, era-only and format-only adversaries, B0, sigma, delta and w.
    Gates {
        #[arg(long)]
        cohort: PathBuf,
        #[arg(long)]
        batch: PathBuf,
        #[arg(long)]
        pool: PathBuf,
        /// NAME=predictions.jsonl, repeated; the first is the reference R0.
        #[arg(long = "pred", required = true)]
        preds: Vec<String>,
        #[arg(long, default_value = "0x3fed1745d1745d17")]
        threshold_bits: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "cc-human-era-probe-20261030")]
        probe_seed: String,
        #[arg(long, default_value = "cc-era-gates-20261031")]
        seed: String,
        #[arg(long, default_value_t = 1000)]
        draws: usize,
        #[arg(long, default_value_t = 5)]
        folds: usize,
        #[arg(long, default_value_t = 5)]
        cv_repeats: usize,
        #[arg(long, default_value_t = 3000)]
        per_domain_class: usize,
        /// Gate definitions: cc_era_gate_defs_v2 (default) or cc_era_gate_defs_v1 (era-v1, pre-registered).
        #[arg(long, default_value = era_gates::DEFAULT_GATE_DEFS)]
        gate_defs: String,
    },
}

fn main() -> Result<()> {
    let out = match Cli::parse().cmd {
        Cmd::Batch { pool, frame, out, seed, human, mixed, mirror_dated, mirror_topic, mirror_plain, cohort_model_only, generators, shares } => {
            era_batch::batch(&era_batch::BatchArgs {
                pool,
                frame,
                out,
                seed,
                counts: [human, mixed, mirror_dated, mirror_topic, mirror_plain],
                cohort_model_only,
                generators,
                shares: ShareTable::by_id(&shares)?,
            })?
        }
        Cmd::Generate { batch, requests, runs, generator, config, endpoint, tokenizer, workers, limit, norm, profile } => {
            let profile = era_norm::Profile::parse(&profile)?;
            era_norm::check_profile_prior(profile, norm.as_deref())?;
            era_batch::generate(&era_batch::GenArgs { batch, requests, runs, generator, config, endpoint, tokenizer, workers, limit, norm, profile })?
        }
        Cmd::NormPrior { batch, out, seed, profile, population } => {
            let profile = era_norm::Profile::parse(&profile)?;
            anyhow::ensure!(profile.normalises(), "profile {} takes no prior", profile.id());
            if profile.transfers() {
                let population = population.context("--population is required for cc_cohort_profile_v2")?;
                anyhow::ensure!(seed.is_none(), "cc_cohort_profile_v2 takes no --seed: its seed is norm_prior_seed(population)");
                era_norm::write_prior_v2(&batch, &out, &population)?
            } else {
                anyhow::ensure!(population.is_none(), "--population is for cc_cohort_profile_v2 only");
                era_norm::write_prior(&batch, &out, seed.as_deref().unwrap_or(era_norm::DEFAULT_SEED))?
            }
        }
        Cmd::Renorm { batch, norm, runs, requests, out, tokenizer, profile } => {
            let profile = era_norm::Profile::parse(&profile)?;
            era_norm::renorm(&era_norm::RenormArgs { batch, norm, profile, runs, requests, out, tokenizer })?
        }
        Cmd::Topup { batch, runs, out, generators } => era_batch::topup(&batch, &runs, &out, &generators)?,
        Cmd::Assemble { batch, runs, topup, topup_runs, out, shards, norm, profile } => {
            let profile = era_norm::Profile::parse(&profile)?;
            era_norm::check_profile_prior(profile, norm.as_deref())?;
            let t = match (&topup, &topup_runs) {
                (Some(t), Some(r)) => Some((t.as_path(), r.as_path())),
                (None, None) => None,
                _ => anyhow::bail!("--topup and --topup-runs go together"),
            };
            era_batch::assemble(&batch, &runs, t, &out, shards, norm.as_deref(), profile)?
        }
        Cmd::Normalise { cohort, out, shards, flat } => era_gates::normalise_cohort(&cohort, &out, shards, flat)?,
        Cmd::Gates { cohort, batch, pool, preds, threshold_bits, out, probe_seed, seed, draws, folds, cv_repeats, per_domain_class, gate_defs } => {
            let bits = u64::from_str_radix(threshold_bits.trim_start_matches("0x"), 16)?;
            let preds = preds
                .iter()
                .map(|s| {
                    let (k, v) = s.split_once('=').context("--pred NAME=PATH")?;
                    Ok((k.to_string(), PathBuf::from(v)))
                })
                .collect::<Result<Vec<_>>>()?;
            era_gates::gates(&era_gates::GatesArgs {
                cohort,
                batch,
                pool,
                preds,
                threshold: f64::from_bits(bits),
                out,
                probe_seed,
                seed,
                draws,
                folds,
                cv_repeats,
                per_domain_class,
                gate_defs,
            })?
        }
    };
    println!("{out}");
    Ok(())
}
