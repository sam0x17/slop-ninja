//! The coordinator's operator decisions and the evidence-based attempt
//! resolution's storage (self-lock phase 1): journal schema v15.

/// v15: the operator action log (`crate::operator`); the attempts table
/// rebuilt so an attempt can settle as `consumed_unverified` (D5) and record
/// that its inclusion block's events were read (`events_checked`); the
/// restart's pre-floor record (`sign_restart_past`, P1-R), which no handoff
/// query reads; and the signed restart records awaiting their write.
pub const SCHEMA_V15: &[&str] = &[
    crate::operator::ACTIONS_TABLE,
    "CREATE TABLE sign_attempts_v15 (
        op_id TEXT NOT NULL,
        attempt INTEGER NOT NULL,
        nonce INTEGER NOT NULL,
        era_birth INTEGER NOT NULL,
        era_period INTEGER NOT NULL,
        expiry INTEGER NOT NULL,
        era_hash TEXT NOT NULL,
        floor_block INTEGER NOT NULL,
        spec_version INTEGER NOT NULL,
        tx_version INTEGER NOT NULL,
        reveal_round INTEGER NOT NULL,
        ciphertext_sha256 TEXT NOT NULL,
        call_sha256 TEXT NOT NULL,
        payload_sha256 TEXT NOT NULL,
        extrinsic_hash TEXT NOT NULL UNIQUE,
        extrinsic_hex TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('signed', 'sending', 'submitted', 'uncertain',
            'rejected', 'included', 'expired', 'displaced', 'consumed_unverified')),
        sends INTEGER NOT NULL CHECK (sends IN (0, 1)),
        submit_detail TEXT,
        search_lo INTEGER,
        search_hi INTEGER,
        checked_block INTEGER,
        included_block INTEGER,
        included_hash TEXT,
        included_index INTEGER,
        dispatch TEXT,
        signed_generation INTEGER NOT NULL,
        signed_at_ms INTEGER NOT NULL,
        sent_at_ms INTEGER,
        resolved_at_ms INTEGER,
        search_scan INTEGER NOT NULL DEFAULT 0,
        events_checked INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (op_id, attempt)
    ) STRICT",
    "INSERT INTO sign_attempts_v15 (op_id, attempt, nonce, era_birth, era_period, expiry, era_hash, floor_block,
        spec_version, tx_version, reveal_round, ciphertext_sha256, call_sha256, payload_sha256, extrinsic_hash,
        extrinsic_hex, state, sends, submit_detail, search_lo, search_hi, checked_block, included_block,
        included_hash, included_index, dispatch, signed_generation, signed_at_ms, sent_at_ms, resolved_at_ms,
        search_scan)
     SELECT op_id, attempt, nonce, era_birth, era_period, expiry, era_hash, floor_block,
        spec_version, tx_version, reveal_round, ciphertext_sha256, call_sha256, payload_sha256, extrinsic_hash,
        extrinsic_hex, state, sends, submit_detail, search_lo, search_hi, checked_block, included_block,
        included_hash, included_index, dispatch, signed_generation, signed_at_ms, sent_at_ms, resolved_at_ms,
        search_scan FROM sign_attempts",
    "DROP TABLE sign_attempts",
    "ALTER TABLE sign_attempts_v15 RENAME TO sign_attempts",
    "CREATE INDEX sign_attempts_state ON sign_attempts (state)",
    "CREATE TABLE sign_restart_past (
        floor INTEGER NOT NULL,
        cohort INTEGER NOT NULL,
        label TEXT,
        state TEXT NOT NULL,
        reason TEXT NOT NULL,
        decision_sha256 TEXT,
        at_ms INTEGER NOT NULL,
        PRIMARY KEY (floor, cohort)
    ) STRICT",
    "CREATE TABLE sign_restart_records (
        id INTEGER PRIMARY KEY,
        floor INTEGER NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('restart', 'addendum')),
        cohort INTEGER,
        record_hex TEXT NOT NULL,
        sha256 TEXT NOT NULL,
        written_ms INTEGER,
        at_ms INTEGER NOT NULL,
        UNIQUE (floor, kind, cohort)
    ) STRICT",
];

use serde_json::{Value, json};
use slop_ninja_protocol::ChainBinding;
use slop_ninja_protocol::operator::OperatorKey;

use super::{SignConfig, attempt, attempt_subject, log};
use crate::operator::{self, Address, Begin, Request};
use crate::sql::{Sql, StoreError, text, uint};

/// The coordinator's address in an operator statement: service
/// `coordinator`, authority `coordinator`, store `journal` (one signing
/// journal per chain), and its chain binding.
pub fn chain_of(cfg: &SignConfig) -> ChainBinding {
    ChainBinding { genesis: slop_ninja_protocol::Digest(cfg.genesis.0), netuid: cfg.netuid, mechanism: 0 }
}

pub fn address(chain: &ChainBinding) -> Address<'_> {
    Address { service: "coordinator", authority: "coordinator", store: "journal", chain, registration: None }
}

/// An operator request's answer: the effect, or a refusal code.
pub type Answer = Result<Value, &'static str>;

/// D5's operator action (`attempt_resolve`, request field `op`: the
/// operation id or its last characters): asks the journal to run the live
/// attempt's evidence check now. It clears that attempt's block backoff and
/// nothing else, so the next signing round reads the inclusion block's
/// events (then its body) again; it never takes the operator's word for the
/// attempt's fate. A token-level action (`expect: {state}`).
pub fn attempt_resolve(
    sql: &impl Sql,
    cfg: &SignConfig,
    req: &Request,
    key: Option<&OperatorKey>,
    now_ms: u64,
) -> Result<Answer, StoreError> {
    let chain = chain_of(cfg);
    match operator::begin(sql, req, "coordinator.attempt_resolve", false, key, &address(&chain))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(_)) => {}
        Err(code) => return Ok(Err(code)),
    }
    let want = req.body.get("op").and_then(Value::as_str).unwrap_or_default();
    if want.is_empty() {
        return Ok(Err("op_missing"));
    }
    let rows = sql.exec(
        "SELECT op_id, attempt, state FROM sign_attempts WHERE state IN ('submitted', 'uncertain', 'included')
         ORDER BY signed_at_ms DESC",
        &[],
    )?;
    let mut found = None;
    for r in &rows {
        let op_id = text(r, 0)?;
        if op_id == want || op_id.ends_with(want) {
            found = Some((op_id.to_owned(), uint(r, 1)? as u32, text(r, 2)?.to_owned()));
            break;
        }
    }
    let Some((op_id, n, state)) = found else {
        return Ok(Err("attempt_not_live"));
    };
    if req.expect.get("state").and_then(Value::as_str) != Some(state.as_str()) {
        return Ok(Err("expect_mismatch"));
    }
    let subject = attempt_subject(&op_id, n);
    let backoff = sql
        .exec("SELECT failures, code FROM sign_failures WHERE subject = ? AND settled = 0", &[subject.as_str().into()])?
        .first()
        .map(|r| Ok::<_, StoreError>((uint(r, 0)?, text(r, 1)?.to_owned())))
        .transpose()?;
    let effect = json!({"attempt": n, "state": state, "cleared_backoff": backoff.as_ref().map(|(f, c)| json!({"failures": f, "code": c}))});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    if attempt(sql, &op_id, n)?.is_none() {
        return Ok(Err("attempt_not_live"));
    }
    sql.exec("DELETE FROM sign_failures WHERE subject = ? AND settled = 0", &[subject.as_str().into()])?;
    log(sql, now_ms, None, Some(&op_id), Some(n), "attempt_resolve", Some("operator: check the evidence now"))?;
    let seq = operator::record(sql, req, "coordinator.attempt_resolve", None, &effect, None, now_ms)?;
    let mut out = effect;
    out["operator_seq"] = json!(seq);
    Ok(Ok(out))
}

/// Dispatches one operator decision posted to `/signing/decide` by its
/// `action` field.
pub fn decide(
    sql: &impl Sql,
    cfg: &SignConfig,
    trusted: &slop_ninja_protocol::TrustedKeys,
    body: Value,
    key: Option<&OperatorKey>,
    finalized_epoch: Option<u64>,
    now_ms: u64,
) -> Result<Answer, StoreError> {
    let action = body.get("action").and_then(Value::as_str).unwrap_or_default().to_owned();
    let req = match operator::parse(body) {
        Ok(r) => r,
        Err(code) => return Ok(Err(code)),
    };
    let _ = (trusted, finalized_epoch);
    match action.as_str() {
        "attempt_resolve" => attempt_resolve(sql, cfg, &req, key, now_ms),
        _ => Ok(Err("unknown_action")),
    }
}
