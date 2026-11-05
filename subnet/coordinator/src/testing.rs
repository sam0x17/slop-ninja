//! Native test support: a SQLite-backed [`Sql`] (the dialect Durable Objects
//! use), a transaction wrapper mirroring `transactionSync`, and a fixture signer.

use crate::sql::{Row, Sql, SqlValue, StoreError};
use crate::webhook::{
    BlockNotification, Contract, DeliveryContext, ReceiverConfig, SignedFields, SigningKey, StateReadiness,
};
use crate::wire::{FinalizedHeader, Id};
use rusqlite::types::ValueRef;
use std::cell::Cell;

pub const SECRET_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";
pub const GENESIS: &str = crate::config::TESTNET_GENESIS;

pub fn id(byte: u8) -> Id {
    Id([byte; 32])
}

/// Deterministic, nonzero hash of the fake chain's block at `number`.
pub fn chain_hash(number: u64) -> Id {
    let mut bytes = [0x5a; 32];
    bytes[..8].copy_from_slice(&number.to_be_bytes());
    Id(bytes)
}

/// An authenticated notification as the Worker passes it to the journal.
pub fn note(event: u8, delivery: u8, body: u8, header: FinalizedHeader) -> BlockNotification {
    BlockNotification {
        delivery_id: id(delivery),
        subscription: id(0x11),
        event_id: id(event),
        attempt: 1,
        body_sha256: id(body),
        journal_position: 1,
        observed_at_ms: 1_793_000_000_000,
        header,
        state_readiness: StateReadiness::HeaderOnly,
        contract: Contract::Unnamed,
    }
}

pub fn receiver() -> ReceiverConfig {
    ReceiverConfig {
        key: SigningKey::from_hex("pilot-v1", SECRET_HEX).unwrap(),
        subscription: id(0x11),
        chain: Id::parse(GENESIS).unwrap(),
    }
}

pub fn context(event: u8, delivery: u8) -> DeliveryContext {
    DeliveryContext {
        subscription: id(0x11),
        event_id: id(event),
        delivery_id: id(delivery),
    }
}

/// Builds an `X-Rtao-Signature` header value for `raw`.
pub fn sign(context: DeliveryContext, attempt: u16, timestamp: u64, raw: &[u8]) -> String {
    let fields = SignedFields {
        version: 1,
        key_id: "pilot-v1".into(),
        timestamp,
        context,
        attempt,
    };
    let mac = receiver().key.mac(&fields, raw);
    header(&fields, &Id(mac))
}

pub fn header(fields: &SignedFields, mac: &Id) -> String {
    serde_json::json!({
        "version": fields.version,
        "key_id": fields.key_id,
        "timestamp": fields.timestamp.to_string(),
        "context": {
            "subscription": fields.context.subscription,
            "event_id": fields.context.event_id,
            "delivery_id": fields.context.delivery_id,
        },
        "attempt": fields.attempt,
        "mac": mac,
    })
    .to_string()
}

/// A finalized-block body as the provider sends it.
pub fn finalized_body(event: u8, number: u64, hash: u8, parent: u8) -> Vec<u8> {
    let parent = if number == 0 { Id::ZERO } else { id(parent) };
    serde_json::json!({
        "event_id": id(event),
        "event": {
            "version": 1,
            "chain": GENESIS,
            "journal_position": "7",
            "observed_at_ms": "1793000000123",
            "kind": {
                "type": "finalized",
                "block": {
                    "number": number.to_string(),
                    "hash": id(hash),
                    "parent_hash": parent,
                    "state_root": id(0xee),
                    "extrinsics_root": id(0xef),
                    "chain_timestamp_ms": "1793000000000",
                    "state_readiness": "header_only",
                },
            },
        },
    })
    .to_string()
    .into_bytes()
}

pub struct Sqlite {
    pub conn: rusqlite::Connection,
    /// Fails the statement whose text contains this marker, once armed.
    pub fail_on: Cell<Option<&'static str>>,
}

impl Sqlite {
    pub fn open() -> Self {
        Self {
            conn: rusqlite::Connection::open_in_memory().unwrap(),
            fail_on: Cell::new(None),
        }
    }

    pub fn count(&self, table: &str) -> i64 {
        self.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

impl Sql for Sqlite {
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, StoreError> {
        if self
            .fail_on
            .get()
            .is_some_and(|marker| sql.contains(marker))
        {
            return Err(StoreError("injected failure"));
        }
        let mut statement = self.conn.prepare(sql).map_err(|_| StoreError("prepare"))?;
        let columns = statement.column_count();
        let bound = params.iter().map(|value| match value {
            SqlValue::Null => rusqlite::types::Value::Null,
            SqlValue::Integer(v) => rusqlite::types::Value::Integer(*v),
            SqlValue::Text(v) => rusqlite::types::Value::Text(v.clone()),
        });
        let mut rows = statement
            .query(rusqlite::params_from_iter(bound))
            .map_err(|_| StoreError("query"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|_| StoreError("step"))? {
            let mut values = Vec::with_capacity(columns);
            for index in 0..columns {
                values.push(
                    match row.get_ref(index).map_err(|_| StoreError("column"))? {
                        ValueRef::Null => SqlValue::Null,
                        ValueRef::Integer(v) => SqlValue::Integer(v),
                        ValueRef::Text(v) => SqlValue::Text(String::from_utf8(v.to_vec()).unwrap()),
                        _ => return Err(StoreError("unsupported column")),
                    },
                );
            }
            out.push(values);
        }
        Ok(out)
    }
}

/// Mirrors `transactionSync`: commit on `Ok`, roll back on `Err`.
pub fn tx<T, E>(db: &Sqlite, operation: impl FnOnce(&Sqlite) -> Result<T, E>) -> Result<T, E> {
    db.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = operation(db);
    db.conn
        .execute_batch(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })
        .unwrap();
    result
}
