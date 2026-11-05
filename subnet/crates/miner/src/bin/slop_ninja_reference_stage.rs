//! Operator tool: stage a packed reference package through the eval
//! Worker's reference staging routes (`slop_ninja_reference_staging_v1`;
//! `slop_ninja_miner::reference`).
//!
//! ```text
//! slop_ninja_reference_stage --eval-url URL --operator-token-file FILE --dir PACK_OUT
//!     --spec v2|v3|v4|<spec id> [--trust TRUST.json] [--attempts N] [--out RECORD.json]
//! ```
//!
//! `PACK_OUT` is `slop_ninja_a_served_profile pack --spec <the same spec> --model-dir
//! <dir> --out PACK_OUT`. Each file goes straight to R2 under a presigned PUT that binds
//! its length and SHA-256 (R2 verifies it); the eval Worker then pins every file's
//! entity tag and writes the package manifest. The tool prints the commit answer
//! (package digest, manifest key, each file's digest, key and entity tag), and writes it
//! to `--out` when given. Presigned URLs are checked against the pinned R2 host and
//! bucket (`--trust`, else the release trust file) before any byte is sent. The token
//! file is read once and its value is sent only as the `Authorization` header.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use slop_ninja_artifact_check::Version;
use slop_ninja_miner::client::{Transfer, http_client};
use slop_ninja_miner::direct::{Progress, PutPolicy};
use slop_ninja_miner::reference::{self, EvalClient, Token};
use slop_ninja_miner::trust::Trust;
use url::Url;

#[derive(Parser, Debug)]
#[command(name = "slop_ninja_reference_stage", about = "Stage a packed reference package (slop_ninja_reference_staging_v1)")]
struct Args {
    /// The eval Worker's origin (`https://...`).
    #[arg(long)]
    eval_url: String,
    /// A file holding the operator token (`COORDINATOR_OPERATOR_TOKEN`).
    #[arg(long)]
    operator_token_file: PathBuf,
    /// The `pack` output directory (`files/<sha256>`, `artifact-files.json`).
    #[arg(long)]
    dir: PathBuf,
    /// The spec the package is for: `v2`, `v3`, `v4` or a full spec id (the
    /// spec the template names).
    #[arg(long)]
    spec: String,
    /// A trust file pinning the upload host and bucket (default: the
    /// release trust file compiled in).
    #[arg(long)]
    trust: Option<PathBuf>,
    /// Attempts per file, each with fresh URLs (an operator choice).
    #[arg(long, default_value_t = 3)]
    attempts: u32,
    /// Where to write the commit answer.
    #[arg(long)]
    out: Option<PathBuf>,
}

struct Lines;

impl Progress for Lines {
    fn start(&self, name: &str, len: u64, attempt: u32) {
        eprintln!("PUT {name} ({len} bytes), attempt {attempt}");
    }
    fn finish(&self, name: &str, ok: bool, why: &str) {
        eprintln!("{} {name}: {why}", if ok { "ok" } else { "failed" });
    }
}

fn spec_of(arg: &str) -> Result<Version> {
    match arg {
        "v2" => Ok(Version::V2),
        "v3" => Ok(Version::V3),
        "v4" => Ok(Version::V4),
        id => Version::from_id(id).with_context(|| format!("--spec {id}: not a spec this build defines")),
    }
}

fn eval_origin(text: &str) -> Result<Url> {
    let url = Url::parse(text).context("--eval-url")?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    ensure!(url.scheme() == "https" || (url.scheme() == "http" && loopback), "--eval-url must be https");
    ensure!(url.path() == "/" && url.query().is_none(), "--eval-url is an origin, without a path or query");
    Ok(url)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let version = spec_of(&args.spec)?;
    let base = eval_origin(&args.eval_url)?;
    let token = std::fs::read_to_string(&args.operator_token_file)
        .with_context(|| format!("{}", args.operator_token_file.display()))?;
    let token = Token::new(token.trim().to_owned());
    let trust = match &args.trust {
        Some(p) => Trust::load(p)?,
        None => Trust::release(),
    };
    ensure!(trust.pins_upload(), "the trust file pins no upload host or bucket");
    let policy = PutPolicy {
        pinned_host: trust.upload_host().map(str::to_owned),
        pinned_host_sha256: trust.upload_host_sha256(),
        bucket: trust.upload_bucket().map(str::to_owned),
        endpoint_override: None,
    };
    let staging = reference::load(&args.dir, version)?;
    eprintln!("{} distinct files under {}", staging.files.len(), version.id());
    let transfer = Transfer::default();
    let client = EvalClient { http: http_client(transfer.timeout)?, base, token };
    let progress: Arc<dyn Progress> = Arc::new(Lines);
    let (outcomes, commit) =
        reference::stage(&client, &policy, &staging, args.attempts, |len| transfer.put_timeout(len), progress).await?;
    let stored = outcomes.iter().filter(|o| **o == reference::Staged::Stored).count();
    eprintln!("{stored} stored, {} already present", outcomes.len() - stored);
    let text = serde_json::to_string_pretty(&commit)?;
    if let Some(out) = &args.out {
        std::fs::write(out, format!("{text}\n")).with_context(|| format!("{}", out.display()))?;
    }
    println!("{text}");
    Ok(())
}
