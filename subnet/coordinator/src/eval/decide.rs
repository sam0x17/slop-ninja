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
//! - [`closure_decline`] (D7(d)(ii)): the operator declines a faulted
//!   closure under the offline key: the cohort never gets a closure record,
//!   no longer holds `closure_due` or succession, and a signed
//!   `ClosureDeclined` record lets the coordinator pass its number (D2).
//! - [`release_objects`] (D7(b)): the operator records every undecided
//!   entrant of a cohort as `withheld`, so its upload objects are released,
//!   with a signed `eval-decision` record (`operator_withheld`).
//!
//! - [`cohort_start`] (D8): queues exactly one cohort from the installed
//!   template now, whether the calendar is paused or not; the calendar stays
//!   as it was. An install keeps the calendar's pause.
//!
//! - [`reference_decision`] (D10(b)): the operator sets the chain's
//!   references for later cohorts under the offline key, with a recorded
//!   reason: the general `override`, and the narrow `span_equivalent`
//!   replacement of R_0 by a staged package whose document outputs equal
//!   R_0's (span gate design section 4.2).
//!
//! Records are queued in `decision_records` and written by their own step
//! ([`engine::Work::DecisionRecord`]); the write never holds other work.

use serde_json::{Value, json};

use crate::eval::engine::{self, EvalConfig, EvalError, R, i, s};
use crate::operator::{self, Address, Begin, Request};
use crate::sql::{Sql, SqlValue, bind_u64, int, text};
use slop_ninja_protocol::operator::OperatorKey;

/// v30: the operator action log, the drain of an operator abandon, the
/// abandonment's decision fields, the operator's reference decisions and the
/// decision records awaiting their write.
pub const SCHEMA_V30: &[&str] = &[
    crate::operator::ACTIONS_TABLE,
    "ALTER TABLE cohorts ADD COLUMN draining_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN drain_reason TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_operator INTEGER",
    "ALTER TABLE cohorts ADD COLUMN abandon_phase TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_decision TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_statement TEXT",
    "ALTER TABLE cohorts ADD COLUMN closure_declined_ms INTEGER",
    // D10(b): the operator's reference decisions, newest last; the chain's
    // resolution reads their records before any cohort's. This table
    // replaces the one of the same name that v29 created for the span gate's
    // interim decision route (`span_gate`): that route refused every request
    // before reading or writing anything, so the dropped table is empty in
    // every store.
    "DROP TABLE reference_decisions",
    "CREATE TABLE reference_decisions (
        seq INTEGER PRIMARY KEY,
        kind TEXT NOT NULL CHECK (kind IN ('span_equivalent', 'override')),
        k INTEGER NOT NULL,
        current_content TEXT NOT NULL,
        current_record TEXT NOT NULL,
        previous_content TEXT,
        previous_record TEXT,
        reason TEXT NOT NULL,
        decision_sha256 TEXT,
        statement_sha256 TEXT NOT NULL,
        at_ms INTEGER NOT NULL
    ) STRICT",
    // Signed, content-free decision records awaiting their write to the
    // receipts bucket (`Work::DecisionRecord`): the record's canonical bytes
    // (signed at the write with the package key) and its key.
    "CREATE TABLE decision_records (
        id INTEGER PRIMARY KEY,
        key TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL CHECK (kind IN ('closure-declined', 'eval-decision')),
        record_hex TEXT NOT NULL,
        written_ms INTEGER,
        retry_ms INTEGER NOT NULL DEFAULT 0,
        error TEXT,
        at_ms INTEGER NOT NULL
    ) STRICT",
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


/// Queues a signed decision record (written by `Work::DecisionRecord`).
pub fn queue_record(sql: &impl Sql, key: &str, kind: &str, record_hex: String, now_ms: u64) -> R<()> {
    sql.exec(
        "INSERT INTO decision_records (key, kind, record_hex, at_ms) VALUES (?, ?, ?, ?) ON CONFLICT(key) DO NOTHING",
        &[s(key), s(kind), s(record_hex), bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// The next decision record to write, once its retry time has passed.
pub fn record_due(sql: &impl Sql, now_ms: u64) -> R<Option<engine::Work>> {
    let rows = sql.exec(
        "SELECT id FROM decision_records WHERE written_ms IS NULL AND retry_ms <= ? ORDER BY id LIMIT 1",
        &[bind_u64(now_ms)?],
    )?;
    Ok(rows.first().map(|r| int(r, 0)).transpose()?.map(|id| engine::Work::DecisionRecord { id }))
}

/// Whether a decision record is still unwritten (whatever its retry time).
pub fn record_pending(sql: &impl Sql) -> R<bool> {
    Ok(!sql.exec("SELECT 1 FROM decision_records WHERE written_ms IS NULL LIMIT 1", &[])?.is_empty())
}

/// A queued record's key, kind and canonical bytes.
pub fn record_of(sql: &impl Sql, id: i64) -> R<(String, String, Vec<u8>)> {
    let rows = sql.exec("SELECT key, kind, record_hex FROM decision_records WHERE id = ?", &[i(id)])?;
    let r = rows.first().ok_or(EvalError::State("decision record"))?;
    let bytes = hex::decode(text(r, 2)?).map_err(|_| EvalError::State("decision record hex"))?;
    Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned(), bytes))
}

/// The record's write: done, or failed (retried after `STEP_RETRY_MS`).
pub fn record_written(sql: &impl Sql, id: i64, error: Option<&str>, now_ms: u64) -> R<()> {
    match error {
        None => sql.exec("UPDATE decision_records SET written_ms = ?, error = NULL WHERE id = ?", &[bind_u64(now_ms)?, i(id)])?,
        Some(e) => sql.exec(
            "UPDATE decision_records SET retry_ms = ?, error = ? WHERE id = ? AND written_ms IS NULL",
            &[bind_u64(now_ms.saturating_add(engine::STEP_RETRY_MS))?, s(e), i(id)],
        )?,
    };
    Ok(())
}

fn label_of(req: &Request) -> String {
    req.body.get("label").and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// D7(d)(ii): declines cohort `label`'s faulted closure (request fields
/// `label`, `expect: {phase: "closed", closure: "faulted"}`), under the
/// offline operator-decision key. Accepted only for a closed cohort with a
/// package, no closure record and a fault. The outcome is never installed:
/// the eval writes no closure for it again, and the signed `ClosureDeclined`
/// record lets the coordinator pass its number. A decline stops the
/// payout; it is not an undo (the references `close` wrote stay).
pub fn closure_decline(sql: &impl Sql, cfg: &EvalConfig, req: &Request, key: Option<&OperatorKey>, now_ms: u64) -> R<Answer> {
    let label = label_of(req);
    let authority = cfg.authority.name().to_owned();
    let statement = match operator::begin(sql, req, "eval.closure_decline", true, key, &address(cfg, &authority))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(st)) => st,
        Err(code) => return Ok(Err(code)),
    };
    let rows = sql.exec(
        "SELECT seq, phase, closure_digest, package_digest, fault, closure_declined_ms, cohort, manifest_digest,
            target_digest, closed_epoch FROM cohorts WHERE label = ?",
        &[s(&label)],
    )?;
    let Some(r) = rows.first() else { return Ok(Err("cohort_unknown")) };
    let col = |idx: usize| match r.get(idx) {
        Some(SqlValue::Text(t)) => Some(t.clone()),
        _ => None,
    };
    let seq = int(r, 0)?;
    let faulted = text(r, 1)? == "closed" && col(2).is_none() && col(3).is_some() && col(4).is_some();
    if r.get(5).is_some_and(|v| matches!(v, SqlValue::Integer(_))) {
        return Ok(Err("closure_already_declined"));
    }
    if !faulted {
        return Ok(Err("closure_not_faulted"));
    }
    if expect_str(req, "phase") != Some("closed") || expect_str(req, "closure") != Some("faulted") {
        return Ok(Err("expect_mismatch"));
    }
    let fault = col(4).unwrap_or_default();
    let digest = |d: Option<String>| d.and_then(|d| engine::parse_digest(&d));
    let o = engine::observed(sql)?;
    let record = slop_ninja_protocol::decisions::ClosureDeclined {
        chain: cfg.chain.clone(),
        cohort: int(r, 6)? as u64,
        cohort_label: label.clone(),
        cohort_manifest: digest(col(7)),
        score_package: digest(col(3)),
        declined_target: digest(col(8)),
        phase: "closed".into(),
        fault: engine::published_code(&fault).to_owned(),
        declined_epoch: o.epoch.map(|e| e.index),
        decision: req.decision_sha256,
        statement: statement.ok_or(EvalError::State("statement"))?,
    };
    let bytes = slop_ninja_protocol::Record::to_bytes(&record).map_err(|_| EvalError::State("closure declined record"))?;
    let effect = json!({"label": label, "cohort": record.cohort, "fault": fault,
        "declined_target": record.declined_target.map(|d| d.hex()), "score_package": record.score_package.map(|d| d.hex())});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    sql.exec("UPDATE cohorts SET closure_declined_ms = ? WHERE seq = ?", &[bind_u64(now_ms)?, i(seq)])?;
    engine::log_note(sql, seq, &format!("closure_declined:{} by operator decision; never installed", req.reason), now_ms)?;
    let key = format!("{}/{}", engine::receipt_prefix(&label), slop_ninja_admission_wire::receipts::CLOSURE_DECLINED_RECORD);
    queue_record(sql, &key, "closure-declined", hex::encode(bytes), now_ms)?;
    let n = operator::record(sql, req, "eval.closure_decline", statement.as_ref(), &effect, o.epoch.map(|e| e.index), now_ms)?;
    let mut out = effect;
    out["operator_seq"] = json!(n);
    Ok(Ok(out))
}

/// D7(b): records every undecided admitted entrant of cohort `label` as
/// `withheld` (request fields `label`, `expect: {phase}`), so its upload
/// objects are released; a token-level action that writes a signed
/// `eval-decision` record (`operator_withheld`), since it stops a winner's
/// artifacts from being published.
pub fn release_objects(sql: &impl Sql, cfg: &EvalConfig, req: &Request, key: Option<&OperatorKey>, now_ms: u64) -> R<Answer> {
    let label = label_of(req);
    let authority = cfg.authority.name().to_owned();
    let statement = match operator::begin(sql, req, "eval.release_objects", false, key, &address(cfg, &authority))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(st)) => st,
        Err(code) => return Ok(Err(code)),
    };
    let Some(c) = engine::cohorts(sql)?.into_iter().find(|c| c.label == label) else {
        return Ok(Err("cohort_unknown"));
    };
    if expect_str(req, "phase") != Some(c.phase.as_str()) {
        return Ok(Err("expect_mismatch"));
    }
    let rows = sql.exec(
        "SELECT commitment FROM adm_sessions WHERE seq = ? AND state = 'admitted' AND published = 0 ORDER BY commitment",
        &[i(c.seq)],
    )?;
    let commitments: Vec<String> = rows.iter().map(|r| text(r, 0).map(str::to_owned)).collect::<Result<_, _>>()?;
    let effect = json!({"label": label, "cohort": c.plan.cohort, "withheld": commitments.len()});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    let entries: Vec<crate::eval::admission::PublicationEntry> = commitments
        .iter()
        .map(|c| crate::eval::admission::PublicationEntry { commitment: c.clone(), state: "withheld".into() })
        .collect();
    crate::eval::admission::publication_report(sql, c.plan.cohort, &label, &entries, now_ms)?;
    let o = engine::observed(sql)?;
    let record = slop_ninja_protocol::decisions::EvalDecision {
        chain: cfg.chain.clone(),
        authority: authority.clone(),
        action: "release_objects".into(),
        reason: "operator_withheld".into(),
        cohort: Some(c.plan.cohort),
        cohort_label: Some(label.clone()),
        digests: Vec::new(),
        epoch: o.epoch.map(|e| e.index),
        decision: req.decision_sha256,
        statement,
    };
    let bytes = slop_ninja_protocol::Record::to_bytes(&record).map_err(|_| EvalError::State("decision record"))?;
    let n = operator::record(sql, req, "eval.release_objects", statement.as_ref(), &effect, o.epoch.map(|e| e.index), now_ms)?;
    let key = format!("{}/decisions/{n}.signed", engine::receipt_prefix(&label));
    queue_record(sql, &key, "eval-decision", hex::encode(bytes), now_ms)?;
    let mut out = effect;
    out["operator_seq"] = json!(n);
    Ok(Ok(out))
}

/// D8: queues one cohort from the installed template (request fields
/// `expect: {next_cohort}`), whether the calendar is paused or not; it opens
/// at the next eligible tempo boundary like any queued cohort, and the
/// calendar's pause is unchanged. Every plan admission check applies. A
/// token-level action: an inline template (which changes reward inputs) is
/// not taken here; install it paused first, then start.
pub fn cohort_start(sql: &impl Sql, cfg: &EvalConfig, req: &Request, key: Option<&OperatorKey>, now_ms: u64) -> R<Answer> {
    let authority = cfg.authority.name().to_owned();
    match operator::begin(sql, req, "eval.cohort_start", false, key, &address(cfg, &authority))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(_)) => {}
        Err(code) => return Ok(Err(code)),
    }
    if req.body.get("template").is_some_and(|t| !t.is_null()) {
        return Ok(Err("inline_template_unsupported"));
    }
    let (next, label, bytes) = match crate::eval::calendar::start_now(sql, cfg)? {
        Ok(v) => v,
        Err(code) => return Ok(Err(code)),
    };
    if req.expect.get("next_cohort").and_then(Value::as_u64) != Some(next) {
        return Ok(Err("expect_mismatch"));
    }
    let paused = crate::eval::calendar::paused(sql)?;
    let effect = json!({"cohort": next, "label": label, "calendar_paused": paused});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    let seq = engine::queue(sql, cfg, &bytes, now_ms)?;
    engine::log_note(sql, seq, "queued by operator decision (cohort/start); the calendar is unchanged", now_ms)?;
    let o = engine::observed(sql)?;
    let n = operator::record(sql, req, "eval.cohort_start", None, &effect, o.epoch.map(|e| e.index), now_ms)?;
    let mut out = effect;
    out["operator_seq"] = json!(n);
    Ok(Ok(out))
}

/// The stored artifact record an operator reference decision names for
/// `content` (current or previous), newest decision first.
pub fn decided_record(sql: &impl Sql, content: &str) -> R<Option<crate::eval::plan::ArtifactJson>> {
    let rows = sql.exec(
        "SELECT current_content, current_record, previous_content, previous_record FROM reference_decisions
         WHERE current_content = ? OR previous_content = ? ORDER BY seq DESC LIMIT 1",
        &[s(content), s(content)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let record = if text(r, 0)? == content { text(r, 1)?.to_owned() } else { engine::opt_text(r, 3).unwrap_or_default() };
    serde_json::from_str(&record).map(Some).map_err(|_| EvalError::State("decided reference record"))
}

fn hex64(v: Option<&Value>) -> Option<String> {
    let t = v?.as_str()?;
    (t.len() == 64 && t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then(|| t.to_owned())
}

/// One probe vector pair a span-equivalent decision compares: the probe
/// version, the candidate's stored vector key, R_0's, and the probe size.
pub type VectorPair = (u32, String, String, u32);

/// The stored probe vectors a `span_equivalent` decision compares, one pair
/// per probe version of the installed template (`candidate_unpublished`
/// when the candidate has no stored vector on one). Read-only.
pub fn reference_vectors(sql: &impl Sql, req: &Request) -> R<Result<Vec<VectorPair>, &'static str>> {
    let Some(candidate) = hex64(req.body.get("current_content")) else { return Ok(Err("current_content_invalid")) };
    let chain = sql.exec("SELECT current FROM reference_state WHERE id = 1", &[])?;
    let Some(r0) = chain.first().map(|r| text(r, 0).map(str::to_owned)).transpose()? else {
        return Ok(Err("chain_missing"));
    };
    let mut out = Vec::new();
    for v in crate::eval::calendar::template_probe_versions(sql)? {
        let key_of = |content: &str| -> R<Option<String>> {
            Ok(sql
                .exec("SELECT vector_key FROM fingerprints WHERE content = ? AND version = ?", &[s(content), i(i64::from(v))])?
                .first()
                .map(|r| text(r, 0).map(str::to_owned))
                .transpose()?)
        };
        let size = sql.exec("SELECT size FROM probe_versions WHERE version = ?", &[i(i64::from(v))])?;
        let size = size.first().map_or(Ok(0), |r| int(r, 0))? as u32;
        match (key_of(&candidate)?, key_of(&r0)?) {
            (Some(c), Some(r)) => out.push((v, c, r, size)),
            (None, _) => return Ok(Err("candidate_unpublished")),
            (_, None) => return Ok(Err("reference_unfingerprinted")),
        }
    }
    Ok(Ok(out))
}

const IDENTITY_FIELDS: [&str; 4] = ["fit_sha256", "selected_checkpoint_sha256", "encoder_sha256", "heads_sha256"];

/// D10(b): an operator reference decision (`POST /eval/references/decision`,
/// under the offline operator-decision key). Body: `kind` (`span_equivalent`
/// or `override`), `current` (the artifact record the chain names next, a
/// staged reference package) with `current_content`, optional `previous` and
/// `previous_content`, `k`, and `expect: {current_content}` (the chain's
/// current content as the operator read it).
///
/// Both kinds need no promotion pending (`promotion_pending`) and a candidate
/// whose content is published here with a fingerprint and stored vector on
/// every probe version of the installed template (`candidate_unpublished`).
/// `span_equivalent` takes R_0's position with no predecessor (`k` 0) and
/// only while the chain is at index 0 (`chain_not_at_r0`); its `static_identity`
/// lists the candidate's and R_0's recorded fit digests, which must be equal
/// (`static_identity_mismatch`); and its stored probe vectors (`vectors`,
/// read by the caller from [`reference_vectors`]) must equal R_0's on every
/// probe version within the decision's `tolerance_ppb`
/// (`not_document_equivalent`). The general `override` (a stronger class)
/// takes the references as given.
///
/// Effect: journaled before it applies; `reference_state` is set for
/// cohorts queued after it (a queued cohort keeps its sealed references);
/// the chain's resolution reads the decision's records first; and a signed,
/// content-free `eval-decision` record states it.
pub fn reference_decision(
    sql: &impl Sql,
    cfg: &EvalConfig,
    req: &Request,
    key: Option<&OperatorKey>,
    vectors: &[(u32, Vec<u8>, Vec<u8>)],
    now_ms: u64,
) -> R<Answer> {
    let authority = cfg.authority.name().to_owned();
    let statement = match operator::begin(sql, req, "eval.reference_decision", true, key, &address(cfg, &authority))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(st)) => st.ok_or(EvalError::State("statement"))?,
        Err(code) => return Ok(Err(code)),
    };
    let kind = req.body.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned();
    if !matches!(kind.as_str(), "span_equivalent" | "override") {
        return Ok(Err("kind_unknown"));
    }
    let Some(current_content) = hex64(req.body.get("current_content")) else { return Ok(Err("current_content_invalid")) };
    let previous_content = match req.body.get("previous_content") {
        None | Some(Value::Null) => None,
        v => Some(hex64(v).ok_or(EvalError::Plan("previous_content"))?),
    };
    let Ok(current) = serde_json::from_value::<crate::eval::plan::ArtifactJson>(req.body.get("current").cloned().unwrap_or_default()) else {
        return Ok(Err("current_record_invalid"));
    };
    let previous = match req.body.get("previous") {
        None | Some(Value::Null) => None,
        Some(v) => match serde_json::from_value::<crate::eval::plan::ArtifactJson>(v.clone()) {
            Ok(a) => Some(a),
            Err(_) => return Ok(Err("previous_record_invalid")),
        },
    };
    if previous.is_some() != previous_content.is_some() {
        return Ok(Err("previous_record_invalid"));
    }
    let k = req.body.get("k").and_then(Value::as_u64).unwrap_or(0);
    let chain = sql.exec("SELECT k, current FROM reference_state WHERE id = 1", &[])?;
    let (chain_k, chain_current) = match chain.first() {
        Some(r) => (int(r, 0)? as u64, Some(text(r, 1)?.to_owned())),
        None => (0, None),
    };
    if req.expect.get("current_content").and_then(Value::as_str) != chain_current.as_deref() {
        return Ok(Err("expect_mismatch"));
    }
    if crate::eval::calendar::promotion_pending(sql)? {
        return Ok(Err("promotion_pending"));
    }
    let published = !sql.exec("SELECT 1 FROM published_contents WHERE content = ?", &[s(&current_content)])?.is_empty();
    if !published {
        return Ok(Err("candidate_unpublished"));
    }
    for v in crate::eval::calendar::template_probe_versions(sql)? {
        let fp = sql.exec("SELECT 1 FROM fingerprints WHERE content = ? AND version = ?", &[s(&current_content), i(i64::from(v))])?;
        if fp.is_empty() {
            return Ok(Err("candidate_unpublished"));
        }
    }
    let mut max_diff_ppb: Option<u64> = None;
    if kind == "span_equivalent" {
        if chain_k != 0 || k != 0 || previous.is_some() {
            return Ok(Err("chain_not_at_r0"));
        }
        let ident = req.body.get("static_identity");
        let side = |name: &str| ident.and_then(|v| v.get(name));
        let same = IDENTITY_FIELDS.iter().all(|f| {
            let a = side("candidate").and_then(|v| hex64(v.get(*f)));
            let b = side("r0").and_then(|v| hex64(v.get(*f)));
            a.is_some() && a == b
        });
        if !same {
            return Ok(Err("static_identity_mismatch"));
        }
        let tolerance = req.body.get("tolerance_ppb").and_then(Value::as_u64).unwrap_or(0);
        let versions = crate::eval::calendar::template_probe_versions(sql)?;
        if vectors.len() != versions.len() || !versions.iter().all(|v| vectors.iter().any(|(x, _, _)| x == v)) {
            return Ok(Err("candidate_unpublished"));
        }
        let mut worst = 0u64;
        for (_, a, b) in vectors {
            let (Ok(a), Ok(b)) = (crate::eval::v3::vector_from_bytes(a), crate::eval::v3::vector_from_bytes(b)) else {
                return Ok(Err("not_document_equivalent"));
            };
            if a.len() != b.len() {
                return Ok(Err("not_document_equivalent"));
            }
            for (x, y) in a.iter().zip(&b) {
                match (x, y) {
                    (None, None) => {}
                    (Some(x), Some(y)) => worst = worst.max(u64::from(x.ppb().abs_diff(y.ppb()))),
                    _ => return Ok(Err("not_document_equivalent")),
                }
            }
        }
        if worst > tolerance {
            return Ok(Err("not_document_equivalent"));
        }
        max_diff_ppb = Some(worst);
    }
    let effect = json!({"kind": kind, "k": k, "current_content": current_content, "previous_content": previous_content,
        "previous_chain_content": chain_current, "max_vector_diff_ppb": max_diff_ppb});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    let o = engine::observed(sql)?;
    let record_json = |a: &crate::eval::plan::ArtifactJson| serde_json::to_string(a).map_err(|_| EvalError::State("reference record"));
    sql.exec(
        "INSERT INTO reference_decisions (kind, k, current_content, current_record, previous_content, previous_record, reason,
            decision_sha256, statement_sha256, at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            s(kind.as_str()),
            bind_u64(k)?,
            s(&current_content),
            s(record_json(&current)?),
            previous_content.as_deref().map_or(SqlValue::Null, s),
            previous.as_ref().map(record_json).transpose()?.map_or(SqlValue::Null, s),
            s(req.reason.as_str()),
            req.decision_sha256.map_or(SqlValue::Null, |d| s(d.hex())),
            s(statement.hex()),
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec(
        "INSERT INTO reference_state (id, k, current, predecessor) VALUES (1, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET k = excluded.k, current = excluded.current, predecessor = excluded.predecessor",
        &[bind_u64(k)?, s(&current_content), previous_content.as_deref().map_or(SqlValue::Null, s)],
    )?;
    let digest_of = |h: &str| engine::parse_digest(h).ok_or(EvalError::State("content digest"));
    let mut digests = vec![slop_ninja_protocol::decisions::NamedDigest { name: "reference_current".into(), digest: digest_of(&current_content)? }];
    if let Some(p) = &previous_content {
        digests.push(slop_ninja_protocol::decisions::NamedDigest { name: "reference_previous".into(), digest: digest_of(p)? });
    }
    if let Some(c) = &chain_current {
        digests.push(slop_ninja_protocol::decisions::NamedDigest { name: "replaced_current".into(), digest: digest_of(c)? });
    }
    let record = slop_ninja_protocol::decisions::EvalDecision {
        chain: cfg.chain.clone(),
        authority,
        action: format!("references.{kind}"),
        reason: req.reason.clone(),
        cohort: None,
        cohort_label: None,
        digests,
        epoch: o.epoch.map(|e| e.index),
        decision: req.decision_sha256,
        statement: Some(statement),
    };
    let bytes = slop_ninja_protocol::Record::to_bytes(&record).map_err(|_| EvalError::State("decision record"))?;
    let n = operator::record(sql, req, "eval.reference_decision", Some(&statement), &effect, o.epoch.map(|e| e.index), now_ms)?;
    queue_record(sql, &format!("references/decisions/{n}.signed"), "eval-decision", hex::encode(bytes), now_ms)?;
    let mut out = effect;
    out["operator_seq"] = json!(n);
    Ok(Ok(out))
}
