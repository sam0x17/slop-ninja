//! Offline journal controls. No key loading, RPC connection or submission command.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use slop_ninja_subnet::JournalControls;

#[derive(Parser)]
#[command(about = "Inspect or pause an existing Slop Ninja native journal offline")]
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
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let result = (|| -> anyhow::Result<()> {
        let controls = JournalControls::open_existing(&args.journal)?;
        if matches!(args.command, Command::Pause) {
            controls.pause()?;
        }
        println!("{}", serde_json::to_string_pretty(&controls.inspect()?)?);
        Ok(())
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            // Storage and application errors can contain private paths or text.
            eprintln!(
                "Journal command refused. Check the path, exclusive owner, schema and application identity; retained evidence was not reset."
            );
            std::process::ExitCode::FAILURE
        }
    }
}
