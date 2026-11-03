//! Round-2 rights snapshot, `jitem_filter_v1`, the P0 row allow-list and the
//! `DATA_RIGHTS.json` generator (`grammar_a_assemble::rights`; protocol A2:
//! A7, S0-4, D2 and item 1.7).
//!
//! ```text
//! a_r2_rights snapshot --version 1 --rules RULES.json \
//!   --population NAME:ROLE=ROWS.jsonl... [--pool NAME[:ITEM]=POOL_METADATA.jsonl...] --stage0 STAGE0.json \
//!   [--allowlist NAME] [--trainer-bundle TRAINER_BUNDLE_DIR] [--require-final] --commit SHA --out NEW_DIR
//! a_r2_rights data-rights --snapshot DIR/rights-jitems-v2.json --out DATA_RIGHTS.json
//! a_r2_rights tag-pool --rules RULES.json --rows POOL_METADATA.jsonl [--expect-item J8] --out SUMMARY.json
//! a_r2_rights record-digest --stage0 STAGE0.json [--item A3]
//! ```
//!
//! `snapshot` writes `rights-jitems-v<N>.json`, `row-tags.jsonl` and one
//! allow-list per `--allowlist` population into a new directory (mode 700,
//! files 600) that appears only once complete. The style libraries are the
//! ones the rules bind. A version-1 run that does not reproduce every count
//! the rules record writes nothing. A snapshot is a draft until A3 is done
//! and the rules bind its record (`record-digest` prints that digest);
//! `--require-final` refuses a draft. `--trainer-bundle` checks the allow-list
//! against the bundle's MANIFEST and Train block only. `data-rights` accepts
//! only a final version-2 snapshot. Every command reads metadata only and
//! prints counts and digests, never text.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_assemble::rights::{self, PoolSpec, PopulationSpec, SnapshotInput, Version};
use grammar_a_assemble::sha_hex;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "a_r2_rights", about = "Rights snapshot, jitem_filter_v1, P0 allow-list and DATA_RIGHTS (no network)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Tag every row of the populations and write the snapshot, row tags and allow-lists.
    Snapshot {
        /// 1 (Train populations; reproduction required) or 2 (the rules' populations, every role).
        #[arg(long)]
        version: String,
        #[arg(long)]
        rules: PathBuf,
        /// NAME:ROLE=ROWS (bundle rows); repeatable.
        #[arg(long = "population", required = true)]
        populations: Vec<String>,
        /// NAME=ROWS or NAME:ITEM=ROWS (pool metadata, tagged by collection; every row must carry ITEM); repeatable.
        #[arg(long = "pool")]
        pools: Vec<String>,
        #[arg(long)]
        stage0: PathBuf,
        /// Train population whose kept rows are written as an allow-list; repeatable.
        #[arg(long = "allowlist")]
        allowlist: Vec<String>,
        /// Trainer-bundle directory whose Train block must hold every allow-listed id.
        #[arg(long)]
        trainer_bundle: Option<PathBuf>,
        #[arg(long)]
        require_final: bool,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Generate DATA_RIGHTS.json from a final version-2 snapshot.
    DataRights {
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Tag pool rows by collection only (rows that are not bundle rows yet).
    TagPool {
        #[arg(long)]
        rules: PathBuf,
        #[arg(long)]
        rows: PathBuf,
        /// The file must hold rows, and every row must carry this item.
        #[arg(long)]
        expect_item: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the digest of a STAGE0.json record that the rules' findings_checked binds.
    RecordDigest {
        #[arg(long)]
        stage0: PathBuf,
        #[arg(long, default_value = "A3")]
        item: String,
    },
}

fn executable_sha256() -> Result<String> {
    let p = std::env::current_exe().context("current executable")?;
    Ok(sha_hex(&fs::read(&p).with_context(|| format!("read {}", p.display()))?))
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Snapshot { version, rules, populations, pools, stage0, allowlist, trainer_bundle, require_final, commit, out } => {
            ensure!(!out.exists(), "{} exists: a snapshot is written once", out.display());
            let inp = SnapshotInput {
                version: Version::parse(&version)?,
                rules,
                populations: populations.iter().map(|s| PopulationSpec::parse(s)).collect::<Result<_>>()?,
                pools: pools.iter().map(|s| PoolSpec::parse(s)).collect::<Result<_>>()?,
                stage0,
                allowlist,
                trainer_bundle,
                require_final,
                commit,
                executable_sha256: Some(executable_sha256()?),
            };
            let o = rights::build_snapshot(&inp)?;
            let sha = rights::write_snapshot(&out, &o)?;
            let s = &o.snapshot;
            let summary = json!({
                "snapshot": out.join(inp.version.file_name()), "sha256": sha, "status": s["status"], "status_reason": s["status_reason"],
                "rules_sha256": s["rules"]["sha256"], "stage0_sha256": s["stage0"]["sha256"],
                "a3_record_sha256": s["findings"]["A3"]["record_sha256"],
                "row_tags": s["row_tags"], "allowlists": s["allowlists"], "reproduce": s["reproduce"]["observed"],
                "filter": s["filters"][rights::JITEM_FILTER_V1]["populations"], "trainer_check": s["trainer_check"],
                "pools": s["pools"].as_array().into_iter().flatten().map(|p| json!({"name": p["name"], "sha256": p["rows"]["sha256"],
                    "rows": p["rows"]["rows"], "expect_item": p["expect_item"], "rows_with_expected_item": p["rows_with_expected_item"]})).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Cmd::DataRights { snapshot, out } => {
            ensure!(!out.exists(), "{} exists: DATA_RIGHTS is written once", out.display());
            let raw = fs::read(&snapshot).with_context(|| format!("read {}", snapshot.display()))?;
            let v: Value = serde_json::from_slice(&raw)?;
            let d = rights::data_rights(&v, &sha_hex(&raw))?;
            let sha = rights::write_record(&out, &d)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"out": out, "sha256": sha, "content_sha256": d["content_sha256"],
                    "open_items": d["content"]["open_items"], "open_markers": d["content"]["open_markers"]}))?
            );
        }
        Cmd::TagPool { rules, rows, expect_item, out } => {
            ensure!(!out.exists(), "{} exists: the summary is written once", out.display());
            let s = rights::tag_pool(&rules, &rows, expect_item.as_deref())?;
            let sha = rights::write_record(&out, &s)?;
            println!("{}", serde_json::to_string_pretty(&json!({"out": out, "sha256": sha, "summary": s}))?);
        }
        Cmd::RecordDigest { stage0, item } => {
            let (file_sha, rec, d) = rights::stage0_record(&stage0, &item)?;
            println!("{}", serde_json::to_string_pretty(&json!({"stage0_sha256": file_sha, "item": item, "status": rec["status"], "record_sha256": d}))?);
        }
    }
    Ok(())
}
