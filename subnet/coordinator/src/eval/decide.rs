//! The eval Worker's operator decisions (self-lock phase 1), each through
//! the shared request mechanism ([`crate::operator`]): journaled in
//! `operator_actions` in the same transaction as its effect, idempotent by
//! decision id, previewable, compare-and-set on `expect`, and leaving a
//! signed, content-free public record.
//!
//! - [`abandon`] (D1): the operator ends any cohort that has not closed, with
//!   or without a fault. A queued cohort is withdrawn, an admission window
//!   closes as abandoned, a cohort with lanes running first drains (no new
//!   claim, renewal or attempt extension; it ends once every held claim has
//!   passed, [`finish_drains`]), and a cohort in packaging or the transition,
//!   whose outcome may already be known, is abandoned in place and needs the
//!   offline operator-decision key. The number stays used, the abandonment
//!   record (version 2) names the phase and the digests that existed, and a
//!   cohort with no fault publishes fault `none`.

use serde_json::{Value, json};

use crate::eval::engine::{self, EvalConfig, EvalError, R, i, s};
use crate::operator::{self, Address, Begin, Request};
use crate::sql::{Sql, SqlValue, bind_u64, int, text};
use slop_ninja_protocol::operator::OperatorKey;

/// v30: the operator action log, the drain of an operator abandon and the
/// abandonment's decision fields.
pub const SCHEMA_V30: &[&str] = &[
    crate::operator::ACTIONS_TABLE,
    "ALTER TABLE cohorts ADD COLUMN draining_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN drain_reason TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_operator INTEGER",
    "ALTER TABLE cohorts ADD COLUMN abandon_phase TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_decision TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_statement TEXT",
];

/// The reason code an operator gives for a decision with no other listed
/// reason.
pub const OPERATOR_DECISION: &str = "operator_decision";

/// The phases whose abandon needs the offline signature: the package (and
/// possibly the target) is already in the bucket.
const SIGNED_PHASES: &[&str] = &["packaging", "transition"];
/// The phases with lanes that may hold claims.
const DRAIN_PHASES: &[&str] = &["generating", "sealing", "scoring"];

/// Who the eval decisions are addressed to.
pub fn address<'a>(cfg: &'a EvalConfig, authority: &'a str) -> Address<'a> {
    Address { service: "eval", authority, store: &cfg.instance, chain: &cfg.chain, registration: None }
}

/// The answer to an operator request: the effect (stored for replays), or a
/// refusal code.
pub type Answer = Result<Value, &'static str>;

fn expect_str<'a>(req: &'a Request, field: &str) -> Option<&'a str> {
    req.expect.get(field).and_then(Value::as_str)
}

/// D1: the operator abandon of cohort `label` (request fields `label`,
/// `reason`, `expect: {phase}`). See the module docs.
pub fn abandon(
    sql: &impl Sql,
    cfg: &EvalConfig,
    req: &Request,
    key: Option<&OperatorKey>,
    now_ms: u64,
) -> R<Answer> {
    let label = req.body.get("label").and_then(Value::as_str).unwrap_or_default().to_owned();
    let Some(c) = engine::cohorts(sql)?.into_iter().find(|c| c.label == label) else {
        return Ok(Err("cohort_unknown"));
    };
    let signed = SIGNED_PHASES.contains(&c.phase.as_str());
    let authority = cfg.authority.name().to_owned();
    let statement = match operator::begin(sql, req, "eval.abandon", signed, key, &address(cfg, &authority))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(st)) => st,
        Err(code) => return Ok(Err(code)),
    };
    match c.phase.as_str() {
        "closed" => return Ok(Err("cohort_closed")),
        "abandoned" => return Ok(Err("cohort_abandoned")),
        _ => {}
    }
    if expect_str(req, "phase") != Some(c.phase.as_str()) {
        return Ok(Err("expect_mismatch"));
    }
    let fault = c.fault.clone();
    let reason_ok = req.reason == OPERATOR_DECISION
        || engine::ABANDON_REASONS.contains(&req.reason.as_str())
        || fault.as_deref() == Some(req.reason.as_str());
    if !reason_ok {
        return Ok(Err("reason_unlisted"));
    }
    let o = engine::observed(sql)?;
    let drains = DRAIN_PHASES.contains(&c.phase.as_str()) && outstanding_claims(sql, c.seq, now_ms)? > 0;
    let effect = json!({
        "label": c.label,
        "cohort": c.plan.cohort,
        "phase_at_abandon": c.phase,
        "fault": fault,
        "signed": statement.is_some(),
        "draining": drains,
        "abandoned": !drains,
        "package_digest": opt_col(sql, c.seq, "package_digest")?,
        "target_digest": opt_col(sql, c.seq, "target_digest")?,
    });
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    sql.exec(
        "UPDATE cohorts SET abandon_operator = 1, abandon_phase = ?, abandon_decision = ?, abandon_statement = ? WHERE seq = ?",
        &[
            s(c.phase.as_str()),
            req.decision_sha256.map_or(SqlValue::Null, |d| s(d.hex())),
            statement.map_or(SqlValue::Null, |d| s(d.hex())),
            i(c.seq),
        ],
    )?;
    if drains {
        sql.exec(
            "UPDATE cohorts SET draining_ms = ?, drain_reason = ? WHERE seq = ?",
            &[bind_u64(now_ms)?, s(req.reason.as_str()), i(c.seq)],
        )?;
        engine::log_note(sql, c.seq, &format!("draining:{} by operator decision; no new claims", req.reason), now_ms)?;
    } else {
        engine::abandon_now(sql, c.seq, &req.reason, fault.as_deref(), now_ms)?;
    }
    let seq = operator::record(sql, req, "eval.abandon", statement.as_ref(), &effect, o.epoch.map(|e| e.index), now_ms)?;
    let mut out = effect;
    out["operator_seq"] = json!(seq);
    Ok(Ok(out))
}

fn opt_col(sql: &impl Sql, seq: i64, col: &str) -> R<Option<String>> {
    let rows = sql.exec(&format!("SELECT {col} FROM cohorts WHERE seq = ?"), &[i(seq)])?;
    Ok(rows.first().and_then(|r| match r.first() {
        Some(SqlValue::Text(t)) => Some(t.clone()),
        _ => None,
    }))
}

/// Claims of cohort `seq` a lane still holds at `now_ms` (documents being
/// generated, chunks being scored).
pub fn outstanding_claims(sql: &impl Sql, seq: i64, now_ms: u64) -> R<i64> {
    let n = bind_u64(now_ms)?;
    let docs = int(&sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ? AND claim_until_ms > ?", &[i(seq), n.clone()])?[0], 0)?;
    let chunks = int(&sql.exec("SELECT COUNT(*) FROM chunks WHERE seq = ? AND claim_until_ms > ?", &[i(seq), n])?[0], 0)?;
    Ok(docs + chunks)
}

/// Completes every operator abandon whose drain has ended: no claim of the
/// cohort is held any longer. Late receipts and spend reported while it
/// drained were recorded against the cohort; a result arriving after this
/// is discarded by the claim check, and its spend is still charged. Returns
/// the labels abandoned.
pub fn finish_drains(sql: &impl Sql, now_ms: u64) -> R<Vec<String>> {
    let rows = sql.exec(
        "SELECT seq, label, drain_reason, fault FROM cohorts WHERE draining_ms IS NOT NULL AND phase NOT IN ('closed','abandoned')",
        &[],
    )?;
    let mut done = Vec::new();
    for r in rows {
        let seq = int(&r, 0)?;
        if outstanding_claims(sql, seq, now_ms)? > 0 {
            continue;
        }
        let reason = text(&r, 2).unwrap_or(OPERATOR_DECISION).to_owned();
        let fault = match r.get(3) {
            Some(SqlValue::Text(t)) => Some(t.clone()),
            _ => None,
        };
        engine::abandon_now(sql, seq, &reason, fault.as_deref(), now_ms)?;
        done.push(text(&r, 1)?.to_owned());
    }
    Ok(done)
}

/// The eval error as a refusal code, for routes that answer one.
pub fn refusal(e: &EvalError) -> &'static str {
    e.code()
}
