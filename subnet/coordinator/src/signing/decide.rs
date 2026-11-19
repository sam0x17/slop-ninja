//! The coordinator's operator decisions and the evidence-based attempt
//! resolution's storage and immutable restart rules: journal schemas v15-v16.

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

use super::{SignConfig, attempt, attempt_subject, held_target, log, restart_checks, restart_floor, restart_target};
use crate::sql::{SqlValue, bind_u64};
use crate::operator::{self, Address, Begin, Request};
use crate::sql::{Sql, StoreError, text, uint};

/// v16 binds each fresh record's rules to the restart payload and its
/// operator evidence. Queue delivery may change; these bindings may not.
pub const SCHEMA_V16: &[&str] = &[
    "CREATE TABLE record_rules (
        floor INTEGER PRIMARY KEY CHECK (floor >= 0),
        reward_rule TEXT NOT NULL, quant_rule TEXT NOT NULL,
        decision_sha256 TEXT, statement_sha256 TEXT,
        record_sha256 TEXT NOT NULL, at_ms INTEGER NOT NULL
    ) STRICT",
    "CREATE TRIGGER record_rules_no_replace BEFORE INSERT ON record_rules
        WHEN EXISTS (SELECT 1 FROM record_rules WHERE floor = NEW.floor)
        BEGIN SELECT RAISE(ABORT, 'record rules are immutable'); END",
    "CREATE TRIGGER record_rules_no_update BEFORE UPDATE ON record_rules
        BEGIN SELECT RAISE(ABORT, 'record rules are immutable'); END",
    "CREATE TRIGGER record_rules_no_delete BEFORE DELETE ON record_rules
        BEGIN SELECT RAISE(ABORT, 'record rules are immutable'); END",
];

struct RestartBinding {
    floor: u64,
    rules: crate::config::RecordRules,
    decision: Option<slop_ninja_protocol::Digest>,
    statement: Option<slop_ninja_protocol::Digest>,
    legacy: bool,
}

impl RestartBinding {
    fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        use slop_ninja_protocol::{Record as _, decisions::{ChainRestart, ChainRestartV2}};
        if let Ok(r) = ChainRestartV2::from_bytes(bytes) {
            Ok(Self {
                floor: r.floor,
                rules: crate::config::RecordRules { reward: r.reward_rule, quant: r.quant_rule },
                decision: r.decision, statement: r.statement, legacy: false,
            })
        } else {
            let r = ChainRestart::from_bytes(bytes).map_err(|_| StoreError("restart record"))?;
            Ok(Self {
                floor: r.floor, rules: crate::config::RecordRules::default(),
                decision: r.decision, statement: r.statement, legacy: true,
            })
        }
    }

    fn values(&self, sha: &str) -> Vec<SqlValue> {
        vec![
            self.rules.reward.name().into(), self.rules.quant.name().into(),
            self.decision.map_or(SqlValue::Null, |d| d.hex().into()),
            self.statement.map_or(SqlValue::Null, |d| d.hex().into()), sha.into(),
        ]
    }

    fn check(&self, sql: &impl Sql, sha: &str) -> Result<(), StoreError> {
        let rows = sql.exec(
            "SELECT reward_rule, quant_rule, decision_sha256, statement_sha256, record_sha256
             FROM record_rules WHERE floor = ?", &[bind_u64(self.floor)?],
        )?;
        match rows.first() {
            Some(row) if *row == self.values(sha) => Ok(()),
            None if self.legacy => Ok(()),
            _ => Err(StoreError("restart rule binding")),
        }
    }
}

fn record_sha(bytes: &[u8]) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))
}

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

/// A refused operator request: its code and, for a restart, the restart
/// set the journal computed (content-free numbers, labels and states), so
/// the operator can name it without guessing.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub code: &'static str,
    pub detail: Option<Value>,
}

impl From<&'static str> for Refused {
    fn from(code: &'static str) -> Self {
        Refused { code, detail: None }
    }
}

impl Refused {
    /// The admin route's answer: `{"refused": code}`, with `detail` beside
    /// it when present.
    pub fn to_json(&self) -> Value {
        match &self.detail {
            Some(d) => json!({"refused": self.code, "detail": d}),
            None => json!({"refused": self.code}),
        }
    }
}

/// An operator request's answer: the effect, or a refusal.
pub type Answer = Result<Value, Refused>;

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
        Err(code) => return Ok(Err(code.into())),
    }
    let want = req.body.get("op").and_then(Value::as_str).unwrap_or_default();
    if want.is_empty() {
        return Ok(Err("op_missing".into()));
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
        return Ok(Err("attempt_not_live".into()));
    };
    if req.expect.get("state").and_then(Value::as_str) != Some(state.as_str()) {
        return Ok(Err("expect_mismatch".into()));
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
        return Ok(Err("attempt_not_live".into()));
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
        Err(code) => return Ok(Err(code.into())),
    };
    match action.as_str() {
        "attempt_resolve" => attempt_resolve(sql, cfg, &req, key, now_ms),
        "restart" => restart(sql, cfg, trusted, &req, key, finalized_epoch, now_ms),
        _ => Ok(Err("unknown_action".into())),
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

/// The restart set as the refusals and the preview show it.
fn set_json(floor: u64, last: Option<u64>, set: &RestartSet) -> Value {
    let (past, info, unlisted) = set;
    let info: Vec<Value> = info.iter().map(|(n, state)| json!({"cohort": n, "state": state})).collect();
    json!({"floor": floor, "last": last, "past": past, "unlisted": unlisted, "info": info})
}

/// P1-R, "burn now" (`action: "restart"`, under the offline key once
/// `OPERATOR_DECISION_KEY` is configured): with signing paused, sets the
/// held target aside and installs the launch burn row from floor `floor`
/// (the existing [`restart_target`] and its floors), with the P1 checks:
/// `floor` at most the finalized epoch + 1 (`restart_future`, D13); the
/// launch authorization issued at `floor` (`restart_issue_mismatch`);
/// `floor` above every epoch holding a signed attempt (`epoch_signed`); no
/// unresolved attempt (`attempt_unresolved`); `expect.held` the held
/// target; [`super::restart_checks`] (signing paused, the floors, the launch
/// bundle), each refused as its own code; and `past` / `unlisted` exactly
/// the journal's [`restart_set`] (`past_missing`, `past_changed`).
///
/// Every refusal after the floor is read carries the restart set the
/// journal computed for this `floor` and `last` (`detail`: `past`,
/// `unlisted` and the preview's `info`), so one refused request tells the
/// operator exactly what to name. A preview runs every check and writes
/// nothing; it may leave `past` and `unlisted` out, and then answers the
/// computed set with `past_named: false` (named ones are compared as in the
/// post). The signing status shows the set at the next floor
/// (`restart.past`).
///
/// Planned operations of epochs at or after `floor` with no attempt are
/// dropped and planned again under the burn row. Each cohort in `past` is
/// journaled as `restarted_past` in `sign_restart_past`, which no handoff
/// query reads, and a signed, content-free `ChainRestartV2` record names the
/// floor, the launch authorization, `last`, `past`, `unlisted` and the fresh
/// record's `reward_rule` and `quant_rule`, which the body must name
/// (`rules_missing`, `rule_unknown`) and the offline statement signs; the
/// handoff installs only targets of those rules from then on
/// ([`record_rules`]). A refusal changes nothing.
///
/// While no operator key is configured (`key` is `None`), the restart is a
/// token-level action, so a coordinator deployed before its key still has a
/// brake: it is logged unsigned and its record names no statement. Once a
/// key is configured it needs the offline signature.
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
    let statement = match operator::begin(sql, req, "coordinator.restart", key.is_some(), key, &address(&chain))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(st)) => st,
        Err(code) => return Ok(Err(code.into())),
    };
    let Some(floor) = req.body.get("floor").and_then(Value::as_u64) else { return Ok(Err("floor_missing".into())) };
    // The fresh record's reward and quantization rules, named in the body
    // (which the offline statement signs): a rule change happens here only.
    let rules = match (req.body.get("reward_rule").and_then(Value::as_str), req.body.get("quant_rule").and_then(Value::as_str)) {
        (Some(r), Some(q)) => match crate::config::RecordRules::parse(Some(&format!("{r},{q}"))) {
            Ok(rules) => rules,
            Err(_) => return Ok(Err("rule_unknown".into())),
        },
        _ => return Ok(Err("rules_missing".into())),
    };
    let bundle = req.body.get("bundle").and_then(Value::as_str).unwrap_or_default().to_owned();
    let last = req.body.get("last").and_then(Value::as_u64);
    // The set this floor passes, from the journal alone; every refusal
    // from here names it.
    let computed_set = restart_set(sql, floor, last)?;
    let mut detail = set_json(floor, last, &computed_set);
    set(&mut detail, "reward_rule", json!(rules.reward.name()));
    set(&mut detail, "quant_rule", json!(rules.quant.name()));
    let refuse = |code: &'static str| -> Result<Answer, StoreError> { Ok(Err(Refused { code, detail: Some(detail.clone()) })) };
    let (past, info, unlisted) = computed_set;
    // D13: the floor is bounded by finalized chain state, from the journal's
    // own schedule and the Worker's RPC reading.
    let finalized = super::authority::finalized_epoch(sql)?.max(observed_epoch);
    let Some(finalized) = finalized else { return refuse("finalized_unknown") };
    if floor > finalized.saturating_add(1) {
        return refuse("restart_future");
    }
    let held = held_target(sql, cfg)?;
    if req.expect.get("held").and_then(Value::as_str) != Some(held.digest().to_string().as_str()) {
        return refuse("expect_mismatch");
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
        return refuse("epoch_signed");
    }
    if !sql.exec("SELECT 1 FROM sign_attempts WHERE state IN ('signed', 'sending', 'submitted', 'uncertain') LIMIT 1", &[])?.is_empty() {
        return refuse("attempt_unresolved");
    }
    // Every check of the restart itself (signing paused, the floors, the
    // launch bundle), before anything is written: each refusal is its own
    // code, in the preview too.
    let launch = match restart_checks(sql, cfg, trusted, &bundle, floor)? {
        Ok(l) => l,
        Err(r) => return refuse(r.code),
    };
    // The launch authorization's issue epoch is the floor: validators see
    // only that number (P1-V).
    if launch.issued_epoch != floor {
        return refuse("restart_issue_mismatch");
    }
    let computed: Vec<u64> = past.iter().map(|p| p.cohort).collect();
    let named = match (u64_list(req.body.get("past")), u64_list(req.body.get("unlisted"))) {
        (Some(mut p), Some(u)) => {
            p.sort_unstable();
            p.dedup();
            let mut c = computed.clone();
            c.sort_unstable();
            c.dedup();
            if p != c || u != unlisted {
                return refuse("past_changed");
            }
            true
        }
        // A preview may ask for the set instead of naming it.
        _ if req.preview => false,
        _ => return refuse("past_missing"),
    };
    let effect = json!({"floor": floor, "held": held.digest().to_string(), "held_activated_epoch": held.activated_epoch(),
        "launch_authorization": launch.digest.to_string(), "last": last, "past": past, "unlisted": unlisted, "info": info,
        "signed": statement.is_some(), "reward_rule": rules.reward.name(), "quant_rule": rules.quant.name()});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "past_named": named, "effect": effect})));
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
        // Checked above in this transaction: a refusal here is a defect,
        // and it rolls the whole action back.
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
    let record = slop_ninja_protocol::decisions::ChainRestartV2 {
        chain: chain.clone(),
        floor,
        launch_authorization: launch.digest,
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
        reward_rule: rules.reward,
        quant_rule: rules.quant,
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
    let seq = operator::record(sql, req, "coordinator.restart", statement.as_ref(), &out, Some(finalized), now_ms)?;
    set(&mut out, "operator_seq", json!(seq));
    Ok(Ok(out))
}

/// The rules of the chain record in force: those the latest restart named
/// (`ChainRestartV2`), or `reward-v2,quant-v1` for the record in force before
/// restarts named their rules (no restart, or a version-1 restart record).
pub fn record_rules(sql: &impl Sql) -> Result<crate::config::RecordRules, StoreError> {
    let rows = sql.exec("SELECT floor, record_hex, sha256, cohort FROM sign_restart_records WHERE kind = 'restart' ORDER BY floor DESC, id DESC LIMIT 1", &[])?;
    let latest = sql.exec("SELECT floor FROM record_rules ORDER BY floor DESC LIMIT 1", &[])?;
    let Some(row) = rows.first() else {
        if !latest.is_empty() {
            return Err(StoreError("restart rule binding"));
        }
        return Ok(crate::config::RecordRules::default());
    };
    let bytes = hex::decode(text(row, 1)?).map_err(|_| StoreError("restart record"))?;
    let binding = RestartBinding::decode(&bytes)?;
    let sha = record_sha(&bytes);
    if binding.floor != uint(row, 0)? || sha != text(row, 2)? || row.get(3) != Some(&SqlValue::Null) {
        return Err(StoreError("restart record binding"));
    }
    if let Some(latest) = latest.first()
        && uint(latest, 0)? != binding.floor
    {
        return Err(StoreError("restart rule floor"));
    }
    binding.check(sql, &sha)?;
    Ok(binding.rules)
}

/// Sign the queued bytes without upgrading a legacy restart's version.
pub fn sign_restart_record(
    kind: &str,
    bytes: &[u8],
    key: &slop_ninja_protocol::SigningKey,
) -> slop_ninja_protocol::Result<Vec<u8>> {
    use slop_ninja_protocol::{Record as _, decisions, sign};
    match kind {
        "restart" => match decisions::ChainRestartV2::from_bytes(bytes) {
            Ok(record) => sign(record, key).and_then(|signed| signed.to_bytes()),
            Err(_) => decisions::ChainRestart::from_bytes(bytes)
                .and_then(|record| sign(record, key))
                .and_then(|signed| signed.to_bytes()),
        },
        "addendum" => decisions::RestartAddendum::from_bytes(bytes)
            .and_then(|record| sign(record, key))
            .and_then(|signed| signed.to_bytes()),
        _ => Err(slop_ninja_protocol::Error::Invalid("restart record kind")),
    }
}

/// Queues a signed restart record (signed with the authorization key and
/// written by the pointer pass). Call in the restart transaction: its rule
/// binding, held-target change and operator action must commit together.
pub fn queue_restart_record<R: slop_ninja_protocol::Record>(
    sql: &impl Sql,
    floor: u64,
    kind: &str,
    cohort: Option<u64>,
    record: &R,
    now_ms: u64,
) -> Result<(), StoreError> {
    let bytes = record.to_bytes().map_err(|_| StoreError("restart record"))?;
    let sha = record_sha(&bytes);
    let binding = match kind {
        "restart" if cohort.is_none() => {
            let binding = RestartBinding::decode(&bytes)?;
            if binding.floor != floor {
                return Err(StoreError("restart record floor"));
            }
            Some(binding)
        }
        "addendum" => {
            use slop_ninja_protocol::Record as _;
            let r = slop_ninja_protocol::decisions::RestartAddendum::from_bytes(&bytes)
                .map_err(|_| StoreError("restart addendum"))?;
            if r.floor != floor || Some(r.cohort) != cohort {
                return Err(StoreError("restart addendum binding"));
            }
            None
        }
        _ => return Err(StoreError("restart record kind")),
    };
    let cohort = cohort.map_or(Ok(SqlValue::Null), bind_u64)?;
    let encoded = hex::encode(&bytes);
    // NULL does not participate in SQLite's UNIQUE constraint. Check the
    // nullable cohort explicitly, including pre-migration duplicate rows.
    let existing = sql.exec(
        "SELECT record_hex, sha256 FROM sign_restart_records WHERE floor = ? AND kind = ? AND cohort IS ?",
        &[bind_u64(floor)?, kind.into(), cohort.clone()],
    )?;
    if !existing.is_empty() {
        for row in &existing {
            if text(row, 0)? != encoded || text(row, 1)? != sha {
                return Err(StoreError("restart record conflict"));
            }
        }
        if let Some(binding) = binding {
            binding.check(sql, &sha)?;
        }
        return Ok(());
    }
    if let Some(binding) = binding {
        let mut values = vec![bind_u64(floor)?];
        values.extend(binding.values(&sha));
        values.push(bind_u64(now_ms)?);
        sql.exec(
            "INSERT INTO record_rules (floor, reward_rule, quant_rule, decision_sha256, statement_sha256, record_sha256, at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?)", &values,
        )?;
    }
    sql.exec(
        "INSERT INTO sign_restart_records (floor, kind, cohort, record_hex, sha256, at_ms) VALUES (?, ?, ?, ?, ?, ?)",
        &[bind_u64(floor)?, kind.into(), cohort, encoded.into(), sha.into(), bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// A restart record to write: its id, key, kind and record bytes.
pub type DueRecord = (i64, String, String, Vec<u8>);

/// The restart records not yet written.
pub fn restart_records_due(sql: &impl Sql) -> Result<Vec<DueRecord>, StoreError> {
    sql.exec("SELECT id, floor, kind, cohort, record_hex, sha256 FROM sign_restart_records WHERE written_ms IS NULL ORDER BY id LIMIT 4", &[])?
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
            let sha = record_sha(&bytes);
            if sha != text(r, 5)? {
                return Err(StoreError("restart record digest"));
            }
            if kind == "restart" {
                let binding = RestartBinding::decode(&bytes)?;
                if binding.floor != floor || r.get(3) != Some(&SqlValue::Null) {
                    return Err(StoreError("restart record binding"));
                }
                binding.check(sql, &sha)?;
            }
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
