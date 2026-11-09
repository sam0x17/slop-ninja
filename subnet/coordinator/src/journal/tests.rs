use super::*;
use crate::sql::{
    CHAIN_JOURNAL_SCHEMA, EVAL_COHORT_SCHEMA, Migration, MigrationError, Schema, migrate,
};
use crate::testing::{Sqlite, chain_hash as h, id, note, tx};

const NOW: u64 = 1_793_000_000_000;

pub(crate) fn journal() -> Sqlite {
    let db = Sqlite::open();
    let report = tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    assert_eq!((report.from, report.to), (0, 14));
    db
}

fn header(number: u64) -> FinalizedHeader {
    FinalizedHeader {
        number,
        hash: h(number),
        parent_hash: h(number - 1),
    }
}

fn block(number: u64) -> BlockRef {
    BlockRef {
        number,
        hash: h(number),
    }
}

fn deliver(db: &Sqlite, delivery: u8, hd: FinalizedHeader) -> Result<DeliveryOutcome, IngestError> {
    let n = note(delivery, delivery, delivery, hd);
    tx(db, |sql| ingest_delivery(sql, &n, NOW))
}

fn drain_now(db: &Sqlite) -> Progress {
    tx(db, |sql| drain(sql, 512, NOW)).unwrap()
}

fn status_of(db: &Sqlite) -> JournalStatus {
    tx(db, |sql| status(sql, NOW)).unwrap()
}

#[test]
fn migrations_are_versioned_and_idempotent() {
    let db = journal();
    let again = tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    assert_eq!((again.from, again.to), (14, 14));
    assert_eq!(
        tx(&db, |sql| migrate(sql, &EVAL_COHORT_SCHEMA)),
        Err(MigrationError::Family)
    );
    db.conn
        .execute(
            "UPDATE schema_meta SET value = '15' WHERE name = 'version'",
            [],
        )
        .unwrap();
    assert_eq!(
        tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)),
        Err(MigrationError::NewerStoredVersion {
            stored: 15,
            supported: 14
        })
    );
    const GAPPED: Schema = Schema {
        family: "gapped",
        migrations: &[Migration {
            version: 2,
            statements: &[],
        }],
    };
    assert_eq!(
        tx(&Sqlite::open(), |sql| migrate(sql, &GAPPED)),
        Err(MigrationError::Definition)
    );
}

#[test]
fn v1_storage_upgrades_in_place_to_latest() {
    const V1_ONLY: Schema = Schema {
        family: "chain_journal",
        migrations: &[Migration {
            version: 1,
            statements: CHAIN_JOURNAL_SCHEMA.migrations[0].statements,
        }],
    };
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &V1_ONLY)).unwrap();
    db.conn
        .execute(
            "INSERT INTO deliveries VALUES ('0x01', '0x02', '0x03', '0x04', 1, 5)",
            [],
        )
        .unwrap();
    let report = tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    assert_eq!((report.from, report.to), (1, 14));
    assert_eq!(db.count("deliveries"), 1);
    assert_eq!(db.count("watchdog"), 1);
    assert_eq!(db.count("rpc_budget"), 1);
}

#[test]
fn migration_upgrades_in_order_and_rolls_back_failures() {
    const V1: Schema = Schema {
        family: "demo",
        migrations: &[Migration {
            version: 1,
            statements: &["CREATE TABLE a (x INTEGER) STRICT"],
        }],
    };
    const V3: Schema = Schema {
        family: "demo",
        migrations: &[
            Migration {
                version: 1,
                statements: &["CREATE TABLE a (x INTEGER) STRICT"],
            },
            Migration {
                version: 2,
                statements: &["ALTER TABLE a ADD COLUMN y INTEGER"],
            },
            Migration {
                version: 3,
                statements: &["CREATE TABLE c (z INTEGER) STRICT"],
            },
        ],
    };
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &V1)).unwrap();
    db.fail_on.set(Some("CREATE TABLE c"));
    assert!(tx(&db, |sql| migrate(sql, &V3)).is_err());
    db.fail_on.set(None);
    assert_eq!(
        tx(&db, crate::sql::stored_version).unwrap(),
        (Some("demo".into()), 1)
    );
    let report = tx(&db, |sql| migrate(sql, &V3)).unwrap();
    assert_eq!((report.from, report.to), (1, 3));
    db.conn
        .execute("INSERT INTO a (x, y) VALUES (1, 2)", [])
        .unwrap();
}

#[test]
fn schemas_avoid_statements_durable_objects_reject() {
    for schema in [&CHAIN_JOURNAL_SCHEMA, &EVAL_COHORT_SCHEMA] {
        for migration in schema.migrations {
            for statement in migration.statements {
                let upper = statement.to_uppercase();
                // Whole SQL words: identifiers such as `commit_ops` are fine.
                let words: Vec<&str> = upper
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .collect();
                for forbidden in [
                    "BEGIN",
                    "COMMIT",
                    "ROLLBACK",
                    "SAVEPOINT",
                    "PRAGMA",
                    "AUTOINCREMENT",
                ] {
                    assert!(!words.contains(&forbidden), "{statement}");
                }
                assert!(!upper.contains("_CF_"), "{statement}");
            }
        }
    }
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &EVAL_COHORT_SCHEMA)).unwrap();
    // v2 replaces the v1 stub's single row with the observation row.
    assert_eq!(db.count("eval_obs"), 1);
    assert_eq!(db.count("cohorts"), 0);
}

#[test]
fn deliveries_deduplicate_by_id_and_blocks_by_stream_and_hash() {
    let db = journal();
    let first = note(0x22, 0x33, 0xb0, header(10));
    assert_eq!(
        tx(&db, |s| ingest_delivery(s, &first, NOW)),
        Ok(DeliveryOutcome::Accepted)
    );
    let retry = BlockNotification {
        attempt: 2,
        ..first.clone()
    };
    assert_eq!(
        tx(&db, |s| ingest_delivery(s, &retry, NOW)),
        Ok(DeliveryOutcome::DuplicateDelivery)
    );
    let changed = BlockNotification {
        body_sha256: id(0xb1),
        ..first.clone()
    };
    assert_eq!(
        tx(&db, |s| ingest_delivery(s, &changed, NOW)),
        Err(IngestError::DeliveryConflict)
    );
    // Re-registration: a new delivery ID (and event) for the same block.
    let again = note(0x23, 0x34, 0xb2, header(10));
    assert_eq!(
        tx(&db, |s| ingest_delivery(s, &again, NOW)),
        Ok(DeliveryOutcome::DuplicateBlock)
    );
    // The same event ID naming a different block is refused.
    let forged = note(0x22, 0x35, 0xb3, header(11));
    assert_eq!(
        tx(&db, |s| ingest_delivery(s, &forged, NOW)),
        Err(IngestError::EventConflict)
    );
    assert_eq!((db.count("deliveries"), db.count("block_events")), (2, 1));
    // RPC backfill of a recorded block is a duplicate too.
    assert!(
        !tx(&db, |s| record_block(
            s,
            &header(10),
            Source::Rpc,
            None,
            NOW
        ))
        .unwrap()
    );
    assert_eq!(db.count("block_events"), 1);
}

#[test]
fn a_failed_ingest_rolls_back_whole() {
    let db = journal();
    db.fail_on.set(Some("INSERT INTO block_events"));
    assert!(matches!(
        deliver(&db, 1, header(10)),
        Err(IngestError::Store(_))
    ));
    db.fail_on.set(None);
    assert_eq!((db.count("deliveries"), db.count("block_events")), (0, 0));
    assert_eq!(deliver(&db, 1, header(10)), Ok(DeliveryOutcome::Accepted));
}

#[test]
fn deliveries_extend_the_cursor_along_parent_links() {
    let db = journal();
    deliver(&db, 1, header(10)).unwrap();
    assert_eq!(drain_now(&db).halt, Some(Halt::NotAnchored));
    assert!(tx(&db, |s| anchor(s, block(10), NOW)).unwrap());
    assert!(!tx(&db, |s| anchor(s, block(12), NOW)).unwrap());
    let p = drain_now(&db);
    assert_eq!((p.applied, p.covered, p.halt), (0, 1, None));

    deliver(&db, 2, header(11)).unwrap();
    deliver(&db, 3, header(12)).unwrap();
    let p = drain_now(&db);
    assert_eq!((p.applied, p.cursor, p.halt), (2, Some(block(12)), None));
    let s = status_of(&db);
    assert_eq!(
        (s.events_applied, s.events_covered, s.retained_blocks),
        (2, 1, 3)
    );
}

#[test]
fn a_missing_height_is_a_gap_and_backfill_closes_it() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    deliver(&db, 1, header(14)).unwrap();
    deliver(&db, 2, header(15)).unwrap();
    assert_eq!(drain_now(&db).halt, Some(Halt::Gap { from: 11, to: 13 }));
    // A hash list from the cursor height joins the retained chain.
    let list: Vec<Option<Id>> = (10..=13).map(|n| Some(h(n))).collect();
    assert_eq!(
        tx(&db, |s| apply_hashes(s, 10, &list, NOW)).unwrap(),
        HashesOutcome::Recorded { recorded: 3, complete: true }
    );
    let p = drain_now(&db);
    assert_eq!((p.applied, p.cursor, p.halt), (5, Some(block(15)), None));
    assert_eq!(counter_of(&db, "applied_rpc"), 3);
    assert_eq!(counter_of(&db, "applied_webhook"), 2);
}

fn counter_of(db: &Sqlite, name: &str) -> u64 {
    tx(db, |s| counter(s, name)).unwrap()
}

#[test]
fn hash_lists_stop_at_unknown_heights_and_must_join_the_cursor() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    let partial = vec![Some(h(10)), Some(h(11)), None, Some(h(13))];
    assert_eq!(
        tx(&db, |s| apply_hashes(s, 10, &partial, NOW)).unwrap(),
        HashesOutcome::Recorded { recorded: 1, complete: false }
    );
    assert_eq!(drain_now(&db).cursor, Some(block(11)));
    // A junction the node does not serve yet records nothing and is not
    // "unusable": the list is asked again later.
    assert_eq!(
        tx(&db, |s| apply_hashes(s, 11, &[None, Some(h(12))], NOW)).unwrap(),
        HashesOutcome::Recorded { recorded: 0, complete: false }
    );
    assert_eq!(drain_now(&db).cursor, Some(block(11)));
    // A list from a height the chain does not retain is unusable.
    assert_eq!(
        tx(&db, |s| apply_hashes(s, 3, &[Some(h(3)), Some(h(4))], NOW)).unwrap(),
        HashesOutcome::Unusable
    );
    // A junction disagreeing with the cursor raises a durable fault.
    assert_eq!(
        tx(&db, |s| apply_hashes(
            s,
            11,
            &[Some(id(0x77)), Some(h(12))],
            NOW
        ))
        .unwrap(),
        HashesOutcome::Fault
    );
    let fault = status_of(&db).fault.unwrap();
    assert_eq!(
        (fault.kind.as_str(), fault.block_number),
        ("rpc_junction_mismatch", 11)
    );
}

#[test]
fn a_broken_parent_link_is_a_durable_fault_that_pauses_until_cleared() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    let forked = FinalizedHeader {
        number: 11,
        hash: id(0x91),
        parent_hash: id(0x90),
    };
    deliver(&db, 1, forked).unwrap();
    let p = drain_now(&db);
    let Some(Halt::Fault { id: fault_id }) = p.halt else {
        panic!("{p:?}")
    };
    assert_eq!(p.cursor, Some(block(10)));
    let fault = status_of(&db).fault.unwrap();
    assert_eq!(
        (
            fault.kind.as_str(),
            fault.expected_hash,
            fault.observed_hash
        ),
        ("parent_mismatch", Some(h(10)), Some(id(0x90)))
    );
    // The fault survives later transactions; good blocks wait while paused.
    deliver(&db, 2, header(11)).unwrap();
    deliver(&db, 3, header(12)).unwrap();
    assert_eq!(drain_now(&db).halt, Some(Halt::Fault { id: fault_id }));
    assert_eq!(status_of(&db).cursor, Some(block(10)));
    assert!(!tx(&db, |s| anchor(s, block(20), NOW)).unwrap());

    assert_eq!(
        tx(&db, |s| clear_fault(s, fault_id + 1, false, NOW)).unwrap(),
        ClearOutcome::WrongFault { active: fault_id }
    );
    assert_eq!(
        tx(&db, |s| clear_fault(s, fault_id, false, NOW)).unwrap(),
        ClearOutcome::Cleared { reanchor: false }
    );
    let p = drain_now(&db);
    assert_eq!((p.cursor, p.halt), (Some(block(12)), None));
    let s = status_of(&db);
    assert_eq!((s.fault, s.faults_total, s.events_conflict), (None, 1, 1));
    assert_eq!(
        tx(&db, |s| clear_fault(s, fault_id, false, NOW)).unwrap(),
        ClearOutcome::NoActiveFault
    );
}

#[test]
fn two_hashes_at_one_height_conflict_and_reanchor_recovers() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    deliver(&db, 1, header(11)).unwrap();
    drain_now(&db);
    // A late delivery disagrees with the retained block at height 11.
    let other = FinalizedHeader {
        number: 11,
        hash: id(0x99),
        parent_hash: h(10),
    };
    deliver(&db, 2, other).unwrap();
    let Some(Halt::Fault { id: fault_id }) = drain_now(&db).halt else {
        panic!()
    };
    assert_eq!(status_of(&db).fault.unwrap().kind, "hash_conflict");
    assert_eq!(
        tx(&db, |s| clear_fault(s, fault_id, true, NOW)).unwrap(),
        ClearOutcome::Cleared { reanchor: true }
    );
    let s = status_of(&db);
    assert_eq!((s.cursor, s.retained_blocks), (None, 0));
    assert_eq!(drain_now(&db).halt, Some(Halt::NotAnchored));
}

#[test]
fn rpc_head_audits_the_webhook_advanced_cursor() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    for n in 11..=13 {
        deliver(&db, n as u8, header(n)).unwrap();
    }
    drain_now(&db);
    // A head at or below the cursor is compared at once.
    tx(&db, |s| note_rpc_head(s, block(12), NOW)).unwrap();
    assert_eq!(counter_of(&db, "rpc_audits_passed"), 1);
    // A head above the cursor is checked when the cursor reaches it.
    tx(&db, |s| note_rpc_head(s, block(15), NOW)).unwrap();
    assert_eq!(status_of(&db).pending_audits, 1);
    deliver(&db, 14, header(14)).unwrap();
    deliver(&db, 15, header(15)).unwrap();
    drain_now(&db);
    assert_eq!(counter_of(&db, "rpc_audits_passed"), 2);
    assert_eq!(status_of(&db).pending_audits, 0);
    // A webhook block that RPC contradicts raises a fault.
    tx(&db, |s| {
        note_rpc_head(
            s,
            BlockRef {
                number: 16,
                hash: id(0x66),
            },
            NOW,
        )
    })
    .unwrap();
    deliver(&db, 16, header(16)).unwrap();
    let p = drain_now(&db);
    assert!(matches!(p.halt, Some(Halt::Fault { .. })));
    let fault = status_of(&db).fault.unwrap();
    assert_eq!(
        (fault.kind.as_str(), fault.block_number),
        ("rpc_head_conflict", 16)
    );
}

#[test]
fn rpc_head_never_regresses() {
    let db = journal();
    tx(&db, |s| note_rpc_head(s, block(30), NOW)).unwrap();
    tx(&db, |s| note_rpc_head(s, block(25), NOW + 1)).unwrap();
    tx(&db, |s| {
        note_rpc_head(
            s,
            BlockRef {
                number: 30,
                hash: id(0x30),
            },
            NOW + 2,
        )
    })
    .unwrap();
    let s = status_of(&db);
    assert_eq!(s.rpc_finalized_head, Some(block(30)));
    assert_eq!(counter_of(&db, "rpc_head_conflicts"), 1);
}

#[test]
fn drain_is_bounded_per_transaction() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    for n in 11..=20 {
        deliver(&db, n as u8, header(n)).unwrap();
    }
    let p = tx(&db, |s| drain(s, 4, NOW)).unwrap();
    assert_eq!((p.applied, p.halt), (4, Some(Halt::Bound)));
    assert_eq!(drain_now(&db).cursor, Some(block(20)));
}

#[test]
fn v2_storage_upgrades_with_the_watermark_seeded_at_its_anchor() {
    const V2_ONLY: Schema = Schema {
        family: "chain_journal",
        migrations: &[
            Migration {
                version: 1,
                statements: CHAIN_JOURNAL_SCHEMA.migrations[0].statements,
            },
            Migration {
                version: 2,
                statements: CHAIN_JOURNAL_SCHEMA.migrations[1].statements,
            },
        ],
    };
    let db = Sqlite::open();
    tx(&db, |sql| migrate(sql, &V2_ONLY)).unwrap();
    // v2 code anchored at 10 and advanced to 13 by webhook alone.
    for (n, source) in [
        (10, "anchor"),
        (11, "webhook"),
        (12, "webhook"),
        (13, "webhook"),
    ] {
        db.conn
            .execute(
                "INSERT INTO finalized_blocks VALUES (?1, ?2, ?3, 5)",
                rusqlite::params![n, h(n).to_hex(), source],
            )
            .unwrap();
    }
    db.conn
        .execute(
            "INSERT INTO finalized_cursor VALUES (1, 13, ?1, 5)",
            [h(13).to_hex()],
        )
        .unwrap();
    let report = tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    assert_eq!((report.from, report.to), (2, 14));
    let s = status_of(&db);
    assert_eq!((s.cursor, s.confirmed), (Some(block(13)), Some(block(10))));
    assert_eq!(s.provisional_blocks, Some(3));
    assert_eq!(s.confirmed_basis.as_deref(), Some("anchor"));
    // An empty v2 journal upgrades with no watermark.
    let empty = Sqlite::open();
    tx(&empty, |sql| migrate(sql, &V2_ONLY)).unwrap();
    tx(&empty, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
    assert_eq!(status_of(&empty).confirmed, None);
}

#[test]
fn webhook_blocks_stay_provisional_until_rpc_evidence_confirms_them() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    let s = status_of(&db);
    assert_eq!(
        (s.confirmed, s.provisional_blocks),
        (Some(block(10)), Some(0))
    );
    for n in 11..=14 {
        deliver(&db, n as u8, header(n)).unwrap();
    }
    let p = drain_now(&db);
    // Authenticated deliveries move only the provisional cursor.
    assert_eq!((p.cursor, p.confirmed), (Some(block(14)), Some(block(10))));
    assert_eq!(status_of(&db).provisional_blocks, Some(4));
    // A head audit at a retained height confirms it and everything below.
    tx(&db, |s| note_rpc_head(s, block(12), NOW)).unwrap();
    assert_eq!(tx(&db, confirmed).unwrap(), Some(block(12)));
    // An audit point above the cursor confirms when the cursor reaches it.
    tx(&db, |s| note_rpc_head(s, block(16), NOW)).unwrap();
    deliver(&db, 15, header(15)).unwrap();
    drain_now(&db);
    assert_eq!(tx(&db, confirmed).unwrap(), Some(block(12)));
    deliver(&db, 16, header(16)).unwrap();
    let p = drain_now(&db);
    assert_eq!((p.cursor, p.confirmed), (Some(block(16)), Some(block(16))));
    let s = status_of(&db);
    assert_eq!(
        (s.confirmed_basis.as_deref(), s.provisional_blocks),
        (Some("rpc_head"), Some(0))
    );
    // The watermark never moves down.
    tx(&db, |s| note_rpc_head(s, block(13), NOW)).unwrap();
    assert_eq!(tx(&db, confirmed).unwrap(), Some(block(16)));
}

#[test]
fn rpc_lists_confirm_only_up_to_the_rpc_finalized_head() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    tx(&db, |s| note_rpc_head(s, block(13), NOW)).unwrap();
    // A webhook block at 20 leaves a gap that an RPC hash list fills.
    deliver(&db, 20, header(20)).unwrap();
    drain_now(&db);
    let list: Vec<_> = (10..=19).map(|n| Some(h(n))).collect();
    tx(&db, |s| apply_hashes(s, 10, &list, NOW)).unwrap();
    let p = drain_now(&db);
    // 11 and 12 confirm as RPC list blocks and 13 by its audit point. 14 to
    // 19 are above the RPC finalized head (RPC's best chain only), and 20 is
    // the provider's, so all of them stay provisional.
    assert_eq!((p.cursor, p.confirmed), (Some(block(20)), Some(block(13))));
    assert_eq!(counter_of(&db, "confirmations_rpc_list"), 2);
    assert_eq!(counter_of(&db, "confirmations_rpc_head"), 1);
    assert_eq!(status_of(&db).provisional_blocks, Some(7));
    // In fallback the head is read first, so the whole list confirms.
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    tx(&db, |s| note_rpc_head(s, block(30), NOW)).unwrap();
    let list: Vec<_> = (10..=25).map(|n| Some(h(n))).collect();
    tx(&db, |s| apply_hashes(s, 10, &list, NOW)).unwrap();
    let p = drain_now(&db);
    assert_eq!((p.cursor, p.confirmed), (Some(block(25)), Some(block(25))));
    assert_eq!(status_of(&db).confirmed_basis.as_deref(), Some("rpc_list"));
}

#[test]
fn a_contradicting_audit_never_confirms_and_reanchor_drops_the_watermark() {
    let db = journal();
    tx(&db, |s| anchor(s, block(10), NOW)).unwrap();
    for n in 11..=12 {
        deliver(&db, n as u8, header(n)).unwrap();
    }
    drain_now(&db);
    let wrong = BlockRef {
        number: 12,
        hash: id(0x99),
    };
    tx(&db, |s| note_rpc_head(s, wrong, NOW)).unwrap();
    let s = status_of(&db);
    assert_eq!(s.fault.as_ref().unwrap().kind, "rpc_head_conflict");
    assert_eq!(
        (s.confirmed, s.provisional_blocks),
        (Some(block(10)), Some(2))
    );
    let fault = s.fault.unwrap().id;
    tx(&db, |s| clear_fault(s, fault, true, NOW)).unwrap();
    let s = status_of(&db);
    assert_eq!((s.cursor, s.confirmed), (None, None));
}

/// Audit E3's guard: `schema_meta` records only a store's family and
/// version, so a migration a store may already hold must never be edited in
/// place or renumbered (a store at that version would keep its old
/// statements unseen, and two branches numbering different migrations alike
/// would skip one). Each migration's statements are pinned by digest; a new
/// migration appends a version and its pin.
#[test]
fn migrations_are_pinned_by_content() {
    use sha2::{Digest as _, Sha256};
    fn digests(schema: &Schema) -> Vec<String> {
        schema
            .migrations
            .iter()
            .map(|m| {
                let mut h = Sha256::new();
                for statement in m.statements {
                    h.update(statement.as_bytes());
                    h.update([0]);
                }
                hex::encode(h.finalize())[..16].to_owned()
            })
            .collect()
    }
    // v1-v17 are deployed on eval-testnet (store 17 since 2026-11-04); v18 is
    // the stuck-states fix round 1's step-error epoch, v19 fix round 2's
    // attempt extensions, budget alert and watermark epoch; v20 the
    // eval-no-skip lane's outage probe and abandonment record; v21 the
    // reader-allowance lane's framing and blob dispatches; v23 the boundary
    // journal (deployed); v24 the reward-v2 / quant-v1 rule floor; v25
    // admission contract v2 (direct upload to R2); v26 generation v2's
    // private state (re-pinned with `gen_results`, the per-claim record of
    // committed generation results that the generation v2 review fixes
    // added before any deploy); v27 the commitment snapshot's not-served
    // backoff.
    const EVAL: &[&str] = &[
        "bb26853f9d8c37f8",
        "a77de0832bb5add7",
        "304e3d790a649d98",
        "71b36fa12a72ed6f",
        "242df39085d9ff7d",
        "3b3ffb24f67367fb",
        "133196c0c6c70e8f",
        "f9a332c3c891c400",
        "e83603bcf596c17f",
        "8c93de08a934046a",
        "69e64bd181d2fd2c",
        "1c2a4876a7892de0",
        "621f1d98b70acc3d",
        "0c390502d4453d38",
        "ef72c4f128f274e4",
        "e5e208c78d7e36b6",
        "2950d55dafc23816",
        "dd90865bbfd05bf0",
        "7a8ff491377e3cc9",
        "942dc264582d1a0c",
        "f5758f190c598abd",
        "8b761151f0241d00",
        "6da42707629d12a8",
        // v24: the reward-v2 / quant-v1 rule floor (quantization lane).
        "e3ef1bff2fbd6125",
        // v25: admission contract v2 (direct upload to R2).
        "6bf062a35e8f2a16",
        // v26: generation v2's private state (gen-v2 follow-ups and review
        // fixes; never deployed).
        "780bf0c3609e954e",
        // v27: the commitment snapshot's not-served backoff (snapshot-retry).
        "ee9f5614de344dc6",
        // v28: live values while a cohort is scored (live-scores lane; never
        // deployed).
        "5ef1a267ae59d3d4",
    ];
    // v1-v10 are deployed on both testnet Workers; v11 is the stuck-states
    // lane's; v12 the automatic target handoff's; v13 the signed coordinator
    // row's (public-allowlist); v14 the kept registration read (status-api
    // intake, `signing::registry`).
    const JOURNAL: &[&str] = &[
        "ee5a5573215817c9",
        "35a786f8d817d001",
        "4c9694e1b875a044",
        "b959d0c7e245ad63",
        "f761291eec8fb730",
        "820713585c3fce1f",
        "d122032fc44e55a9",
        "da75cd430f733bf4",
        "f48d266267134443",
        "e3de0843c295d069",
        "bcbc696385d2f3db",
        "229ffbcbe5e0c9c3",
        "215d13cb5f86f2f7",
        "7a26f0d76a54ed59",
    ];
    let (e, j) = (digests(&EVAL_COHORT_SCHEMA), digests(&CHAIN_JOURNAL_SCHEMA));
    assert!(e == EVAL && j == JOURNAL, "migrations changed or were not pinned: eval_cohort {e:?}, chain_journal {j:?}");
}
