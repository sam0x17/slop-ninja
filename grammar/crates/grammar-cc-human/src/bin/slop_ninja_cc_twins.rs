//! Membership twins for the CC lookup canary. See
//! `grammar_cc_human::twins` for the steps and rules.
use anyhow::Result;
use clap::{Parser, Subcommand};
use grammar_a_pools::psl::Psl;
use grammar_cc_human::{fetch, store, twins, twins_bank, twins_report};
use std::{collections::HashSet, path::PathBuf};

#[derive(Parser)]
struct Cli {
    /// Output root, for example data/task-roots/cc-human/twins-v1.
    #[arg(long)]
    root: PathBuf,
    /// The frame crawl table (89 crawls, CC-MAIN-2013-20..2022-49).
    #[arg(long)]
    per_crawl: PathBuf,
    /// Total bytes this run may receive (whole files plus range reads).
    #[arg(long, default_value_t = 30_000_000_000)]
    budget_bytes: u64,
    #[arg(long, default_value_t = 400_000_000_000)]
    min_free_bytes: u64,
    #[arg(long, default_value_t = 2)]
    connections: usize,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download cluster.idx for every frame crawl and both post-cutoff crawls.
    Index,
    /// Choose host archive sub-ranges from the first post-cutoff crawl.
    Select {
        #[arg(long)]
        psl: PathBuf,
        #[arg(long)]
        fineweb_excluded: PathBuf,
        #[arg(long, default_value_t = 300)]
        hosts: usize,
        #[arg(long, default_value_t = 300)]
        sub_lines: usize,
    },
    /// Report the CDX bytes the chosen sub-ranges need, without reading them.
    Plan,
    /// Read the CDX blocks of every sub-range in every crawl.
    Cdx {
        /// Batch name: reads select/batches/<batch>.json, writes cdx/<batch>/.
        #[arg(long)]
        batch: String,
        #[arg(long, default_value_t = 2000)]
        max_blocks_per_crawl: usize,
        /// Read only these crawls (comma-separated), for a pre-screen batch.
        #[arg(long)]
        only_crawls: Option<String>,
    },
    /// Classify keys into twins and frame URLs and draw the matched picks.
    Build {
        #[arg(long, default_value_t = 16)]
        per_host: usize,
        /// Batches to build (comma-separated); default every non-prescreen batch.
        #[arg(long)]
        batches: Option<String>,
    },
    /// Read the picked WARC records.
    Records,
    /// Extract, screen and excerpt every record; write A request shards.
    Extract {
        #[arg(long)]
        psl: PathBuf,
        #[arg(long)]
        fineweb_excluded: PathBuf,
        #[arg(long, default_value_t = 8)]
        shards: usize,
    },
    /// Join A predictions; matched gaps, canary statistic and power.
    Report {
        #[arg(long)]
        a_pred: PathBuf,
        #[arg(long)]
        tierc_rows: PathBuf,
        #[arg(long)]
        tierc_pred: PathBuf,
        #[arg(long, default_value_t = 0.9090909090909091)]
        threshold: f64,
        #[arg(long, default_value_t = 200)]
        sims: usize,
        #[arg(long, default_value_t = 100)]
        boot: usize,
        #[arg(long)]
        site_roles: Option<PathBuf>,
        /// Keep mirror, syndication and archive hosts in the matched analysis.
        #[arg(long)]
        keep_copy_risk_hosts: bool,
    },
    /// Measure the honest-model offset tau: honest models scored on the twin set against R0,
    /// the canary statistic per model, the panel's tau and the power with it (tau/out/tau.json).
    Tau {
        /// R0 (released seed 17) predictions on the twin set.
        #[arg(long)]
        a_pred: PathBuf,
        /// NAME=PREDICTIONS.jsonl of an honest model scored on the same requests (repeatable).
        #[arg(long = "model", required = true)]
        models: Vec<String>,
        /// The report step's out/report.json, for each model's composition part.
        #[arg(long)]
        composition: Option<PathBuf>,
        #[arg(long, default_value_t = 2000)]
        boot: usize,
        #[arg(long, default_value_t = 200)]
        sims: usize,
        #[arg(long, default_value_t = 100)]
        sim_boot: usize,
        #[arg(long)]
        keep_copy_risk_hosts: bool,
    },
    /// Twin bank: plan the candidate host ranges and their CDX reads from cluster.idx
    /// (no network); writes plan/PLAN.json (every bank parameter) and plan/ranges.jsonl.zst.
    BankPlan {
        #[arg(long)]
        psl: PathBuf,
        #[arg(long)]
        fineweb_excluded: PathBuf,
        /// twins-v1 select/ranges.json, for the host-overlap count.
        #[arg(long)]
        v1_ranges: Option<PathBuf>,
        #[arg(long, default_value_t = 10_000)]
        target_twins: usize,
        #[arg(long, default_value_t = 10_000)]
        target_frame: usize,
        #[arg(long, default_value_t = 120)]
        per_host: usize,
        #[arg(long, default_value_t = 2.0)]
        frame_ratio: f64,
        #[arg(long, default_value_t = 40_000_000)]
        max_range_cdx_bytes: u64,
    },
    /// Twin bank: process the planned ranges in order (CDX, picks, records, extraction,
    /// one DONE.json per range) until the matched targets are reached, then assemble the
    /// bank. Resumable. Exit 0 complete or paused, 74 throttled (restartable), 75 rate block,
    /// 76 byte budget, 77 disk headroom, 78 plan exhausted, 1 other errors.
    BankRun {
        #[arg(long)]
        psl: PathBuf,
        #[arg(long)]
        fineweb_excluded: PathBuf,
        /// Stop after this many ranges (a bounded smoke run).
        #[arg(long)]
        max_ranges: Option<usize>,
    },
    /// Twin bank: assemble extract/ from the finished ranges (bank-wide twin text check).
    BankFinish {
        #[arg(long)]
        shards: Option<usize>,
    },
}

fn excluded(p: &PathBuf) -> Result<HashSet<String>> {
    Ok(std::fs::read_to_string(p)?.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let frame = twins::frame_crawls(&cli.per_crawl)?;
    let mut all: Vec<String> = frame.clone();
    all.extend(twins::POST_CRAWLS.iter().map(|s| s.to_string()));
    let v = match cli.cmd {
        Cmd::Index => twins::index(
            &cli.root,
            &all,
            fetch::Limits {
                budget_bytes: cli.budget_bytes,
                min_free_bytes: cli.min_free_bytes,
                connections: cli.connections,
            },
        )?,
        Cmd::Select { psl, fineweb_excluded, hosts, sub_lines } => {
            let psl = Psl::parse(&std::fs::read_to_string(&psl)?);
            let ex = excluded(&fineweb_excluded)?;
            twins::select(&twins::SelectArgs {
                root: &cli.root,
                psl: &psl,
                fineweb_excluded: &ex,
                hosts,
                sub_lines,
                threads: cli.connections,
                budget: cli.budget_bytes,
                min_free: cli.min_free_bytes,
            })?
        }
        Cmd::Plan => {
            let ranges: Vec<twins::Range> = store::read_json(&cli.root.join("select").join("ranges.json"))?;
            let (blocks, bytes, per) = twins::cdx_plan(&cli.root, &all, &ranges)?;
            let mut costs: Vec<u64> = per.iter().map(|x| x.1).collect();
            costs.sort();
            let q = |f: f64| costs.get(((costs.len() as f64 - 1.0) * f) as usize).copied().unwrap_or(0);
            serde_json::json!({"ranges": ranges.len(), "blocks": blocks, "bytes": bytes,
                "per_range_bytes_p10_p50_p90_max": [q(0.1), q(0.5), q(0.9), q(1.0)],
                "per_range": per.iter().map(|(r, b)| serde_json::json!([r, b])).collect::<Vec<_>>()})
        }
        Cmd::Cdx { batch, max_blocks_per_crawl, only_crawls } => {
            let sel: Vec<String> = match &only_crawls {
                Some(c) => c.split(',').map(str::to_owned).collect(),
                None => all.clone(),
            };
            anyhow::ensure!(sel.iter().all(|c| all.contains(c)), "unknown crawl in --only-crawls");
            twins::cdx(&cli.root, &batch, &sel, cli.connections, cli.budget_bytes, cli.min_free_bytes, max_blocks_per_crawl)?
        }
        Cmd::Build { per_host, batches } => {
            let names: Vec<String> = match batches {
                Some(b) => b.split(',').map(str::to_owned).collect(),
                None => twins::batches(&cli.root)?,
            };
            twins::build(&cli.root, &frame, per_host, &names)?
        }
        Cmd::Records => twins::records(&cli.root, cli.connections, cli.budget_bytes, cli.min_free_bytes)?,
        Cmd::Extract { psl, fineweb_excluded, shards } => {
            let psl = Psl::parse(&std::fs::read_to_string(&psl)?);
            let ex = excluded(&fineweb_excluded)?;
            twins::extract(&cli.root, &psl, &ex, shards)?
        }
        Cmd::Report { a_pred, tierc_rows, tierc_pred, threshold, sims, boot, site_roles, keep_copy_risk_hosts } => twins_report::report(&twins_report::Args {
            root: &cli.root,
            a_pred: &a_pred,
            tierc_rows: &tierc_rows,
            tierc_pred: &tierc_pred,
            threshold,
            sims,
            boot,
            site_roles: site_roles.as_deref(),
            copy_risk_rule: !keep_copy_risk_hosts,
        })?,
        Cmd::Tau { a_pred, models, composition, boot, sims, sim_boot, keep_copy_risk_hosts } => {
            let models = models
                .iter()
                .map(|m| {
                    let (n, p) = m.split_once('=').ok_or_else(|| anyhow::anyhow!("--model NAME=PATH"))?;
                    Ok((n.to_owned(), PathBuf::from(p)))
                })
                .collect::<Result<Vec<_>>>()?;
            twins_report::tau(&twins_report::TauArgs {
                root: &cli.root,
                a_pred: &a_pred,
                models,
                composition: composition.as_deref(),
                boot,
                sims,
                sim_boot,
                copy_risk_rule: !keep_copy_risk_hosts,
            })?
        }
        Cmd::BankPlan { psl, fineweb_excluded, v1_ranges, target_twins, target_frame, per_host, frame_ratio, max_range_cdx_bytes } => {
            let psl = Psl::parse(&std::fs::read_to_string(&psl)?);
            let ex = excluded(&fineweb_excluded)?;
            let mut params = twins_bank::BankParams::v2_default();
            params.target_matched_twins = target_twins;
            params.target_matched_frame = target_frame;
            params.per_host_twin_cap = per_host;
            params.frame_ratio = frame_ratio;
            params.max_range_cdx_bytes = max_range_cdx_bytes;
            twins_bank::plan(&twins_bank::PlanArgs {
                root: &cli.root,
                crawls: &all,
                psl: &psl,
                fineweb_excluded: &ex,
                params,
                v1_ranges: v1_ranges.as_deref(),
            })?
        }
        Cmd::BankRun { psl, fineweb_excluded, max_ranges } => {
            let psl = Psl::parse(&std::fs::read_to_string(&psl)?);
            let ex = excluded(&fineweb_excluded)?;
            let out = twins_bank::run(&twins_bank::RunArgs {
                root: &cli.root,
                frame: &frame,
                psl: &psl,
                fineweb_excluded: &ex,
                connections: cli.connections,
                budget: cli.budget_bytes,
                min_free: cli.min_free_bytes,
                max_ranges,
            })?;
            let code = out.exit_code();
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({"outcome": format!("{out:?}"), "exit_code": code}))?);
            std::process::exit(code);
        }
        Cmd::BankFinish { shards } => twins_bank::finish(&cli.root, shards)?,
    };
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}
