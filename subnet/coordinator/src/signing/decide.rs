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

use super::{SignConfig, attempt, attempt_subject, held_target, log, restart_floor, restart_target};
use crate::sql::{SqlValue, bind_u64};
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

/// Sets `key` in a JSON object (a no-op on any other value).
fn set(v: &mut Value, key: &str, value: Value) {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_owned(), value);
    }
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
    set(&mut out, "operator_seq", json!(seq));
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
    match action.as_str() {
        "attempt_resolve" => attempt_resolve(sql, cfg, &req, key, now_ms),
        "restart" => restart(sql, cfg, trusted, &req, key, finalized_epoch, now_ms),
        _ => Ok(Err("unknown_action")),
    }
}


// ------------------------------------------------------------------ P1-R

/// One cohort of a restart's set S: its number, label and what the
/// coordinator knows of it (`pending`: its closure was checked and it waits
/// as an intermediate; `in_record`: listed with a manifest, not handled).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PastCohort {
    pub cohort: u64,
    pub label: String,
    pub state: String,
}

/// A restart's set S, the preview's information list and the unlisted
/// numbers ([`restart_set`]).
pub type RestartSet = (Vec<PastCohort>, Vec<(u64, String)>, Vec<u64>);

/// What a restart at floor `floor` passes, computed from the journal alone
/// (review-p1-final M1: bounded by opening epoch, never by number):
///
/// - `past` (S): every pending intermediate, and every in-record label that
///   opened in [current floor, `floor`) and was neither handled, passed,
///   recorded absent nor named by an earlier restart;
/// - `info`: for the preview only, every number in (the last handled or
///   pending one, `last`] outside S that is not passed or absent, with
///   `not_listed`, `not_opened` or `opened_at_or_after_floor`;
/// - `unlisted`: the `not_listed` numbers of `info`, which the signed
///   record also names (S1).
pub fn restart_set(sql: &impl Sql, floor: u64, last: Option<u64>) -> Result<RestartSet, StoreError> {
    let current = restart_floor(sql)?.unwrap_or(0);
    let mut past = Vec::new();
    for r in sql.exec("SELECT cohort, label FROM sign_auto_pending ORDER BY cohort", &[])? {
        past.push(PastCohort { cohort: uint(&r, 0)?, label: text(&r, 1)?.to_owned(), state: "pending".into() });
    }
    for r in sql.exec(
        "SELECT cohort, label FROM sign_auto_labels l WHERE state = 'in_record' AND opened_epoch >= ? AND opened_epoch < ?
           AND cohort NOT IN (SELECT cohort FROM sign_auto_absent)
           AND cohort NOT IN (SELECT cohort FROM sign_restart_past)
           AND label NOT IN (SELECT label FROM sign_auto_pending)
         ORDER BY cohort, label",
        &[bind_u64(current)?, bind_u64(floor)?],
    )? {
        past.push(PastCohort { cohort: uint(&r, 0)?, label: text(&r, 1)?.to_owned(), state: "in_record".into() });
    }
    past.sort_by(|a, b| (a.cohort, &a.label).cmp(&(b.cohort, &b.label)));
    let mut info = Vec::new();
    let mut unlisted = Vec::new();
    if let Some(last) = last {
        let after = sql
            .exec(
                "SELECT MAX(c) FROM (SELECT MAX(cohort) AS c FROM sign_auto_cohorts WHERE opened_epoch >= ?
                   UNION ALL SELECT MAX(cohort) FROM sign_auto_pending WHERE opened_epoch >= ?)",
                &[bind_u64(current)?, bind_u64(current)?],
            )?
            .first()
            .and_then(|r| match r.first() {
                Some(SqlValue::Integer(n)) => Some(*n as u64),
                _ => None,
            });
        // Nothing handled since the floor: the range starts at the record's
        // lowest listed number, so numbers below the record are no noise.
        let lowest = sql
            .exec("SELECT MIN(cohort) FROM sign_auto_labels WHERE cohort IS NOT NULL AND opened_epoch >= ?", &[bind_u64(current)?])?
            .first()
            .and_then(|r| match r.first() {
                Some(SqlValue::Integer(n)) => Some(*n as u64),
                _ => None,
            });
        let from = match (after, lowest) {
            (Some(a), _) => a + 1,
            (None, Some(l)) => l,
            (None, None) => last.saturating_add(1),
        };
        for n in from..=last {
            if past.iter().any(|p| p.cohort == n) {
                continue;
            }
            let absent = !sql.exec("SELECT 1 FROM sign_auto_absent WHERE cohort = ?", &[bind_u64(n)?])?.is_empty();
            let labels = sql.exec("SELECT state, opened_epoch FROM sign_auto_labels WHERE cohort = ?", &[bind_u64(n)?])?;
            if absent || labels.iter().any(|r| matches!(text(r, 0), Ok("passed" | "handled"))) {
                continue;
            }
            let state = match labels.first() {
                None => "not_listed",
                Some(r) => match r.get(1) {
                    Some(SqlValue::Integer(o)) if (*o as u64) >= floor => "opened_at_or_after_floor",
                    Some(SqlValue::Integer(_)) => continue,
                    _ => "not_opened",
                },
            };
            if state == "not_listed" {
                unlisted.push(n);
            }
            info.push((n, state.to_owned()));
        }
    }
    Ok((past, info, unlisted))
}

fn u64_list(v: Option<&Value>) -> Option<Vec<u64>> {
    v?.as_array()?.iter().map(Value::as_u64).collect()
}

/// P1-R, "burn now" (`action: "restart"`, under the offline key): with
/// signing paused, sets the held target aside and installs the launch burn
/// row from floor `floor` (the existing [`restart_target`] and its floors),
/// with the P1 checks: `floor` at most the finalized epoch + 1
/// (`restart_future`, D13); the launch authorization issued at `floor`
/// (`restart_issue_mismatch`); `floor` above every epoch holding a signed
/// attempt (`epoch_signed`); no unresolved attempt (`attempt_unresolved`);
/// `expect.held` the held target; and `past` / `unlisted` exactly the
/// journal's [`restart_set`] (`past_changed`). Planned operations of epochs
/// at or after `floor` with no attempt are dropped and planned again under
/// the burn row. Each cohort in `past` is journaled as `restarted_past` in
/// `sign_restart_past`, which no handoff query reads, and a signed,
/// content-free `ChainRestart` record names the floor, the launch
/// authorization, `last`, `past` and `unlisted`. A refusal changes nothing.
pub fn restart(
    sql: &impl Sql,
    cfg: &SignConfig,
    trusted: &slop_ninja_protocol::TrustedKeys,
    req: &Request,
    key: Option<&OperatorKey>,
    observed_epoch: Option<u64>,
    now_ms: u64,
) -> Result<Answer, StoreError> {
    let chain = chain_of(cfg);
    let statement = match operator::begin(sql, req, "coordinator.restart", true, key, &address(&chain))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(st)) => match st {
            Some(st) => st,
            None => return Ok(Err("authorization_required")),
        },
        Err(code) => return Ok(Err(code)),
    };
    let Some(floor) = req.body.get("floor").and_then(Value::as_u64) else { return Ok(Err("floor_missing")) };
    let bundle = req.body.get("bundle").and_then(Value::as_str).unwrap_or_default().to_owned();
    let last = req.body.get("last").and_then(Value::as_u64);
    let (Some(named_past), Some(named_unlisted)) = (u64_list(req.body.get("past")), u64_list(req.body.get("unlisted"))) else {
        return Ok(Err("past_missing"));
    };
    // D13: the floor is bounded by finalized chain state, from the journal's
    // own schedule and the Worker's RPC reading.
    let finalized = super::authority::finalized_epoch(sql)?.max(observed_epoch);
    let Some(finalized) = finalized else { return Ok(Err("finalized_unknown")) };
    if floor > finalized.saturating_add(1) {
        return Ok(Err("restart_future"));
    }
    let held = held_target(sql, cfg)?;
    if req.expect.get("held").and_then(Value::as_str) != Some(held.digest().to_string().as_str()) {
        return Ok(Err("expect_mismatch"));
    }
    // No epoch at or after the floor holds a signed attempt, and no attempt
    // is unresolved: an epoch holds one operation, and nothing is signed
    // twice.
    let signed_max = sql
        .exec("SELECT MAX(o.epoch) FROM commit_ops o WHERE EXISTS (SELECT 1 FROM sign_attempts a WHERE a.op_id = o.op_id)", &[])?
        .first()
        .and_then(|r| match r.first() {
            Some(SqlValue::Integer(n)) => Some(*n as u64),
            _ => None,
        });
    if signed_max.is_some_and(|e| e >= floor) {
        return Ok(Err("epoch_signed"));
    }
    if !sql.exec("SELECT 1 FROM sign_attempts WHERE state IN ('signed', 'sending', 'submitted', 'uncertain') LIMIT 1", &[])?.is_empty() {
        return Ok(Err("attempt_unresolved"));
    }
    // The launch authorization's issue epoch is the floor: validators see
    // only that number (P1-V).
    let issued = match crate::target::parse_v2(bundle.as_bytes()).and_then(|b| {
        let launch = slop_ninja_protocol::ActiveTarget::launch(&chain).map_err(|_| crate::target::Refusal { code: "launch_target", detail: String::new() })?;
        crate::target::verify_v2(&b, trusted, &cfg.genesis.0, cfg.netuid, &launch)
    }) {
        Ok(v) => match v.checked_terms(trusted) {
            Ok(t) => (t.issued_epoch, t.digest),
            Err(_) => return Ok(Err("authorization_refused")),
        },
        Err(r) => return Ok(Err(r.code)),
    };
    if issued.0 != floor {
        return Ok(Err("restart_issue_mismatch"));
    }
    let (past, info, unlisted) = restart_set(sql, floor, last)?;
    let computed: Vec<u64> = past.iter().map(|p| p.cohort).collect();
    let mut named = named_past.clone();
    named.sort_unstable();
    named.dedup();
    let mut computed_sorted = computed.clone();
    computed_sorted.sort_unstable();
    computed_sorted.dedup();
    if named != computed_sorted || named_unlisted != unlisted {
        return Ok(Err("past_changed"));
    }
    let effect = json!({"floor": floor, "held": held.digest().to_string(), "held_activated_epoch": held.activated_epoch(),
        "launch_authorization": issued.1.to_string(), "last": last, "past": past, "unlisted": unlisted, "info": info});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    // Planned operations at or after the floor that never signed are dropped
    // and planned again under the burn row.
    let dropped = sql.exec(
        "DELETE FROM commit_ops WHERE epoch >= ? AND NOT EXISTS (SELECT 1 FROM sign_attempts a WHERE a.op_id = commit_ops.op_id)
         RETURNING epoch",
        &[bind_u64(floor)?],
    )?;
    let previous = (!held.is_launch_burn()).then(|| (held.digest(), held.activated_epoch()));
    let restarted = match restart_target(sql, cfg, trusted, &bundle, floor, now_ms)? {
        Ok(r) => r,
        // A refusal after the checks above rolls the whole action back.
        Err(r) => return Err(StoreError(r.code)),
    };
    for p in &past {
        sql.exec(
            "INSERT INTO sign_restart_past (floor, cohort, label, state, reason, decision_sha256, at_ms)
             VALUES (?, ?, ?, ?, 'restarted_past', ?, ?)",
            &[
                bind_u64(floor)?,
                bind_u64(p.cohort)?,
                p.label.as_str().into(),
                p.state.as_str().into(),
                req.decision_sha256.map_or(SqlValue::Null, |d| d.hex().into()),
                bind_u64(now_ms)?,
            ],
        )?;
    }
    // The numbers the signed record names as unlisted: a label found later
    // for one of them is already named (`note_pre_floor`).
    for n in &unlisted {
        sql.exec(
            "INSERT INTO sign_restart_past (floor, cohort, label, state, reason, decision_sha256, at_ms)
             VALUES (?, ?, NULL, 'unlisted', 'named_unlisted', ?, ?) ON CONFLICT(floor, cohort) DO NOTHING",
            &[bind_u64(floor)?, bind_u64(*n)?, req.decision_sha256.map_or(SqlValue::Null, |d| d.hex().into()), bind_u64(now_ms)?],
        )?;
    }
    let record = slop_ninja_protocol::decisions::ChainRestart {
        chain: chain.clone(),
        floor,
        launch_authorization: issued.1,
        previous_target: previous.map(|p| p.0),
        previous_activated_epoch: previous.and_then(|p| p.1),
        last,
        past: past
            .iter()
            .map(|p| slop_ninja_protocol::decisions::RestartPassed { cohort: p.cohort, state: p.state.clone() })
            .collect(),
        unlisted: unlisted.clone(),
        decision: req.decision_sha256,
        statement,
    };
    queue_restart_record(sql, floor, "restart", None, &record, now_ms)?;
    log(sql, now_ms, None, None, None, "chain_restart", Some(&format!("floor {floor} past {computed:?} unlisted {unlisted:?}")))?;
    let dropped: Vec<u64> = dropped.iter().filter_map(|r| match r.first() {
        Some(SqlValue::Integer(e)) => Some(*e as u64),
        _ => None,
    }).collect();
    let mut out = effect;
    set(&mut out, "restarted", serde_json::to_value(&restarted).unwrap_or_default());
    set(&mut out, "replanned_epochs", json!(dropped));
    let seq = operator::record(sql, req, "coordinator.restart", Some(&statement), &out, Some(finalized), now_ms)?;
    set(&mut out, "operator_seq", json!(seq));
    Ok(Ok(out))
}

/// Queues a signed restart record (signed with the authorization key and
/// written by the pointer pass).
pub fn queue_restart_record<R: slop_ninja_protocol::Record>(
    sql: &impl Sql,
    floor: u64,
    kind: &str,
    cohort: Option<u64>,
    record: &R,
    now_ms: u64,
) -> Result<(), StoreError> {
    let bytes = record.to_bytes().map_err(|_| StoreError("restart record"))?;
    let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    sql.exec(
        "INSERT INTO sign_restart_records (floor, kind, cohort, record_hex, sha256, at_ms) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(floor, kind, cohort) DO NOTHING",
        &[bind_u64(floor)?, kind.into(), cohort.map_or(Ok(SqlValue::Null), bind_u64)?, hex::encode(&bytes).into(), sha.into(), bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// A restart record to write: its id, key, kind and record bytes.
pub type DueRecord = (i64, String, String, Vec<u8>);

/// The restart records not yet written.
pub fn restart_records_due(sql: &impl Sql) -> Result<Vec<DueRecord>, StoreError> {
    sql.exec("SELECT id, floor, kind, cohort, record_hex FROM sign_restart_records WHERE written_ms IS NULL ORDER BY id LIMIT 4", &[])?
        .iter()
        .map(|r| {
            let id = match r.first() {
                Some(SqlValue::Integer(i)) => *i,
                _ => return Err(StoreError("restart record id")),
            };
            let floor = uint(r, 1)?;
            let kind = text(r, 2)?.to_owned();
            let key = match r.get(3) {
                Some(SqlValue::Integer(c)) => format!("chain-restarts/{floor}/addendum-{c}.signed"),
                _ => format!("chain-restarts/{floor}/restart.signed"),
            };
            let bytes = hex::decode(text(r, 4)?).map_err(|_| StoreError("restart record hex"))?;
            Ok((id, key, kind, bytes))
        })
        .collect()
}

/// A restart record is durable in the receipts bucket.
pub fn restart_record_written(sql: &impl Sql, id: i64, now_ms: u64) -> Result<(), StoreError> {
    sql.exec("UPDATE sign_restart_records SET written_ms = ? WHERE id = ?", &[bind_u64(now_ms)?, SqlValue::Integer(id)])?;
    Ok(())
}

/// A cohort found after a restart that opened before its floor (P1-R,
/// review-p1-final M1 and S1): an in-record label `label` with number
/// `cohort` and opening epoch `opened`, in [the restart's previous floor,
/// its floor), not handled or passed, and with no `sign_restart_past` row,
/// is journaled `restarted_past_unlisted` with a sign-log entry; when the
/// restart's signed record did not name its number (as `past` or
/// `unlisted`), a signed `RestartAddendum` is queued. Such a cohort chained
/// from the target the restart set aside, so it could never install.
pub fn note_pre_floor(sql: &impl Sql, cfg: &SignConfig, label: &str, cohort: u64, opened: u64, now_ms: u64) -> Result<(), StoreError> {
    let resets = sql.exec("SELECT from_epoch FROM sign_held_target_resets ORDER BY seq DESC LIMIT 2", &[])?;
    let Some(floor) = resets.first().map(|r| uint(r, 0)).transpose()? else { return Ok(()) };
    let previous = resets.get(1).map(|r| uint(r, 0)).transpose()?.unwrap_or(0);
    if opened < previous || opened >= floor {
        return Ok(());
    }
    let state = sql.exec("SELECT state FROM sign_auto_labels WHERE label = ?", &[label.into()])?;
    if state.first().is_some_and(|r| matches!(text(r, 0), Ok("handled" | "passed"))) {
        return Ok(());
    }
    let row = sql.exec("SELECT reason FROM sign_restart_past WHERE floor = ? AND cohort = ?", &[bind_u64(floor)?, bind_u64(cohort)?])?;
    match row.first().map(|r| text(r, 0).map(str::to_owned)).transpose()? {
        Some(reason) if reason != "named_unlisted" => return Ok(()),
        Some(_) => {
            sql.exec(
                "UPDATE sign_restart_past SET label = ?, state = 'in_record', reason = 'restarted_past_unlisted', at_ms = ?
                 WHERE floor = ? AND cohort = ?",
                &[label.into(), bind_u64(now_ms)?, bind_u64(floor)?, bind_u64(cohort)?],
            )?;
        }
        None => {
            sql.exec(
                "INSERT INTO sign_restart_past (floor, cohort, label, state, reason, at_ms)
                 VALUES (?, ?, ?, 'in_record', 'restarted_past_unlisted', ?)",
                &[bind_u64(floor)?, bind_u64(cohort)?, label.into(), bind_u64(now_ms)?],
            )?;
            let restart = sql.exec("SELECT sha256 FROM sign_restart_records WHERE floor = ? AND kind = 'restart'", &[bind_u64(floor)?])?;
            let restart = restart
                .first()
                .map(|r| text(r, 0).map(str::to_owned))
                .transpose()?
                .and_then(|h| hex::decode(h).ok())
                .and_then(|b| <[u8; 32]>::try_from(b).ok())
                .map(slop_ninja_protocol::Digest)
                .unwrap_or(slop_ninja_protocol::Digest([0; 32]));
            let addendum = slop_ninja_protocol::decisions::RestartAddendum {
                chain: chain_of(cfg),
                floor,
                restart,
                cohort,
                cohort_label: label.to_owned(),
                opened_epoch: opened,
            };
            queue_restart_record(sql, floor, "addendum", Some(cohort), &addendum, now_ms)?;
        }
    }
    log(sql, now_ms, None, None, None, "restarted_past_unlisted", Some(&format!("{cohort} {label} opened {opened} below floor {floor}")))
}
