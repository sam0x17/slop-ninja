//! Commitment-log tests over the recorded runtime-486 metadata: keys and
//! values by name, the commitment event's extrinsic index, the log's
//! queries, and one whole reader pass from replies as a node returns them.

use super::*;
use crate::chain::ChainRuntime;
use crate::sql::EVAL_COHORT_SCHEMA;
use crate::testing::{Sqlite, tx};
use parity_scale_codec::{Compact, Encode};

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
const NETUID: u16 = 601;
/// A reader share large enough that the budget never limits a test round.
const BUDGET: u64 = 1_000;

fn rt() -> ChainRuntime {
    ChainRuntime::parse(METADATA, 486, 1).unwrap()
}

fn who(n: u8) -> AccountId {
    AccountId([n; 32])
}

fn encode(rt: &ChainRuntime, pallet: &str, item: &str, v: &Value) -> Vec<u8> {
    let entry = rt.core().storage_entry(pallet, item).unwrap();
    let mut out = Vec::new();
    rt.core()
        .encode_id(entry.value_type, v, &mut out)
        .unwrap_or_else(|e| panic!("{pallet}.{item}: {e}"));
    out
}

fn registration(block: u64, payload: &[u8]) -> Value {
    Value::record(vec![
        ("deposit".into(), Value::Int(0)),
        ("block".into(), Value::Int(i128::from(block))),
        (
            "info".into(),
            Value::record(vec![(
                "fields".into(),
                Value::List(vec![Value::record(vec![(
                    format!("Raw{}", payload.len()),
                    Value::Bytes(payload.to_vec()),
                )])]),
            )]),
        ),
    ])
}

fn event(phase: Option<u32>, netuid: u16, w: &AccountId) -> Value {
    Value::record(vec![
        (
            "phase".into(),
            match phase {
                Some(i) => Value::record(vec![("ApplyExtrinsic".into(), Value::Int(i128::from(i)))]),
                None => Value::str("Initialization"),
            },
        ),
        (
            "event".into(),
            Value::record(vec![(
                PALLET.into(),
                Value::record(vec![(
                    EVENT.into(),
                    Value::record(vec![
                        ("netuid".into(), Value::Int(i128::from(netuid))),
                        ("who".into(), Value::Bytes(w.0.to_vec())),
                    ]),
                )]),
            )]),
        ),
        ("topics".into(), Value::List(vec![])),
    ])
}

fn empty_commitment(block: u64) -> Value {
    Value::record(vec![
        ("deposit".into(), Value::Int(0)),
        ("block".into(), Value::Int(i128::from(block))),
        (
            "info".into(),
            Value::record(vec![("fields".into(), Value::List(vec![]))]),
        ),
    ])
}

fn payload(n: u8) -> Vec<u8> {
    let mut p = b"sna1".to_vec();
    p.extend_from_slice(&7u64.to_le_bytes());
    p.extend_from_slice(&[n; 32]);
    p
}

#[test]
fn keys_values_and_events_decode_by_name() {
    let rt = rt();
    let prefix = commitments_prefix(&rt, NETUID).unwrap();
    let key = commitment_key(&rt, NETUID, &who(3)).unwrap();
    assert!(key.starts_with(&prefix), "a hotkey's key extends the subnet prefix");
    assert_eq!(hotkey_of_key(&rt, &key).unwrap(), who(3));
    assert_ne!(commitments_prefix(&rt, 602).unwrap(), prefix);
    let raw = encode(&rt, PALLET, ITEM, &registration(8_340_000, &payload(9)));
    assert_eq!(
        decode_commitment(&rt, &raw).unwrap(),
        Committed {
            block: 8_340_000,
            payload: payload(9)
        }
    );
    // The last extrinsic that emitted the hotkey's event on this subnet.
    let events = Value::List(vec![
        event(Some(1), NETUID, &who(3)),
        event(Some(2), 602, &who(3)),
        event(Some(4), NETUID, &who(4)),
        event(Some(5), NETUID, &who(3)),
        event(None, NETUID, &who(3)),
    ]);
    let raw = encode(&rt, "System", "Events", &events);
    assert_eq!(
        commitment_extrinsic(&rt, &raw, NETUID, &who(3)).unwrap(),
        Position::At(5)
    );
    assert_eq!(
        commitment_extrinsic(&rt, &raw, NETUID, &who(4)).unwrap(),
        Position::At(4)
    );
    assert_eq!(
        commitment_extrinsic(&rt, &raw, NETUID, &who(5)).unwrap(),
        Position::Absent
    );
    // One extrinsic that committed for two hotkeys on the subnet (a batch):
    // neither has a position the protocol orders.
    let batch = encode(
        &rt,
        "System",
        "Events",
        &Value::List(vec![
            event(Some(7), NETUID, &who(3)),
            event(Some(7), NETUID, &who(4)),
            event(Some(7), 602, &who(5)),
        ]),
    );
    assert_eq!(
        commitment_extrinsic(&rt, &batch, NETUID, &who(3)).unwrap(),
        Position::Shared(7)
    );
    assert_eq!(
        commitment_extrinsic(&rt, &batch, NETUID, &who(5)).unwrap(),
        Position::Absent
    );
    assert_eq!(
        commitment_extrinsic(&rt, &batch, 602, &who(5)).unwrap(),
        Position::At(7),
        "another subnet's commitment"
    );
    // An empty field list (set_commitment accepts one) opens no submission.
    let empty = encode(&rt, PALLET, ITEM, &empty_commitment(8_340_060));
    assert_eq!(
        decode_commitment(&rt, &empty).unwrap(),
        Committed {
            block: 8_340_060,
            payload: vec![]
        }
    );
}

#[test]
fn the_log_answers_latest_and_last_before_the_cutoff() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let e = |block: u64, x: u32| LogEntry {
        hotkey: who(1),
        block,
        block_hash: format!("0x{}", hex::encode([block as u8; 32])),
        extrinsic: x,
        payload: payload(block as u8),
        uid: Some(3),
        registration_block: Some(10),
    };
    for (b, x) in [(100, 1), (200, 0), (300, 2)] {
        tx(&db, |s| record(s, &e(b, x), 1)).unwrap();
    }
    assert_eq!(tx(&db, |s| latest(s, &who(1))).unwrap().unwrap().block, 300);
    assert_eq!(
        tx(&db, |s| last_before(s, &who(1), 300)).unwrap().unwrap().block,
        200,
        "the cutoff block itself does not count"
    );
    assert!(tx(&db, |s| last_before(s, &who(1), 100)).unwrap().is_none());
    assert!(tx(&db, |s| latest(s, &who(2))).unwrap().is_none());
    // Recording again is idempotent.
    tx(&db, |s| record(s, &e(300, 2), 2)).unwrap();
    assert_eq!(db.count("commit_log"), 3);
}

fn log_at(db: &Sqlite, n: u8, block: u64, uid: Option<u16>, registration_block: Option<u64>) {
    let e = LogEntry {
        hotkey: who(n),
        block,
        block_hash: format!("0x{}", hex::encode([block as u8; 32])),
        extrinsic: 0,
        payload: payload(n),
        uid,
        registration_block,
    };
    tx(db, |s| record(s, &e, 1)).unwrap();
}

fn map_rows(v: &serde_json::Value) -> Vec<(u64, String)> {
    v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["uid"].as_u64().unwrap(), r["hotkey"].as_str().unwrap().to_owned()))
        .collect()
}

#[test]
fn the_uid_map_follows_the_latest_registration_up_to_the_boundary() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let h = |n: u8| hex::encode(who(n).0);
    // Hotkey 1 holds UID 3, commits twice.
    log_at(&db, 1, 100, Some(3), Some(50));
    log_at(&db, 1, 150, Some(3), Some(50));
    // Hotkey 2 held UID 4, then UID 4 was re-registered by hotkey 5.
    log_at(&db, 2, 110, Some(4), Some(60));
    log_at(&db, 5, 160, Some(4), Some(140));
    // Hotkey 6 held UID 7, then moved to UID 8: UID 7 drops it.
    log_at(&db, 6, 120, Some(7), Some(70));
    log_at(&db, 6, 170, Some(8), Some(165));
    // Hotkey 9 held UID 9, then committed with no UID: dropped.
    log_at(&db, 9, 130, Some(9), Some(80));
    log_at(&db, 9, 175, None, None);
    // After the boundary: hotkey 10 takes UID 3; not read yet.
    log_at(&db, 10, 400, Some(3), Some(390));
    // After the boundary, holders move: hotkey 1 to UID 5, hotkey 5 to no
    // UID. The boundary's map still names them under their old UIDs.
    log_at(&db, 1, 380, Some(5), Some(370));
    log_at(&db, 5, 390, None, None);
    let v = tx(&db, |s| uid_map(s, 12, 360)).unwrap();
    assert_eq!((v["rule"].as_str(), v["epoch"].as_u64(), v["block"].as_u64()), (Some(UID_MAP_RULE), Some(12), Some(360)));
    assert_eq!(map_rows(&v), vec![(3, h(1)), (4, h(5)), (8, h(6))]);
    // The next boundary reads the later registration of UID 3 and the moves.
    let v = tx(&db, |s| uid_map(s, 13, 720)).unwrap();
    assert_eq!(map_rows(&v), vec![(3, h(10)), (5, h(1)), (8, h(6))]);
    // Before any commitment: an empty map, not null.
    let v = tx(&db, |s| uid_map(s, 1, 99)).unwrap();
    assert!(map_rows(&v).is_empty());
    assert!(v.get("refused").is_none());
}

#[test]
fn a_tied_uid_is_left_out_and_an_invalid_log_row_refuses_the_map() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let h = |n: u8| hex::encode(who(n).0);
    log_at(&db, 1, 100, Some(3), Some(50));
    // Three hotkeys tie for UID 4 (same registration and commitment block).
    for n in [2, 5, 6] {
        log_at(&db, n, 120, Some(4), Some(60));
    }
    log_at(&db, 9, 130, Some(9), Some(80));
    let v = tx(&db, |s| uid_map(s, 2, 360)).unwrap();
    assert_eq!(map_rows(&v), vec![(3, h(1)), (9, h(9))]);
    // A row that breaks the log's invariants refuses the map whole; the
    // call itself still answers.
    tx(&db, |s| {
        s.exec(
            "INSERT INTO commit_log (hotkey, block, block_hash, extrinsic, payload, uid, registration_block, logged_ms)
             VALUES ('zz', 140, '0x00', 0, '00', 11, 90, 1)",
            &[],
        )
    })
    .unwrap();
    let v = tx(&db, |s| uid_map(s, 2, 360)).unwrap();
    assert!(v["rows"].is_null());
    assert_eq!(v["refused"], "invalid_log");
    assert_eq!((v["epoch"].as_u64(), v["block"].as_u64()), (Some(2), Some(360)));
}

#[test]
fn a_uid_map_over_its_bound_is_refused_whole() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    for uid in 0..=UID_MAP_ROWS as u16 {
        let e = LogEntry {
            hotkey: AccountId([&[(uid >> 8) as u8, uid as u8][..], &[7; 30]].concat().try_into().unwrap()),
            block: 100,
            block_hash: "0x00".into(),
            extrinsic: 0,
            payload: payload(1),
            uid: Some(uid),
            registration_block: Some(10),
        };
        tx(&db, |s| record(s, &e, 1)).unwrap();
    }
    let v = tx(&db, |s| uid_map(s, 1, 360)).unwrap();
    assert!(v["rows"].is_null());
    assert_eq!(v["refused"], "over_bound");
    assert_eq!(v["uids"].as_u64(), Some(UID_MAP_ROWS as u64 + 1));
    // At the bound exactly, the map is whole.
    tx(&db, |s| s.exec("DELETE FROM commit_log WHERE uid = 0", &[])).unwrap();
    let v = tx(&db, |s| uid_map(s, 1, 360)).unwrap();
    assert_eq!(map_rows(&v).len(), UID_MAP_ROWS);
}

#[test]
fn the_uid_map_bound_equals_the_dashboard_contracts() {
    assert_eq!(UID_MAP_ROWS, dashboard_contract::uid_map::ROWS);
    // The per-hotkey admission report: the map plus as many outside it,
    // the dashboard roster's bound.
    assert_eq!(crate::eval::admission::HOTKEY_ROWS, dashboard_contract::roster::ROWS);
    // The coordinator's registry read: one row per UID, the registry's.
    assert_eq!(crate::signing::registry::ROWS, dashboard_contract::registry::ROWS);
}

#[path = "../../../dashboard-feed/src/contract.rs"]
#[allow(dead_code)]
mod dashboard_contract;

fn reply(id: u64, result: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn answer(round: &Round, f: impl Fn(&Read) -> serde_json::Value) -> Vec<ReadResult> {
    round
        .reads
        .iter()
        .map(|r| ReadResult {
            tag: r.tag.clone(),
            id: r.id,
            reply: Some(reply(r.id, f(r))),
        })
        .collect()
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

#[test]
fn a_reader_pass_logs_a_new_commitment_with_its_position_and_registration() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let at = BlockRef {
        number: 8_340_100,
        hash: Id([0xab; 32]),
    };
    let at_hex = at.hash.to_hex();
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    // No runtime yet: the runtime version and metadata first.
    let round = tx(&db, |s| plan(s, None, NETUID, Some(at), &needs, BUDGET, 0)).unwrap();
    let mut opaque = vec![1u8];
    opaque.extend(Compact(METADATA.len() as u32).encode());
    opaque.extend_from_slice(METADATA);
    assert_eq!(round.reads.len(), 3, "version, metadata and the upgrade marker");
    let parsed = rt();
    assert_eq!(
        marker_key(),
        upgrade_key(&parsed).unwrap(),
        "the marker's fixed key is the item's key by name"
    );
    let up = encode(
        &parsed,
        "System",
        "LastRuntimeUpgrade",
        &Value::record(vec![
            ("spec_version".into(), Value::Int(486)),
            ("spec_name".into(), Value::str("node-subtensor")),
        ]),
    );
    let results = answer(&round, |r| {
        if r.tag.starts_with("version@") {
            serde_json::json!({"specVersion": 486, "transactionVersion": 1})
        } else if r.tag.starts_with("marker@") {
            serde_json::json!([{"block": at.hash.to_hex(), "changes": [[marker_key(), hx(&up)]]}])
        } else {
            serde_json::json!(hx(&opaque))
        }
    });
    let out = tx(&db, |s| ingest(s, None, NETUID, &results, 0)).unwrap();
    let (spec, txv, meta, _, marker) = out.runtime.unwrap();
    assert_eq!(
        (spec, txv, meta.as_slice(), marker.as_str()),
        (486, 1, METADATA, hex::encode(&up).as_str())
    );
    tx(&db, |s| store_runtime(s, spec, txv, &meta, at.number, &marker)).unwrap();
    let (spec, txv, meta) = tx(&db, load_runtime).unwrap().unwrap();
    let rt = ChainRuntime::parse(&meta, spec, txv).unwrap();
    // The poll: one key page, then the values with LastRuntimeUpgrade.
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    assert_eq!(round.reads.len(), 1);
    assert!(round.reads[0].tag.starts_with("keys@"));
    let key = commitment_key(&rt, NETUID, &who(3)).unwrap();
    let results = answer(&round, |_| serde_json::json!([key]));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    assert!(round.reads[0].tag.starts_with("values@"));
    let value = encode(&rt, PALLET, ITEM, &registration(8_340_050, &payload(9)));
    let upgrade = upgrade_key(&rt).unwrap();
    let up = encode(
        &rt,
        "System",
        "LastRuntimeUpgrade",
        &Value::record(vec![
            ("spec_version".into(), Value::Int(486)),
            ("spec_name".into(), Value::str("node-subtensor")),
        ]),
    );
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": at_hex, "changes": [[key, hx(&value)], [upgrade, hx(&up)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_000)).unwrap();
    assert!(tx(&db, |s| unresolved(s, &who(3))).unwrap(), "seen, not logged yet");
    // Resolution: the block hash, then its events and the hotkey's UID, then
    // the UID's registration block, all at the commitment block.
    let c_hash = Id([0xcd; 32]);
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 100_001)
    })
    .unwrap();
    assert!(round.reads[0].tag.starts_with("hash@"));
    let results = answer(&round, |_| serde_json::json!(c_hash.to_hex()));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_001)).unwrap();
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 100_002)
    })
    .unwrap();
    assert!(round.reads[0].tag.starts_with("events@"));
    let events = encode(
        &rt,
        "System",
        "Events",
        &Value::List(vec![event(Some(1), NETUID, &who(4)), event(Some(6), NETUID, &who(3))]),
    );
    let uid = encode(&rt, chain::SUBTENSOR, "Uids", &Value::Int(12));
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(3)).unwrap());
    let mk = marker_key();
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, hx(&events)], [uk, hx(&uid)], [mk, hx(&up)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_002)).unwrap();
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 100_003)
    })
    .unwrap();
    assert!(round.reads[0].tag.starts_with("reg@"));
    let rk = registration_key(&rt, NETUID, 12).unwrap();
    let reg = encode(&rt, chain::SUBTENSOR, "BlockAtRegistration", &Value::Int(8_330_000));
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[rk, hx(&reg)], [marker_key(), hx(&up)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_003)).unwrap();
    let e = tx(&db, |s| latest(s, &who(3))).unwrap().unwrap();
    assert_eq!(
        (e.block, e.extrinsic, e.uid, e.registration_block),
        (8_340_050, 6, Some(12), Some(8_330_000))
    );
    assert_eq!(e.payload, payload(9));
    assert!(!tx(&db, |s| unresolved(s, &who(3))).unwrap());
    // Nothing left to read until the next poll is due.
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_004)).unwrap();
    assert!(round.reads.is_empty());
    // A value decoded under another spec asks for the new runtime first.
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 200_000)).unwrap();
    let results = answer(&round, |_| serde_json::json!([key]));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 200_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 200_000)).unwrap();
    let up487 = encode(
        &rt,
        "System",
        "LastRuntimeUpgrade",
        &Value::record(vec![
            ("spec_version".into(), Value::Int(487)),
            ("spec_name".into(), Value::str("node-subtensor")),
        ]),
    );
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": at_hex, "changes": [[key, hx(&value)], [upgrade, hx(&up487)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 200_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 200_001)).unwrap();
    assert!(
        round.reads.iter().any(|r| r.tag.starts_with("metadata@")),
        "spec 487 seen: re-read the runtime"
    );
    // Status is content-free.
    let st = tx(&db, status).unwrap();
    assert!(!st.to_string().contains(&hex::encode(payload(9))));
}

#[test]
fn a_commitment_whose_block_state_is_gone_stays_unresolved() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let rt = rt();
    tx(&db, |s| {
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage, block_hash) VALUES (?, 5, '00', 9, 'events', ?)",
            &[s_hex(&who(3)), SqlValue::Text(Id([1; 32]).to_hex())],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
    let at = BlockRef {
        number: 10,
        hash: Id([2; 32]),
    };
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 0)
    })
    .unwrap();
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(3)).unwrap());
    let gone = |round: &Round| {
        answer(round, |_| serde_json::json!([{"block": Id([1; 32]).to_hex(), "changes": [[ek, null], [uk, null]]}]))
    };
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &gone(&round), 0)).unwrap();
    // An answer without state is never final (fix round 2, finding 7): the
    // stage is read again after a backoff (from the archive endpoint when
    // configured), and after a longer one when that answer has none either.
    let backoff = crate::eval::admission::backoff_ms;
    assert!(tx(&db, |s| unresolved(s, &who(3))).unwrap());
    assert!(tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 1)).unwrap().reads.is_empty());
    let t1 = backoff(1);
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t1)).unwrap();
    assert!(round.reads[0].archive);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &gone(&round), t1)).unwrap();
    assert!(tx(&db, |s| unresolved(s, &who(3))).unwrap(), "still pending");
    assert!(tx(&db, |s| terminal(s, &who(3))).unwrap().is_none());
    let t2 = t1 + backoff(2);
    assert!(tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t2 - 1)).unwrap().reads.is_empty());
    assert!(!tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t2)).unwrap().reads.is_empty());
    assert_eq!(tx(&db, status).unwrap()["state_absent_pending"], 1);
    assert!(
        tx(&db, |s| latest(s, &who(3))).unwrap().is_none(),
        "never substituted by later state"
    );
}

fn s_hex(a: &AccountId) -> SqlValue {
    SqlValue::Text(hex::encode(a.0))
}

// ------------------------------------------- review round 1 regressions

/// A store with the runtime and its upgrade marker (spec 486) stored.
fn with_runtime() -> (Sqlite, ChainRuntime, Vec<u8>) {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let rt = rt();
    let up = marker_value(&rt, 486);
    tx(&db, |s| {
        store_runtime(s, 486, 1, METADATA, 8_340_100, &hex::encode(&up))
    })
    .unwrap();
    (db, rt, up)
}

fn marker_value(rt: &ChainRuntime, spec: i128) -> Vec<u8> {
    encode(
        rt,
        "System",
        "LastRuntimeUpgrade",
        &Value::record(vec![
            ("spec_version".into(), Value::Int(spec)),
            ("spec_name".into(), Value::str("node-subtensor")),
        ]),
    )
}

fn seen(db: &Sqlite, w: &AccountId) -> Option<(String, Option<String>)> {
    tx(db, |s| {
        let r = s.exec("SELECT stage, note FROM commit_seen WHERE hotkey = ?", &[s_hex(w)])?;
        Ok::<_, EvalError>(r.first().map(|r| (text(r, 0).unwrap().to_owned(), opt_text(r, 1))))
    })
    .unwrap()
}

fn pending_events(db: &Sqlite, w: &AccountId, block: u64, hash: Id) {
    tx(db, |s| {
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage, block_hash) VALUES (?, ?, '00', 9, 'events', ?)",
            &[s_hex(w), SqlValue::Integer(block as i64), SqlValue::Text(hash.to_hex())],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
}

fn error_reply(round: &Round, message: &str) -> Vec<ReadResult> {
    round
        .reads
        .iter()
        .map(|r| ReadResult {
            tag: r.tag.clone(),
            id: r.id,
            reply: Some(
                serde_json::json!({"jsonrpc": "2.0", "id": r.id, "error": {"code": 4003, "message": message}})
                    .to_string(),
            ),
        })
        .collect()
}

#[test]
fn one_value_that_opens_nothing_or_does_not_decode_never_aborts_the_round() {
    let (db, rt, up) = with_runtime();
    let at = BlockRef {
        number: 8_340_100,
        hash: Id([0xab; 32]),
    };
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    let keys: Vec<String> = [3, 4, 5]
        .iter()
        .map(|n| commitment_key(&rt, NETUID, &who(*n)).unwrap())
        .collect();
    let results = answer(&round, |_| serde_json::json!(keys));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    assert!(round.reads[0].tag.starts_with("values@"));
    let good = encode(&rt, PALLET, ITEM, &registration(8_340_050, &payload(9)));
    let empty = encode(&rt, PALLET, ITEM, &empty_commitment(8_340_060));
    let garbage = vec![0xffu8; 3];
    let upgrade = upgrade_key(&rt).unwrap();
    let results = answer(&round, |_| {
        serde_json::json!([{"block": at.hash.to_hex(), "changes": [[keys[0], hx(&good)], [keys[1], hx(&empty)],
            [keys[2], hx(&garbage)], [upgrade, hx(&up)]]}])
    });
    let out = tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_000)).unwrap();
    assert_eq!(out.applied, 1);
    assert!(
        tx(&db, |s| unresolved(s, &who(3))).unwrap(),
        "the valid commitment is seen"
    );
    assert_eq!(
        seen(&db, &who(4)).unwrap().0,
        "hash",
        "an empty one is resolved like any other"
    );
    assert_eq!(
        seen(&db, &who(5)),
        Some(("unresolvable".into(), Some("undecodable".into())))
    );
    // The next poll does not record the undecodable value again.
    assert_eq!(tx(&db, status).unwrap()["undecodable"], 1);
}

#[test]
fn state_a_node_no_longer_keeps_ends_the_resolution_once() {
    let (db, rt, _) = with_runtime();
    pending_events(&db, &who(3), 5, Id([1; 32]));
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 0)
    })
    .unwrap();
    assert!(round.reads[0].tag.starts_with("events@"));
    let results = error_reply(
        &round,
        "Client error: UnknownBlock: State already discarded for BlockId::Hash(0x0101..)",
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 0)).unwrap();
    assert_eq!(
        seen(&db, &who(3)),
        Some(("unresolvable".into(), Some("state_discarded".into())))
    );
    assert!(!tx(&db, |s| unresolved(s, &who(3))).unwrap());
    for k in 1..5u64 {
        let round = tx(&db, |s| {
            plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, k)
        })
        .unwrap();
        assert!(round.reads.is_empty(), "never read again");
    }
    // A key listing pinned to a block whose state is gone restarts at the head.
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    let results = answer(&round, |_| {
        serde_json::json!(vec![commitment_key(&rt, NETUID, &who(3)).unwrap()])
    });
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    assert!(round.reads[0].tag.starts_with("values@"));
    tx(&db, |s| {
        ingest(
            s,
            Some(&rt),
            NETUID,
            &error_reply(&round, "State already discarded"),
            100_000,
        )
    })
    .unwrap();
    let head = BlockRef {
        number: 10_500,
        hash: Id([3; 32]),
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(head), &needs, BUDGET, 100_000)).unwrap();
    assert_eq!(
        round.reads[0].tag, "keys@10500",
        "the listing restarts at the current block"
    );
    assert!(!discarded(&ReadResult {
        tag: "events@x".into(),
        id: 1,
        reply: Some(reply(1, serde_json::json!([]))),
    }));
}

#[test]
fn a_batch_commitment_is_unresolvable_and_a_runtime_change_at_the_block_is_read_again() {
    let (db, rt, up) = with_runtime();
    let c_hash = Id([0xcd; 32]);
    pending_events(&db, &who(3), 5, c_hash);
    pending_events(&db, &who(6), 6, c_hash);
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let batch = encode(
        &rt,
        "System",
        "Events",
        &Value::List(vec![event(Some(3), NETUID, &who(3)), event(Some(3), NETUID, &who(4))]),
    );
    let uid = encode(&rt, chain::SUBTENSOR, "Uids", &Value::Int(12));
    let up487 = marker_value(&rt, 487);
    // One large read a round: the older resolution first.
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 0)
    })
    .unwrap();
    assert_eq!(round.reads.len(), 1);
    assert_eq!(round.reads[0].tag, format!("events@{}@5", hex::encode(who(3).0)));
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(3)).unwrap());
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, hx(&batch)], [uk, hx(&uid)], [marker_key(), hx(&up)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 0)).unwrap();
    assert_eq!(
        seen(&db, &who(3)),
        Some(("unresolvable".into(), Some(SHARED_EXTRINSIC.into())))
    );
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 1)
    })
    .unwrap();
    let uk6 = uids_key(&rt, NETUID, &who(6)).unwrap();
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, hx(&batch)], [uk6, hx(&uid)], [marker_key(), hx(&up487)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 1)).unwrap();
    // Not terminal: the stage waits while the metadata is read at the
    // commitment's own block (the no-hard-runtime-gates rule).
    assert_eq!(seen(&db, &who(6)), Some(("events".into(), None)));
    assert!(tx(&db, |s| terminal(s, &who(6))).unwrap().is_none());
    assert_eq!(tx(&db, |s| terminal(s, &who(3))).unwrap(), Some((5, SHARED_EXTRINSIC.into())));
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 2)
    })
    .unwrap();
    assert_eq!(
        round.reads.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
        vec!["version@6", "metadata@6", "marker@6"],
        "the runtime is read at the commitment block"
    );
    assert_eq!(round.reads[1].body, crate::rpc::metadata_request(12, &c_hash));
    // With that runtime stored, the same stage is read again and resolves.
    tx(&db, |s| store_runtime(s, 487, 1, METADATA, 6, &hex::encode(&up487))).unwrap();
    let round = tx(&db, |s| {
        plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 3)
    })
    .unwrap();
    assert_eq!(round.reads[0].tag, format!("events@{}@6", hex::encode(who(6).0)));
    let single = encode(&rt, "System", "Events", &Value::List(vec![event(Some(2), NETUID, &who(6))]));
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, hx(&single)], [uk6, hx(&uid)], [marker_key(), hx(&up487)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 3)).unwrap();
    assert_eq!(seen(&db, &who(6)).map(|x| x.0), Some("reg".into()));
}

/// A runtime change at a block whose own metadata no endpoint serves any
/// longer: the resolution ends as `state_discarded` (never decoded with
/// another runtime), and the reader goes back to the head's runtime instead
/// of reading that block again forever.
#[test]
fn a_runtime_change_at_a_block_whose_metadata_is_gone_ends_the_resolution() {
    let (db, rt, _) = with_runtime();
    let c_hash = Id([0xce; 32]);
    pending_events(&db, &who(7), 6, c_hash);
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 0)).unwrap();
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(7)).unwrap());
    let up487 = marker_value(&rt, 487);
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, null], [uk, null], [marker_key(), hx(&up487)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 0)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 1)).unwrap();
    assert_eq!(round.reads[0].tag, "version@6");
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, "State already discarded"), 1)).unwrap();
    assert_eq!(seen(&db, &who(7)), Some(("unresolvable".into(), Some("state_discarded".into()))));
    assert_eq!(tx(&db, |s| terminal(s, &who(7))).unwrap(), Some((6, "state_discarded".into())));
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 2)).unwrap();
    assert!(
        round.reads.first().is_none_or(|r| r.tag != "version@6"),
        "the block is not read again: {:?}",
        round.reads.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>()
    );
}

/// Fix round 1, finding 10, and fix round 2, finding 7: a resolution read
/// that served no state for its block (no upgrade marker) is never
/// terminal. The stage stays pending, its next read (after a backoff) is
/// flagged for the archive endpoint, and further answers without state only
/// lengthen the backoff.
#[test]
fn a_resolution_read_without_state_is_read_again_from_the_archive() {
    let (db, rt, up) = with_runtime();
    let c_hash = Id([0xcf; 32]);
    pending_events(&db, &who(8), 6, c_hash);
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(8)).unwrap());
    let empty = |round: &Round| {
        answer(round, |_| {
            serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, null], [uk, null], [marker_key(), null]]}])
        })
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 0)).unwrap();
    assert_eq!(round.reads[0].tag, format!("events@{}@6", hex::encode(who(8).0)));
    assert!(!round.reads[0].archive);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &empty(&round), 0)).unwrap();
    assert_eq!(seen(&db, &who(8)).map(|x| x.0), Some("events".into()), "not terminal on one empty answer");
    assert!(tx(&db, |s| terminal(s, &who(8))).unwrap().is_none());
    // Read again after the backoff, from the archive endpoint when
    // configured; a state-holding answer resolves it.
    let t1 = crate::eval::admission::backoff_ms(1);
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t1)).unwrap();
    assert_eq!(round.reads[0].tag, format!("events@{}@6", hex::encode(who(8).0)));
    assert!(round.reads[0].archive);
    let single = encode(&rt, "System", "Events", &Value::List(vec![event(Some(2), NETUID, &who(8))]));
    let uid = encode(&rt, chain::SUBTENSOR, "Uids", &Value::Int(12));
    let results = answer(&round, |_| {
        serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, hx(&single)], [uk, hx(&uid)], [marker_key(), hx(&up)]]}])
    });
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, t1)).unwrap();
    assert_eq!(seen(&db, &who(8)).map(|x| x.0), Some("reg".into()));
    // The registration stage starts afresh (no backoff, a plain read); its
    // empty answers are never final either.
    let t2 = t1 + 1;
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t2)).unwrap();
    assert!(round.reads[0].tag.starts_with("reg@") && !round.reads[0].archive);
    let rk = registration_key(&rt, NETUID, 12).unwrap();
    let none = |round: &Round| {
        answer(round, |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[rk, null], [marker_key(), null]]}]))
    };
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &none(&round), t2)).unwrap();
    assert_eq!(seen(&db, &who(8)).map(|x| x.0), Some("reg".into()));
    let t3 = t2 + crate::eval::admission::backoff_ms(1);
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t3)).unwrap();
    assert!(round.reads[0].archive);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &none(&round), t3)).unwrap();
    assert_eq!(seen(&db, &who(8)).map(|x| x.0), Some("reg".into()), "still pending");
    assert!(tx(&db, |s| terminal(s, &who(8))).unwrap().is_none());
}

#[test]
fn the_reader_keeps_to_its_share_of_the_rpc_budget() {
    let (db, rt, _) = with_runtime();
    for n in 1..=5u8 {
        tx(&db, |s| {
            s.exec(
                "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage) VALUES (?, ?, '00', 9, 'hash')",
                &[s_hex(&who(n)), SqlValue::Integer(i64::from(n))],
            )?;
            Ok::<_, EvalError>(())
        })
        .unwrap();
    }
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    // A share of 4: four reads (oldest resolutions first), then none in the
    // same minute; a new window grants the share again.
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, 4, 100_000)).unwrap();
    assert_eq!(round.reads.len(), 4);
    assert_eq!(round.reads[0].tag, format!("hash@{}@1", hex::encode(who(1).0)));
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, 4, 130_000)).unwrap();
    assert!(round.reads.is_empty());
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, 4, 160_000)).unwrap();
    assert_eq!(round.reads.len(), 4);
    // The default share leaves the heartbeat's floor and the epoch read.
    assert_eq!(reader_share(10, None), 5);
    assert_eq!(reader_share(10, Some(9)), 5, "never above the floor");
    assert_eq!(reader_share(12, Some(3)), 3);
    assert_eq!(reader_share(4, None), 0);
}

#[test]
fn a_snapshot_under_another_runtime_is_decoded_with_the_metadata_at_its_block() {
    let (db, rt, _) = with_runtime();
    let cutoff = 9_360u64;
    let snap = Id([0x5a; 32]);
    tx(&db, |s| {
        s.exec(
            "INSERT INTO adm_snapshot_wanted (cutoff_block, hotkey) VALUES (?, ?)",
            &[SqlValue::Integer(cutoff as i64), s_hex(&who(3))],
        )?;
        s.exec(
            "INSERT INTO adm_snapshot_blocks (cutoff_block, block_hash) VALUES (?, ?)",
            &[SqlValue::Integer(cutoff as i64), SqlValue::Text(snap.to_hex())],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
    let at = BlockRef {
        number: 9_400,
        hash: Id([2; 32]),
    };
    let needs = Needs {
        poll: false,
        snapshot: Some((cutoff, vec![who(3)])),
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 0)).unwrap();
    assert!(round.reads[0].tag.starts_with("snapvals@"));
    let (ck, uk) = (
        commitment_key(&rt, NETUID, &who(3)).unwrap(),
        uids_key(&rt, NETUID, &who(3)).unwrap(),
    );
    let value = encode(&rt, PALLET, ITEM, &registration(9_300, &payload(9)));
    let uid = encode(&rt, chain::SUBTENSOR, "Uids", &Value::Int(12));
    let up487 = marker_value(&rt, 487);
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": snap.to_hex(), "changes": [[ck, hx(&value)], [uk, hx(&uid)], [marker_key(), hx(&up487)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 0)).unwrap();
    assert_eq!(db.count("adm_snapshot"), 0, "nothing decoded under the stored runtime");
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 1)).unwrap();
    assert_eq!(
        round.reads.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
        vec!["version@9359", "metadata@9359", "marker@9359"],
        "the runtime is read at the snapshot block"
    );
    // A snapshot whose block's state is gone is counted (and faults its
    // closing cohort, which this store does not hold).
    tx(&db, |s| store_runtime(s, 487, 1, METADATA, 9_359, &hex::encode(&up487))).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 2)).unwrap();
    tx(&db, |s| {
        ingest(s, Some(&rt), NETUID, &error_reply(&round, "State already discarded"), 2)
    })
    .unwrap();
    assert_eq!(tx(&db, status).unwrap()["state_discarded"], 1);
    // A lagging node's unknown-block answer keeps the snapshot being read
    // but is not counted as discarded state.
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 3)).unwrap();
    assert!(round.reads[0].tag.starts_with("snapvals@"), "the snapshot keeps being read");
    assert!(!round.reads[0].archive, "a discarded answer alone sets no backoff or archive routing");
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, "UnknownBlock: Header was not found"), 3)).unwrap();
    assert_eq!(tx(&db, status).unwrap()["state_discarded"], 1, "a block not served yet is not discarded");
    // It is read again after the backoff, as a resolution stage is, and
    // from the archive endpoint when one is configured; meanwhile a plan
    // reads nothing for it and parses no runtime for it.
    let due = 3 + crate::eval::admission::backoff_ms(1);
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, due - 1)).unwrap();
    assert!(round.reads.is_empty(), "backing off");
    assert!(!tx(&db, |s| wants_round(s, &needs, due - 1)).unwrap());
    assert!(tx(&db, |s| wants_round(s, &needs, due)).unwrap());
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, due)).unwrap();
    assert!(round.reads[0].tag.starts_with("snapvals@"), "and is read again");
    assert!(round.reads[0].archive, "from the archive endpoint");
    // An answer without state (no upgrade marker) is a lagging node too:
    // the backoff grows and nothing is counted as discarded.
    let results = answer(&round, |_| serde_json::json!([{"block": snap.to_hex(), "changes": []}]));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, due)).unwrap();
    assert_eq!(tx(&db, status).unwrap()["state_discarded"], 1, "an answer without state is not discarded");
    let again = due + crate::eval::admission::backoff_ms(2);
    assert!(crate::eval::admission::backoff_ms(2) > crate::eval::admission::backoff_ms(1));
    assert!(tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, again - 1)).unwrap().reads.is_empty());
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, again)).unwrap();
    assert!(round.reads[0].tag.starts_with("snapvals@") && round.reads[0].archive);
    // A read that answers ends the run: the next plan reads at once, from
    // the RPC endpoint.
    let results = answer(&round, |_| {
        serde_json::json!([{"block": snap.to_hex(), "changes": [[ck, hx(&value)], [uk, hx(&uid)], [marker_key(), hx(&up487)]]}])
    });
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, again)).unwrap();
    assert_eq!(db.count("adm_snapshot"), 1, "the snapshot was decoded");
    let tries = tx(&db, |s| s.exec("SELECT absent_tries, retry_ms FROM adm_snapshot_blocks", &[])).unwrap();
    assert_eq!((int(&tries[0], 0).unwrap(), int(&tries[0], 1).unwrap()), (0, 0));
}

/// Each block's delivery drives the reader, so invocations overlap: while a
/// planned round's reads are in flight (until its ingest or the planning
/// invocation's end), an overlapping plan sends nothing and is counted;
/// rounds skipped for want of invocation time are counted too.
#[test]
fn an_overlapping_round_plans_nothing_while_reads_are_in_flight() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let at = BlockRef {
        number: 8_340_100,
        hash: Id([0xab; 32]),
    };
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    let first = tx(&db, |s| plan_claimed(s, None, NETUID, Some(at), &needs, BUDGET, 1_000, 30_000, None)).unwrap();
    assert_eq!(first.reads.len(), 3, "the runtime reads");
    let overlap = tx(&db, |s| plan_claimed(s, None, NETUID, Some(at), &needs, BUDGET, 12_000, 30_000, None)).unwrap();
    assert!(overlap.reads.is_empty(), "the same reads are already in flight");
    // The ingest ends the claim (here with no replies: nothing was sent).
    tx(&db, |s| ingest(s, None, NETUID, &[], 13_000)).unwrap();
    let again = tx(&db, |s| plan_claimed(s, None, NETUID, Some(at), &needs, BUDGET, 14_000, 30_000, None)).unwrap();
    assert_eq!(again.reads, first.reads);
    // A claim whose invocation ended without an ingest expires with it.
    let expired = tx(&db, |s| plan_claimed(s, None, NETUID, Some(at), &needs, BUDGET, 44_000, 30_000, None)).unwrap();
    assert_eq!(expired.reads.len(), 3);
    tx(&db, note_starved).unwrap();
    let st = tx(&db, status).unwrap();
    assert_eq!((st["rounds_overlapped"].as_i64(), st["rounds_starved"].as_i64()), (Some(1), Some(1)));
}

/// Each block's delivery drives the reader inside its 30 s `waitUntil`
/// continuation, after the heartbeat kicks and a signing round: a round with
/// a large read (every poll is one) starts there, and the runtime round,
/// ingested only whole, is sent whole or not at all. With the heartbeat's
/// allowances (10 s a call, 20 s a large read) no round ever started.
#[test]
fn a_reader_round_fits_the_delivery_continuation() {
    use slop_ninja_arena::limits::WAIT_UNTIL_MS;
    const { assert!(ROUND_START_MS < WAIT_UNTIL_MS && RUNTIME_ROUND_MS < WAIT_UNTIL_MS) };
    let read = |tag: &str, large: bool| Read { tag: tag.into(), id: 1, body: String::new(), large, archive: false };
    // A fresh continuation, less the plan call.
    let poll = Round { reads: vec![read("keys@1", true)] };
    assert_eq!(fitting(&poll, WAIT_UNTIL_MS - READER_OBJECT_CALL_MS), 1);
    assert_eq!(fitting(&poll, ROUND_START_MS - READER_OBJECT_CALL_MS), 1);
    assert_eq!(fitting(&poll, ROUND_START_MS - READER_OBJECT_CALL_MS - 1), 0);
    let runtime = Round {
        reads: vec![read("version@1", false), read("metadata@1", true), read("marker@1", false)],
    };
    assert_eq!(fitting(&runtime, WAIT_UNTIL_MS - READER_OBJECT_CALL_MS), 3);
    assert_eq!(fitting(&runtime, RUNTIME_ROUND_MS - READER_OBJECT_CALL_MS), 3);
    assert_eq!(fitting(&runtime, RUNTIME_ROUND_MS - READER_OBJECT_CALL_MS - 1), 0, "whole or not at all");
    // Resolutions go out while they fit; the rest stay due for the next block.
    let many = Round { reads: (0..6).map(|i| read(&format!("hash@{i}"), false)).collect() };
    assert_eq!(fitting(&many, READER_OBJECT_CALL_MS + 2 * read_step_ms(false)), 2);
}

/// The plan keeps only the reads that fit the invocation's time left after
/// it (`fit_ms`) and charges the reader's share for those alone; a runtime
/// round that does not fit whole is neither sent nor charged nor claimed.
#[test]
fn a_plan_charges_only_the_reads_that_fit() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let at = BlockRef {
        number: 8_340_100,
        hash: Id([0xab; 32]),
    };
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    let used = |db: &Sqlite| {
        tx(db, |s| {
            let r = s.exec("SELECT budget_used, round_until_ms FROM commit_reader WHERE id = 1", &[])?;
            Ok::<_, EvalError>((int(&r[0], 0)?, int(&r[0], 1)?))
        })
        .unwrap()
    };
    // No runtime stored: the runtime round, 3 reads, does not fit 27 s.
    let short = tx(&db, |s| {
        plan_claimed(s, None, NETUID, Some(at), &needs, BUDGET, 1_000, 30_000, Some(RUNTIME_ROUND_MS - READER_OBJECT_CALL_MS - 1))
    })
    .unwrap();
    assert!(short.reads.is_empty());
    assert_eq!(used(&db), (0, 0), "nothing charged, no claim");
    let whole = tx(&db, |s| {
        plan_claimed(s, None, NETUID, Some(at), &needs, BUDGET, 2_000, 30_000, Some(RUNTIME_ROUND_MS - READER_OBJECT_CALL_MS))
    })
    .unwrap();
    assert_eq!(whole.reads.len(), 3);
    assert_eq!(used(&db), (3, 32_000));
}

/// The plan parses the stored runtime only when it could plan a round, and
/// an ingest only when it decodes with it; the Worker's observations of its
/// previous calls show in the status.
#[test]
fn reader_calls_parse_only_when_needed_and_are_observed() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let idle = Needs::default();
    assert!(!tx(&db, |s| wants_round(s, &idle, 1_000)).unwrap());
    let poll = Needs {
        poll: true,
        snapshot: None,
    };
    assert!(tx(&db, |s| wants_round(s, &poll, 1_000)).unwrap());
    let r = |tag: &str| ReadResult { tag: tag.into(), id: 1, reply: None };
    assert!(!ingest_needs_runtime(&[]));
    assert!(!ingest_needs_runtime(&[r("version@5"), r("metadata@5"), r("marker@5")]));
    assert!(ingest_needs_runtime(&[r("keys@5")]));
    tx(&db, |s| note_prior(s, &Prior { plan_ms: Some(40), ingest_ms: Some(900), failed: None })).unwrap();
    tx(&db, |s| note_prior(s, &Prior { plan_ms: Some(3_000), ingest_ms: None, failed: Some("plan".into()) })).unwrap();
    tx(&db, |s| note_prior(s, &Prior { plan_ms: Some(60), ingest_ms: None, failed: None })).unwrap();
    let calls = tx(&db, status).unwrap()["calls"].clone();
    assert_eq!(
        calls,
        serde_json::json!({"failed": 1, "failed_last": "plan", "plan_ms_last": 60, "plan_ms_max": 3_000,
            "ingest_ms_last": 900, "ingest_ms_max": 900, "maxima_cohort": null})
    );
}

// ------------------------------------- lagging node: unknown block (11-05)

/// The answer a node of a load-balanced endpoint gives for a block it has
/// not imported yet.
const NOT_IMPORTED: &str = "Client error: UnknownBlock: Header was not found in the database: 0xcf..";

fn refused(message: &str) -> ReadResult {
    ReadResult {
        tag: "events@x".into(),
        id: 1,
        reply: Some(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 4003, "message": message}}).to_string(),
        ),
    }
}

#[test]
fn refusals_tell_discarded_state_from_a_block_not_served_yet() {
    for m in [
        "State already discarded",
        "Client error: UnknownBlock: State already discarded for BlockId::Hash(0x0101..)",
    ] {
        assert_eq!(refusal(&refused(m)), Some(Refusal::Discarded), "{m}");
        assert!(discarded(&refused(m)) && !not_served(&refused(m)));
    }
    for m in [NOT_IMPORTED, "Unknown block", "UnknownBlock", "Header not found"] {
        assert_eq!(refusal(&refused(m)), Some(Refusal::NotServed), "{m}");
        assert!(!discarded(&refused(m)) && not_served(&refused(m)));
    }
    assert_eq!(refusal(&refused("Rate limited")), None);
    let data = ReadResult {
        tag: "events@x".into(),
        id: 1,
        reply: Some(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "Client error", "data": "UnknownBlock: Header was not found"}})
                .to_string(),
        ),
    };
    assert_eq!(refusal(&data), Some(Refusal::NotServed), "the error's data is read too");
    assert_eq!(refusal(&ReadResult { tag: "x".into(), id: 1, reply: None }), None);
    assert_eq!(
        refusal(&ReadResult { tag: "x".into(), id: 1, reply: Some(reply(1, serde_json::json!(["unknown block"]))) }),
        None,
        "a result is never a refusal"
    );
}

/// Review finding 2 of a0f2faef: an unknown-block answer for a resolution
/// read at an endpoint-returned hash is a lagging node, not pruned state.
/// The events and registration stages stay pending, back off, are read
/// again from the archive endpoint and resolve once a node serves them.
#[test]
fn a_lagging_nodes_unknown_block_never_ends_a_resolution() {
    let (db, rt, up) = with_runtime();
    let c_hash = Id([0xcf; 32]);
    pending_events(&db, &who(9), 6, c_hash);
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let plan_at = |t: u64| tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t)).unwrap();
    let round = plan_at(0);
    assert!(round.reads[0].tag.starts_with("events@") && !round.reads[0].archive);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, NOT_IMPORTED), 0)).unwrap();
    assert_eq!(seen(&db, &who(9)), Some(("events".into(), None)), "not terminal");
    assert!(tx(&db, |s| terminal(s, &who(9))).unwrap().is_none());
    let st = tx(&db, status).unwrap();
    assert_eq!(st["state_discarded"], 0);
    assert_eq!(st["state_absent_pending"], 1);
    assert!(plan_at(1).reads.is_empty(), "backs off");
    let t1 = crate::eval::admission::backoff_ms(1);
    let round = plan_at(t1);
    assert!(round.reads[0].tag.starts_with("events@") && round.reads[0].archive);
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(9)).unwrap());
    let single = encode(&rt, "System", "Events", &Value::List(vec![event(Some(2), NETUID, &who(9))]));
    let uid = encode(&rt, chain::SUBTENSOR, "Uids", &Value::Int(12));
    let results = answer(&round, |_| {
        serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, hx(&single)], [uk, hx(&uid)], [marker_key(), hx(&up)]]}])
    });
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, t1)).unwrap();
    assert_eq!(seen(&db, &who(9)).map(|x| x.0), Some("reg".into()));
    // The registration stage: the same.
    let t2 = t1 + 1;
    let round = plan_at(t2);
    assert!(round.reads[0].tag.starts_with("reg@") && !round.reads[0].archive);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, "Unknown block"), t2)).unwrap();
    assert_eq!(seen(&db, &who(9)).map(|x| x.0), Some("reg".into()), "still pending");
    assert!(plan_at(t2 + 1).reads.is_empty());
    let t3 = t2 + crate::eval::admission::backoff_ms(1);
    let round = plan_at(t3);
    assert!(round.reads[0].tag.starts_with("reg@") && round.reads[0].archive);
    let rk = registration_key(&rt, NETUID, 12).unwrap();
    let reg = encode(&rt, chain::SUBTENSOR, "BlockAtRegistration", &Value::Int(3));
    let results = answer(&round, |_| {
        serde_json::json!([{"block": c_hash.to_hex(), "changes": [[rk, hx(&reg)], [marker_key(), hx(&up)]]}])
    });
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, t3)).unwrap();
    assert_eq!(seen(&db, &who(9)).map(|x| x.0), Some("logged".into()));
    let e = tx(&db, |s| latest(s, &who(9))).unwrap().unwrap();
    assert_eq!((e.block, e.uid, e.registration_block), (6, Some(12), Some(3)));
    assert_eq!(tx(&db, status).unwrap()["state_discarded"], 0);
}

/// The block hash read: an unknown-block error or a `null` hash (a node that
/// has not imported the height) backs off and is read again; a hash moves
/// the resolution on with its absent answers cleared.
#[test]
fn a_hash_read_a_lagging_node_cannot_answer_is_read_again() {
    let (db, rt, _) = with_runtime();
    tx(&db, |s| {
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage) VALUES (?, 6, '00', 9, 'hash')",
            &[s_hex(&who(10))],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let plan_at = |t: u64| tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, t)).unwrap();
    let round = plan_at(0);
    assert_eq!(round.reads[0].tag, format!("hash@{}@6", hex::encode(who(10).0)));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, NOT_IMPORTED), 0)).unwrap();
    assert_eq!(seen(&db, &who(10)), Some(("hash".into(), None)));
    assert!(plan_at(1).reads.is_empty());
    let t1 = crate::eval::admission::backoff_ms(1);
    let round = plan_at(t1);
    assert!(round.reads[0].archive);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &answer(&round, |_| serde_json::Value::Null), t1)).unwrap();
    assert_eq!(seen(&db, &who(10)), Some(("hash".into(), None)), "a null hash is not final either");
    assert!(tx(&db, |s| terminal(s, &who(10))).unwrap().is_none());
    let t2 = t1 + crate::eval::admission::backoff_ms(2);
    assert!(plan_at(t2 - 1).reads.is_empty(), "the backoff grows");
    let round = plan_at(t2);
    let c_hash = Id([0xc6; 32]);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &answer(&round, |_| serde_json::json!(c_hash.to_hex())), t2)).unwrap();
    assert_eq!(seen(&db, &who(10)).map(|x| x.0), Some("events".into()));
    assert_eq!(tx(&db, status).unwrap()["state_absent_pending"], 0);
    let round = plan_at(t2 + 1);
    assert!(round.reads[0].tag.starts_with("events@") && !round.reads[0].archive, "the next stage starts afresh");
    // An explicit discarded answer is still final.
    tx(&db, |s| {
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage) VALUES (?, 7, '00', 9, 'hash')",
            &[s_hex(&who(11))],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
    let round = plan_at(t2 + 2);
    let tag = format!("hash@{}@7", hex::encode(who(11).0));
    let mine: Vec<ReadResult> =
        error_reply(&round, "State already discarded").into_iter().filter(|r| r.tag == tag).collect();
    assert_eq!(mine.len(), 1);
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &mine, t2 + 2)).unwrap();
    assert_eq!(seen(&db, &who(11)), Some(("unresolvable".into(), Some("block_unknown".into()))));
}

/// A key listing whose pinned block a lagging node does not serve yet is
/// read again at that block (not restarted, not counted as discarded); a
/// runtime read refused that way leaves the pending resolution alone.
#[test]
fn a_listing_and_a_runtime_read_a_lagging_node_refuses_are_read_again() {
    let (db, rt, _) = with_runtime();
    let at = BlockRef {
        number: 10_000,
        hash: Id([2; 32]),
    };
    let needs = Needs {
        poll: true,
        snapshot: None,
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &needs, BUDGET, 100_000)).unwrap();
    assert_eq!(round.reads[0].tag, "keys@10000");
    let out = tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, NOT_IMPORTED), 100_000)).unwrap();
    assert_eq!(out.failed, 1);
    let head = BlockRef {
        number: 10_500,
        hash: Id([3; 32]),
    };
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(head), &needs, BUDGET, 100_000)).unwrap();
    assert_eq!(round.reads[0].tag, "keys@10000", "the listing keeps its block");
    let results = answer(&round, |_| serde_json::json!(vec![commitment_key(&rt, NETUID, &who(3)).unwrap()]));
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 100_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(head), &needs, BUDGET, 100_000)).unwrap();
    assert_eq!(round.reads[0].tag, "values@10000");
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, NOT_IMPORTED), 100_000)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(head), &needs, BUDGET, 100_000)).unwrap();
    assert_eq!(round.reads[0].tag, "values@10000", "read again at its own block");
    assert_eq!(tx(&db, status).unwrap()["state_discarded"], 0);

    // A runtime read at a past block (another runtime wrote it) that a
    // lagging node refuses: the resolution stays pending and the same
    // block's runtime is read again.
    let c_hash = Id([0xce; 32]);
    pending_events(&db, &who(7), 6, c_hash);
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 0)).unwrap();
    let (ek, uk) = (events_key(&rt).unwrap(), uids_key(&rt, NETUID, &who(7)).unwrap());
    let up487 = marker_value(&rt, 487);
    let results = answer(
        &round,
        |_| serde_json::json!([{"block": c_hash.to_hex(), "changes": [[ek, null], [uk, null], [marker_key(), hx(&up487)]]}]),
    );
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &results, 0)).unwrap();
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 1)).unwrap();
    assert_eq!(round.reads[0].tag, "version@6");
    tx(&db, |s| ingest(s, Some(&rt), NETUID, &error_reply(&round, NOT_IMPORTED), 1)).unwrap();
    assert_eq!(seen(&db, &who(7)), Some(("events".into(), None)), "not ended");
    let round = tx(&db, |s| plan(s, Some(&rt), NETUID, Some(at), &Needs::default(), BUDGET, 2)).unwrap();
    assert_eq!(round.reads[0].tag, "version@6", "the same block's runtime is read again");
}
