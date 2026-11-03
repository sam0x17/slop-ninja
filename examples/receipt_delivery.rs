//! Local encrypted-delivery demonstration. Uses synthetic assignments and
//! disposable signing keys, never an installed wallet or a new detector task.

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use schnorrkel::{ExpansionMode, Keypair, MiniSecretKey};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use unslop::{
    public_result,
    receipt_envelope::{
        self as envelope, CommitmentContext, DeliveryContext, Envelope, PreparedPayload,
        RecipientKeyRecord, RecipientRegistration, RecipientSecret,
    },
};
use zeroize::Zeroizing;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare a local assignment fixture and recipient-specific ciphertext.
    Demo {
        #[arg(long)]
        payload: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Decrypt using separately trusted assignment/key-registration files.
    Open {
        #[arg(long)]
        envelope: PathBuf,
        #[arg(long)]
        assignment: PathBuf,
        #[arg(long)]
        registration: PathBuf,
        #[arg(long)]
        secret_key: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Read the existing public report after decryption; no inference POST.
        #[arg(long)]
        fetch_report: bool,
    },
}

fn limited_read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "Input exceeds size limit");
    Ok(bytes)
}

fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn demo_signer() -> Result<Keypair> {
    let mut seed = Zeroizing::new([0; 32]);
    getrandom::fill(seed.as_mut()).map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
    Ok(MiniSecretKey::from_bytes(seed.as_ref())
        .map_err(|_| anyhow::anyhow!("Invalid demo signing seed"))?
        .expand_to_keypair(ExpansionMode::Ed25519))
}

fn main() -> Result<()> {
    let now = chrono::Utc::now().timestamp().try_into()?;
    match Args::parse().command {
        Command::Demo { payload, out } => {
            let bytes = Zeroizing::new(limited_read(&payload, envelope::MAX_PAYLOAD_BYTES)?);
            let miner = demo_signer()?;
            let validator = demo_signer()?;
            let secret = RecipientSecret::generate()?;
            let wrong_secret = RecipientSecret::generate()?;
            let mut task_id = [0; 32];
            getrandom::fill(&mut task_id)
                .map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
            let committed = CommitmentContext {
                network_genesis: [1; 32],
                netuid: 0,
                epoch: 1,
                task_id,
                miner_hotkey: miner.public.to_bytes(),
                beacon_chain: [2; 32],
                beacon_round: 1,
            };
            let opening = PreparedPayload::new(committed.clone(), &bytes)?;
            let registration = RecipientKeyRecord {
                network_genesis: committed.network_genesis,
                netuid: 0,
                epoch: 1,
                validator_hotkey: validator.public.to_bytes(),
                key_id: [3; 32],
                public_key: secret.public_key(),
                not_before: now,
                expires_at: now + 3600,
            }
            .sign(&validator)?;
            let context = DeliveryContext {
                committed,
                commitment: opening.commitment(),
                recipient_hotkey: validator.public.to_bytes(),
                key_id: registration.record.key_id,
                recipient_public_key: secret.public_key(),
                not_before: now,
                expires_at: now + 3600,
            };
            let sealed = envelope::seal(&opening, &registration, context.clone(), &miner, now)?;
            private_dir(&out)?;
            private_dir(&out.join("public"))?;
            private_dir(&out.join("private"))?;
            private_write(
                &out.join("public/envelope.json"),
                &serde_json::to_vec(&sealed)?,
            )?;
            private_write(
                &out.join("public/assignment-fixture.json"),
                &serde_json::to_vec(&context)?,
            )?;
            private_write(
                &out.join("public/recipient-registration.json"),
                &serde_json::to_vec(&registration)?,
            )?;
            private_write(
                &out.join("private/recipient-key.bin"),
                secret.export_private().as_ref(),
            )?;
            private_write(
                &out.join("private/wrong-recipient-key.bin"),
                wrong_secret.export_private().as_ref(),
            )?;
            println!(
                "{}",
                json!({"local_assignment_fixture":true,"encrypted_delivery_written":true,"ciphertext_bytes":sealed.ciphertext.len(),"on_chain_assignment_verified":false})
            );
        }
        Command::Open {
            envelope: envelope_path,
            assignment,
            registration,
            secret_key,
            out,
            fetch_report,
        } => {
            // These files are independently supplied trusted inputs in this
            // local demonstration, not an on-chain registry implementation.
            let expected: DeliveryContext =
                serde_json::from_slice(&limited_read(&assignment, 8192)?)
                    .context("Invalid trusted assignment")?;
            let registration: RecipientRegistration =
                serde_json::from_slice(&limited_read(&registration, 8192)?)
                    .context("Invalid trusted key registration")?;
            let sealed = Envelope::from_json(&limited_read(&envelope_path, 2 * 1024 * 1024)?)?;
            let key_bytes = Zeroizing::new(limited_read(&secret_key, 32)?);
            let secret = RecipientSecret::from_bytes(&key_bytes)?;
            let payload = envelope::open(&sealed, &expected, &registration, &secret, now)?;
            private_dir(&out)?;
            private_write(&out.join("decrypted-payload.json"), payload.as_bytes())?;
            if fetch_report {
                #[derive(Deserialize)]
                struct ReportRequest {
                    report_id: String,
                    candidate_text: String,
                    version: String,
                    not_before: chrono::DateTime<chrono::Utc>,
                    before: chrono::DateTime<chrono::Utc>,
                }
                let request: ReportRequest = serde_json::from_slice(payload.as_bytes())
                    .map_err(|_| anyhow::anyhow!("Invalid private report request"))?;
                let bytes = Zeroizing::new(public_result::fetch(&request.report_id)?);
                private_write(&out.join("provider-response.json"), &bytes)?;
                let observation = public_result::verify(
                    &bytes,
                    &request.report_id,
                    &request.candidate_text,
                    &request.version,
                    request.not_before,
                    request.before,
                )?;
                private_write(
                    &out.join("observation.json"),
                    &serde_json::to_vec(&observation)?,
                )?;
            }
            // URLs, text, individual scores and unsalted hashes stay private.
            println!(
                "{}",
                json!({"decrypted_and_commitment_checked":true,"provider_report_checked":fetch_report,"on_chain_assignment_verified":false,"replay_checked":false})
            );
        }
    }
    Ok(())
}
