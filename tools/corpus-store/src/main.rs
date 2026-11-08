//! `slop_ninja_corpus`: the consolidated, content-addressed SlopNinja corpus.

mod adapters;
mod archive;
mod db;
mod export;
mod link;
mod packs;
mod plan;
mod public_candidate;
mod runner;
mod sink;
mod skeleton;
mod textstats;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_public_candidate;
mod tokens;
mod verify;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "slop_ninja_corpus", about = "Consolidated SlopNinja corpus store")]
struct Cli {
    /// Store directory (holds corpus.sqlite and packs/).
    #[arg(long)]
    store: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create or open the store and print its schema identity.
    Init,
    /// Ingest the plan's sources in priority order (all, or the listed ids).
    Ingest {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long = "source")]
        sources: Vec<String>,
        /// Re-run sources whose inputs are unchanged.
        #[arg(long)]
        force: bool,
        #[arg(long, default_value_t = 4)]
        threads: usize,
    },
    /// Resolve and hash a plan's inputs without ingesting (inventory).
    Manifest {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long = "source")]
        sources: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        /// Sizes only (no hashing); `.zst` sizes are compressed sizes.
        #[arg(long)]
        stat_only: bool,
    },
    /// Derived joins: event outputs, record ancestry, view expectations.
    Link,
    /// Token counts with a tokenizer.json for texts that lack them.
    Tokens {
        #[arg(long)]
        tokenizer: PathBuf,
        #[arg(long, default_value_t = 4)]
        threads: usize,
    },
    /// Rebuild one stored file byte for byte.
    Rebuild {
        #[arg(long)]
        file_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Integrity checks; writes a JSON report.
    Verify {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        expectations: Option<PathBuf>,
        /// Rebuild every view file and compare digests (slow).
        #[arg(long)]
        rebuild_views: bool,
    },
    /// Counts per source and table.
    Status,
    /// Export checksummed zstd JSONL tables: `all` (private), `redistributable`, or
    /// `public-candidate-v1` (needs `--rules`; opens the store read-only).
    Export {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "all")]
        subset: String,
        /// Selection, licence and allowlist rules for `public-candidate-v1`.
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Repository root (default: the nearest ancestor of the store holding `.git`).
        #[arg(long)]
        repo_root: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Export { out, subset, rules, repo_root } = &cli.cmd {
        if subset == "public-candidate-v1" {
            let rules = rules.as_ref().context("--rules is required for public-candidate-v1")?;
            let db = db::open_ro(&cli.store.join("corpus.sqlite"))?;
            let r = public_candidate::run(&db, &cli.store, out, rules, repo_root.as_deref())?;
            println!("{}", serde_json::to_string(&r["files"])?);
            return Ok(());
        }
    }
    std::fs::create_dir_all(&cli.store)?;
    let db = db::open(&cli.store.join("corpus.sqlite"))?;
    match cli.cmd {
        Cmd::Init => {
            packs::Packs::open(&db, &cli.store.join("packs"))?;
            println!("{}", serde_json::json!({"store": cli.store, "schema": db::STORE_SCHEMA}));
        }
        Cmd::Ingest { plan, sources, force, threads } => {
            rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().ok();
            let p = plan::load(&plan)?;
            let out = runner::run(&db, &cli.store, &p, &sources, force)?;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Manifest { plan, sources, out, stat_only } => {
            let p = plan::load(&plan)?;
            let mut rows = Vec::new();
            for s in p.sources.iter().filter(|s| sources.is_empty() || sources.contains(&s.id)) {
                let (inputs, msha) = runner::manifest_opt(&p, s, !stat_only)?;
                let bytes: u64 = inputs.iter().map(|i| i.n_bytes).sum();
                rows.push(serde_json::json!({
                    "source": s.id, "adapter": s.adapter, "priority": s.priority, "root": s.root,
                    "files": inputs.len(), "bytes": bytes, "input_manifest_sha256": msha,
                    "inputs": inputs.iter().map(|i| serde_json::json!({"path": i.rel, "role": i.role, "sha256": i.sha, "bytes": i.n_bytes})).collect::<Vec<_>>(),
                }));
                eprintln!("{} {} files {} bytes", s.id, inputs.len(), bytes);
            }
            std::fs::write(&out, serde_json::to_vec_pretty(&rows)?)?;
        }
        Cmd::Link => {
            let r = link::run(&db)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        Cmd::Tokens { tokenizer, threads } => {
            rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().ok();
            let r = tokens::run(&db, &cli.store, &tokenizer)?;
            println!("{}", r);
        }
        Cmd::Rebuild { file_sha256, out } => {
            ensure!(!out.exists(), "{} exists", out.display());
            let mut f = std::io::BufWriter::new(std::fs::File::create(&out)?);
            let got = archive::rebuild_file(&db, &cli.store.join("packs"), &file_sha256, &mut f)?;
            f.flush()?;
            ensure!(got == file_sha256, "rebuilt digest {got} differs from {file_sha256}");
            println!("{got}");
        }
        Cmd::Verify { out, expectations, rebuild_views } => {
            let r = verify::run(&db, &cli.store, expectations.as_deref(), rebuild_views)?;
            std::fs::write(&out, serde_json::to_vec_pretty(&r)?).with_context(|| format!("write {}", out.display()))?;
            println!("{}", serde_json::to_string(&r["summary"])?);
        }
        Cmd::Export { out, subset, .. } => {
            let r = export::run(&db, &cli.store, &out, &subset)?;
            println!("{}", serde_json::to_string(&r["files"])?);
        }
        Cmd::Status => {
            let r = verify::status(&db)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
    }
    Ok(())
}
