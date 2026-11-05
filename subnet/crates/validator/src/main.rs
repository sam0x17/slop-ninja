//! `slop_ninja_validator`: offline configuration checks, journal creation, the
//! signer-free startup sequence, the published-row fetcher and the
//! package-to-row application stage. Only `run --dispatch` loads a private
//! key or submits.

use std::{path::PathBuf, process::ExitCode, sync::OnceLock};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
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

/// `slop_ninja_validator --version`: this crate's version and the build's
/// commit (recorded by the subnet crate's build script; same repository).
fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| slop_ninja_subnet::build_info::version_text(env!("CARGO_PKG_VERSION")))
}

#[derive(Parser)]
#[command(
    name = "slop_ninja_validator",
    version = version(),
    about = "Slop Ninja native validator"
)]
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
    /// a landed commit not yet handed back, or a pending reveal whose reveal
    /// period changed (run this build until it settles); a retained finalized
    /// failure or a compacted journal (no rollback past this pin). Reads no
    /// chain and loads no key.
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
    /// Print the systemd drop-in that runs a persistent unit inside a
    /// bubblewrap sandbox showing the validator only its binary, this
    /// configuration, the hotkey files it names and its state directories
    /// (`deploy/install.sh` writes it). With `--check`, instead run
    /// `check-config` and a visibility test inside that sandbox and print a
    /// content-free report. Reads no chain and no private key.
    SandboxUnit {
        /// The unit the drop-in is for.
        #[arg(long, value_enum)]
        unit: SandboxUnit,
        /// The installed validator binary the unit starts.
        #[arg(long)]
        binary: PathBuf,
        /// The bubblewrap executable.
        #[arg(long, default_value = "/usr/bin/bwrap")]
        bwrap: PathBuf,
        /// Test the sandbox instead of printing the drop-in.
        #[arg(long)]
        check: bool,
        /// Testnet drills only: pass `SLOP_NINJA_DRILL_FAULT=<phase>[:<n>]`
        /// into the sandbox (the units clear the environment, so a unit's
        /// `Environment=` never reaches the validator). Install the result
        /// as a drill-only drop-in and remove it after the drill.
        #[arg(long, value_name = "PHASE[:N]")]
        drill_fault: Option<String>,
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

#[derive(Clone, Copy, ValueEnum)]
enum SandboxUnit {
    /// `slop_ninja_validator-dispatch@`: `run --dispatch`.
    Dispatch,
    /// `slop_ninja_validator@`: the dry run.
    DryRun,
}

fn main() -> ExitCode {
    // As PID 1 of a sandbox a signal with no handler is discarded, not
    // deferred, so a SIGTERM that arrived before the handlers exist (while
    // the arguments and configuration load, the sandbox check runs or the
    // journal opens) would be lost and the stop would end in SIGKILL. A
    // blocked signal stays pending instead: block both stop signals before
    // anything else, register the run's handlers, then unblock, which
    // delivers a pending one to them.
    let blocked = StopSignals::block();
    let args = Args::parse();
    let mut early = match args.command {
        Command::Run { .. } => match Stopping::register() {
            Ok(stopping) => Some(stopping),
            Err(error) => {
                drop(blocked);
                slop_ninja_validator::logging::init(&LogConfig::default());
                tracing::error!(event = "command_failed", error = %error);
                return ExitCode::from(EXIT_ERROR);
            }
        },
        _ => None,
    };
    drop(blocked);
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
        Command::CheckConfig => {
            if let Err(error) = sandbox_guard(&config, &args.config, None) {
                tracing::error!(event = "sandbox_refused", error = %error);
                return ExitCode::from(EXIT_CONFIG);
            }
            report(check_config(&config))
        }
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
        Command::SandboxUnit {
            unit,
            binary,
            bwrap,
            check,
            drill_fault,
        } => report(sandbox_unit(
            &config,
            &args.config,
            unit,
            &binary,
            &bwrap,
            check,
            drill_fault,
        )),
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
            let (mode, kind) = if dispatch {
                (Mode::Dispatch, slop_ninja_validator::sandbox::UnitKind::Dispatch)
            } else {
                (Mode::DryRun, slop_ninja_validator::sandbox::UnitKind::DryRun)
            };
            // Before anything opens the journal: a sandbox rendered for other
            // paths, or a journal directory shared with another journal
            // while PID 1, needs the operator.
            if let Err(error) = sandbox_guard(&config, &args.config, Some(kind)) {
                tracing::error!(event = "sandbox_refused", error = %error);
                return ExitCode::from(EXIT_CONFIG);
            }
            match early.take() {
                Some(stopping) => run(config, mode, stopping),
                None => Some(EXIT_ERROR),
            }
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

/// [`slop_ninja_validator::sandbox::guard`] for this process: PID 1 of its
/// namespace, `SLOP_NINJA_SANDBOX_BINDS`, started as `argv[0]`.
fn sandbox_guard(
    config: &ValidatorConfig,
    config_path: &std::path::Path,
    unit: Option<slop_ninja_validator::sandbox::UnitKind>,
) -> Result<()> {
    use slop_ninja_validator::sandbox::{self, Guarded};
    let marker = std::env::var(sandbox::SANDBOX_ENV).ok();
    let pid_one = std::process::id() == 1;
    if marker.is_none() && !pid_one {
        return Ok(());
    }
    let binary = PathBuf::from(
        std::env::args_os()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no argv[0]"))?,
    );
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    match sandbox::guard(
        config,
        config_path,
        &binary,
        &home,
        unit,
        marker.as_deref(),
        pid_one,
    )? {
        Guarded::Unverified => tracing::warn!(
            event = "sandbox_unverified",
            hint = "PID 1 without SLOP_NINJA_SANDBOX_BINDS: re-run deploy/install.sh to render a drop-in this build checks"
        ),
        Guarded::Matches | Guarded::NotSandboxed => {}
    }
    if pid_one {
        // Another journal whose lock no process holds (a retired one): no
        // collision is possible now, but the directory should be this
        // instance's alone.
        match sandbox::foreign_journal_locks(&config.journal_path) {
            Ok((_, unheld)) if !unheld.is_empty() => tracing::warn!(
                event = "journal_dir_shared",
                locks = %unheld.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "),
                hint = "another journal shares this journal's directory; move it out and re-run deploy/install.sh"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(event = "journal_dir_unlisted", error = %error),
        }
    }
    Ok(())
}

fn sandbox_unit(
    config: &ValidatorConfig,
    config_path: &std::path::Path,
    unit: SandboxUnit,
    binary: &std::path::Path,
    bwrap: &std::path::Path,
    check: bool,
    drill_fault: Option<String>,
) -> Result<()> {
    use slop_ninja_validator::sandbox::{self, Layout, UnitKind};
    let kind = match unit {
        SandboxUnit::Dispatch => UnitKind::Dispatch,
        SandboxUnit::DryRun => UnitKind::DryRun,
    };
    let home = PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?,
    );
    anyhow::ensure!(home.is_absolute(), "HOME must be absolute");
    let layout = Layout::host()?;
    let binds = sandbox::binds(config, config_path, binary, kind, &home)?;
    let strays = sandbox::bind_strays(config)?;
    anyhow::ensure!(
        strays.is_empty(),
        "a directory the sandbox binds read-write as a whole (the journal's, the package's or \
         a cache) also holds entries that are not this instance's journal, package or caches: \
         {}. Give each instance a state directory of its own \
         (~/.local/state/slop_ninja_validator/<instance>/) and move other journals, packages, \
         key files and other tools' files out",
        strays
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(value) = &drill_fault {
        let phase = value.split_once(':').map_or(value.as_str(), |(phase, _)| phase);
        slop_ninja_validator::faults::Phase::parse(phase)?;
    }
    let env: Vec<(&str, String)> = drill_fault
        .map(|value| (slop_ninja_validator::faults::DRILL_FAULT_ENV, value))
        .into_iter()
        .collect();
    if check {
        anyhow::ensure!(
            sandbox::available(bwrap),
            "bubblewrap cannot create a sandbox on this host (user namespaces refused to it)"
        );
        let report = sandbox::check(
            bwrap,
            &layout,
            &binds,
            binary,
            config_path,
            &home,
            kind,
            config.journal_path.is_file(),
        )?;
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!(
            "{}",
            sandbox::render_drop_in(bwrap, &layout, &binds, binary, config_path, kind, &env)?
        );
    }
    Ok(())
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

/// SIGTERM and SIGINT blocked for this (still single-threaded) process
/// until dropped, which restores the previous mask.
struct StopSignals {
    #[cfg(unix)]
    previous: Option<libc::sigset_t>,
}

impl StopSignals {
    fn block() -> Self {
        #[cfg(unix)]
        {
            // SAFETY: sigemptyset/sigaddset initialise a local set, and
            // pthread_sigmask only changes this thread's mask (no other
            // thread exists yet); every pointer is to a live local.
            let previous = unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                let mut previous: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGTERM);
                libc::sigaddset(&mut set, libc::SIGINT);
                (libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous) == 0).then_some(previous)
            };
            Self { previous }
        }
        #[cfg(not(unix))]
        Self {}
    }
}

impl Drop for StopSignals {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(previous) = self.previous.take() {
            // SAFETY: restores the mask saved by `block` on this thread.
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
            }
        }
    }
}

/// The run's runtime with its stop signals registered on it.
struct Stopping {
    runtime: tokio::runtime::Runtime,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
}

impl Stopping {
    fn register() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        #[cfg(unix)]
        let (terminate, interrupt) = {
            use tokio::signal::unix::{SignalKind, signal};
            let _entered = runtime.enter();
            (signal(SignalKind::terminate())?, signal(SignalKind::interrupt())?)
        };
        Ok(Self {
            runtime,
            #[cfg(unix)]
            terminate,
            #[cfg(unix)]
            interrupt,
        })
    }
}

fn run(config: ValidatorConfig, mode: Mode, stopping: Stopping) -> Option<u8> {
    let Stopping {
        runtime,
        #[cfg(unix)]
        mut terminate,
        #[cfg(unix)]
        mut interrupt,
    } = stopping;
    runtime.block_on(async move {
        #[cfg(unix)]
        let stop = async move {
            tokio::select! {
                biased;
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        };
        #[cfg(not(unix))]
        let stop = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        run_async(config, mode, stop).await
    })
}

/// The startup result, or the application result after a resumed startup.
enum Finished {
    Startup(StartupOutcome),
    App(Box<AppOutcome>),
    /// No bundle is installed and the fetcher found none to install.
    NoBundle,
}

async fn run_async(
    config: ValidatorConfig,
    mode: Mode,
    stop: impl std::future::Future<Output = ()>,
) -> Option<u8> {
    // Polled first at every wakeup, so a stop that arrived during a
    // synchronous step (the journal's open) wins at the next await.
    let mut stop = std::pin::pin!(stop);
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
        biased;
        () = &mut stop => None,
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
                        // A successor that waits for work in flight over a
                        // retired installed bundle: no dispatch loop runs to
                        // settle that work, so the run exits for a restart,
                        // whose finalized recovery settles it (the same as
                        // `pending_operation_unsettled`). A bundle of the held
                        // target itself never waits (`decide_holding`).
                        AppOutcome::Deferred { code, .. }
                            if *code == "bundle_schema_retired" && ports.startup_waiting() =>
                        {
                            tracing::warn!(event = "restart_for_recovery", code = *code);
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
                } if slop_ninja_validator::app::stop_retries(&report.stop) => EXIT_RETRY,
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
