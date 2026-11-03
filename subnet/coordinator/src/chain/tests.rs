//! Native checks of the chain seam against recorded testnet metadata.
//!
//! `testnet-486.v15.scale` is the V15 metadata testnet served at spec 486
//! (shared with the native validator's tests). It is test data, not an
//! accepted-runtime list: production resolves every call and storage item by
//! name in whatever metadata the chain serves.
//!
//! The commit call and signing payloads are compared with the native
//! validator's golden file, which rtao `b5553a76`'s own builders produced for
//! the same inputs. `parity/` repeats the comparison live against rtao.

use super::*;
use std::collections::BTreeMap;

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const RTAO_GOLDEN: &str = include_str!("../../../crates/validator/fixtures/payload-486.txt");
const GENESIS: [u8; 32] = slop_ninja_protocol::TESTNET_GENESIS.0;

/// drand quicknet round 1000, as served by api.drand.sh and
/// drand.cloudflare.com (identical) on 2026-10-30.
const ROUND_1000_SIGNATURE: &str = "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

fn runtime() -> ChainRuntime {
    ChainRuntime::parse(METADATA, 486, 1).expect("recorded metadata parses")
}

fn golden() -> BTreeMap<String, String> {
    RTAO_GOLDEN
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// The validator golden's opaque stand-in ciphertext.
fn ciphertext() -> Vec<u8> {
    (0..96u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect()
}

#[test]
fn commit_call_composed_by_name_matches_rtaos_builder() {
    let call = runtime()
        .commit_call(601, 0, &ciphertext(), 27_123_456, 4)
        .unwrap();
    let g = golden();
    assert_eq!(
        hex::encode(&call),
        g["SubtensorModule.commit_timelocked_mechanism_weights.call"]
    );
    assert_eq!((call[0], call[1]), (7, 118));
}

#[test]
fn signing_payloads_match_rtaos_for_immortal_and_mortal_params() {
    let rt = runtime();
    let call = rt
        .commit_call(601, 0, &ciphertext(), 27_123_456, 4)
        .unwrap();
    let g = golden();
    let immortal = tx_params(Value::Str("00".into()), 0, GENESIS, GENESIS);
    let mortal = tx_params(
        Value::record(vec![
            ("period".into(), Value::Uint(128)),
            ("current".into(), Value::Uint(8_332_729)),
        ]),
        3,
        GENESIS,
        [0x77; 32],
    );
    for (label, params) in [("immortal", immortal), ("mortal128", mortal)] {
        let (extra, additional) = rt.core().signature_payload_parts(&params).unwrap();
        let key = format!("commit_timelocked_mechanism_weights.payload.{label}");
        assert_eq!(hex::encode(&extra), g[&format!("{key}.extra")]);
        assert_eq!(hex::encode(&additional), g[&format!("{key}.additional")]);
        let payload = rt.signing_payload(&call, &params).unwrap();
        assert_eq!(sha256_hex(&payload), g[&format!("{key}.sha256")]);
    }
}

#[test]
fn mortal_era_matches_the_rtao_client_shape() {
    // Below 4096 blocks the phase is not quantized, so the era is born at
    // the anchoring block itself and the anchor hash is that block's.
    let (era, birth, expiry) = mortal_era(64, 8_334_501);
    assert_eq!(birth, 8_334_501);
    assert_eq!(expiry, 8_334_565);
    let mut out = Vec::new();
    runtime().core().encode_era_value(&era, &mut out).unwrap();
    assert_eq!(out.len(), 2);
}

#[test]
fn signed_extrinsic_verifies_and_round_trips_through_the_decoder() {
    let rt = runtime();
    // Substrate's public //Alice mini-secret; no real key is involved.
    let alice =
        Hotkey::from_seed_hex("0xe5be9a5092b81bca64be81d212e7f2f9eba183bb7a90954f7b76361f6edb5c0a")
            .unwrap();
    assert_eq!(
        hex::encode(alice.public()),
        "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"
    );
    let call = rt
        .commit_call(601, 0, &ciphertext(), 27_123_456, 4)
        .unwrap();
    let (era, birth, _) = mortal_era(64, 8_334_501);
    let params = tx_params(era, 5, GENESIS, [0x42; 32]);
    let payload = rt.signing_payload(&call, &params).unwrap();
    let signature = alice.sign(&payload).unwrap();
    assert!(verify_sr25519(&alice.public(), &payload, &signature));
    let mut tampered = payload.clone();
    tampered[0] ^= 1;
    assert!(!verify_sr25519(&alice.public(), &tampered, &signature));
    let (bytes, hash) = rt
        .signed_extrinsic(&call, alice.public(), &signature, &params)
        .unwrap();
    assert_eq!(hash.len(), 32);
    assert_eq!(
        rt.extrinsic_call(&bytes).unwrap(),
        (SUBTENSOR.into(), COMMIT_CALL.into())
    );
    let decoded = rt.core().decode_extrinsic(&bytes, true).unwrap();
    assert_eq!(as_u64(field(&decoded, "nonce").unwrap()), Some(5));
    let _ = birth;
}

#[test]
fn bad_seeds_fail_with_fixed_codes_only() {
    for bad in ["", "0x00", "zz", &"g".repeat(64)] {
        let error = Hotkey::from_seed_hex(bad).err().unwrap();
        assert_eq!(error.code, "hotkey_seed_invalid");
        assert!(!error.detail.contains(bad) || bad.is_empty());
    }
}

#[test]
fn storage_keys_resolve_by_name_and_values_decode_with_defaults() {
    let rt = runtime();
    let net = [Value::Int(601)];
    let tempo = rt.storage_key(SUBTENSOR, "Tempo", &net).unwrap();
    // twox128("SubtensorModule") ++ twox128("Tempo") ++ identity(601 LE).
    assert_eq!(
        &hex::encode(&tempo)[..32],
        "658faa385070e074c85bf6b568cf0555"
    );
    assert!(hex::encode(&tempo).ends_with("5902"));
    assert_eq!(
        as_u64(
            &rt.decode_storage(SUBTENSOR, "Tempo", Some(&[0x68, 0x01]))
                .unwrap()
        ),
        Some(360)
    );
    // Uids is optional: absent means not registered.
    assert_eq!(
        rt.decode_storage(SUBTENSOR, "Uids", None).unwrap(),
        Value::Null
    );
    // A value query falls back to the metadata default.
    assert!(
        as_bool(
            &rt.decode_storage(SUBTENSOR, "CommitRevealWeightsEnabled", None)
                .unwrap()
        )
        .is_some()
    );
    let account = rt
        .storage_key("System", "Account", &[Value::Bytes(vec![0x22; 32])])
        .unwrap();
    assert_eq!(account.len(), 32 + 16 + 32);
    let info = rt.decode_storage("System", "Account", None).unwrap();
    assert_eq!(as_u64(field(&info, "nonce").unwrap()), Some(0));
    assert!(rt.storage_key(SUBTENSOR, "Tempo", &[]).is_err());
    assert!(rt.storage_key(SUBTENSOR, "NoSuchItem", &net).is_err());
    assert!(rt.has_storage("System", "LastRuntimeUpgrade"));
}

#[test]
fn the_payload_is_upstreams_scale_and_decrypts_with_the_round_signature() {
    let hotkey = [7u8; 32];
    let payload = commit_payload(hotkey, &[1, 2], &[65535, 1], 42);
    // Vec<u8>(32) ++ Vec<u16> ++ Vec<u16> ++ u64.
    assert_eq!(payload.len(), 1 + 32 + 1 + 4 + 1 + 4 + 8);
    let ciphertext = encrypt(&payload, 1000).unwrap();
    let again = encrypt(&payload, 1000).unwrap();
    assert_ne!(ciphertext, again, "encryption is randomized");
    assert!(ciphertext.len() < YUMA_COMMIT_LIMIT);
    let signature = hex::decode(ROUND_1000_SIGNATURE).unwrap();
    assert_eq!(decrypt(&ciphertext, &signature).unwrap(), payload);
    assert_eq!(decrypt(&again, &signature).unwrap(), payload);
    // Another round's signature does not open it.
    let wrong = encrypt(&payload, 1001).unwrap();
    assert!(decrypt(&wrong, &signature).is_err());
}

#[test]
fn reveal_round_matches_rtaos_clock_vector() {
    // rtao-core-ext timelock_commit::clock_and_security_offset_pin_round.
    let state = schedule(50, 10, 0, 0, 0, 10);
    let genesis = rtao_core::timelock::constants::GENESIS_TIME as f64;
    assert_eq!(reveal_round(genesis + 100.0, &state, 1).unwrap(), 245);
    assert_eq!(reveal_round(genesis + 103.0, &state, 1).unwrap(), 246);
}

#[test]
fn epochs_follow_the_pallet_schedule() {
    // Testnet 601 shape: tempo 360, epoch 6 began at 8,333,679.
    let state = schedule(360, 8_333_679, 0, 6, 100, 8_333_779);
    assert_eq!(commit_epoch(&state), 6);
    let next = next_epoch_block(&state, 400).unwrap();
    assert!(next > 8_333_779 && next <= 8_333_679 + 361);
    let reveal = first_reveal_block(&state, 1).unwrap();
    assert_eq!(reveal, next);
    let late = schedule(360, 8_333_679, 0, 6, 359, next - 1);
    assert_eq!(
        commit_epoch(&late),
        7,
        "a commit in the boundary block lands in the next epoch"
    );
}

#[test]
fn opaque_metadata_unwraps_the_runtime_api_option() {
    let mut scale = vec![1u8];
    let body = b"meta\x0fpayload".to_vec();
    rtao_core::codec::encode::compact(body.len() as u128, &mut scale).unwrap();
    scale.extend_from_slice(&body);
    assert_eq!(opaque_metadata(&scale).unwrap(), body);
    assert_eq!(
        opaque_metadata(&[0]).unwrap_err().code,
        "metadata_unavailable"
    );
    scale.push(0);
    assert!(opaque_metadata(&scale).is_err());
}
