//! `slop_ninja_fixture_package`: testnet shortcut T12, removed by M6.
//!
//! Builds the minimal coordinator-signed fixture package that drives the M2
//! native commit, and generates the application signing seeds it is signed
//! with. These seeds are Slop Ninja application keys, never chain keys. No
//! command connects to a chain, and no seed is ever printed.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use slop_ninja_protocol::{AccountId, NeuronIdentity, SigningKey};
use slop_ninja_validator::fixture::{FixtureParams, build};
use slop_ninja_validator::package::{self, Bundle};

#[derive(Parser)]
#[command(about = "Build a T12 coordinator-signed fixture package (testnet only)")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create package, receipt and authorization seed files (mode 0600) in an
    /// existing directory, and print the validator's `[package.trusted_keys]`.
    Keygen {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Print `[package.trusted_keys]` for existing seed files.
    PublicKeys {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Build and sign a bundle, then verify it as a validator would.
    Build {
        #[arg(long)]
        netuid: u16,
        #[arg(long, default_value_t = 0)]
        mechanism: u8,
        #[arg(long)]
        cohort: u64,
        #[arg(long, default_value_t = 1)]
        revision: u64,
        /// Finalized tempo-boundary block recorded as the cohort's opening.
        #[arg(long)]
        opened_block: u64,
        /// Owner registration at UID 0, as `0:<ss58>:<registration block>`.
        #[arg(long)]
        burn: String,
        /// Allow-listed miner, as `<uid>:<ss58>:<registration block>`. Repeat.
        #[arg(long = "miner", required = true)]
        miners: Vec<String>,
        /// Directory holding the seed files from `keygen`.
        #[arg(long)]
        keys: PathBuf,
        /// Output bundle path; refused if it exists.
        #[arg(long)]
        out: PathBuf,
    },
}

const ROLES: [&str; 3] = ["package", "receipt", "authorization"];

fn seed_path(dir: &Path, role: &str) -> PathBuf {
    dir.join(format!("{role}.seed"))
}

fn write_seed(path: &Path) -> Result<()> {
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut seed))
        .context("cannot read the operating system's random source")?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot create {} (it must not exist)", path.display()))?;
    file.write_all(hex::encode(seed).as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn read_seed(path: &Path) -> Result<SigningKey> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .permissions()
            .mode();
        ensure!(
            mode & 0o077 == 0,
            "{} must be owner-only (mode 0600)",
            path.display()
        );
    }
    let text = std::fs::read_to_string(path)?;
    let mut seed = [0u8; 32];
    hex::decode_to_slice(text.trim(), &mut seed)
        .with_context(|| format!("{} is not a 32-byte hex seed", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn print_public_keys(dir: &Path) -> Result<()> {
    println!("[package.trusted_keys]");
    for role in ROLES {
        let key = read_seed(&seed_path(dir, role))?;
        println!(
            "{role} = \"{}\"",
            hex::encode(key.verifying_key().to_bytes())
        );
    }
    Ok(())
}

fn identity(text: &str) -> Result<NeuronIdentity> {
    let parts: Vec<_> = text.split(':').collect();
    let [uid, hotkey, block] = parts.as_slice() else {
        bail!("identity `{text}` is not <uid>:<ss58>:<registration block>");
    };
    let hotkey: rtao_primitives::AccountId = hotkey.parse().context("invalid SS58 hotkey")?;
    Ok(NeuronIdentity {
        uid: uid.parse().context("invalid uid")?,
        hotkey: AccountId(hotkey.0),
        registration_block: block.parse().context("invalid registration block")?,
    })
}

fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Keygen { dir } => {
            ensure!(dir.is_dir(), "--dir must be an existing directory");
            for role in ROLES {
                write_seed(&seed_path(&dir, role))?;
            }
            print_public_keys(&dir)
        }
        Command::PublicKeys { dir } => print_public_keys(&dir),
        Command::Build {
            netuid,
            mechanism,
            cohort,
            revision,
            opened_block,
            burn,
            miners,
            keys,
            out,
        } => {
            ensure!(!out.exists(), "--out already exists; choose a new path");
            let params = FixtureParams {
                netuid,
                mechanism,
                cohort,
                cohort_label: format!("testnet-t12-cohort-{cohort}"),
                revision,
                opened_block,
                burn_identity: identity(&burn)?,
                miners: miners
                    .iter()
                    .map(|miner| identity(miner))
                    .collect::<Result<_>>()?,
            };
            let package_key = read_seed(&seed_path(&keys, "package"))?;
            let receipt_key = read_seed(&seed_path(&keys, "receipt"))?;
            let authorization = read_seed(&seed_path(&keys, "authorization"))?;
            let bundle = build(&params, &package_key, &receipt_key)
                .map_err(|error| anyhow::anyhow!("fixture build refused: {error}"))?;
            let trusted = slop_ninja_protocol::TrustedKeys::new(
                package_key.verifying_key().to_bytes(),
                receipt_key.verifying_key().to_bytes(),
                authorization.verifying_key().to_bytes(),
            )
            .map_err(|error| anyhow::anyhow!("{error}"))?;
            let json = bundle.to_json();
            let verified = package::verify(
                &Bundle::parse(json.as_bytes())?,
                &trusted,
                &slop_ninja_protocol::TESTNET_GENESIS.0,
                netuid,
            )?;
            std::fs::write(&out, json)?;
            println!("{}", serde_json::to_string_pretty(&verified.summary())?);
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}
