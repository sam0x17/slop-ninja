//! The coordinator's Wasm transaction path against the pinned rtao's native
//! builders (`0c128160`, tag `snapshot-2026-11-03.2`; first written against
//! `b5553a76`), byte for byte, on the recorded testnet spec 486 V15 metadata.
//!
//! The coordinator composes by name with plain values; rtao composes with its
//! generated descriptors and typed primitives (`NetUid`, `MechId`,
//! `AccountId`, `NetUidStorageIndex`). Every vector below feeds both the same
//! inputs. Signatures and timelock ciphertexts are randomized, so a signature
//! is attached to both assemblies and a ciphertext is checked by decryption
//! with a recorded drand quicknet round signature.

use rtao_core::codec::Value;
use rtao_primitives::{AccountId, IntoCoreValue, MechId, NetUid, NetUidStorageIndex, Uid};
use rtao_runtime::RuntimeHandle;
use rtao_runtime::calls::subtensor_module as sm;
use rtao_signer::Signer;
use slop_ninja_coordinator::chain::{self, ChainRuntime};

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const GENESIS: [u8; 32] = slop_ninja_protocol::TESTNET_GENESIS.0;
const ALICE_SEED: &str = "e5be9a5092b81bca64be81d212e7f2f9eba183bb7a90954f7b76361f6edb5c0a";
const ROUND_1000_SIGNATURE: &str = "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

fn rtao() -> RuntimeHandle {
    RuntimeHandle::parse(METADATA, 486, 1, 42).unwrap()
}

fn ours() -> ChainRuntime {
    ChainRuntime::parse(METADATA, 486, 1).unwrap()
}

fn ciphertexts() -> Vec<Vec<u8>> {
    vec![
        vec![],
        vec![0xab],
        (0..96u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect(),
        (0..=255u8).cycle().take(4_321).collect(),
    ]
}

#[test]
fn commit_calls_match_rtaos_generated_builder() {
    let (r, o) = (rtao(), ours());
    for ct in ciphertexts() {
        for (netuid, round, version) in [
            (601u16, 27_123_456u64, 4u16),
            (1, 0, 3),
            (u16::MAX, u64::MAX, 4),
        ] {
            let theirs = sm::commit_timelocked_mechanism_weights(
                NetUid(netuid),
                MechId(0),
                ct.clone(),
                round,
                version,
            )
            .compose(&r)
            .unwrap();
            let mine = o.commit_call(netuid, 0, &ct, round, version).unwrap();
            assert_eq!(
                hex::encode(theirs.as_bytes()),
                hex::encode(&mine),
                "netuid {netuid} round {round}"
            );
        }
    }
}

#[test]
fn storage_keys_match_rtaos_typed_values() {
    let (r, o) = (rtao(), ours());
    let hotkey = AccountId([0x22; 32]);
    let index = NetUidStorageIndex::new(NetUid(601), MechId(0)).unwrap();
    let cases: Vec<(&str, &str, Vec<Value>, Vec<Value>)> = vec![
        (
            "SubtensorModule",
            "Uids",
            vec![NetUid(601).into_core_value(), hotkey.into_core_value()],
            vec![Value::Int(601), Value::Bytes(vec![0x22; 32])],
        ),
        (
            "SubtensorModule",
            "Tempo",
            vec![NetUid(601).into_core_value()],
            vec![Value::Int(601)],
        ),
        (
            "SubtensorModule",
            "LastUpdate",
            vec![index.into_core_value()],
            vec![Value::Int(601)],
        ),
        (
            "SubtensorModule",
            "Weights",
            vec![index.into_core_value(), Uid(3).into_core_value()],
            vec![Value::Int(601), Value::Int(3)],
        ),
        (
            "SubtensorModule",
            "TimelockedWeightCommits",
            vec![index.into_core_value(), 7u64.into_core_value()],
            vec![Value::Int(601), Value::Int(7)],
        ),
        (
            "SubtensorModule",
            "Keys",
            vec![NetUid(601).into_core_value(), Uid(2).into_core_value()],
            vec![Value::Int(601), Value::Int(2)],
        ),
        (
            "SubtensorModule",
            "BlockAtRegistration",
            vec![NetUid(601).into_core_value(), Uid(2).into_core_value()],
            vec![Value::Int(601), Value::Int(2)],
        ),
        (
            "System",
            "Account",
            vec![hotkey.into_core_value()],
            vec![Value::Bytes(vec![0x22; 32])],
        ),
        ("System", "Events", vec![], vec![]),
        ("System", "LastRuntimeUpgrade", vec![], vec![]),
        ("Timestamp", "Now", vec![], vec![]),
    ];
    for (pallet, item, typed, plain) in cases {
        let entry = r.storage_entry(pallet, item).unwrap();
        let theirs = r.core().storage_key(entry, &typed).unwrap();
        let mine = o.storage_key(pallet, item, &plain).unwrap();
        assert_eq!(hex::encode(theirs), hex::encode(mine), "{pallet}.{item}");
    }
}

fn mortal(current: u64) -> Value {
    Value::record(vec![
        ("period".into(), Value::Uint(64)),
        ("current".into(), Value::Uint(u128::from(current))),
    ])
}

#[test]
fn signing_payload_and_signed_extrinsic_match_rtaos_client_parameters() {
    let (r, o) = (rtao(), ours());
    let signer = rtao_signer::KeypairSigner::from_seed(
        &hex::decode(ALICE_SEED).unwrap(),
        rtao_primitives::CryptoType::Sr25519,
    )
    .unwrap();
    let hotkey = chain::Hotkey::from_seed_hex(ALICE_SEED).unwrap();
    assert_eq!(signer.account().0, hotkey.public());
    let ct = ciphertexts()[2].clone();
    let call = o.commit_call(601, 0, &ct, 33_517_241, 4).unwrap();
    for (nonce, current) in [(0u64, 8_334_501u64), (7, 8_334_376), (1_000_000, 1)] {
        // rtao-client's mortal parameters: {period, current} at the finalized
        // height, anchored at the birth block, CheckMetadataHash disabled.
        let theirs_params = rtao_core::extrinsic::TxParams {
            era: mortal(current),
            nonce,
            tip: 0,
            tip_asset_id: None,
            genesis_hash: GENESIS,
            era_block_hash: [0x42; 32],
            metadata_hash: None,
        };
        let (era, birth, expiry) = chain::mortal_era(64, current);
        assert_eq!(birth, rtao_core::extrinsic::era_birth(64, current));
        assert_eq!(expiry, birth + 64);
        let mine_params = chain::tx_params(era, nonce, GENESIS, [0x42; 32]);
        let theirs_payload = r.core().signature_payload(&call, &theirs_params).unwrap();
        let mine_payload = o.signing_payload(&call, &mine_params).unwrap();
        assert_eq!(theirs_payload, mine_payload);
        // One signature, attached by both: rtao passes the MultiSignature
        // variant `u8::from(CryptoType)`.
        let signature = hotkey.sign(&mine_payload).unwrap();
        let (theirs_ext, theirs_hash) = r
            .core()
            .encode_signed_extrinsic(
                &call,
                signer.account().0,
                &signature,
                u8::from(rtao_primitives::CryptoType::Sr25519),
                &theirs_params,
            )
            .unwrap();
        let (mine_ext, mine_hash) = o
            .signed_extrinsic(&call, hotkey.public(), &signature, &mine_params)
            .unwrap();
        assert_eq!(hex::encode(&theirs_ext), hex::encode(&mine_ext));
        assert_eq!(theirs_hash, mine_hash);
        assert!(chain::verify_sr25519(
            &hotkey.public(),
            &mine_payload,
            &signature
        ));
        assert!(
            rtao_core::keys::verify_strict(
                &mine_payload,
                &signature,
                &signer.account().to_string(),
                rtao_core::keys::CRYPTO_SR25519
            )
            .unwrap()
        );
    }
}

#[test]
fn timelock_payload_round_and_ciphertext_match_rtaos_commit() {
    let hotkey = [0x22u8; 32];
    let (uids, values) = (vec![0u16, 2, 5], vec![65535u16, 12, 1]);
    let theirs = rtao_core::timelock::timelock_commit::commit_payload(
        hotkey.to_vec(),
        uids.clone(),
        values.clone(),
        0,
    );
    let mine = chain::commit_payload(hotkey, &uids, &values, 0);
    assert_eq!(theirs, mine);
    let state = chain::schedule(360, 8_334_040, 0, 7, 100, 8_334_140);
    let now = 1_793_000_000.0;
    let theirs_round =
        rtao_core::timelock::timelock_commit::reveal_round_at(now, &state, 1, 12.0).unwrap();
    assert_eq!(theirs_round, chain::reveal_round(now, &state, 1).unwrap());
    assert_eq!(
        rtao_core::timelock::epoch_schedule::predict_first_reveal_block(&state, 1).unwrap(),
        chain::first_reveal_block(&state, 1).unwrap()
    );
    // rtao's own randomized commit and ours both open with the round's
    // signature to the identical payload.
    let signature = hex::decode(ROUND_1000_SIGNATURE).unwrap();
    let ours_ct = chain::encrypt(&mine, 1000).unwrap();
    let theirs_ct = rtao_core::timelock::encrypt_and_compress(&theirs, 1000).unwrap();
    assert_eq!(
        rtao_core::timelock::decrypt_and_decompress(&ours_ct, &signature).unwrap(),
        theirs
    );
    assert_eq!(chain::decrypt(&theirs_ct, &signature).unwrap(), mine);
}
