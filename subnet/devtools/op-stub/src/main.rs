//! `slop_ninja_op_stub`: dev-only signed operator ops for the local dev
//! session. Every command prints one JSON value.
//!
//! ```text
//! slop_ninja_op_stub pub  --seed-file <file>
//! slop_ninja_op_stub sign --seed-file <file> --target detect|account|edge --env staging|production \
//!                         --op <name> [--args <json object>] [--issued-ms <ms>] [--valid-ms <ms>] [--nonce <32 hex>]
//! ```
//!
//! `pub` prints the verifying key (the Workers' `DETECT_OP_KEY`) and its
//! fingerprint. `sign` prints the op route's body. `--args -` reads the
//! arguments from stdin. The issue time defaults to now and the window to
//! five minutes; the nonce is random unless given (a test of the nonce rule
//! names one twice).

use clap::{Parser, Subcommand};
use serde_json::json;
use slop_ninja_dev_op_stub::{
    DEFAULT_VALID_MS, Spec, args, env, key_from_seed, nonce, public, signed, target,
};
use slop_ninja_signed_op::Nonce;
use std::io::Read;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "slop_ninja_op_stub",
    about = "Dev only: signed operator ops for the local dev session"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// The verifying key and its fingerprint.
    Pub {
        #[arg(long)]
        seed_file: String,
    },
    /// One signed op body.
    Sign {
        #[arg(long)]
        seed_file: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        env: String,
        #[arg(long)]
        op: String,
        /// The arguments, a JSON object (`-`: stdin).
        #[arg(long, default_value = "{}")]
        args: String,
        #[arg(long)]
        issued_ms: Option<u64>,
        #[arg(long, default_value_t = DEFAULT_VALID_MS)]
        valid_ms: u64,
        #[arg(long)]
        nonce: Option<String>,
    },
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 16 bytes from the system's random source.
fn random_nonce() -> Result<Nonce, String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("random: {e}"))?;
    Ok(Nonce(b))
}

fn seed(path: &str) -> Result<slop_ninja_signed_op::SigningKey, String> {
    key_from_seed(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?)
}

fn run(cli: Cli) -> Result<serde_json::Value, String> {
    match cli.cmd {
        Cmd::Pub { seed_file } => {
            let (key, fingerprint) = public(&seed(&seed_file)?);
            Ok(json!({"key": key, "fingerprint": fingerprint}))
        }
        Cmd::Sign {
            seed_file,
            target: t,
            env: e,
            op,
            args: a,
            issued_ms,
            valid_ms,
            nonce: n,
        } => {
            let text = if a == "-" {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| e.to_string())?;
                s
            } else {
                a
            };
            let spec = Spec {
                target: target(&t)?,
                env: env(&e)?,
                op,
                args: args(&text)?,
                issued_ms: issued_ms.unwrap_or_else(now_ms),
                valid_ms,
                nonce: match n {
                    Some(n) => nonce(&n)?,
                    None => random_nonce()?,
                },
            };
            let wire = signed(&seed(&seed_file)?, &spec)?;
            serde_json::to_value(&wire).map_err(|e| e.to_string())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("slop_ninja_op_stub: {e}");
            ExitCode::from(2)
        }
    }
}
