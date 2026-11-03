use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_baseline::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "slop_ninja_a_baseline",
    about = "Frozen numeric-only starter A comparisons; no parser, text model or network"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Print a validated contract identity. Does not fit or inspect a corpus.
    ContractId {
        #[arg(long)]
        contract: PathBuf,
    },
    /// Verify declared export bindings, numeric shapes and split/ancestry fences.
    Validate {
        #[arg(long)]
        contract: PathBuf,
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        audit: PathBuf,
    },
    /// Fit all three heads on the same Train/Development/Calibration rows.
    Train {
        #[arg(long)]
        contract: PathBuf,
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        audit: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Score a new numeric-only request. Labels and provenance are not accepted.
    Predict {
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Evaluate frozen heads on Test labels without fitting anything.
    Evaluate {
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        audit: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NumericRequest {
    contract_sha256: StableId,
    inputs: Vec<NumericInput>,
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(
        fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
    ))
    .with_context(|| format!("read {}", path.display()))
}
fn rows(path: &Path) -> Result<Vec<NumericRow>> {
    BufReader::new(fs::File::open(path)?)
        .lines()
        .enumerate()
        .filter_map(|(i, line)| match line {
            Ok(s) if s.trim().is_empty() => None,
            other => Some((i, other)),
        })
        .map(|(i, line)| {
            Ok(serde_json::from_str(&line?)
                .with_context(|| format!("numeric row {}:{}", path.display(), i + 1))?)
        })
        .collect()
}
fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = BufWriter::new(
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?,
    );
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}
fn print(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().command {
        Command::ContractId { contract } => {
            let c: Contract = read(&contract)?;
            print(&serde_json::json!({"contract_sha256":c.identity()?}))
        }
        Command::Validate {
            contract,
            rows: input,
            audit,
        } => {
            let c: Contract = read(&contract)?;
            let r = rows(&input)?;
            let a: ExportAudit = read(&audit)?;
            validate_export(&c, &r, &a)?;
            print(
                &serde_json::json!({"valid":true,"contract_sha256":c.identity()?,"rows":r.len(),"rows_sha256":rows_identity(&r)?,"grammar_evidence":c.grammar_evidence,"semantic_admission":false}),
            )
        }
        Command::Train {
            contract,
            rows: input,
            audit,
            config,
            out,
        } => {
            let c: Contract = read(&contract)?;
            let r = rows(&input)?;
            let a: ExportAudit = read(&audit)?;
            let config = if let Some(path) = config {
                read(&path)?
            } else {
                TrainConfig::default()
            };
            ensure!(!out.exists(), "output already exists");
            let result = fit(&c, &r, &a, &config)?;
            fs::create_dir(&out).context("create output directory (parent must exist)")?;
            write_new(&out.join("outcome.json"), &result)?;
            match result {
                FitOutcome::Ready { artifact, report } => {
                    write_new(&out.join("artifact.json"), &artifact)?;
                    write_new(&out.join("report.json"), &report)?;
                    print(
                        &serde_json::json!({"status":"ready","artifact_sha256":artifact.sha256,"out":out,"semantic_admission":false}),
                    )
                }
                FitOutcome::Unavailable { reason, .. } => {
                    print(&serde_json::json!({"status":"unavailable","reason":reason,"out":out}))?;
                    anyhow::bail!("numeric comparison unavailable; retained outcome.json")
                }
            }
        }
        Command::Predict {
            artifact,
            request,
            out,
        } => {
            let a: Artifact = read(&artifact)?;
            a.validate()?;
            let request: NumericRequest = read(&request)?;
            ensure!(
                request.contract_sha256 == a.content.contract_sha256,
                "request/model contract mismatch"
            );
            let predictions = request
                .inputs
                .iter()
                .map(|input| predict_validated(&a, input))
                .collect::<Result<Vec<_>>>()?;
            write_new(
                &out,
                &serde_json::json!({"artifact_sha256":a.sha256,"predictions":predictions,"semantic_admission":false}),
            )
        }
        Command::Evaluate {
            artifact,
            rows: input,
            audit,
            out,
        } => {
            let a: Artifact = read(&artifact)?;
            let r = rows(&input)?;
            let audit: ExportAudit = read(&audit)?;
            let result = evaluate(&a, &r, &audit)?;
            write_new(&out, &result)?;
            ensure!(
                result.available,
                "evaluation unavailable: no eligible labelled paired documents"
            );
            Ok(())
        }
    }
}
