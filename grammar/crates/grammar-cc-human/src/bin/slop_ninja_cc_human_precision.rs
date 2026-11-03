//! `slop_ninja_cc_human_precision`: the domain precision review of the
//! Common Crawl pilot pool. Steps: `sample` (seeded
//! per-domain draw stratified by crawl year, joined to the pass-1 page
//! text; or, from a `reassign --per-doc` file, a draw on the re-assigned
//! domains that withholds documents already read), `score` (precision and
//! register tables from hand labels, and how a second rule set moves each
//! sampled document) and `reassign` (a rule set re-run over every admitted
//! page, with rule ids and the email channel split), `caps` (density-rule
//! caps of every domain and email channel under one assignment) and
//! `derive-shares` (a launch share table cut to those caps). No network,
//! model or detector calls.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use grammar_cc_human::{
    domain::Rules,
    precision,
    shares::{self, ShareTable},
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Parser)]
#[command(name = "slop_ninja_cc_human_precision")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Seeded per-domain draw, proportional to crawl-year counts.
    Sample {
        /// Pilot root (holds pool/ and pages/).
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "cc-human-precision-20261030")]
        seed: String,
        #[arg(long, default_value_t = 50)]
        per_domain: usize,
        /// Per-document assignment from `reassign --per-doc`: stratify on its
        /// domains instead of the pool's.
        #[arg(long)]
        assignments: Option<PathBuf>,
        /// The rule file that made the assignment (default: the crate's
        /// current rules); every pick must reproduce under it.
        #[arg(long, requires = "assignments")]
        rules: Option<PathBuf>,
        /// Files of ids never to draw (one per line, or JSONL with `id`).
        #[arg(long)]
        withhold: Vec<PathBuf>,
    },
    /// Precision tables from hand labels.
    Score {
        #[arg(long)]
        sample: PathBuf,
        #[arg(long)]
        labels: PathBuf,
        /// SAMPLE.json of the draw (admitted counts per domain).
        #[arg(long)]
        sample_summary: PathBuf,
        /// Launch shares, `domain=share` (any scale).
        #[arg(long, value_delimiter = ',', conflicts_with = "shares")]
        mix: Vec<String>,
        /// Weight domains by a launch share table instead of `--mix`.
        #[arg(long)]
        shares: Option<String>,
        /// A second rule file to re-run on the sample (default: none).
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Use the crate's current default rules as the second rule set.
        #[arg(long)]
        current_rules: bool,
        #[arg(long)]
        out: PathBuf,
    },
    /// Re-run the current (or given) rules over every admitted page.
    Reassign {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// The rule file the pool was built with (default: pilot rules v1),
        /// used to read the pool's rule ids.
        #[arg(long)]
        pool_rules: Option<PathBuf>,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        #[arg(long)]
        out: PathBuf,
        /// Also write every document's re-assignment (zstd JSONL, no text).
        #[arg(long)]
        per_doc: Option<PathBuf>,
        /// Domains whose rule-to-rule transitions are written in full.
        #[arg(long, value_delimiter = ',', default_value = "email")]
        detail: Vec<String>,
    },
    /// Density-rule caps of every domain and email channel under one assignment.
    Caps {
        /// lookup-v1 frame.json.
        #[arg(long)]
        frame: PathBuf,
        /// Pilot root (holds pool/).
        #[arg(long)]
        root: PathBuf,
        /// Per-document assignment from `reassign --per-doc`.
        #[arg(long)]
        assignments: PathBuf,
        /// The rule file that made the assignment (default: the crate's current rules).
        #[arg(long)]
        rules: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// A launch share table cut to the caps of a `caps` record.
    DeriveShares {
        /// Base table id.
        #[arg(long)]
        base: String,
        #[arg(long)]
        caps: PathBuf,
        /// Strata to cut: domains, press_release or list_mail.
        #[arg(long, value_delimiter = ',')]
        cut: Vec<String>,
        /// The domain that receives the freed points.
        #[arg(long, default_value = "essays")]
        to: String,
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Sample {
            root,
            out,
            seed,
            per_domain,
            assignments,
            rules,
            withhold,
        } => {
            let r = match (&assignments, &rules) {
                (Some(_), Some(p)) => Some(Rules::parse(&std::fs::read_to_string(p)?)?),
                (Some(_), None) => Some(Rules::pilot()),
                (None, _) => None,
            };
            let mut ids = std::collections::HashSet::new();
            for p in &withhold {
                ids.extend(precision::read_ids(p)?);
            }
            let v = precision::sample(&precision::SampleArgs {
                root: &root,
                out: &out,
                seed: &seed,
                per_domain,
                assignments: assignments.as_deref(),
                rules: r.as_ref(),
                withhold: &ids,
            })?;
            println!("{v}");
        }
        Cmd::Score {
            sample,
            labels,
            sample_summary,
            mix,
            shares,
            rules,
            current_rules,
            out,
        } => {
            let s: serde_json::Value = grammar_cc_human::store::read_json(&sample_summary)?;
            let pool_counts: BTreeMap<String, usize> = s["allocation"]
                .as_object()
                .context("allocation")?
                .iter()
                .map(|(d, v)| (d.clone(), v["admitted"].as_u64().unwrap_or(0) as usize))
                .collect();
            let mut m = BTreeMap::new();
            for kv in &mix {
                let (k, v) = kv.split_once('=').with_context(|| format!("--mix {kv}"))?;
                m.insert(k.to_owned(), v.parse::<f64>()?);
            }
            if let Some(id) = &shares {
                for (d, f) in ShareTable::by_id(id)?.fractions() {
                    m.insert(d.to_owned(), f);
                }
            }
            let r = match (&rules, current_rules) {
                (Some(p), _) => Some(Rules::parse(&std::fs::read_to_string(p)?)?),
                (None, true) => Some(Rules::pilot()),
                (None, false) => None,
            };
            let v = precision::score(&precision::ScoreArgs {
                sample: &sample,
                labels: &labels,
                pool_counts: &pool_counts,
                mix: &m,
                rules_new: r.as_ref(),
                out: &out,
            })?;
            println!("{}", serde_json::to_string_pretty(&v["pool_weighted"])?);
        }
        Cmd::Reassign {
            root,
            rules,
            pool_rules,
            threads,
            out,
            per_doc,
            detail,
        } => {
            let r = match &rules {
                Some(p) => Rules::parse(&std::fs::read_to_string(p)?)?,
                None => Rules::pilot(),
            };
            let pr = match &pool_rules {
                Some(p) => Rules::parse(&std::fs::read_to_string(p)?)?,
                None => Rules::pilot_v1(),
            };
            let v = precision::reassign(&precision::ReassignArgs {
                root: &root,
                rules: &r,
                pool_rules: &pr,
                threads,
                out: &out,
                per_doc: per_doc.as_deref(),
                detail: &detail,
            })?;
            println!(
                "{}",
                serde_json::to_string(&json!({"after": v["after"], "email_channels": v["email_channels"]["after"]}))?
            );
        }
        Cmd::Caps {
            frame,
            root,
            assignments,
            rules,
            out,
        } => {
            let r = match &rules {
                Some(p) => Rules::parse(&std::fs::read_to_string(p)?)?,
                None => Rules::pilot(),
            };
            let v = shares::caps(&shares::CapsArgs {
                frame: &frame,
                root: &root,
                assignments: &assignments,
                rules: &r,
                out: &out,
            })?;
            println!("{}", serde_json::to_string(&v["strata"])?);
        }
        Cmd::DeriveShares {
            base,
            caps,
            cut,
            to,
            id,
            note,
            out,
        } => {
            let c: serde_json::Value = grammar_cc_human::store::read_json(&caps)?;
            let t = shares::derive(&ShareTable::by_id(&base)?, &c, &cut, &to, &id, &note)?;
            let over = shares::over_caps(&t, &c);
            std::fs::write(&out, serde_json::to_string_pretty(&t)? + "\n")?;
            println!("{}", json!({"table": t, "over_caps": over}));
        }
    }
    Ok(())
}
