//! Read-only chain observations the cohort engine needs beyond headers: the
//! subnet's epoch index and last epoch block, read from finalized state with one
//! `state_queryStorageAt` call at an RPC-confirmed block hash.
//!
//! Cohort boundaries are finalized increments of `SubtensorModule.SubnetEpochIndex`
//! for the subnet, never block number modulo the tempo, because epochs can slip.
//! Both items are `StorageMap<NetUid, u64>` with the `Identity` hasher (rtao's
//! generated storage surface), so a key is
//! `twox128("SubtensorModule") ++ twox128(item) ++ u16_le(netuid)`.

use crate::wire::{BlockRef, Id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(acc: u64, val: u64) -> u64 {
    (acc ^ round(0, val)).wrapping_mul(P1).wrapping_add(P4)
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"))
}

/// XXH64 of `input` with `seed`.
pub fn xxh64(input: &[u8], seed: u64) -> u64 {
    let len = input.len();
    let mut i = 0;
    let mut h = if len >= 32 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while i + 32 <= len {
            for (lane, acc) in v.iter_mut().enumerate() {
                *acc = round(*acc, u64_at(input, i + lane * 8));
            }
            i += 32;
        }
        let mut h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
        for acc in v {
            h = merge(h, acc);
        }
        h
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(len as u64);
    while i + 8 <= len {
        h ^= round(0, u64_at(input, i));
        h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        i += 8;
    }
    if i + 4 <= len {
        let w = u32::from_le_bytes(input[i..i + 4].try_into().expect("4 bytes"));
        h ^= u64::from(w).wrapping_mul(P1);
        h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        i += 4;
    }
    while i < len {
        h ^= u64::from(input[i]).wrapping_mul(P5);
        h = h.rotate_left(11).wrapping_mul(P1);
        i += 1;
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}

/// Substrate's `twox_128`.
pub fn twox128(input: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&xxh64(input, 0).to_le_bytes());
    out[8..].copy_from_slice(&xxh64(input, 1).to_le_bytes());
    out
}

/// Storage key of a `SubtensorModule` map keyed by `NetUid` with the Identity hasher.
pub fn netuid_map_key(item: &str, netuid: u16) -> String {
    let mut key = twox128(b"SubtensorModule").to_vec();
    key.extend_from_slice(&twox128(item.as_bytes()));
    key.extend_from_slice(&netuid.to_le_bytes());
    format!("0x{}", hex::encode(key))
}

/// The epoch facts at one confirmed block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochObservation {
    pub block: BlockRef,
    pub index: u64,
    pub last_epoch_block: u64,
}

pub fn epoch_request(id: u64, netuid: u16, at: &Id) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "state_queryStorageAt",
        "params": [
            [netuid_map_key("SubnetEpochIndex", netuid), netuid_map_key("LastEpochBlock", netuid)],
            at,
        ],
    })
    .to_string()
}

fn scale_u64(value: &Value) -> Option<u64> {
    match value {
        // Default-valued items are absent from storage.
        Value::Null => Some(0),
        Value::String(s) => {
            let b = hex::decode(s.strip_prefix("0x")?).ok()?;
            Some(u64::from_le_bytes(b.as_slice().try_into().ok()?))
        }
        _ => None,
    }
}

/// Parses the `state_queryStorageAt` reply for [`epoch_request`].
pub fn parse_epoch(id: u64, body: &[u8], netuid: u16, at: BlockRef) -> Option<EpochObservation> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v["id"] != id {
        return None;
    }
    let sets = v["result"].as_array()?;
    let set = sets.first()?;
    let want = Id::parse(set["block"].as_str()?).ok()?;
    if want != at.hash {
        return None;
    }
    let changes = set["changes"].as_array()?;
    let find = |key: String| -> Option<u64> {
        let pair = changes.iter().find(|c| c[0].as_str() == Some(key.as_str()))?;
        scale_u64(&pair[1])
    };
    Some(EpochObservation {
        block: at,
        index: find(netuid_map_key("SubnetEpochIndex", netuid))?,
        last_epoch_block: find(netuid_map_key("LastEpochBlock", netuid))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twox128_matches_substrate_vectors() {
        assert_eq!(
            hex::encode(twox128(b"System")),
            "26aa394eea5630e07c48ae0c9558cef7"
        );
        assert_eq!(
            hex::encode(twox128(b"Balances")),
            "c2261276cc9d1f8598ea4b6a74b15c2f"
        );
        // XXH64 reference values (empty input, and a 32+ byte input).
        assert_eq!(xxh64(b"", 0), 0xEF46_DB37_51D8_E999);
        assert_eq!(
            xxh64(b"Nobody inspects the spammish repetition", 0),
            0xFBCE_A83C_8A37_8BF1
        );
    }

    #[test]
    fn epoch_reply_is_bound_to_the_requested_block() {
        let at = BlockRef {
            number: 10,
            hash: Id([7; 32]),
        };
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 9, "result": [{
            "block": Id([7; 32]),
            "changes": [
                [netuid_map_key("SubnetEpochIndex", 601), "0x0600000000000000"],
                [netuid_map_key("LastEpochBlock", 601), "0x6f297f0000000000"],
            ]}]})
        .to_string();
        let obs = parse_epoch(9, body.as_bytes(), 601, at).unwrap();
        assert_eq!(obs.index, 6);
        assert_eq!(obs.last_epoch_block, 8_333_679);
        let other = BlockRef {
            number: 10,
            hash: Id([8; 32]),
        };
        assert!(parse_epoch(9, body.as_bytes(), 601, other).is_none());
        assert!(parse_epoch(8, body.as_bytes(), 601, at).is_none());
    }
}
