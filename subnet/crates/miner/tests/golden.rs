//! The chain payload against the protocol fixtures, and the
//! `Commitments.set_commitment` call bytes rtao composes for it against the
//! recorded spec-486 testnet metadata.
//!
//! The metadata is test data, not an accepted-runtime list: production code
//! composes the call by name in whatever metadata the chain serves. When a
//! later runtime changes this encoding, the golden file is regenerated with
//! `SLOP_NINJA_BLESS_COMMIT_CALL=1 cargo test -p slop_ninja_miner --test golden`;
//! nothing refuses that runtime. No call is signed or submitted.

use rtao_runtime::calls::commitments::SET_COMMITMENT;
use rtao_runtime::{CallBytes, RuntimeHandle};
use slop_ninja_miner::chain::{commitment_call, compose};
use slop_ninja_miner::client::ss58;
use slop_ninja_miner::opening::OpeningFile;
use slop_ninja_protocol::fixtures_reward_v1 as fx;
use slop_ninja_protocol::submission::{SUBMISSION_PAYLOAD_LEN, SubmissionOpening};
use slop_ninja_protocol::{ChainBinding, TESTNET_GENESIS, VerifiedArtifact};

const PROTOCOL_GOLDEN: &str = include_str!("../../protocol/fixtures/golden-reward-v1.txt");
const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const GOLDEN: &str = "fixtures/commit-call-486.txt";

fn protocol_golden(key: &str) -> &'static str {
    PROTOCOL_GOLDEN
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("protocol golden line {key}"))
}

fn runtime() -> RuntimeHandle {
    // 486 and 1 are the observed spec and transaction versions; they only
    // stamp the composed call here.
    RuntimeHandle::parse(METADATA, 486, 1, 42).expect("recorded metadata parses")
}

/// The protocol fixture's first entrant and the cohort its golden payload names.
fn fixture_opening() -> (SubmissionOpening, VerifiedArtifact) {
    let spec = fx::models()[0].spec.clone();
    let payload = hex::decode(protocol_golden("submission.chain_payload")).unwrap();
    let cohort = u64::from_le_bytes(payload[4..12].try_into().unwrap());
    let opening = fx::opening(&fx::chain(), cohort, &spec);
    let artifact = VerifiedArtifact::from_bytes(&fx::artifact_bytes(&spec)).unwrap();
    (opening, artifact)
}

#[test]
fn payload_and_commitment_match_the_protocol_golden() {
    let (opening, artifact) = fixture_opening();
    assert_eq!(
        opening.commitment().hex(),
        protocol_golden("submission.commitment")
    );
    assert_eq!(
        hex::encode(opening.chain_payload()),
        protocol_golden("submission.chain_payload")
    );
    // The stored opening carries the same payload and opens back to the same opening.
    let file = OpeningFile::new(&opening, &artifact, &ss58(&opening.hotkey));
    assert_eq!(
        file.chain_payload,
        protocol_golden("submission.chain_payload")
    );
    assert_eq!(file.opening().unwrap(), opening);
}

fn expected_call(netuid: u16, payload: &[u8; SUBMISSION_PAYLOAD_LEN]) -> Vec<u8> {
    // Commitments (18) . set_commitment (0), u16 netuid, one field,
    // Data::Raw44 (variant 45) and the raw bytes without a length prefix.
    let mut out = vec![SET_COMMITMENT.pallet_index, SET_COMMITMENT.call_index];
    out.extend_from_slice(&netuid.to_le_bytes());
    out.push(0x04);
    out.push(0x2d);
    out.extend_from_slice(payload);
    out
}

fn call_bytes(netuid: u16, payload: &[u8; SUBMISSION_PAYLOAD_LEN]) -> CallBytes {
    compose(&commitment_call(netuid, payload), &runtime()).expect("composes")
}

#[test]
fn set_commitment_call_bytes() {
    let (opening, _) = fixture_opening();
    let payload = opening.chain_payload();
    let fixture_netuid = opening.chain.netuid;
    let at_fixture = call_bytes(fixture_netuid, &payload);
    let testnet = ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 601,
        mechanism: 0,
    };
    let at_601 = call_bytes(testnet.netuid, &payload);
    assert_eq!(
        (SET_COMMITMENT.pallet_index, SET_COMMITMENT.call_index),
        (18, 0)
    );
    assert_eq!(
        at_fixture.as_bytes(),
        expected_call(fixture_netuid, &payload).as_slice()
    );
    assert_eq!(at_601.as_bytes(), expected_call(601, &payload).as_slice());
    assert_eq!(
        &at_601.as_bytes()[..6],
        &[0x12, 0x00, 0x59, 0x02, 0x04, 0x2d]
    );
    // The bytes are the runtime's canonical encoding of the call.
    assert_eq!(
        CallBytes::decode(at_601.as_bytes(), &runtime())
            .unwrap()
            .as_bytes(),
        at_601.as_bytes()
    );

    let report = format!(
        "schema=slop_ninja.miner.golden/commit-call-486\n\
         metadata.spec=486\n\
         payload={}\n\
         call.netuid_{fixture_netuid}={}\n\
         call.netuid_601={}\n",
        hex::encode(payload),
        hex::encode(at_fixture.as_bytes()),
        hex::encode(at_601.as_bytes()),
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("SLOP_NINJA_BLESS_COMMIT_CALL").is_some() {
        std::fs::write(&path, &report).unwrap();
    }
    let golden = std::fs::read_to_string(&path).expect("golden file; bless it once");
    assert_eq!(
        report, golden,
        "commit call bytes changed; review and bless"
    );
}
