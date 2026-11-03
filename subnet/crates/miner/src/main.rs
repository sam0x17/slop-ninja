//! `slop_ninja_miner`: pack, check, commit, upload, status and next-cohort.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use rtao_signer::KeyView as _;
use slop_ninja_artifact_check::spec;
use slop_ninja_miner::check::{
    ArtifactCheck, CheckContext, Outcome, ProvenanceCheck, SpecCheck, StructuralCheck, file_lines,
    run_all,
};
use slop_ninja_miner::client::{
    AdmissionClient, RtaoSigner, Transfer, cohort_info, http_client, ss58,
};
use slop_ninja_miner::opening::{OpeningFile, OsNonce, Store, default_state_dir, load_opening};
use slop_ninja_miner::trust::{Source, Trust};
use slop_ninja_miner::upload::{UploadOptions, UploadOutcome, describe};
use slop_ninja_miner::wire::json::{CohortInfo, CommitmentView};
use slop_ninja_miner::wire::{SessionState, is_session_id};
use slop_ninja_miner::{chain, commit, object, provenance, public};
use slop_ninja_protocol::{AccountId, NeuronIdentity};

/// Exit status when an admission check refused the artifact, or the
/// coordinator refused the upload session.
const EXIT_REFUSED: u8 = 1;
/// Exit status for an error: usage, configuration, network or file. Nothing
/// was decided about the artifact. (clap also exits 2 on a usage error.)
const EXIT_ERROR: u8 = 2;
/// Exit status when `check` refused nothing but could not run every check.
const EXIT_INCOMPLETE: u8 = 3;

#[derive(Parser)]
#[command(
    name = "slop_ninja_miner",
    version,
    about = "Slop Ninja miner client",
    after_help = "Exit status: 0 success or pass; 1 refused (an admission check, or the \
                  upload session); 2 error (usage, configuration, network or file); \
                  3 check incomplete."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build an artifact object from model files.
    Pack(PackArgs),
    /// Verify an artifact object and run the artifact spec's admission checks.
    Check(CheckArgs),
    /// Make a fresh opening for the admitting cohort and commit its digest on chain.
    Commit(CommitArgs),
    /// Upload the committed artifact to the coordinator; resumes an interrupted upload.
    Upload(UploadArgs),
    /// Abort this hotkey's upload session (to upload a different object).
    Abort(AbortArgs),
    /// Show the public status, local openings and the coordinator's view of this hotkey.
    Status(StatusArgs),
    /// Show the admitting cohort, or the next one.
    NextCohort(PublicArgs),
}

#[derive(Args)]
struct PackArgs {
    /// Model directory holding exactly the artifact spec's six files
    /// (a-fp32band.onnx, heads.json, contract.json, calibration.json,
    /// manifest.json, tokenizer.json). Their roles and the output contract
    /// come from the spec.
    #[arg(long, conflicts_with_all = ["files", "root", "contract"])]
    dir: Option<PathBuf>,
    /// The output contract, with --file (default: the artifact spec's).
    #[arg(long)]
    contract: Option<String>,
    /// Directory the --file paths are relative to.
    #[arg(long)]
    root: Option<PathBuf>,
    /// ROLE:PATH, repeated, instead of --dir. Roles: graph, weights, tokenizer,
    /// calibration, output-schema, reference-vectors, owner-metadata,
    /// publisher-signature.
    #[arg(long = "file", required_unless_present = "dir")]
    files: Vec<String>,
    /// Output object path; must not exist. Written with mode 0600.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args)]
struct CheckArgs {
    /// The artifact object.
    object: PathBuf,
    /// Expected output contract (default: the coordinator's, when given).
    #[arg(long)]
    contract: Option<String>,
    /// Size bound (default: the coordinator's, when given).
    #[arg(long)]
    max_object_len: Option<u64>,
    #[command(flatten)]
    endpoints: Endpoints,
    /// Print the reports as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone)]
struct Endpoints {
    /// Coordinator admission base URL. When a trust file pins coordinator
    /// origins, it must be one of them.
    #[arg(long, env = "SLOP_NINJA_COORDINATOR")]
    coordinator: Option<String>,
    /// Public host base URL (the unsigned status snapshot).
    #[arg(long, env = "SLOP_NINJA_PUBLIC")]
    public: Option<String>,
    /// The release trust file (testnet-trust.json). Its admission_origins pin
    /// the coordinators the client may upload to.
    #[arg(long, env = "SLOP_NINJA_TRUST")]
    trust: Option<PathBuf>,
    /// Bound on each request without a part body, in seconds.
    #[arg(long, default_value_t = 300)]
    http_timeout_secs: u64,
}

#[derive(Args, Clone)]
struct Wallet {
    /// Wallet root [default: ~/.bittensor/wallets].
    #[arg(long, env = "SLOP_NINJA_WALLET_PATH")]
    wallet_path: Option<PathBuf>,
    /// Wallet name.
    #[arg(long, env = "SLOP_NINJA_WALLET")]
    wallet: Option<String>,
    /// Hotkey name within the wallet.
    #[arg(long, env = "SLOP_NINJA_HOTKEY")]
    hotkey: Option<String>,
    /// Explicit hotkey keyfile (instead of the wallet names).
    #[arg(long)]
    hotkey_file: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct State {
    /// State directory for openings and upload progress.
    #[arg(long, env = "SLOP_NINJA_MINER_HOME")]
    state_dir: Option<PathBuf>,
}

#[derive(Args)]
struct CommitArgs {
    /// The packed artifact object.
    #[arg(long)]
    artifact: PathBuf,
    /// Require this cohort to be the admitting one.
    #[arg(long)]
    cohort: Option<u64>,
    /// rtao network label.
    #[arg(long, default_value = "test")]
    network: String,
    /// RPC endpoint, repeated for fallbacks (default: the network's).
    #[arg(long = "rpc")]
    rpc: Vec<String>,
    /// Client-side RPC request budget per minute.
    #[arg(long)]
    rpc_per_minute: Option<u32>,
    /// Commit again for a cohort this hotkey already has an opening for.
    #[arg(long)]
    replace: bool,
    /// Show the opening and the call without storing, signing or submitting
    /// anything. The dry run's opening is discarded.
    #[arg(long)]
    dry_run: bool,
    /// Bound on signing, submission and finalization, in seconds.
    #[arg(long, default_value_t = 600)]
    deadline_secs: u64,
    #[command(flatten)]
    endpoints: Endpoints,
    #[command(flatten)]
    wallet: Wallet,
    #[command(flatten)]
    state: State,
}

#[derive(Args)]
struct UploadArgs {
    /// The packed artifact object.
    #[arg(long)]
    artifact: PathBuf,
    /// The opening file (default: the local opening of the admitting cohort
    /// whose payload is the commitment the coordinator observed).
    #[arg(long)]
    opening: Option<PathBuf>,
    /// Attempts per request for retryable errors.
    #[arg(long, default_value_t = 6)]
    attempts: u32,
    /// How long to wait for verification after completion, in seconds.
    #[arg(long, default_value_t = 600)]
    wait_secs: u64,
    /// Seconds between session reads while the coordinator verifies.
    #[arg(long, default_value_t = 10)]
    poll_secs: u64,
    /// Stop after this many parts in this run; run upload again to resume.
    #[arg(long)]
    stop_after_parts: Option<u32>,
    /// Slowest sustained upload rate a part may take, in KiB/s; a part's
    /// request is bounded by --http-timeout-secs plus its size at this rate.
    #[arg(long, default_value_t = 32)]
    min_upload_kib_per_sec: u64,
    /// If the session was refused (for example object_mismatch), abort its
    /// attempt and send every part again.
    #[arg(long)]
    restart: bool,
    #[command(flatten)]
    endpoints: Endpoints,
    #[command(flatten)]
    wallet: Wallet,
    #[command(flatten)]
    state: State,
}

#[derive(Args)]
struct AbortArgs {
    /// The session id, or a unique prefix of it (default: this hotkey's live
    /// session in the cohort).
    session: Option<String>,
    /// The cohort (default: the admitting one).
    #[arg(long)]
    cohort: Option<u64>,
    #[command(flatten)]
    endpoints: Endpoints,
    #[command(flatten)]
    wallet: Wallet,
}

#[derive(Args)]
struct StatusArgs {
    /// Cohort to ask the coordinator about (default: the admitting one).
    #[arg(long)]
    cohort: Option<u64>,
    #[command(flatten)]
    endpoints: Endpoints,
    #[command(flatten)]
    wallet: Wallet,
    #[command(flatten)]
    state: State,
}

#[derive(Args)]
struct PublicArgs {
    #[command(flatten)]
    endpoints: Endpoints,
}

impl Wallet {
    fn hotkey_path(&self) -> Result<PathBuf> {
        if let Some(path) = &self.hotkey_file {
            return Ok(path.clone());
        }
        let (Some(wallet), Some(hotkey)) = (&self.wallet, &self.hotkey) else {
            bail!("name the hotkey: --wallet and --hotkey, or --hotkey-file");
        };
        for name in [wallet, hotkey] {
            ensure!(
                !name.is_empty() && !name.contains('/') && name != "." && name != "..",
                "wallet and hotkey names are single path components"
            );
        }
        let root = match &self.wallet_path {
            Some(root) => root.clone(),
            None => PathBuf::from(
                std::env::var_os("HOME").context("HOME is not set; pass --wallet-path")?,
            )
            .join(".bittensor")
            .join("wallets"),
        };
        Ok(root.join(wallet).join("hotkeys").join(hotkey))
    }

    fn named(&self) -> bool {
        self.hotkey_file.is_some() || (self.wallet.is_some() && self.hotkey.is_some())
    }

    fn file(&self) -> Result<rtao_wallet::HotkeyFile> {
        Ok(rtao_wallet::HotkeyFile::new(self.hotkey_path()?))
    }

    /// The hotkey's public key, from the keyfile's public companion only.
    fn account(&self) -> Result<AccountId> {
        let address = self
            .file()?
            .hotkey_address()
            .context("read the hotkey's public companion file")?;
        Ok(AccountId(address.0))
    }

    async fn signer(&self) -> Result<Arc<RtaoSigner>> {
        let signer = self.file()?.unlock().await.context("unlock the hotkey")?;
        Ok(Arc::new(RtaoSigner(signer)))
    }
}

impl State {
    fn store(&self) -> Result<Store> {
        let root = match &self.state_dir {
            Some(dir) => dir.clone(),
            None => default_state_dir()?,
        };
        Store::open(&root)
    }
}

impl Endpoints {
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.http_timeout_secs)
    }

    fn http(&self) -> Result<reqwest::Client> {
        http_client(self.timeout())
    }

    fn any(&self) -> bool {
        self.coordinator.is_some() || self.public.is_some()
    }

    fn trust(&self) -> Result<Trust> {
        match &self.trust {
            Some(path) => Trust::load(path),
            None => Ok(Trust::default()),
        }
    }

    async fn public_status(&self, http: &reqwest::Client) -> Result<Option<public::PublicStatus>> {
        match &self.public {
            Some(public) => Ok(Some(
                public::fetch_status(http, &slop_ninja_miner::client::base_url(public)?).await?,
            )),
            None => Ok(None),
        }
    }

    /// The coordinator to talk to and why it was chosen: `--coordinator`, or
    /// the unsigned snapshot's coordinator when the trust file pins it.
    async fn coordinator(&self, http: &reqwest::Client) -> Result<(url::Url, Source)> {
        let trust = self.trust()?;
        let advertised = match &self.coordinator {
            Some(_) => None,
            None => {
                self.public_status(http)
                    .await?
                    .context(
                        "pass --coordinator, or --public with a trust file (--trust) that pins \
                         the coordinator's origin",
                    )?
                    .coordinator
            }
        };
        trust.choose(self.coordinator.as_deref(), advertised.as_deref())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    match runtime.block_on(run(cli.command)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

async fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Pack(args) => pack(args),
        Command::Check(args) => check(args).await,
        Command::Commit(args) => commit_cmd(args).await,
        Command::Upload(args) => upload_cmd(args).await,
        Command::Abort(args) => abort(args).await,
        Command::Status(args) => status(args).await,
        Command::NextCohort(args) => next_cohort(args).await,
    }
}

fn pack(args: PackArgs) -> Result<ExitCode> {
    let verified = match &args.dir {
        Some(dir) => object::pack_dir(dir, &args.out)?,
        None => {
            let root = args.root.clone().unwrap_or_else(|| PathBuf::from("."));
            let inputs = args
                .files
                .iter()
                .map(|spec| object::PackInput::parse(spec, &root))
                .collect::<Result<Vec<_>>>()?;
            let contract = args.contract.as_deref().unwrap_or(spec::CONTRACT_ID);
            object::pack(contract, &inputs, &args.out)?
        }
    };
    println!("wrote {} (mode 0600)", args.out.display());
    println!("object sha256 {}  length {}", verified.sha256, verified.len);
    println!("contract {}", verified.header.contract);
    println!("inference content {}", verified.content);
    for line in file_lines(&verified) {
        println!("  {line}");
    }
    // Local paths in the JSON files: a warning, never a refusal; a scan that
    // cannot run is said, and pack still succeeds.
    match provenance::scan_object(&args.out, &verified) {
        Ok(scans) => {
            for scan in scans {
                match &scan.scan {
                    provenance::Scan::Paths(hits) if !hits.is_empty() => {
                        eprintln!("warning: {}", provenance::advice(&scan.name, hits));
                    }
                    provenance::Scan::Unreadable(why) => {
                        eprintln!("warning: {}: not scanned for local paths: {why}", scan.name);
                    }
                    _ => {}
                }
            }
        }
        Err(error) => eprintln!("warning: not scanned for local paths: {error:#}"),
    }
    println!("next: slop_ninja_miner check {}", args.out.display());
    Ok(ExitCode::SUCCESS)
}

/// The admitting cohort, read from the coordinator the endpoints choose.
async fn admitting_cohort(
    endpoints: &Endpoints,
) -> Result<Option<slop_ninja_miner::wire::json::Admitting>> {
    let http = endpoints.http()?;
    let (base, _) = endpoints.coordinator(&http).await?;
    Ok(cohort_info(&http, &base).await?.admitting)
}

async fn check(args: CheckArgs) -> Result<ExitCode> {
    // An object that does not parse or verify is a refusal (the coordinator
    // refuses it too); a file that cannot be read is an error.
    let verified = match object::verify_file_checked(&args.object)? {
        Ok(verified) => verified,
        Err(invalid) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "outcome": Outcome::Refused,
                        "refusal": {"code": "object", "message": invalid},
                    }))?
                );
            } else {
                println!("  [REFUSED] object: {invalid}");
            }
            return Ok(ExitCode::from(EXIT_REFUSED));
        }
    };
    let (mut contract, mut max) = (args.contract.clone(), args.max_object_len);
    let mut cohort_spec = None;
    let mut cohort_unknown = None;
    if args.endpoints.any() {
        // A coordinator that cannot be chosen or read leaves the check
        // incomplete; the local checks still run.
        match admitting_cohort(&args.endpoints).await {
            Ok(Some(admitting)) => {
                contract.get_or_insert(admitting.output_contract);
                max.get_or_insert(admitting.max_object_len);
                cohort_spec = admitting.artifact_spec;
            }
            Ok(None) => cohort_unknown = Some("no cohort is admitting".to_owned()),
            Err(error) => {
                eprintln!("warning: the cohort could not be read: {error:#}");
                cohort_unknown = Some(format!("the cohort could not be read: {error:#}"));
            }
        }
    }
    let cx = CheckContext {
        object: &args.object,
        verified: &verified,
        expected_contract: contract.as_deref(),
        max_object_len: max,
        artifact_spec: cohort_spec.as_ref(),
        cohort_unknown: cohort_unknown.as_deref(),
    };
    let checks: [&dyn ArtifactCheck; 3] = [&StructuralCheck, &SpecCheck, &ProvenanceCheck];
    let (outcome, reports) = run_all(&checks, &cx)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "object_sha256": verified.sha256.hex(),
                "object_len": verified.len,
                "content": verified.content.hex(),
                "outcome": outcome,
                "reports": reports,
            }))?
        );
    } else {
        println!("object sha256 {}  length {}", verified.sha256, verified.len);
        println!("inference content {}", verified.content);
        for report in &reports {
            println!("{}: {:?}", report.checker, report.outcome);
            for finding in &report.findings {
                let mark = if finding.refuses {
                    "REFUSED"
                } else if finding.warning {
                    "WARNING"
                } else {
                    "note"
                };
                println!("  [{mark}] {}: {}", finding.code, finding.message);
            }
        }
    }
    Ok(match outcome {
        Outcome::Pass => ExitCode::SUCCESS,
        Outcome::Refused => ExitCode::from(EXIT_REFUSED),
        Outcome::Incomplete => ExitCode::from(EXIT_INCOMPLETE),
    })
}

async fn commit_cmd(args: CommitArgs) -> Result<ExitCode> {
    let artifact = object::verify_file(&args.artifact)?;
    let http = args.endpoints.http()?;
    let (base, source) = args.endpoints.coordinator(&http).await?;
    let info = cohort_info(&http, &base).await?;
    println!(
        "coordinator {} ({})",
        base.origin().ascii_serialization(),
        source.describe()
    );
    let chain_binding = info
        .chain
        .binding()
        .context("the coordinator reported a malformed chain")?;
    let Some(admitting) = info.admitting else {
        bail!(
            "no cohort is admitting{}",
            info.next
                .map(|n| format!("; the next is cohort {}", n.cohort))
                .unwrap_or_default()
        );
    };
    if let Some(cohort) = args.cohort {
        ensure!(
            cohort == admitting.cohort,
            "cohort {cohort} is not admitting; cohort {} is",
            admitting.cohort
        );
    }
    ensure!(
        !admitting.closing,
        "cohort {} is closing admission",
        admitting.cohort
    );
    ensure!(
        artifact.header.contract == admitting.output_contract,
        "the artifact implements {:?}; cohort {} expects {:?}",
        artifact.header.contract,
        admitting.cohort,
        admitting.output_contract
    );
    ensure!(
        artifact.len <= admitting.max_object_len,
        "the object is {} bytes; cohort {} allows {}",
        artifact.len,
        admitting.cohort,
        admitting.max_object_len
    );
    let hotkey = args.wallet.account()?;
    let store = args.state.store()?;

    let endpoints = args
        .rpc
        .iter()
        .map(|u| url::Url::parse(u).with_context(|| format!("{u:?} is not a URL")))
        .collect::<Result<Vec<_>>>()?;
    let client = chain::connect(&chain::ChainConfig {
        network: args.network.clone(),
        endpoints,
        requests_per_minute: args.rpc_per_minute,
        startup_timeout: Duration::from_secs(60),
    })
    .await?;
    let live = chain::chain_binding(&client, chain_binding.netuid, chain_binding.mechanism);
    ensure!(
        live == chain_binding,
        "the RPC endpoint serves another chain than the coordinator"
    );
    let (finalized, identity) = chain::registration(&client, chain_binding.netuid, &hotkey).await?;
    let identity: NeuronIdentity = identity.with_context(|| {
        format!(
            "{} is not registered on subnet {} at finalized block {finalized}",
            ss58(&hotkey),
            chain_binding.netuid
        )
    })?;
    let draft = commit::make(
        &store,
        &chain_binding,
        admitting.cohort,
        &identity,
        &artifact,
        &mut OsNonce,
        args.replace,
    )?;
    println!(
        "cohort {}  hotkey {} (uid {})",
        admitting.cohort,
        ss58(&hotkey),
        identity.uid
    );
    println!("commitment {}", draft.opening.commitment());
    println!("chain payload {}", hex::encode(draft.payload));
    let runtime = client.refresh_runtime().await?;
    let call = chain::compose(
        &chain::commitment_call(chain_binding.netuid, &draft.payload),
        &runtime,
    )?;
    println!("call {}", hex::encode(call.as_bytes()));
    if args.dry_run {
        println!("dry run: nothing stored, signed or submitted; this opening is discarded");
        return Ok(ExitCode::SUCCESS);
    }
    // Stored before signing, so a commitment that lands is never without its opening.
    let path = store.save_new_opening(&draft.file)?;
    println!("opening stored (mode 0600): {}", path.display());
    let file = args.wallet.file()?;
    let submitted = chain::submit_commitment(
        &client,
        &file,
        chain_binding.netuid,
        &draft.payload,
        Duration::from_secs(args.deadline_secs),
    )
    .await?;
    let mut record = draft.file.clone();
    record.submitted = Some(submitted.clone());
    store.update_opening(&path, &record)?;
    println!(
        "committed: block {} ({}); finalized commitment block {}",
        submitted.block_hash.as_deref().unwrap_or("?"),
        submitted.extrinsic_id.as_deref().unwrap_or("?"),
        submitted
            .finalized_commitment_block
            .map_or_else(|| "?".to_owned(), |b| b.to_string())
    );
    if submitted.finalized_commitment_block.is_none() {
        println!(
            "note: the extrinsic was finalized, but finalized state did not show the commitment \
             before the deadline; the opening records the inclusion. Check `status` before uploading."
        );
    }
    println!(
        "next: slop_ninja_miner upload --artifact {}",
        args.artifact.display()
    );
    Ok(ExitCode::SUCCESS)
}

async fn admission_client(
    endpoints: &Endpoints,
    wallet: &Wallet,
) -> Result<(AdmissionClient, CohortInfo, Source)> {
    let http = endpoints.http()?;
    let (base, source) = endpoints.coordinator(&http).await?;
    let signer = wallet.signer().await?;
    let (client, info) = AdmissionClient::connect(http, base, signer, None).await?;
    Ok((client, info, source))
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The opening to upload for `cohort` and `artifact` (see [`pick_opening`]).
async fn choose_opening(
    store: &Store,
    client: &mut AdmissionClient,
    cohort: u64,
    artifact: &slop_ninja_protocol::VerifiedArtifact,
) -> Result<PathBuf> {
    let openings = store.openings_for(client.chain().netuid, cohort, &ss58(&client.hotkey()))?;
    ensure!(
        !openings.is_empty(),
        "no opening for cohort {cohort}; run commit first"
    );
    let view = client.hotkey_view(cohort).await?;
    let mut dated: Vec<_> = openings
        .into_iter()
        .map(|(path, file)| {
            let m = modified(&path);
            (path, file, m)
        })
        .collect();
    dated.sort_by_key(|(path, _, m)| (*m, path.clone()));
    let (path, note) = pick_opening(&dated, view.commitment.as_ref(), &artifact.sha256.hex(), artifact.len)?;
    if let Some(note) = note {
        println!("note: {note}");
    }
    Ok(path)
}

/// Picks the opening to upload from the local openings for one cohort
/// (oldest first) and the commitment the coordinator observed:
///
/// - The newest opening this wallet confirmed on chain at a later block than
///   the observed commitment (a commitment just replaced, or a returning
///   miner's new one, that the coordinator has not logged yet), when it is
///   for `artifact`; when it is for another artifact, the observed
///   commitment is already superseded, so nothing is picked.
/// - Otherwise the opening whose payload is the observed commitment, when it
///   is for `artifact`.
/// - Otherwise the newest opening for `artifact` with a submission record.
///
/// An opening that was never committed is chosen only by `--opening`.
fn pick_opening(
    openings: &[(PathBuf, OpeningFile, Option<std::time::SystemTime>)],
    observed: Option<&CommitmentView>,
    artifact_sha256: &str,
    artifact_len: u64,
) -> Result<(PathBuf, Option<&'static str>)> {
    let fits = |f: &OpeningFile| f.artifact_sha256 == artifact_sha256 && f.artifact_len == artifact_len;
    let confirmed_at = |f: &OpeningFile| f.submitted.as_ref().and_then(|s| s.finalized_commitment_block);
    let newest_confirmed = openings
        .iter()
        .filter_map(|(path, f, m)| confirmed_at(f).map(|b| (b, *m, path, f)))
        .max_by_key(|(b, m, path, _)| (*b, *m, (*path).clone()));
    if let Some((block, _, path, f)) = newest_confirmed
        && observed.is_none_or(|o| block > o.block)
    {
        ensure!(
            fits(f),
            "the newest commitment this wallet confirmed on chain (block {block}, opening {}) is for \
             another artifact than {artifact_sha256}; upload that artifact, or pass --opening FILE",
            path.display()
        );
        let note = observed.map(|_| {
            "the coordinator has not logged this wallet's newest commitment yet; the upload sends \
             its confirmed block, and the coordinator answers commitment_not_found until its \
             reader has polled at or after that block"
        });
        return Ok((path.clone(), note));
    }
    if let Some(o) = observed
        && let Some((path, f, _)) = openings.iter().find(|(_, f, _)| f.chain_payload == o.payload)
    {
        ensure!(
            fits(f),
            "the commitment the coordinator observed (opening {}) is for another artifact than \
             {artifact_sha256}; upload that artifact, or commit again for this one",
            path.display()
        );
        return Ok((path.clone(), None));
    }
    if let Some((path, _, _)) = openings.iter().rev().find(|(_, f, _)| f.submitted.is_some() && fits(f)) {
        return Ok((
            path.clone(),
            Some("the coordinator has observed no commitment matching a local opening yet; using the newest opening for this artifact submitted on chain"),
        ));
    }
    bail!(
        "none of the {} local openings for this cohort is for artifact {artifact_sha256} and matches \
         the commitment the coordinator observed{} or was submitted on chain; pass --opening FILE",
        openings.len(),
        if observed.is_some() { "" } else { " (it has observed none yet)" }
    )
}

async fn upload_cmd(args: UploadArgs) -> Result<ExitCode> {
    let store = args.state.store()?;
    let artifact = object::verify_file(&args.artifact)?;
    let (client, info, source) = admission_client(&args.endpoints, &args.wallet).await?;
    let mut client = client.with_transfer(Transfer {
        timeout: args.endpoints.timeout(),
        min_upload_rate: args.min_upload_kib_per_sec.saturating_mul(1024),
    });
    let opening_path = match &args.opening {
        Some(path) => path.clone(),
        None => {
            let cohort = info
                .admitting
                .as_ref()
                .map(|a| a.cohort)
                .context("no cohort is admitting")?;
            choose_opening(&store, &mut client, cohort, &artifact).await?
        }
    };
    let opening: OpeningFile = load_opening(&opening_path)?;
    println!(
        "uploading opening {} to coordinator {} ({})",
        opening_path.display(),
        client.origin(),
        source.describe()
    );
    let options = UploadOptions {
        attempts: args.attempts.max(1),
        wait: Duration::from_secs(args.wait_secs),
        poll: Duration::from_secs(args.poll_secs),
        stop_after_parts: args.stop_after_parts,
        restart: args.restart,
        ..UploadOptions::default()
    };
    match slop_ninja_miner::upload::upload(
        &mut client,
        &store,
        &opening,
        &args.artifact,
        &artifact,
        &options,
    )
    .await?
    {
        UploadOutcome::Interrupted {
            uploaded,
            remaining,
        } => {
            println!(
                "stopped after {uploaded} parts; {remaining} remain. Run upload again to resume."
            );
            Ok(ExitCode::SUCCESS)
        }
        UploadOutcome::Finished(session) => {
            println!("{}", describe(&session));
            let refused = session.refusal.is_some();
            Ok(if refused {
                ExitCode::from(EXIT_REFUSED)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

/// Resolves `given` (a full id, a unique prefix, or nothing for the live
/// session) among this hotkey's sessions in `cohort`.
async fn resolve_session(
    client: &mut AdmissionClient,
    cohort: Option<u64>,
    given: Option<&str>,
) -> Result<String> {
    if let Some(id) = given.filter(|id| is_session_id(id)) {
        return Ok(id.to_owned());
    }
    let cohort = cohort.context("no cohort is admitting; pass --cohort")?;
    let view = client.hotkey_view(cohort).await?;
    let candidates: Vec<&str> = view
        .sessions
        .iter()
        .filter(|s| match given {
            Some(prefix) => s.session.starts_with(prefix),
            None => SessionState::parse(&s.state).is_some_and(|state| !state.ended()),
        })
        .map(|s| s.session.as_str())
        .collect();
    match candidates.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => bail!(
            "no {} session of this hotkey in cohort {cohort}",
            given.map_or_else(|| "live".to_owned(), |p| format!("{p:?}"))
        ),
        many => bail!(
            "{} sessions match in cohort {cohort}; name one: {}",
            many.len(),
            many.join(", ")
        ),
    }
}

async fn abort(args: AbortArgs) -> Result<ExitCode> {
    let (mut client, info, _) = admission_client(&args.endpoints, &args.wallet).await?;
    let cohort = args.cohort.or(info.admitting.as_ref().map(|a| a.cohort));
    let id = resolve_session(&mut client, cohort, args.session.as_deref()).await?;
    let session = client.session(&id).await?;
    let aborted = client.abort(&id, session.attempt).await?;
    println!("{}", describe(&aborted));
    Ok(ExitCode::SUCCESS)
}

async fn status(args: StatusArgs) -> Result<ExitCode> {
    let http = args.endpoints.http()?;
    if let Some(status) = args.endpoints.public_status(&http).await? {
        println!("{}", public::next_cohort_report(&status));
        if let Some(phase) = &status.phase {
            println!(
                "phase {phase}{}",
                status
                    .mode
                    .as_ref()
                    .map(|m| format!(", mode {m}"))
                    .unwrap_or_default()
            );
        }
    }
    let store = args.state.store()?;
    let account = if args.wallet.named() {
        Some(args.wallet.account()?)
    } else {
        None
    };
    let openings = store.all_openings()?;
    let mut shown = 0usize;
    for (path, file) in &openings {
        if account.is_some_and(|a| ss58(&a) != file.hotkey) {
            continue;
        }
        shown += 1;
        let submitted = match &file.submitted {
            Some(s) => format!(
                "committed at finalized block {}",
                s.finalized_commitment_block
                    .map_or_else(|| "?".into(), |b| b.to_string())
            ),
            None => "not confirmed on chain".into(),
        };
        println!(
            "opening cohort {} commitment {}: {submitted} ({})",
            file.cohort,
            file.commitment,
            path.display()
        );
    }
    if shown == 0 {
        println!(
            "no local openings{} in state directory {}",
            if account.is_some() {
                " for this hotkey"
            } else {
                ""
            },
            store.root().display()
        );
    }
    if !args.endpoints.any() {
        println!(
            "no --public or --coordinator given: the public snapshot and the coordinator's view \
             of the hotkey were not read"
        );
        return Ok(ExitCode::SUCCESS);
    }
    if !args.wallet.named() {
        println!(
            "no hotkey named (--wallet and --hotkey, or --hotkey-file): the coordinator's view of \
             the hotkey was not read"
        );
        return Ok(ExitCode::SUCCESS);
    }
    let (mut client, info, source) = admission_client(&args.endpoints, &args.wallet).await?;
    println!("coordinator {} ({})", client.origin(), source.describe());
    let cohort = match args.cohort.or(info.admitting.as_ref().map(|a| a.cohort)) {
        Some(cohort) => cohort,
        None => {
            println!("no cohort is admitting; pass --cohort to ask about an earlier one");
            return Ok(ExitCode::SUCCESS);
        }
    };
    let view = client.hotkey_view(cohort).await?;
    println!(
        "coordinator view of {} in cohort {} at finalized block {}:",
        view.hotkey, cohort, view.finalized_block
    );
    if let Some(cutoff) = view.cutoff_block {
        println!("  admission closed at cutoff block {cutoff}");
    }
    match &view.registration {
        Some(r) => println!(
            "  registered: uid {} since block {}",
            r.uid, r.registration_block
        ),
        None => println!("  not registered on the subnet"),
    }
    match &view.commitment {
        Some(c) => {
            let local = openings.iter().any(|(_, f)| f.chain_payload == c.payload);
            println!(
                "  commitment at block {} extrinsic {} for cohort {}{}{}",
                c.block,
                c.extrinsic,
                c.payload_cohort
                    .map_or_else(|| "?".into(), |n| n.to_string()),
                if c.in_window {
                    ""
                } else {
                    " (outside this cohort's window)"
                },
                if local {
                    "; matches a local opening"
                } else {
                    "; matches no local opening"
                }
            );
        }
        None => println!("  no finalized commitment observed (none made, or not final yet)"),
    }
    if view.sessions.is_empty() {
        println!("  no upload session");
    }
    for summary in &view.sessions {
        match client.session(&summary.session).await {
            Ok(session) => println!("  {}", describe(&session)),
            Err(_) => println!(
                "  session {}: {}{}",
                summary.session,
                summary.state,
                summary
                    .refusal
                    .as_ref()
                    .map(|r| format!(" ({}: {})", r.code, r.message))
                    .unwrap_or_default()
            ),
        }
    }
    if let Some(refused) = &view.last_refusal {
        let mut line = format!(
            "  last refused request: {} ({}) at finalized block {}, envelope {} naming cohort {}",
            refused.code,
            refused.message,
            refused.finalized_block,
            refused.session,
            refused.opening_cohort
        );
        if let Some(live) = &refused.detail.session {
            line.push_str(&format!(
                "; the live session is {live}: abort it (slop_ninja_miner abort {live}) to upload \
                 another object for this commitment"
            ));
        }
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

async fn next_cohort(args: PublicArgs) -> Result<ExitCode> {
    let http = args.endpoints.http()?;
    let status = args
        .endpoints
        .public_status(&http)
        .await?
        .context("pass --public (or set SLOP_NINJA_PUBLIC)")?;
    println!("{}", public::next_cohort_report(&status));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_miner::opening::Submitted;

    fn opening(payload: &str, artifact: &str, confirmed: Option<Option<u64>>) -> OpeningFile {
        OpeningFile {
            schema: String::new(),
            opening: String::new(),
            commitment: String::new(),
            chain_payload: payload.into(),
            cohort: 7,
            netuid: 601,
            hotkey: String::new(),
            registration_block: 1,
            content: String::new(),
            artifact_sha256: artifact.into(),
            artifact_len: 10,
            submitted: confirmed.map(|b| Submitted { finalized_commitment_block: b, ..Default::default() }),
        }
    }

    fn observed(payload: &str, block: u64) -> CommitmentView {
        CommitmentView {
            block,
            block_hash: None,
            extrinsic: 0,
            payload: payload.into(),
            payload_cohort: Some(7),
            in_window: true,
        }
    }

    /// Fix round 1, finding 3: after `commit --replace` the coordinator still
    /// observes the older commitment until its reader logs the new one. The
    /// new opening (confirmed at a later block) is picked for its artifact;
    /// the old artifact is refused locally rather than uploaded for a
    /// commitment the chain no longer holds.
    #[test]
    fn a_replaced_commitment_picks_the_newer_confirmed_opening() {
        let list = vec![
            (PathBuf::from("c1.json"), opening("p1", "a1", Some(Some(100))), None),
            (PathBuf::from("c2.json"), opening("p2", "a2", Some(Some(120))), None),
        ];
        let seen = observed("p1", 100);
        let (path, note) = pick_opening(&list, Some(&seen), "a2", 10).unwrap();
        assert_eq!(path, PathBuf::from("c2.json"));
        assert!(note.is_some());
        let err = pick_opening(&list, Some(&seen), "a1", 10).unwrap_err().to_string();
        assert!(err.contains("another artifact"), "{err}");
        // Once the coordinator logs the new commitment, it is matched directly.
        let (path, note) = pick_opening(&list, Some(&observed("p2", 120)), "a2", 10).unwrap();
        assert_eq!((path, note), (PathBuf::from("c2.json"), None));
        // A commitment the coordinator logged at a later block than any local
        // confirmation (made elsewhere) is matched by payload, or refused.
        let (path, _) = pick_opening(&list, Some(&observed("p1", 130)), "a1", 10).unwrap();
        assert_eq!(path, PathBuf::from("c1.json"));
        // Nothing observed yet: the newest confirmed opening for the artifact.
        let (path, _) = pick_opening(&list, None, "a2", 10).unwrap();
        assert_eq!(path, PathBuf::from("c2.json"));
        // An inclusion whose read-back gave up (no finalized block) is used
        // only as the fallback for its artifact; a never-submitted opening
        // never.
        let list = vec![
            (PathBuf::from("d1.json"), opening("q1", "b1", Some(None)), None),
            (PathBuf::from("d2.json"), opening("q2", "b2", None), None),
        ];
        let (path, note) = pick_opening(&list, None, "b1", 10).unwrap();
        assert_eq!(path, PathBuf::from("d1.json"));
        assert!(note.is_some());
        assert!(pick_opening(&list, None, "b2", 10).is_err());
    }
}
