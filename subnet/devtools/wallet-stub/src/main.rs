//! `slop_ninja_wallet_stub`: dev-only sr25519 signatures over named test
//! seeds for wallet sign-in tests. Every command prints one JSON value.
//!
//! ```text
//! slop_ninja_wallet_stub address  --seed alice [--prefix 42]
//! slop_ninja_wallet_stub sign     --seed alice --variant raw|wrapped|wrong-key|wrong-message <message>
//! slop_ninja_wallet_stub variants --seed alice <message>
//! slop_ninja_wallet_stub verify   --ss58 <address> --signature <hex> <message>
//! ```
//!
//! `<message>` is `--message <text>`, `--message-hex <hex>` or
//! `--message-file <path>` (`-` reads stdin); the bytes are used exactly,
//! with no trailing newline added or removed.

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use slop_ninja_dev_wallet_stub::{PREFIX_GENERIC, Signed, Variant, hex0x, keypair, seed, sign, ss58, unhex, verify};
use std::io::Read;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "slop_ninja_wallet_stub", about = "Dev only: sr25519 test signatures for wallet sign-in")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct Message {
    /// The message as UTF-8 text.
    #[arg(long)]
    message: Option<String>,
    /// The message as hex (with or without 0x).
    #[arg(long)]
    message_hex: Option<String>,
    /// A file holding the message bytes; `-` reads stdin.
    #[arg(long)]
    message_file: Option<String>,
}

impl Message {
    fn bytes(&self) -> Result<Vec<u8>, String> {
        if let Some(m) = &self.message {
            return Ok(m.as_bytes().to_vec());
        }
        if let Some(h) = &self.message_hex {
            return unhex(h).ok_or_else(|| "--message-hex is not hex".to_owned());
        }
        match self.message_file.as_deref() {
            Some("-") => {
                let mut b = Vec::new();
                std::io::stdin().read_to_end(&mut b).map_err(|e| e.to_string())?;
                Ok(b)
            }
            Some(p) => std::fs::read(p).map_err(|e| format!("{p}: {e}")),
            None => Err("no message".to_owned()),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// The SS58 address and public key of a test seed.
    Address {
        #[arg(long)]
        seed: String,
        #[arg(long, default_value_t = PREFIX_GENERIC)]
        prefix: u16,
    },
    /// One signature variant.
    Sign {
        #[arg(long)]
        seed: String,
        #[arg(long, value_enum)]
        variant: Variant,
        #[command(flatten)]
        message: Message,
    },
    /// All four variants.
    Variants {
        #[arg(long)]
        seed: String,
        #[command(flatten)]
        message: Message,
    },
    /// Whether a signature verifies over the raw bytes or the `<Bytes>` wrapping.
    Verify {
        #[arg(long)]
        ss58: String,
        #[arg(long)]
        signature: String,
        #[command(flatten)]
        message: Message,
    },
}

fn signed_json(s: &Signed) -> Value {
    json!({"variant": s.variant.name(), "ss58": s.ss58, "public_key": hex0x(&s.public_key),
        "signed_hex": hex0x(&s.signed), "signature": hex0x(&s.signature)})
}

fn run(cli: Cli) -> Result<Value, String> {
    Ok(match cli.cmd {
        Cmd::Address { seed: name, prefix } => {
            let pk = keypair(&seed(&name)).public.to_bytes();
            json!({"seed": name, "ss58": ss58(&pk, prefix), "prefix": prefix, "public_key": hex0x(&pk)})
        }
        Cmd::Sign { seed: name, variant, message } => signed_json(&sign(&name, &message.bytes()?, variant)),
        Cmd::Variants { seed: name, message } => {
            let m = message.bytes()?;
            Value::Array(Variant::ALL.iter().map(|v| signed_json(&sign(&name, &m, *v))).collect())
        }
        Cmd::Verify { ss58: addr, signature, message } => {
            let sig = unhex(&signature).ok_or("--signature is not hex")?;
            let (raw, wrapped) = verify(&addr, &message.bytes()?, &sig).ok_or("not an sr25519 address and 64-byte signature")?;
            json!({"raw": raw, "wrapped": wrapped, "accepted": raw || wrapped})
        }
    })
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("slop_ninja_wallet_stub: {e}");
            ExitCode::from(2)
        }
    }
}
