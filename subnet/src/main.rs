//! Offline journal controls. No key loading, RPC connection or submission command.
//!
//! `review-failure` records an operator's review of one finalized failed
//! attempt, so finalized recovery stops halting on it (`recovery_requires_review`).
//! It deletes nothing: the attempt and its outcome stay as evidence.

use std::{path::PathBuf, sync::OnceLock};

use clap::{Parser, Subcommand};
use slop_ninja_subnet::{JournalControls, ReviewRefusal};

/// `slop-ninja-journal --version`: the crate version and the build's commit.
fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| slop_ninja_subnet::build_info::version_text(env!("CARGO_PKG_VERSION")))
}

#[derive(Parser)]
#[command(
    name = "slop-ninja-journal",
    version = version(),
    about = "Inspect, pause or review failures in an existing SlopNinja native journal offline"
)]
struct Args {
    #[arg(long)]
    journal: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print counts and pause state. No payloads, accounts or signed bytes.
    Inspect,
    /// Persist pause. Use the embedded adapter for finalized recovery and resume.
    Pause,
    /// Record a permanent review of one finalized failed attempt (listed by
    /// `inspect` under `review_required`, or under `awaiting_decode` when its
    /// receipt has never been readable), so recovery no longer halts or
    /// waits on it.
    /// Refuses unknown IDs, unresolved attempts and anything that is not a
    /// finalized failure. Look at the failure first: an unrecognized error
    /// can fail every later tempo too.
    ReviewFailure {
        /// Operation ID, as `inspect` prints it.
        #[arg(long)]
        operation: String,
        /// Attempt number within the operation, as `inspect` prints it.
        #[arg(long)]
        attempt: u64,
        /// Accept a failure whose receipt contradicts the journal
        /// (`receipt_contradiction`): the call may have landed, so check the
        /// chain first. Recorded as a reviewed contradiction, not a
        /// reviewed failure. Without it such a failure is refused.
        #[arg(long)]
        accept_contradiction: bool,
    },
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let result = (|| -> anyhow::Result<()> {
        let controls = JournalControls::open_existing(&args.journal)?;
        let output = match &args.command {
            Command::Inspect => serde_json::to_value(controls.inspect()?)?,
            Command::Pause => {
                controls.pause()?;
                serde_json::to_value(controls.inspect()?)?
            }
            // One JSON document: the marker and the summary after it.
            Command::ReviewFailure { operation, attempt, accept_contradiction } => {
                let review = controls.review_failure(operation, *attempt, *accept_contradiction)?;
                serde_json::json!({ "review": review, "summary": controls.inspect()? })
            }
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        Ok(())
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(refusal) = error.downcast_ref::<ReviewRefusal>() {
                // Fixed text naming the refusal; no journal content.
                eprintln!("Review refused; nothing was written. {refusal}");
            } else {
                // Storage and application errors can contain private paths or text.
                eprintln!(
                    "Journal command refused. Check the path, exclusive owner, schema and application identity; retained evidence was not reset."
                );
            }
            std::process::ExitCode::FAILURE
        }
    }
}
