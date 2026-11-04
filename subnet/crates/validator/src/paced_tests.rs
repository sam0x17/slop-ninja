//! rtao's changes for a validator on a paced endpoint (D-095 to D-097), as
//! this validator configures them: the weight admission window, the session's
//! reconnect policy and the dispatch loop's finalized-head policy. The session
//! and head-stream tests run against rtao-testkit's in-process fake node and
//! fake chain over loopback; nothing leaves the host and no key is loaded.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt as _;
use rtao_client::{Client, ClientConfig, Finality, Network};
use rtao_primitives::{AccountId, NetUid};
use rtao_rpc::{ReadMethod, Session, SessionState};
use rtao_testkit::fakes::{FakeChain, FakeNode, FakeState, Fault, Reply};
use serde_json::{Value, json};
use slop_ninja_subnet::ChainIdentity;
use tokio::time::timeout;

use crate::config::{NetworkName, ValidatorConfig};
use crate::live::{
    DISPATCH_BACKFILL, MEASURED_ADMISSION_CALLS, UNCOUNTED_ADMISSION_CALLS, client_config, finalized_heads, init_journal,
    session_failure, signing_client_config, weight_submit_deadline,
};

const EXAMPLE: &str = include_str!("../validator.example.toml");

fn scratch() -> Result<tempfile::TempDir> {
    // A task-owned root when SLOP_NINJA_TEST_TMPDIR is set, otherwise the
    // system temporary directory (TMPDIR). Each TempDir removes itself.
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Ok(tempfile::Builder::new().prefix("validator-paced-").tempdir_in(root)?)
}

fn config(root: &Path) -> Result<ValidatorConfig> {
    ValidatorConfig::parse(
        &EXAMPLE
            .replace("netuid = 0", "netuid = 7")
            .replace(
                "/home/operator/.local/state/slop_ninja_validator/NAME/weights-journal.json",
                &root.join("journal.json").display().to_string(),
            )
            .replace("/home/operator/.bittensor/wallets", &root.join("wallets").display().to_string()),
    )
}

fn fake(error: rtao_testkit::fakes::FakeError) -> anyhow::Error {
    anyhow::anyhow!(error.to_string())
}

/// The window holds the measured admission calls and the allowance for the
/// calls this pin adds at the signing transport's budget from an empty token
/// bucket, never less than rtao's default, and
/// the adapter's scheduler accepts it.
#[test]
fn the_admission_window_is_sized_from_the_budget_of_the_transport_that_signs() -> Result<()> {
    let root = scratch()?;
    let mut config = config(root.path())?;
    // The testnet budgets (TESTNET-M2.md): 13 a minute for reads, 20 for signing.
    config.rpc.max_requests_per_minute = 13;
    config.rpc.signing_requests_per_minute = Some(20);
    assert_eq!(weight_submit_deadline(&config), Duration::from_secs(147));
    config.rpc.signing_requests_per_minute = None;
    assert_eq!(weight_submit_deadline(&config), Duration::from_secs(227));
    config.rpc.signing_requests_per_minute = Some(60);
    assert_eq!(weight_submit_deadline(&config), Duration::from_secs(49));
    // rtao's default is the floor.
    config.rpc.signing_requests_per_minute = Some(600);
    assert_eq!(
        weight_submit_deadline(&config),
        rtao_neuron::weights::DEFAULT_WEIGHT_SUBMIT_DEADLINE
    );
    // Both testnet windows fit between a 320-block offset and the end of a
    // 361-block epoch at 12 s blocks.
    config.rpc.signing_requests_per_minute = None;
    assert!(weight_submit_deadline(&config) < Duration::from_secs((361 - 320) * 12));
    for per_minute in 1..=60u32 {
        config.rpc.signing_requests_per_minute = Some(per_minute);
        let window = weight_submit_deadline(&config).as_secs_f64();
        let paced = f64::from(MEASURED_ADMISSION_CALLS + UNCOUNTED_ADMISSION_CALLS + 1) * 60.0 / f64::from(per_minute);
        assert!(window >= paced, "{per_minute} a minute: {window} s < {paced} s");
        assert!(window < paced + 1.0, "{per_minute} a minute: {window} s");
    }
    // The adapter opens its scheduler with the window (rtao refuses zero).
    config.rpc.signing_requests_per_minute = Some(20);
    init_journal(
        &config,
        ChainIdentity {
            genesis: NetworkName::Test.genesis()?,
            netuid: NetUid(7),
            hotkey: AccountId([2; 32]),
        },
    )?;
    Ok(())
}

fn refusal(status: u16, retry_after: &str) -> Fault {
    Fault::HandshakeRefuse {
        status,
        headers: vec![("Retry-After".into(), retry_after.into())],
        body: "slow down".into(),
    }
}

/// D-097 replaces the restart this validator made after a refused reconnect:
/// with the session settings it configures, HTTP 429 refusals of a reconnect,
/// more of them than `max_retries` allows, are waited out within the session,
/// which reconnects and answers, and the dispatch loop's terminal-session
/// check reports nothing. A policy close (1008) still ends the session, which
/// that check reports so the unit restarts with fresh connections.
#[tokio::test]
async fn a_refused_reconnect_keeps_the_session_and_only_a_policy_close_ends_it() -> Result<()> {
    let node = FakeNode::start().await.map_err(fake)?;
    node.mutate_state(|state| {
        state.set_handler("system_chain", |_, request| Reply::result(request.params[0].clone()));
    });
    let root = scratch()?;
    let mut config = config(root.path())?;
    config.rpc.endpoints = vec![node.url().to_owned()];
    config.rpc.signing_requests_per_minute = Some(20);
    let read = client_config(&config, false)?.session;
    let signing = signing_client_config(&config, 20)?.session;
    assert!(read.retry_forever && signing.retry_forever);
    let mut session_config = read;
    // Two refusals against one permitted retry: without `retry_forever` the
    // second would end the session.
    session_config.max_retries = 1;
    let session = Session::connect(session_config).await?;
    node.push_fault(refusal(429, "1"));
    node.push_fault(refusal(429, "0"));
    node.drop_connection().map_err(fake)?;
    let echoed: String = timeout(
        Duration::from_secs(30),
        session.request(ReadMethod::SystemChain, json!(["after the refusals"])),
    )
    .await??;
    assert_eq!(echoed, "after the refusals");
    assert!(node.connect_attempts() >= 4, "{} connects", node.connect_attempts());
    // `Connected` follows once the resends have completed.
    timeout(Duration::from_secs(10), async {
        while !matches!(&*session.state().borrow(), SessionState::Connected { .. }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("the session did not return to Connected")?;
    assert_eq!(session_failure(&session), None);

    node.mutate_state(|state| state.set_handler("system_chain", |_, _| Reply::silent()));
    let pending = {
        let session = session.clone();
        tokio::spawn(async move { session.request::<Value>(ReadMethod::SystemChain, json!([])).await })
    };
    timeout(Duration::from_secs(10), async {
        while !node.requests().iter().any(|r| r.method == "system_chain" && r.params.as_array().is_some_and(Vec::is_empty)) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    node.close_connection(1008, "policy").map_err(fake)?;
    assert!(timeout(Duration::from_secs(10), pending).await??.is_err());
    timeout(Duration::from_secs(10), async {
        while session_failure(&session).is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("a policy close did not end the session")?;
    session.close().await;
    Ok(())
}

async fn next(stream: &mut rtao_client::BlockStream) -> Result<rtao_client::BlockHeader> {
    Ok(timeout(Duration::from_secs(5), stream.next())
        .await?
        .context("finalized stream ended")??)
}

fn fake_client_config() -> Result<ClientConfig> {
    let mut config = ClientConfig::new(Network::resolve_with("local", |_| None)?)?;
    config.runtime_cache_dir = None;
    config.config_store = None;
    config.session.rate_limit = None;
    Ok(config)
}

/// D-096: after an iteration during which many heights finalized, the
/// dispatch loop's stream delivers the newest finalized head next, where a
/// `Complete` follower of the same client first replays every missed height
/// in order. The loop reads the epoch, and so the commit window, from the
/// head it is given, so it reaches the window no later than a replaying
/// follower would.
#[tokio::test]
async fn the_dispatch_loop_receives_the_newest_finalized_head_after_a_long_iteration() -> Result<()> {
    assert_eq!(DISPATCH_BACKFILL, rtao_client::Backfill::Latest);
    let raw = FakeChain::from_state(FakeState::default());
    let client =
        Client::from_rpc(rtao_testkit::rpc::RpcClient::new(raw.clone()), fake_client_config()?).await?;
    let mut heads = finalized_heads(&client)?;
    let mut complete = client.blocks(Finality::Finalized)?;
    let first = next(&mut heads).await?.number;
    assert_eq!(next(&mut complete).await?.number, first);
    // More heights than the shared tracker's own gap recovery (16) finalize
    // while neither follower is polled.
    let gap = rtao_client::head::SHARED_FINALIZED_BACKFILL + 24;
    raw.advance(gap)?;
    let newest = first + gap;
    timeout(Duration::from_secs(5), async {
        while client.heads().map(|heads| heads.finalized().borrow().number) != Some(newest) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert_eq!(next(&mut heads).await?.number, newest);
    assert_eq!(next(&mut complete).await?.number, first + 1);
    raw.advance(1)?;
    assert_eq!(next(&mut heads).await?.number, newest + 1);
    client.close().await;
    Ok(())
}
