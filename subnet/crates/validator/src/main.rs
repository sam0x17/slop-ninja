//! `slop_ninja_validator`: offline configuration checks, journal creation, the
//! signer-free startup sequence, the published-package fetcher and the
//! package-to-row application stage. Only `run --dispatch` loads a private
//! key or submits.

use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use slop_ninja_validator::app::{AppOutcome, Mode, apply};
use slop_ninja_validator::config::{LogConfig, ValidatorConfig};
use slop_ninja_validator::faults::Faults;
use slop_ninja_validator::live::{LivePorts, chain_identity, init_journal};
use slop_ninja_validator::startup::{StartupOutcome, start};

/// Startup reached an explicit resume, or was interrupted; the journal is paused.
const EXIT_OK: u8 = 0;
/// Unexpected or transient failure; systemd restarts the unit.
const EXIT_ERROR: u8 = 1;
/// Invalid configuration or usage; systemd does not restart.
const EXIT_CONFIG: u8 = 2;
/// A policy check halted startup; operator action is required.
const EXIT_HALTED: u8 = 3;
/// Recovery is still in progress; systemd restarts the unit later.
const EXIT_RETRY: u8 = 75;

#[derive(Parser)]
#[command(about = "Slop Ninja native validator")]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate configuration offline and print a redacted summary.
    CheckConfig,
    /// Create the journal once. Refuses an existing path.
    Init,
    /// Open, recover, check commit/reveal, evaluate live metadata and resume;
    /// then verify the package, derive the row and dry-run it (the journal
    /// is not written), or enqueue and dispatch it with `--dispatch`.
    Run {
        /// Sign and submit through the journal with `[dispatch] hotkey_file`.
        /// Without it, the commit payload is built and hashed, never signed.
        #[arg(long)]
        dispatch: bool,
    },
    /// One fetch session from the public status host (`[fetch]`): assembles
    /// the current bundle and verifies it against the held target, then
    /// prints a content-free report. Installs nothing and reads no chain;
    /// `run` decides and installs. Fails fast (75) while a running validator
    /// holds the cache.
    Fetch,
    /// Set aside the held-target record and the installed bundle, so the
    /// next run starts again from the launch burn row: for a chain record
    /// restarted under the same trust roots. Refuses while a run holds the
    /// journal. (A change of trust roots needs no reset: a record of another
    /// authority is set aside automatically.)
    ResetTarget,
    /// Report whether a build at the previous rtao pin can open this
    /// journal, and with `--retire` first retire the work that blocks it and
    /// never signed (`rollback_prepared`). Run with the unit stopped (it
    /// refuses while a run holds the journal), immediately before installing
    /// the older build. Exits 0 when nothing blocks the rollback and 3 when
    /// something still does: work with a signed attempt awaiting recovery or
    /// a landed commit not yet handed back (run this build until it settles),
    /// or checkpoint state from a followed non-native commit-reveal tag (no
    /// rollback past this pin). Reads no chain and loads no key.
    RollbackCheck {
        /// Retire the retirable blocking work before checking.
        #[arg(long)]
        retire: bool,
    },
    /// Journal a signed reward-intent revocation (a file of its signed
    /// bytes, as `reward_v1_verify revoke` writes it): checked under the
    /// configured trust roots and this chain, then kept in
    /// `<journal>.revocations.json`. Safe while the unit runs; it stops new
    /// commitments of the withdrawn target at the next finalized head, and
    /// only a newer authorization of that target resumes them. Reads no
    /// chain and loads no key.
    Revoke {
        /// The signed revocation file.
        file: PathBuf,
    },
    /// Read-only finalized observation: epoch state, this hotkey's
    /// `TimelockedWeightCommits` entries for recent epochs, its `Weights` row,
    /// and `Incentive` at the row's UIDs. Opens no journal or key.
    Observe {
        /// Epochs to read back, ending at the current one.
        #[arg(long, default_value_t = 3)]
        epochs: u64,
        /// Read at this finalized block number instead of the finalized head.
        #[arg(long)]
        at: Option<u64>,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let config = match ValidatorConfig::load(&args.config) {
        Ok(config) => config,
        Err(error) => {
            slop_ninja_validator::logging::init(&LogConfig::default());
            tracing::error!(event = "config_refused", error = %error);
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    slop_ninja_validator::logging::init(&config.log);
    let code = match args.command {
        Command::CheckConfig => report(check_config(&config)),
        Command::Init => {
            report(chain_identity(&config).and_then(|identity| init_journal(&config, identity)))
                .inspect(|_| tracing::info!(event = "journal_created"))
        }
        Command::Fetch => fetch_command(config),
        Command::ResetTarget => report(slop_ninja_validator::live::reset_target(&config).and_then(|done| {
            println!("{}", serde_json::to_string_pretty(&done)?);
            tracing::info!(event = "target_reset");
            Ok(())
        })),
        Command::Revoke { file } => report(slop_ninja_validator::live::record_revocation(&config, &file).and_then(|done| {
            println!("{}", serde_json::to_string_pretty(&done)?);
            tracing::info!(event = "revocation_recorded", new = done["new"].as_bool().unwrap_or(false));
            Ok(())
        })),
        Command::RollbackCheck { retire } => match slop_ninja_validator::live::rollback_check(&config, retire) {
            Ok((report, safe)) => {
                println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
                tracing::info!(event = "rollback_check", safe);
                Some(if safe { EXIT_OK } else { EXIT_HALTED })
            }
            Err(error) => report(Err(error)),
        },
        Command::Observe { epochs, at } => {
            if !(1..=32).contains(&epochs) {
                tracing::error!(event = "config_refused", error = "--epochs must be 1..=32");
                return ExitCode::from(EXIT_CONFIG);
            }
            observe_command(&config, epochs, at)
        }
        Command::Run { dispatch } => {
            if dispatch && (config.dispatch.is_none() || config.package.is_none()) {
                tracing::error!(
                    event = "config_refused",
                    error = "--dispatch requires [package] and [dispatch] hotkey_file"
                );
                return ExitCode::from(EXIT_CONFIG);
            }
            run(
                config,
                if dispatch {
                    Mode::Dispatch
                } else {
                    Mode::DryRun
                },
            )
        }
    };
    ExitCode::from(code.unwrap_or(EXIT_ERROR))
}

/// Log a command failure with its full context chain (the inner messages
/// name checks and codes, never key material or signed bytes).
fn report(result: Result<()>) -> Option<u8> {
    match result {
        Ok(()) => Some(EXIT_OK),
        Err(error) => {
            tracing::error!(event = "command_failed", error = %error, chain = %format!("{error:#}"));
            None
        }
    }
}

fn observe_command(config: &ValidatorConfig, epochs: u64, at: Option<u64>) -> Option<u8> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return report(Err(error.into())),
    };
    report(runtime.block_on(async {
        let observed = slop_ninja_validator::live::observe(config, epochs, at).await?;
        println!("{}", serde_json::to_string_pretty(&observed)?);
        Ok(())
    }))
}

/// Exit 0 with a verified bundle, 75 when the records are not (yet) complete
/// on the host or the cache is busy, 3 when the host served records that
/// failed a check, 1 on an error.
fn fetch_command(config: ValidatorConfig) -> Option<u8> {
    if config.fetch.is_none() {
        tracing::error!(
            event = "config_refused",
            error = "fetch requires [fetch] and [package]"
        );
        return Some(EXIT_CONFIG);
    }
    let identity = match chain_identity(&config) {
        Ok(identity) => identity,
        Err(error) => {
            report(Err(error));
            return Some(EXIT_CONFIG);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return report(Err(error.into())),
    };
    let mut ports = LivePorts::new(config, identity).with_dry_run();
    match runtime.block_on(ports.fetch_only()) {
        Ok((fetched, staged)) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&fetched).unwrap_or_default()
            );
            Some(if staged.is_some() {
                EXIT_OK
            } else if fetched.retryable {
                EXIT_RETRY
            } else {
                EXIT_HALTED
            })
        }
        Err(error) if format!("{error}").starts_with("cache_busy") => {
            tracing::warn!(event = "fetch_cache_busy", hint = "a running validator holds the cache; its own fetches continue");
            Some(EXIT_RETRY)
        }
        Err(error) => report(Err(error)),
    }
}

fn check_config(config: &ValidatorConfig) -> Result<()> {
    chain_identity(config)?;
    let summary = serde_json::json!({
        "config": config.summary(),
        "journal_exists": config.journal_path.is_file(),
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn run(config: ValidatorConfig, mode: Mode) -> Option<u8> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return report(Err(error.into())),
    };
    runtime.block_on(run_async(config, mode))
}

/// The startup result, or the application result after a resumed startup.
enum Finished {
    Startup(StartupOutcome),
    App(Box<AppOutcome>),
    /// No bundle is installed and the fetcher found none to install.
    NoBundle,
}

async fn run_async(config: ValidatorConfig, mode: Mode) -> Option<u8> {
    let summary = serde_json::to_string(&config.summary()).unwrap_or_default();
    tracing::info!(event = "startup", config = %summary);
    if !config.journal_path.is_file() {
        tracing::error!(
            event = "journal_missing",
            hint = "create it once with `init`; run never creates a journal"
        );
        return Some(EXIT_CONFIG);
    }
    let identity = match chain_identity(&config) {
        Ok(identity) => identity,
        Err(error) => {
            report(Err(error));
            return Some(EXIT_CONFIG);
        }
    };
    let faults = match Faults::from_env(config.network) {
        Ok(faults) => faults,
        Err(error) => {
            report(Err(error));
            return Some(EXIT_CONFIG);
        }
    };
    let rounds = config.recovery.max_rounds;
    let has_package = config.package.is_some();
    let package_path = config.package.as_ref().map(|package| package.path.clone());
    let mut ports = LivePorts::new(config, identity);
    if mode == Mode::Dispatch {
        ports = ports.with_head_tracking();
    } else {
        ports = ports.with_dry_run();
    }
    // A continuous dispatch run never exits for routine work: it continues
    // on an installed successor in this process, and waits out a deferred
    // tempo for the next epoch (or a fetched newer authorization) instead of exiting, so
    // neither a supersession nor a burn-only row costs a restart.
    let continuous = mode == Mode::Dispatch && ports.continuous();

    let result = tokio::select! {
        result = async {
            let outcome = start(&mut ports, rounds, &faults).await?;
            if !matches!(outcome, StartupOutcome::Resumed { .. }) || !has_package {
                return Ok(Finished::Startup(outcome));
            }
            // An install a crash interrupted after staging its bundle is
            // completed (or discarded) before anything is verified.
            ports.complete_install()?;
            let mut head = ports
                .finalized()
                .ok_or_else(|| anyhow::anyhow!("startup recorded no finalized head"))?;
            let mut cycle = 0u64;
            loop {
                if cycle > 0 {
                    head = ports.refresh_head().await?;
                }
                cycle += 1;
                if ports.fetches() {
                    // A failed or timed-out session leaves the installed bundle.
                    match ports.startup_fetch(mode == Mode::Dispatch).await {
                        Ok(fetched) => tracing::info!(
                            event = "fetch_startup",
                            report = %serde_json::to_string(&fetched).unwrap_or_default()
                        ),
                        Err(error) => tracing::warn!(event = "fetch_failed", error = %format!("{error:#}")),
                    }
                }
                if package_path.as_ref().is_some_and(|path| !path.exists()) && !ports.has_staged_bundle() {
                    if continuous {
                        tracing::warn!(event = "bundle_unavailable", hint = "no bundle is installed yet; waiting for the fetcher");
                        ports.wait_after_deferral().await?;
                        continue;
                    }
                    return Ok(Finished::NoBundle);
                }
                let outcome = apply(&mut ports, &head, mode, &faults).await?;
                if continuous {
                    match &outcome {
                        AppOutcome::Enqueued { dispatch, .. } if dispatch.stop == "superseded" => {
                            tracing::info!(event = "continuing_on_successor");
                            continue;
                        }
                        AppOutcome::Deferred { code, .. } if *code != "pending_operation_unsettled" => {
                            tracing::info!(event = "tempo_waiting", code = *code);
                            ports.wait_after_deferral().await?;
                            continue;
                        }
                        AppOutcome::Enqueued { dispatch, .. }
                            if dispatch.stop.starts_with("tempo_deferred:authorization_refused")
                                || dispatch.stop.starts_with("tempo_deferred:authorization_revoked") =>
                        {
                            tracing::info!(event = "tempo_waiting", code = dispatch.stop.trim_start_matches("tempo_deferred:"));
                            ports.wait_after_deferral().await?;
                            continue;
                        }
                        _ => {}
                    }
                }
                return Ok::<_, anyhow::Error>(Finished::App(Box::new(outcome)));
            }
        } => Some(result),
        () = shutdown_signal() => None,
    };
    // A resumed journal is paused again before exit, so the next start
    // reconciles finalized history first. Retained work survives the pause.
    if let Err(error) = ports.pause() {
        tracing::error!(event = "pause_failed", error = %error);
    }
    ports.close().await;
    match result {
        None => {
            tracing::info!(event = "interrupted", paused = true);
            Some(EXIT_OK)
        }
        Some(Ok(Finished::Startup(outcome))) => {
            let text = serde_json::to_string(&outcome).unwrap_or_default();
            tracing::info!(event = "startup_complete", outcome = %text, paused = true);
            Some(match outcome {
                StartupOutcome::Resumed { .. } => EXIT_OK,
                StartupOutcome::Halted { .. } => EXIT_HALTED,
                StartupOutcome::Retry { .. } => EXIT_RETRY,
            })
        }
        Some(Ok(Finished::NoBundle)) => {
            tracing::warn!(
                event = "bundle_unavailable",
                hint = "no bundle is installed yet; the fetcher retries at the next start"
            );
            Some(EXIT_RETRY)
        }
        Some(Ok(Finished::App(outcome))) => {
            let text = serde_json::to_string_pretty(&outcome).unwrap_or_default();
            println!("{text}");
            tracing::info!(event = "run_complete", paused = true);
            Some(match outcome.as_ref() {
                AppOutcome::Refused { .. } => EXIT_HALTED,
                AppOutcome::Deferred { .. } => EXIT_RETRY,
                AppOutcome::Enqueued {
                    dispatch: report, ..
                } if report.stop.starts_with("tempo_deferred:") => EXIT_RETRY,
                AppOutcome::Enqueued {
                    dispatch: report, ..
                } if report.stop == "recovery_requires_review"
                    || report.stop == "duplicate_commit"
                    || report.chain_weights_match == Some(false) =>
                {
                    EXIT_HALTED
                }
                AppOutcome::Enqueued { .. } | AppOutcome::DryRun { .. } => EXIT_OK,
            })
        }
        Some(Err(error)) => report(Err(error)),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
