//! Offline tests. No client, signer, RPC endpoint or chain is contacted.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Context, Result};
use rtao_primitives::{AccountId, H256, NetUid};
use slop_ninja_subnet::{ChainIdentity, JournalControls};

use crate::config::{NetworkName, ValidatorConfig};
use crate::faults::{Faults, InjectedFault, Phase};
use crate::live::{LivePorts, init_journal, public_hotkey};
use crate::startup::{
    FinalizedHead, RecoveryCounts, Stage, StartupOutcome, StartupPorts, SubnetState, SurfaceStatus,
    start,
};

const EXAMPLE: &str = include_str!("../validator.example.toml");

fn test_genesis() -> H256 {
    NetworkName::Test.genesis().unwrap()
}

fn finney_genesis() -> H256 {
    NetworkName::Finney.genesis().unwrap()
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn ready() -> RecoveryCounts {
    RecoveryCounts {
        ready_to_resume: true,
        reveal_history_caught_up: true,
        ..RecoveryCounts::default()
    }
}

/// Records the order of effects and keeps a simulated persisted pause flag.
struct Fake {
    events: Vec<&'static str>,
    journal_genesis: H256,
    chain_genesis: H256,
    recoveries: VecDeque<RecoveryCounts>,
    subnet: SubnetState,
    /// Live-surface evaluation result, or `None` to simulate an unreadable surface.
    surface: Option<SurfaceStatus>,
    connect_fails: bool,
    /// Simulate rtao's finalized recovery returning an error.
    recover_fails: bool,
    persisted_paused: bool,
    /// Journal generation, as the adapter's resume gate sees it.
    generation: u64,
    /// Generation recorded by the last recovery that made resume possible.
    recovered_generation: Option<u64>,
    /// Whether evaluating the surface writes readiness (a first evaluation or
    /// a readiness change does).
    surface_writes: bool,
}

impl Fake {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            journal_genesis: test_genesis(),
            chain_genesis: test_genesis(),
            recoveries: VecDeque::from([ready()]),
            subnet: SubnetState {
                subnet_exists: true,
                hotkey_registered: true,
                commit_reveal_enabled: true,
            },
            surface: Some(SurfaceStatus {
                observed_spec_version: Some(486),
                ..SurfaceStatus::default()
            }),
            connect_fails: false,
            recover_fails: false,
            persisted_paused: false,
            generation: 1,
            recovered_generation: None,
            surface_writes: true,
        }
    }
}

impl StartupPorts for Fake {
    fn open_journal(&mut self) -> Result<H256> {
        self.events.push("open");
        self.persisted_paused = true;
        Ok(self.journal_genesis)
    }
    async fn connect(&mut self) -> Result<H256> {
        self.events.push("connect");
        anyhow::ensure!(!self.connect_fails, "synthetic transport failure");
        Ok(self.chain_genesis)
    }
    async fn finalized_head(&mut self) -> Result<FinalizedHead> {
        self.events.push("head");
        Ok(FinalizedHead {
            number: 100,
            hash: H256([9; 32]),
        })
    }
    async fn recover_finalized(&mut self, _: &FinalizedHead) -> Result<RecoveryCounts> {
        self.events.push("recover");
        anyhow::ensure!(!self.recover_fails, "synthetic typed storage refusal");
        let counts = self.recoveries.pop_front().unwrap_or_default();
        self.recovered_generation = counts.ready_to_resume.then_some(self.generation);
        Ok(counts)
    }
    async fn subnet_state(&mut self, _: &FinalizedHead) -> Result<SubnetState> {
        self.events.push("commit_reveal");
        Ok(self.subnet)
    }
    async fn runtime_surface(&mut self, _: &FinalizedHead) -> Result<SurfaceStatus> {
        self.events.push("runtime");
        let surface = self
            .surface
            .clone()
            .context("synthetic live metadata read failure")?;
        if self.surface_writes {
            self.generation += 1;
        }
        Ok(surface)
    }
    fn resume(&mut self) -> Result<()> {
        self.events.push("resume");
        anyhow::ensure!(self.persisted_paused, "resume without an open journal");
        // The adapter refuses resume after any journal write since recovery.
        anyhow::ensure!(
            self.recovered_generation == Some(self.generation),
            "journal changed since recovery; reconcile again before resume"
        );
        self.persisted_paused = false;
        Ok(())
    }
}

const FULL_ORDER: [&str; 7] = [
    "open",
    "connect",
    "head",
    "runtime",
    "recover",
    "commit_reveal",
    "resume",
];

#[test]
fn startup_order_is_open_surface_recover_commit_reveal_then_resume() -> Result<()> {
    let mut fake = Fake::new();
    let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
    assert_eq!(
        outcome,
        StartupOutcome::Resumed {
            finalized_height: 100,
            deferred_operations: Vec::new(),
        }
    );
    assert_eq!(fake.events, FULL_ORDER);
    Ok(())
}

/// Found by the first live dry run on testnet 601 (2026-10-30): readiness was
/// recorded after recovery, so the first evaluation's journal write made the
/// adapter refuse resume ("journal changed since recovery"). Any later
/// readiness change, such as a runtime upgrade, would refuse it again.
#[test]
fn a_readiness_write_never_refuses_resume() -> Result<()> {
    for surface_writes in [true, false] {
        let mut fake = Fake::new();
        fake.surface_writes = surface_writes;
        let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
        assert!(matches!(outcome, StartupOutcome::Resumed { .. }));
        assert_eq!(fake.recovered_generation, Some(fake.generation));
    }
    // A readiness write after recovery is what the gate refuses.
    let mut fake = Fake::new();
    block_on(fake.recover_finalized(&FinalizedHead {
        number: 100,
        hash: H256([9; 32]),
    }))?;
    block_on(fake.runtime_surface(&FinalizedHead {
        number: 100,
        hash: H256([9; 32]),
    }))?;
    fake.persisted_paused = true;
    assert!(fake.resume().is_err());
    Ok(())
}

#[test]
fn no_runtime_spec_ever_halts_startup() -> Result<()> {
    for spec_version in [1, 481, 482, 486, 487, 10_000, u32::MAX] {
        let mut fake = Fake::new();
        fake.surface = Some(SurfaceStatus {
            observed_spec_version: Some(spec_version),
            ..SurfaceStatus::default()
        });
        let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
        assert!(
            matches!(outcome, StartupOutcome::Resumed { .. }),
            "{spec_version}"
        );
        assert_eq!(fake.events, FULL_ORDER);
    }
    Ok(())
}

#[test]
fn deferred_operations_are_reported_and_startup_continues() -> Result<()> {
    let deferred = vec![(
        "SubtensorModule.commit_timelocked_mechanism_weights".to_string(),
        "call_absent".to_string(),
    )];
    let mut fake = Fake::new();
    fake.surface = Some(SurfaceStatus {
        deferred: deferred.clone(),
        adapted: Vec::new(),
        observed_spec_version: Some(487),
    });
    let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
    assert_eq!(
        outcome,
        StartupOutcome::Resumed {
            finalized_height: 100,
            deferred_operations: deferred,
        }
    );
    // An unreadable live surface defers nothing and stops nothing either; the
    // signing boundary re-resolves against live metadata at every attempt.
    let mut fake = Fake::new();
    fake.surface = None;
    let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
    assert!(matches!(outcome, StartupOutcome::Resumed { .. }));
    assert_eq!(fake.events, FULL_ORDER);
    Ok(())
}

#[test]
fn an_upstream_storage_refusal_during_recovery_is_recorded_and_retried() -> Result<()> {
    let mut fake = Fake::new();
    fake.recover_fails = true;
    fake.surface = Some(SurfaceStatus {
        deferred: vec![
            (
                "rtao.finalized_observation".into(),
                crate::startup::UPSTREAM_STORAGE_REFUSED.into(),
            ),
            (
                "SubtensorModule.commit_timelocked_mechanism_weights".into(),
                crate::startup::UPSTREAM_STORAGE_REFUSED.into(),
            ),
        ],
        adapted: Vec::new(),
        observed_spec_version: Some(487),
    });
    let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
    assert_eq!(
        outcome,
        StartupOutcome::Retry {
            stage: Stage::Recovery,
            code: crate::startup::UPSTREAM_STORAGE_REFUSED,
        }
    );
    // Readiness was evaluated (and so persisted) before recovery; the
    // journal stays paused and nothing resumed.
    assert_eq!(
        fake.events,
        ["open", "connect", "head", "runtime", "recover"]
    );
    assert!(fake.persisted_paused);
    // Any other recovery error stays an error, after recording readiness.
    let mut fake = Fake::new();
    fake.recover_fails = true;
    let error = block_on(start(&mut fake, 4, &Faults::none())).unwrap_err();
    assert_eq!(error.to_string(), "finalized recovery failed");
    assert_eq!(
        fake.events,
        ["open", "connect", "head", "runtime", "recover"]
    );
    // So is a recovery error with an unreadable surface.
    let mut fake = Fake::new();
    fake.recover_fails = true;
    fake.surface = None;
    let error = block_on(start(&mut fake, 4, &Faults::none())).unwrap_err();
    assert_eq!(error.to_string(), "finalized recovery failed");
    Ok(())
}

#[test]
fn subnet_checks_halt_before_resume() -> Result<()> {
    let present = SubnetState {
        subnet_exists: true,
        hotkey_registered: true,
        commit_reveal_enabled: true,
    };
    // `CommitRevealWeightsEnabled` reads `true` for an absent netuid, so an
    // enabled flag alone never passes.
    for (subnet, code) in [
        (
            SubnetState {
                subnet_exists: false,
                hotkey_registered: false,
                ..present
            },
            "subnet_not_registered",
        ),
        (
            SubnetState {
                subnet_exists: false,
                ..present
            },
            "subnet_not_registered",
        ),
        (
            SubnetState {
                hotkey_registered: false,
                ..present
            },
            "hotkey_not_registered",
        ),
        (
            SubnetState {
                commit_reveal_enabled: false,
                ..present
            },
            "commit_reveal_disabled",
        ),
    ] {
        let mut fake = Fake::new();
        fake.subnet = subnet;
        let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
        assert_eq!(
            outcome,
            StartupOutcome::Halted {
                stage: Stage::CommitReveal,
                code
            }
        );
        assert_eq!(fake.events, FULL_ORDER[..6]);
        assert!(fake.persisted_paused);
    }
    Ok(())
}

#[test]
fn chain_identity_mismatch_halts_before_recovery() -> Result<()> {
    let mut fake = Fake::new();
    fake.chain_genesis = finney_genesis();
    let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
    assert_eq!(
        outcome,
        StartupOutcome::Halted {
            stage: Stage::Connect,
            code: "chain_identity_mismatch"
        }
    );
    assert_eq!(fake.events, ["open", "connect"]);
    Ok(())
}

#[test]
fn transport_failure_is_an_error_after_pause_and_before_recovery() {
    let mut fake = Fake::new();
    fake.connect_fails = true;
    assert!(block_on(start(&mut fake, 4, &Faults::none())).is_err());
    assert_eq!(fake.events, ["open", "connect"]);
    assert!(fake.persisted_paused);
}

#[test]
fn recovery_is_bounded_continues_and_halts_for_review() -> Result<()> {
    let pending = RecoveryCounts {
        unresolved_attempts: 1,
        ..RecoveryCounts::default()
    };
    let mut fake = Fake::new();
    fake.recoveries = VecDeque::from([pending.clone(), pending.clone(), ready()]);
    let outcome = block_on(start(&mut fake, 3, &Faults::none()))?;
    assert!(matches!(outcome, StartupOutcome::Resumed { .. }));
    assert_eq!(fake.events.iter().filter(|e| **e == "recover").count(), 3);

    let mut fake = Fake::new();
    fake.recoveries = VecDeque::from([pending.clone(), pending.clone(), ready()]);
    let outcome = block_on(start(&mut fake, 2, &Faults::none()))?;
    assert_eq!(
        outcome,
        StartupOutcome::Retry {
            stage: Stage::Recovery,
            code: "recovery_incomplete"
        }
    );
    assert!(!fake.events.contains(&"commit_reveal"));

    for counts in [
        RecoveryCounts {
            finalized_failures: 1,
            ..RecoveryCounts::default()
        },
        RecoveryCounts {
            semantic_contract_mismatches: 1,
            ..ready()
        },
        RecoveryCounts {
            paused_during_recovery: true,
            ..RecoveryCounts::default()
        },
    ] {
        let mut fake = Fake::new();
        fake.recoveries = VecDeque::from([counts]);
        let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
        assert!(matches!(
            outcome,
            StartupOutcome::Halted {
                stage: Stage::Recovery,
                ..
            }
        ));
        assert!(!fake.events.contains(&"resume"));
    }
    Ok(())
}

#[test]
fn injected_crash_at_each_boundary_restarts_from_open_and_recovers_first() -> Result<()> {
    let cases = [
        (Phase::JournalOpened, 1),
        (Phase::RecoveryRound, 1),
        (Phase::RecoveryRound, 2),
        (Phase::CommitRevealChecked, 1),
        (Phase::RuntimeSurfaceEvaluated, 1),
        (Phase::Resumed, 1),
    ];
    for (phase, occurrence) in cases {
        let mut fake = Fake::new();
        fake.recoveries = VecDeque::from([RecoveryCounts::default(), ready()]);
        let error = block_on(start(&mut fake, 4, &Faults::at(phase, occurrence))).unwrap_err();
        assert_eq!(
            error.downcast_ref::<InjectedFault>(),
            Some(&InjectedFault(phase))
        );
        let resumed = fake.events.contains(&"resume");
        assert_eq!(resumed, phase == Phase::Resumed);
        // Restart: the same durable state opens paused and recovers before resume.
        fake.events.clear();
        fake.recoveries = VecDeque::from([ready()]);
        let outcome = block_on(start(&mut fake, 4, &Faults::none()))?;
        assert!(matches!(outcome, StartupOutcome::Resumed { .. }));
        assert_eq!(fake.events, FULL_ORDER);
    }
    Ok(())
}

// Real journal tests: the native adapter and file journal, with no client.

fn scratch() -> Result<tempfile::TempDir> {
    // A task-owned root when SLOP_NINJA_TEST_TMPDIR is set, otherwise the
    // system temporary directory (TMPDIR). Each TempDir removes itself.
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Ok(tempfile::Builder::new()
        .prefix("validator-")
        .tempdir_in(root)?)
}

fn config_text(root: &Path, network: &str, netuid: u16) -> String {
    EXAMPLE
        .replace("network = \"test\"", &format!("network = \"{network}\""))
        .replace("netuid = 0", &format!("netuid = {netuid}"))
        .replace(
            "/home/operator/.local/state/slop_ninja_validator/weights-journal.json",
            &root.join("journal.json").display().to_string(),
        )
        .replace(
            "/home/operator/.bittensor/wallets",
            &root.join("wallets").display().to_string(),
        )
}

fn config(root: &Path, network: &str, netuid: u16) -> Result<ValidatorConfig> {
    ValidatorConfig::parse(&config_text(root, network, netuid))
}

fn identity(genesis: H256, netuid: u16) -> ChainIdentity {
    ChainIdentity {
        genesis,
        netuid: NetUid(netuid),
        hotkey: AccountId([2; 32]),
    }
}

/// Wraps the live journal side with fake chain responses.
struct Hybrid {
    live: LivePorts,
    chain: Fake,
}

impl StartupPorts for Hybrid {
    fn open_journal(&mut self) -> Result<H256> {
        self.live.open_journal()
    }
    async fn connect(&mut self) -> Result<H256> {
        self.chain.connect().await
    }
    async fn finalized_head(&mut self) -> Result<FinalizedHead> {
        self.chain.finalized_head().await
    }
    async fn recover_finalized(&mut self, head: &FinalizedHead) -> Result<RecoveryCounts> {
        self.chain.recover_finalized(head).await
    }
    async fn subnet_state(&mut self, head: &FinalizedHead) -> Result<SubnetState> {
        self.chain.subnet_state(head).await
    }
    async fn runtime_surface(&mut self, head: &FinalizedHead) -> Result<SurfaceStatus> {
        self.chain.runtime_surface(head).await
    }
    fn resume(&mut self) -> Result<()> {
        self.live.resume()
    }
}

#[test]
fn example_config_requires_a_netuid_and_parses_once_set() -> Result<()> {
    let root = scratch()?;
    assert!(ValidatorConfig::parse(EXAMPLE).is_err());
    let config = config(root.path(), "test", 7)?;
    assert_eq!(config.netuid, 7);
    assert_eq!(config.network, NetworkName::Test);
    assert!(config.rpc.user_agent.starts_with("slop_ninja_validator/"));
    assert_eq!(config.rpc.signing_requests_per_minute, None);
    assert_eq!(config.rpc.signing_connect_quiet_secs, 60);
    let missing: String = config_text(root.path(), "test", 7)
        .lines()
        .filter(|line| !line.starts_with("netuid"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(ValidatorConfig::parse(&missing).is_err());
    Ok(())
}

#[test]
fn config_refuses_unsafe_values() -> Result<()> {
    let root = scratch()?;
    let base = config_text(root.path(), "test", 7);
    let endpoint = "wss://test.finney.opentensor.ai:443";
    assert!(base.contains(endpoint));
    for bad in [
        "ws://test.finney.opentensor.ai:443",
        "wss://user:secret@test.finney.opentensor.ai:443",
        "wss://test.finney.opentensor.ai:443/?key=secret",
        "wss://test.finney.opentensor.ai:443/secret-path",
        "https://test.finney.opentensor.ai",
        "ws://10.0.0.1:9944",
        "ws://localhost.example:9944",
    ] {
        assert!(
            ValidatorConfig::parse(&base.replace(endpoint, bad)).is_err(),
            "{bad}"
        );
    }
    // Plaintext only to a loopback proxy, which connects onward with TLS.
    for good in [
        "ws://127.0.0.1:9955",
        "ws://[::1]:9955",
        "ws://localhost:9955",
    ] {
        assert!(
            ValidatorConfig::parse(&base.replace(endpoint, good)).is_ok(),
            "{good}"
        );
    }
    for (from, to) in [
        ("name = \"my-wallet\"", "name = \"../escape\""),
        ("hotkey = \"my-validator\"", "hotkey = \"\""),
        (
            "max_requests_per_minute = 15",
            "max_requests_per_minute = 600",
        ),
        ("[log]", "unknown_field = true\n[log]"),
        (
            "startup_timeout_secs = 60",
            "startup_timeout_secs = 60\nsigning_connect_quiet_secs = 301",
        ),
    ] {
        assert!(base.contains(from), "{from}");
        assert!(
            ValidatorConfig::parse(&base.replace(from, to)).is_err(),
            "{to}"
        );
    }
    // The quiet period around the signing transport's connection may be off.
    let quiet = base.replace(
        "startup_timeout_secs = 60",
        "startup_timeout_secs = 60\nsigning_requests_per_minute = 20\nsigning_connect_quiet_secs = 0",
    );
    let parsed = ValidatorConfig::parse(&quiet)?;
    assert_eq!(parsed.rpc.signing_requests_per_minute, Some(20));
    assert_eq!(parsed.rpc.signing_connect_quiet_secs, 0);
    let relative = base.replace(
        &root.path().join("journal.json").display().to_string(),
        "journal.json",
    );
    assert!(ValidatorConfig::parse(&relative).is_err());
    // There is no runtime policy to configure; a spec allow-list is refused.
    let gated = format!("{base}\n[guard]\nruntime = \"refuse_all\"\n");
    assert!(ValidatorConfig::parse(&gated).is_err());
    Ok(())
}

#[test]
fn summary_omits_paths_and_endpoint_paths() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path(), "test", 7)?;
    let text = serde_json::to_string(&config.summary())?;
    assert!(!text.contains(&root.path().display().to_string()));
    assert!(text.contains("test.finney.opentensor.ai"));
    assert!(!text.contains("journal.json") && !text.contains("wallets"));
    Ok(())
}

#[test]
fn hotkey_is_read_only_from_the_public_companion() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path(), "test", 7)?;
    let hotkeys = root.path().join("wallets/my-wallet/hotkeys");
    std::fs::create_dir_all(&hotkeys)?;
    // A keyfile without a companion is refused without being read.
    std::fs::write(hotkeys.join("my-validator"), b"not parsed")?;
    assert!(public_hotkey(&config).is_err());
    // Synthetic public fields for a fixed test key; no secret is involved.
    let address = AccountId([2; 32]);
    std::fs::write(
        hotkeys.join("my-validatorpub.txt"),
        serde_json::json!({
            "accountId": format!("0x{}", hex::encode(address.0)),
            "publicKey": format!("0x{}", hex::encode(address.0)),
            "ss58Address": address.to_string(),
            "cryptoType": 1,
        })
        .to_string(),
    )?;
    assert_eq!(public_hotkey(&config)?, address);
    std::fs::write(
        hotkeys.join("my-validatorpub.txt"),
        serde_json::json!({
            "publicKey": format!("0x{}", hex::encode([3; 32])),
            "ss58Address": address.to_string(),
        })
        .to_string(),
    )?;
    assert!(public_hotkey(&config).is_err());
    Ok(())
}

#[test]
fn init_creates_once_and_run_never_creates_a_journal() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path(), "test", 7)?;
    let mut ports = LivePorts::new(config.clone(), identity(test_genesis(), 7));
    assert!(ports.open_journal().is_err());
    assert!(!config.journal_path.exists());
    init_journal(&config, identity(test_genesis(), 7))?;
    assert!(init_journal(&config, identity(test_genesis(), 7)).is_err());
    assert_eq!(ports.open_journal()?, test_genesis());
    drop(ports);
    let summary = JournalControls::open_existing(&config.journal_path)?.inspect()?;
    assert!(summary.paused);
    assert_eq!(summary.operations, 0);
    Ok(())
}

#[test]
fn journal_for_another_subnet_or_chain_is_refused_unchanged() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path(), "test", 7)?;
    init_journal(&config, identity(test_genesis(), 7))?;
    let before = std::fs::read(&config.journal_path)?;
    for other in [identity(test_genesis(), 8), identity(finney_genesis(), 7)] {
        let mut ports = LivePorts::new(config.clone(), other);
        assert!(ports.open_journal().is_err());
    }
    assert_eq!(std::fs::read(&config.journal_path)?, before);
    Ok(())
}

#[test]
fn fake_recovery_cannot_satisfy_the_adapter_resume_gate() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path(), "test", 7)?;
    init_journal(&config, identity(test_genesis(), 7))?;
    let mut ports = Hybrid {
        live: LivePorts::new(config.clone(), identity(test_genesis(), 7)),
        chain: Fake::new(),
    };
    let error = block_on(start(&mut ports, 4, &Faults::none())).unwrap_err();
    assert_eq!(error.to_string(), "explicit resume refused");
    drop(ports);
    assert!(
        JournalControls::open_existing(&config.journal_path)?
            .inspect()?
            .paused
    );
    Ok(())
}

#[test]
fn crash_after_open_leaves_the_real_journal_paused() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path(), "test", 7)?;
    init_journal(&config, identity(test_genesis(), 7))?;
    let mut ports = Hybrid {
        live: LivePorts::new(config.clone(), identity(test_genesis(), 7)),
        chain: Fake::new(),
    };
    let error = block_on(start(&mut ports, 4, &Faults::at(Phase::JournalOpened, 1))).unwrap_err();
    assert!(error.downcast_ref::<InjectedFault>().is_some());
    assert_eq!(ports.chain.events, Vec::<&str>::new());
    drop(ports);
    assert!(
        JournalControls::open_existing(&config.journal_path)?
            .inspect()?
            .paused
    );
    Ok(())
}

#[test]
fn fault_specs_name_every_phase_and_count_from_one() -> Result<()> {
    for (spec, expected) in [
        ("journal_opened", (Phase::JournalOpened, 1)),
        (
            "runtime_surface_evaluated:1",
            (Phase::RuntimeSurfaceEvaluated, 1),
        ),
        ("recovery_round:2", (Phase::RecoveryRound, 2)),
        ("commit_reveal_checked", (Phase::CommitRevealChecked, 1)),
        ("resumed", (Phase::Resumed, 1)),
        ("package_verified", (Phase::PackageVerified, 1)),
        ("row_derived", (Phase::RowDerived, 1)),
        ("enqueued", (Phase::Enqueued, 1)),
        ("dispatched:3", (Phase::Dispatched, 3)),
        ("submitted", (Phase::Submitted, 1)),
        ("tempo_enqueued", (Phase::TempoEnqueued, 1)),
    ] {
        assert_eq!(Faults::parse_spec(spec)?, expected, "{spec}");
    }
    for bad in ["", "resume", "dispatched:0", "dispatched:x", "Enqueued"] {
        assert!(Faults::parse_spec(bad).is_err(), "{bad}");
    }
    Ok(())
}

/// The drill variable is process-global; this is the only test that sets it.
#[test]
fn drill_faults_arm_only_on_the_test_network_in_a_drill_build() -> Result<()> {
    use crate::faults::DRILL_FAULT_ENV;
    // SAFETY: the tests run single-threaded (`--test-threads=1`), and no
    // other test reads or writes this variable.
    unsafe { std::env::set_var(DRILL_FAULT_ENV, "enqueued:2") };
    let test = Faults::from_env(NetworkName::Test);
    let finney = Faults::from_env(NetworkName::Finney);
    unsafe { std::env::remove_var(DRILL_FAULT_ENV) };
    assert!(finney.is_err(), "never on Finney");
    if cfg!(feature = "testnet-drills") {
        let faults = test?;
        // An armed drill hook exits the process, so only the unarmed arrival
        // and other phases are exercised here.
        faults.hit(Phase::Enqueued)?;
        faults.hit(Phase::Resumed)?;
    } else {
        assert!(
            test.is_err(),
            "a build without the feature refuses a set variable"
        );
    }
    assert!(
        Faults::from_env(NetworkName::Finney)?
            .hit(Phase::Enqueued)
            .is_ok()
    );
    Ok(())
}

#[test]
fn per_tempo_labels_follow_the_last_commit_epoch() -> Result<()> {
    use crate::app::next_tempo;
    assert_eq!(next_tempo(23_083, &[None])?, 23_084);
    assert_eq!(next_tempo(23_083, &[None, Some(23_083)])?, 23_084);
    // A label already retained (for example committed late, after an outage)
    // is never reused.
    assert_eq!(next_tempo(23_083, &[None, Some(23_084)])?, 23_085);
    assert_eq!(
        next_tempo(23_083, &[None, Some(23_084), Some(23_085)])?,
        23_086
    );
    assert!(next_tempo(u64::MAX, &[None]).is_err());
    Ok(())
}

#[test]
fn recommit_defaults_to_per_tempo_and_once_is_explicit() -> Result<()> {
    use crate::config::Recommit;
    let root = scratch()?;
    let base = config_text(root.path(), "test", 7);
    let section =
        |extra: &str| format!("{base}\n[dispatch]\nhotkey_file = \"/abs/hotkeys/v2\"\n{extra}");
    let parse = |text: String| ValidatorConfig::parse(&text).map(|c| c.dispatch.unwrap().recommit);
    assert_eq!(parse(section(""))?, Recommit::PerTempo);
    assert_eq!(
        parse(section("recommit = \"per_tempo\"\n"))?,
        Recommit::PerTempo
    );
    assert_eq!(parse(section("recommit = \"once\"\n"))?, Recommit::Once);
    assert!(parse(section("recommit = \"twice\"\n")).is_err());
    Ok(())
}
