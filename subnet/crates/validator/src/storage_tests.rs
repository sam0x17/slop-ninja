//! Storage the live ports read by name, against the recorded 486 metadata.
//!
//! The validator reads `SubtensorModule` items by name with live key types and
//! hashers, passing integer keys as `Value::Int` (see `live::keys`). These tests
//! pin each key the live ports build to a storage key built independently of
//! rtao: subtensor's declared hashers applied with a separate xxh64/BLAKE2b
//! implementation to explicitly little-endian key material. On 2026-10-30 that
//! implementation's keys for `Uids`, `Keys`, `BlockAtRegistration`,
//! `SubnetworkN`, `SubnetOwnerHotkey`, `LastEpochBlock` and `ValidatorPermit`
//! on subnet 601 returned the expected values from testnet. Values are SCALE
//! bytes written by hand and decoded through rtao's by-name decoder, exactly as
//! `query_batch_dyn` does, then through the live ports' value parsers.
//!
//! The metadata is test data, not an accepted-runtime list (see
//! `payload_tests`); a later runtime that changes one of these items changes
//! the vectors, never what production accepts.

use anyhow::{Context, Result};
use rtao_core::DynValue;
use rtao_core::codec::Value;
use rtao_primitives::{AccountId, NetUid};
use rtao_runtime::RuntimeHandle;
use slop_ninja_protocol::NeuronIdentity;

use crate::live::{
    account_from_value, commits_from_value, flag_from_value, keys, neurons_from_values, u128_of,
    uid_from_value, weights_from_value,
};

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const NETUID: NetUid = NetUid(601);
/// Synthetic accounts; no real key is involved.
const HOTKEY: AccountId = AccountId([0x22; 32]);
const OTHER: [u8; 32] = [0x33; 32];
const PREFIX: &str = "0x658faa385070e074c85bf6b568cf0555";

fn runtime() -> RuntimeHandle {
    RuntimeHandle::parse(METADATA, 486, 1, 42).expect("recorded metadata parses")
}

fn entry<'a>(
    runtime: &'a RuntimeHandle,
    item: &str,
) -> Result<&'a rtao_core::runtime::StorageInfo> {
    runtime
        .storage_entry("SubtensorModule", item)
        .with_context(|| format!("SubtensorModule.{item} is not in the recorded metadata"))
}

fn storage_key(runtime: &RuntimeHandle, item: &str, params: &[Value]) -> Result<String> {
    let key = runtime
        .core()
        .storage_key(entry(runtime, item)?, params)
        .map_err(|error| anyhow::anyhow!("{item} key: {error}"))?;
    Ok(format!("0x{}", hex::encode(key)))
}

/// Decode raw storage bytes (`None`: the key is absent) as `query_batch_dyn` does.
fn decode(runtime: &RuntimeHandle, item: &str, raw: Option<&[u8]>) -> Result<DynValue> {
    let value =
        rtao_core::ops::storage::decode_storage_bytes(runtime.core(), entry(runtime, item)?, raw)
            .map_err(|error| anyhow::anyhow!("{item} value: {error}"))?;
    Ok(DynValue::new(value))
}

fn compact_len(len: usize, out: &mut Vec<u8>) {
    if len < 64 {
        out.push((len as u8) << 2);
    } else {
        assert!(len < 1 << 14);
        out.extend_from_slice(&(((len as u16) << 2) | 1).to_le_bytes());
    }
}

#[test]
fn by_name_storage_keys_match_independently_built_keys() -> Result<()> {
    let runtime = runtime();
    let hot = "22".repeat(32);
    // (item, live key arguments, declared hashers, independent key)
    let cases: Vec<(&str, Vec<Value>, &[&str], String)> = vec![
        (
            "NetworksAdded",
            keys::subnet(NETUID),
            &["Identity"],
            format!("{PREFIX}0e30450fc4d507a846032a7fa65d9a435902"),
        ),
        (
            "CommitRevealWeightsEnabled",
            keys::subnet(NETUID),
            &["Identity"],
            format!("{PREFIX}8fb0d3eb3d4ea01122652b09d1d43c655902"),
        ),
        (
            "SubnetworkN",
            keys::subnet(NETUID),
            &["Identity"],
            format!("{PREFIX}a1048e9d244171852dfe8db314dc68ca5902"),
        ),
        (
            "SubnetOwnerHotkey",
            keys::subnet(NETUID),
            &["Identity"],
            format!("{PREFIX}68b7553499633fe05caf4d8a51aefe5c5902"),
        ),
        (
            "RevealPeriodEpochs",
            keys::subnet(NETUID),
            &["Twox64Concat"],
            format!("{PREFIX}8f232bcbac768c1d6201850c60f161ed4caf2598548d0fbd5902"),
        ),
        (
            "Uids",
            keys::hotkey(NETUID, HOTKEY),
            &["Identity", "Blake2_128Concat"],
            format!(
                "{PREFIX}aab1b4e78e1ea8305462ee53b3686dc8590235d2115efb6fd10f123610fe629899cb{hot}"
            ),
        ),
        (
            "Keys",
            keys::neuron(NETUID, 3),
            &["Identity", "Identity"],
            format!("{PREFIX}9f99a2ce711f3a31b2fc05604c93f17959020300"),
        ),
        (
            "BlockAtRegistration",
            keys::neuron(NETUID, 3),
            &["Identity", "Identity"],
            format!("{PREFIX}5306000b65aa58c97bf98ad9f861904959020300"),
        ),
        (
            "Weights",
            keys::weights(NETUID, 0, 2)?,
            &["Identity", "Identity"],
            format!("{PREFIX}a1e7036061f484bc3048a64b64e35ec459020200"),
        ),
        (
            // Mechanism 1: storage index 4096 + 601 = 0x1259.
            "Weights",
            keys::weights(NETUID, 1, 2)?,
            &["Identity", "Identity"],
            format!("{PREFIX}a1e7036061f484bc3048a64b64e35ec459120200"),
        ),
        (
            "TimelockedWeightCommits",
            keys::timelocked_commits(NETUID, 0, 23_148)?,
            &["Twox64Concat", "Twox64Concat"],
            format!(
                "{PREFIX}119e1c428ff5c7c6dac4c888a10d14434caf2598548d0fbd5902fdefdf070ad718296c5a000000000000"
            ),
        ),
        (
            "TimelockedWeightCommits",
            keys::timelocked_commits(NETUID, 1, 23_148)?,
            &["Twox64Concat", "Twox64Concat"],
            format!(
                "{PREFIX}119e1c428ff5c7c6dac4c888a10d1443d1372ce868b376be5912fdefdf070ad718296c5a000000000000"
            ),
        ),
    ];
    for (item, params, hashers, expected) in cases {
        assert_eq!(
            entry(&runtime, item)?.hashers,
            hashers,
            "{item}: live metadata hashers differ from subtensor's declaration"
        );
        assert_eq!(
            storage_key(&runtime, item, &params)?,
            expected,
            "{item}: by-name key differs from the independently built key"
        );
        // The encoder's u128 form of the same integers yields the same key.
        let widened: Vec<Value> = params
            .iter()
            .map(|value| match value {
                Value::Int(n) => Value::Uint(u128::try_from(*n).expect("keys are unsigned")),
                other => other.clone(),
            })
            .collect();
        assert_eq!(storage_key(&runtime, item, &widened)?, expected, "{item}");
    }
    // Every integer key is narrowed to its declared width, never truncated.
    let entry_weights = entry(&runtime, "Weights")?;
    for oversized in [
        vec![Value::Int(65_536), Value::Int(2)],
        vec![Value::Int(601), Value::Int(-1)],
    ] {
        assert!(
            runtime
                .core()
                .storage_key(entry_weights, &oversized)
                .is_err()
        );
    }
    // Mechanism 16 would need a storage index above u16.
    assert!(keys::storage_index(NETUID, 15).is_ok());
    assert!(keys::weights(NETUID, 16, 0).is_err());
    assert!(keys::timelocked_commits(NETUID, 16, 0).is_err());
    Ok(())
}

#[test]
fn by_name_storage_values_decode_through_live_metadata() -> Result<()> {
    let runtime = runtime();
    let a = HOTKEY.0;

    // Registration snapshot: Keys renders AccountId32 as SS58, decoded back.
    let hotkeys = vec![
        decode(&runtime, "Keys", Some(&a))?,
        decode(&runtime, "Keys", Some(&OTHER))?,
    ];
    assert!(matches!(hotkeys[0].as_core(), Value::Str(_)));
    let blocks = vec![
        decode(
            &runtime,
            "BlockAtRegistration",
            Some(&8_331_760u64.to_le_bytes()),
        )?,
        decode(
            &runtime,
            "BlockAtRegistration",
            Some(&8_333_167u64.to_le_bytes()),
        )?,
    ];
    assert_eq!(
        neurons_from_values(&hotkeys, &blocks)?,
        vec![
            NeuronIdentity {
                uid: 0,
                hotkey: slop_ninja_protocol::AccountId(a),
                registration_block: 8_331_760,
            },
            NeuronIdentity {
                uid: 1,
                hotkey: slop_ninja_protocol::AccountId(OTHER),
                registration_block: 8_333_167,
            },
        ]
    );
    assert!(neurons_from_values(&hotkeys, &blocks[..1]).is_err());
    let count = decode(&runtime, "SubnetworkN", Some(&4u16.to_le_bytes()))?;
    assert_eq!(u128_of(&count, "SubnetworkN")?, 4);
    assert_eq!(
        u128_of(&decode(&runtime, "SubnetworkN", None)?, "SubnetworkN")?,
        0
    );

    // Uids is an OptionQuery: absent means unregistered.
    assert_eq!(
        uid_from_value(&decode(&runtime, "Uids", Some(&[2, 0]))?)?,
        Some(2)
    );
    assert_eq!(uid_from_value(&decode(&runtime, "Uids", None)?)?, None);

    let owner = decode(&runtime, "SubnetOwnerHotkey", Some(&OTHER))?;
    assert_eq!(account_from_value(owner.as_core())?, OTHER);

    // Startup's by-name fallback: an absent NetworksAdded is false, an absent
    // CommitRevealWeightsEnabled is true (the chain default).
    let flag = |item: &str, raw: Option<&[u8]>| -> Result<bool> {
        flag_from_value(&decode(&runtime, item, raw)?, item)
    };
    assert!(!flag("NetworksAdded", None)?);
    assert!(flag("NetworksAdded", Some(&[1]))?);
    assert!(flag("CommitRevealWeightsEnabled", None)?);
    assert!(!flag("CommitRevealWeightsEnabled", Some(&[0]))?);
    assert!(flag_from_value(&count, "SubnetworkN").is_err());

    // RevealPeriodEpochs defaults to 1.
    let period = decode(&runtime, "RevealPeriodEpochs", None)?;
    assert_eq!(u128_of(&period, "RevealPeriodEpochs")?, 1);
    let period = decode(&runtime, "RevealPeriodEpochs", Some(&3u64.to_le_bytes()))?;
    assert_eq!(u128_of(&period, "RevealPeriodEpochs")?, 3);

    // Weights: Vec<(u16, u16)>, empty when absent.
    let mut raw = Vec::new();
    compact_len(2, &mut raw);
    for (uid, weight) in [(3u16, 65_535u16), (1, 7)] {
        raw.extend_from_slice(&uid.to_le_bytes());
        raw.extend_from_slice(&weight.to_le_bytes());
    }
    let weights = decode(&runtime, "Weights", Some(&raw))?;
    assert_eq!(
        weights_from_value(weights.as_core())?,
        vec![(3, 65_535), (1, 7)]
    );
    assert_eq!(
        weights_from_value(decode(&runtime, "Weights", None)?.as_core())?,
        Vec::new()
    );

    // TimelockedWeightCommits: VecDeque<(AccountId, commit block,
    // BoundedVec<u8> ciphertext, reveal round)>, empty when absent.
    let mut raw = Vec::new();
    compact_len(2, &mut raw);
    for (account, block, cipher_len, round) in [
        (a, 8_333_400u64, 100usize, 27_123_456u64),
        (OTHER, 8_333_401, 3, 27_123_460),
    ] {
        raw.extend_from_slice(&account);
        raw.extend_from_slice(&block.to_le_bytes());
        compact_len(cipher_len, &mut raw);
        raw.extend((0..cipher_len).map(|i| (i as u8).wrapping_mul(37) ^ 0xa5));
        raw.extend_from_slice(&round.to_le_bytes());
    }
    let commits = decode(&runtime, "TimelockedWeightCommits", Some(&raw))?;
    assert_eq!(
        commits_from_value(commits.as_core())?,
        vec![(a, 8_333_400, 27_123_456), (OTHER, 8_333_401, 27_123_460)]
    );
    assert_eq!(
        commits_from_value(decode(&runtime, "TimelockedWeightCommits", None)?.as_core())?,
        Vec::new()
    );
    Ok(())
}
