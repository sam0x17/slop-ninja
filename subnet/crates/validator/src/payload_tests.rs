//! Step 0 of M2 and payload golden vectors against recorded live metadata.
//!
//! `testnet-486.v15.scale` is the V15 metadata testnet served at spec 486
//! (re-fetched on 2026-10-30 at finalized block 8,332,729 and byte-identical,
//! SHA-256 `582b1d05...1a81`). It is test data, not an accepted-runtime list:
//! production code resolves every call by name in whatever metadata the chain
//! serves. When a later runtime changes one of these encodings, these vectors
//! are updated; nothing in production refuses that runtime.
//!
//! For every transaction Slop Ninja's owner, validators and miners send, the
//! test checks that the pinned rtao revision's generated descriptor (pallet and call
//! index, argument names, shape hash) matches the live metadata, that the
//! indices match `docs/subnet-runtime-486-diff.md`, and that the generated
//! bytes equal a by-name re-encoding through live metadata. The golden file
//! then pins call bytes and signing payloads (`call ++ extra ++ additional`),
//! with any timelock ciphertext treated as an opaque field.
//!
//! Regenerate after an intended change with
//! `SLOP_NINJA_BLESS_PAYLOADS=1 cargo test -p slop_ninja_validator payload`.

use std::fmt::Write as _;

use rtao_core::codec::Value;
use rtao_core::extrinsic::TxParams;
use rtao_primitives::{AccountId, MechId, NetUid, TaoBalance};
use rtao_runtime::calls::{admin_utils, commitments, subtensor_module as sm};
use rtao_runtime::models::{CommitmentInfo, Data};
use rtao_runtime::{Call, CallDescriptor, RuntimeHandle, SurfaceDescriptor};
use slop_ninja_protocol::TESTNET_GENESIS;

use crate::payload::{sha256_hex, signing_payload, timelocked_commit_call};

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const GOLDEN: &str = "fixtures/payload-486.txt";
const NETUID: NetUid = NetUid(601);

/// Synthetic accounts; no real key is involved.
const HOTKEY: AccountId = AccountId([0x22; 32]);
/// A fixed, opaque stand-in for a timelock ciphertext.
fn ciphertext() -> Vec<u8> {
    (0..96u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect()
}
const REVEAL_ROUND: u64 = 27_123_456;

fn runtime() -> RuntimeHandle {
    // 486 and 1 are the observed spec and transaction versions; here they
    // only fill the CheckSpecVersion/CheckTxVersion fields of the vectors.
    RuntimeHandle::parse(METADATA, 486, 1, 42).expect("recorded metadata parses")
}

struct Transaction {
    descriptor: &'static CallDescriptor,
    /// `pallet.call` index from docs/subnet-runtime-486-diff.md.
    documented: (u8, u8),
    /// Who signs it.
    signer: &'static str,
    call: Call,
}

/// Every transaction M2 sends (or, for direct weights, refuses to send).
fn transactions() -> Vec<Transaction> {
    vec![
        Transaction {
            descriptor: &sm::COMMIT_TIMELOCKED_MECHANISM_WEIGHTS,
            documented: (7, 118),
            signer: "validator hotkey",
            call: sm::commit_timelocked_mechanism_weights(
                NETUID,
                MechId(0),
                ciphertext(),
                REVEAL_ROUND,
                4,
            ),
        },
        Transaction {
            descriptor: &sm::COMMIT_TIMELOCKED_WEIGHTS,
            documented: (7, 113),
            signer: "validator hotkey",
            call: sm::commit_timelocked_weights(NETUID, ciphertext(), REVEAL_ROUND, 4),
        },
        Transaction {
            descriptor: &sm::SET_MECHANISM_WEIGHTS,
            documented: (7, 119),
            signer: "never signed (refused fallback)",
            call: sm::set_mechanism_weights(NETUID, MechId(0), vec![2], vec![65535], 0),
        },
        Transaction {
            descriptor: &sm::BURNED_REGISTER,
            documented: (7, 7),
            signer: "coldkey",
            call: sm::burned_register(NETUID, HOTKEY),
        },
        Transaction {
            descriptor: &sm::ADD_STAKE,
            documented: (7, 2),
            signer: "coldkey",
            call: sm::add_stake(HOTKEY, NETUID, TaoBalance(10_000_000_000)),
        },
        Transaction {
            descriptor: &sm::SERVE_AXON,
            documented: (7, 4),
            signer: "miner hotkey (only if an axon is served)",
            // 192.0.2.10 (TEST-NET-1), port 8091, IPv4, protocol 4.
            call: sm::serve_axon(NETUID, 1, 0xc000_020a, 8091, 4, 4, 0, 0),
        },
        Transaction {
            descriptor: &commitments::SET_COMMITMENT,
            documented: (18, 0),
            signer: "miner hotkey (M5 admission digest)",
            call: commitments::set_commitment(
                NETUID,
                CommitmentInfo {
                    fields: vec![Data::Sha256([0xd1; 32])],
                },
            ),
        },
        Transaction {
            descriptor: &admin_utils::SUDO_SET_COMMIT_REVEAL_WEIGHTS_ENABLED,
            documented: (19, 49),
            signer: "subnet owner coldkey",
            call: admin_utils::sudo_set_commit_reveal_weights_enabled(NETUID, true),
        },
        Transaction {
            descriptor: &admin_utils::SUDO_SET_COMMIT_REVEAL_WEIGHTS_INTERVAL,
            documented: (19, 57),
            signer: "subnet owner coldkey",
            call: admin_utils::sudo_set_commit_reveal_weights_interval(NETUID, 1),
        },
    ]
}

#[test]
fn step0_generated_calls_match_live_486_by_name_index_and_type() {
    let runtime = runtime();
    for tx in transactions() {
        let d = tx.descriptor;
        let id = format!("call:{}.{}", d.pallet, d.name);
        let report = rtao_runtime::verify_surface_with(&runtime, &[SurfaceDescriptor::Call(d)]);
        assert!(
            report.shape_matches(&id) && report.changes.is_empty(),
            "{id}: {:?}",
            report.changes
        );
        assert_eq!((d.pallet_index, d.call_index), tx.documented, "{id} index");
        let bytes = tx.call.compose(&runtime).expect("composes");
        assert_eq!(
            (bytes.as_bytes()[0], bytes.as_bytes()[1]),
            tx.documented,
            "{id} encoded index"
        );
        let (pallet, name, args) =
            slop_ninja_subnet::surface::decode_named(runtime.core(), bytes.as_bytes())
                .expect("decodes by name");
        assert_eq!((pallet.as_str(), name.as_str()), (d.pallet, d.name));
        let names: Vec<_> = args.iter().map(|(name, _)| name.as_str()).collect();
        let expected: Vec<_> = d.fields.iter().map(|field| field.name).collect();
        assert_eq!(names, expected, "{id} argument names and order");
        let by_name = runtime
            .core()
            .compose_call(
                d.pallet,
                d.name,
                &Value::Dict(
                    args.into_iter()
                        .map(|(name, value)| (Value::Str(name), value))
                        .collect(),
                ),
            )
            .expect("re-encodes by name");
        assert_eq!(by_name, bytes.as_bytes(), "{id} by-name encoding");
    }
}

#[test]
fn the_dry_run_commit_builder_matches_rtaos_scheduler_composition() {
    let runtime = runtime();
    let ours = timelocked_commit_call(
        &runtime,
        NETUID,
        MechId(0),
        ciphertext(),
        REVEAL_ROUND,
        rtao_neuron::weights::NATIVE_CR_PAYLOAD_VERSION,
    )
    .expect("type-checks against live metadata");
    let rtao = transactions()[0].call.compose(&runtime).unwrap();
    assert_eq!(ours.as_bytes(), rtao.as_bytes());
}

fn params(mortal: bool) -> TxParams {
    TxParams {
        era: if mortal {
            Value::record(vec![
                ("period".into(), Value::Uint(128)),
                ("current".into(), Value::Uint(8_332_729)),
            ])
        } else {
            Value::Str("00".into())
        },
        nonce: if mortal { 3 } else { 0 },
        tip: 0,
        tip_asset_id: None,
        genesis_hash: TESTNET_GENESIS.0,
        era_block_hash: if mortal {
            [0x77; 32]
        } else {
            TESTNET_GENESIS.0
        },
        metadata_hash: None,
    }
}

fn report() -> String {
    let runtime = runtime();
    let mut out = String::new();
    writeln!(
        out,
        "# Payload golden vectors on recorded testnet spec 486 V15 metadata (sha256 {}).",
        sha256_hex(METADATA)
    )
    .unwrap();
    writeln!(
        out,
        "# Test data only; production resolves calls by name in live metadata."
    )
    .unwrap();
    for tx in transactions() {
        let d = tx.descriptor;
        let bytes = tx.call.compose(&runtime).unwrap();
        let key = format!("{}.{}", d.pallet, d.name);
        writeln!(out, "{key}.index={}.{}", d.pallet_index, d.call_index).unwrap();
        writeln!(out, "{key}.signer={}", tx.signer).unwrap();
        writeln!(out, "{key}.call={}", hex::encode(bytes.as_bytes())).unwrap();
    }
    let commit = transactions()[0].call.compose(&runtime).unwrap();
    for (label, mortal) in [("immortal", false), ("mortal128", true)] {
        let params = params(mortal);
        let payload = signing_payload(&runtime, commit.as_bytes(), &params).unwrap();
        let (extra, additional) = runtime.core().signature_payload_parts(&params).unwrap();
        assert_eq!(
            payload,
            [commit.as_bytes(), extra.as_slice(), additional.as_slice()].concat(),
            "payload is call ++ extra ++ additional"
        );
        let key = format!("commit_timelocked_mechanism_weights.payload.{label}");
        writeln!(out, "{key}.extra={}", hex::encode(&extra)).unwrap();
        writeln!(out, "{key}.additional={}", hex::encode(&additional)).unwrap();
        writeln!(out, "{key}.sha256={}", sha256_hex(&payload)).unwrap();
    }
    out
}

#[test]
fn payload_golden_vectors_match_recorded_486_metadata() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    let report = report();
    if std::env::var_os("SLOP_NINJA_BLESS_PAYLOADS").is_some() {
        std::fs::write(&path, &report).unwrap();
    }
    let golden = std::fs::read_to_string(&path).expect("golden file exists");
    assert_eq!(report, golden);
}

#[test]
fn the_signing_payload_follows_the_live_runtime_not_a_pinned_version() {
    // The same call and parameters under another spec number differ only in
    // the CheckSpecVersion field; nothing refuses the new runtime.
    let commit = transactions()[0].call.compose(&runtime()).unwrap();
    let later = RuntimeHandle::parse(METADATA, 487, 1, 42).unwrap();
    let recomposed = transactions()[0].call.compose(&later).unwrap();
    assert_eq!(commit.as_bytes(), recomposed.as_bytes());
    let params = params(true);
    let now = signing_payload(&runtime(), commit.as_bytes(), &params).unwrap();
    let next = signing_payload(&later, recomposed.as_bytes(), &params).unwrap();
    assert_ne!(now, next);
    assert_eq!(now.len(), next.len());
    let differing: Vec<_> = now
        .iter()
        .zip(&next)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    assert!(!differing.is_empty() && differing.len() <= 4);
}

/// The dry run follows the stored commit-reveal tag under any runtime code
/// and reports rtao's basis for it (D-122): native, reviewed code, unreviewed
/// code (still followed) or an unread code. rtao's tag constants are the
/// core's, which the coordinator also follows.
#[test]
fn the_dry_run_follows_any_stored_tag_and_reports_its_basis() {
    use rtao_neuron::weights::{NATIVE_CR_PAYLOAD_VERSION, REVIEWED_CR_RUNTIME_CODE};
    assert_eq!(NATIVE_CR_PAYLOAD_VERSION, rtao_core::commit_reveal::NATIVE_PAYLOAD_VERSION);
    let code = |hex_code: &str| {
        let bytes = hex::decode(hex_code.trim_start_matches("0x")).expect("hex");
        rtao_primitives::H256(<[u8; 32]>::try_from(bytes).expect("32 bytes"))
    };
    let reviewed = code(REVIEWED_CR_RUNTIME_CODE[REVIEWED_CR_RUNTIME_CODE.len() - 1].1);
    let unlisted = rtao_primitives::H256([0x48; 32]);
    let native = NATIVE_CR_PAYLOAD_VERSION;
    for runtime_code in [None, Some(reviewed), Some(unlisted)] {
        assert_eq!(crate::payload::dry_run_commit_tag(native, runtime_code), (native, "native"));
    }
    assert_eq!(crate::payload::dry_run_commit_tag(native + 1, Some(reviewed)), (native + 1, "reviewed_code"));
    assert_eq!(crate::payload::dry_run_commit_tag(native + 1, Some(unlisted)), (native + 1, "unreviewed_code"));
    assert_eq!(crate::payload::dry_run_commit_tag(0, Some(unlisted)), (0, "unreviewed_code"));
    assert_eq!(crate::payload::dry_run_commit_tag(native + 1, None), (native + 1, "code_unknown"));
}
