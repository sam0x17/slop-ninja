//! The shared operator-decision mechanism: one request shape for every
//! operator override in the eval Worker and the coordinator, so there is one
//! thing to build, review and test.
//!
//! ```text
//! { "decision": "<id>",          // unique per store; same id + same body = replay
//!   "decision_sha256": "<hex>"?, // SHA-256 of the decision record
//!   "reason":   "<code>",        // a listed code of the action
//!   "expect":   { ... },         // compare-and-set on the state the operator read
//!   "preview":  true | false,    // true: every check, the effect, no write
//!   "authorization": { "statement": "...", "signature": "-----BEGIN SSH SIGNATURE-----..." }?,
//!   ...action fields }
//! ```
//!
//! - **Body digest.** `body_sha256` is the SHA-256 of the body's canonical
//!   JSON without `preview` and `authorization`: keys sorted, no whitespace
//!   (what `jq -cS 'del(.preview, .authorization)'` prints, without its
//!   newline). One signature therefore serves the preview and the request.
//! - **Signed actions** (those that decline a result, change reward inputs
//!   or restart the record) need an `authorization`: a
//!   [`slop_ninja_protocol::operator::Statement`] the offline
//!   operator-decision key signed (`ssh-keygen -Y sign`), verified against the
//!   Worker's configured `OPERATOR_DECISION_KEY` ([`authorize`]). The bearer
//!   token only carries the request. A statement over another body is
//!   `authorization_mismatch`; one addressed to another service, authority,
//!   store or chain is `authorization_misaddressed`.
//! - **Replays.** The same decision id with the same body returns the stored
//!   effect and changes nothing; with another body it is `decision_reused`
//!   ([`prior`]).
//! - **The action log** (`operator_actions`) is written in the same
//!   transaction as the effect ([`record`]): append-only, a per-store
//!   sequence with no gaps, never pruned, and outside every retention rule.
//!   A preview writes nothing.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::operator::{OperatorKey, Statement};
use slop_ninja_protocol::{ChainBinding, Digest};

use crate::sql::{Sql, SqlValue, StoreError, bind_u64, text};

/// The action log's table, created by each store's migration (the eval
/// store's v30, the chain journal's v15). Its schema never changes.
pub const ACTIONS_TABLE: &str = "CREATE TABLE operator_actions (
    seq INTEGER PRIMARY KEY,
    decision TEXT NOT NULL UNIQUE,
    decision_sha256 TEXT,
    body_sha256 TEXT NOT NULL,
    action TEXT NOT NULL,
    reason TEXT NOT NULL,
    expect TEXT NOT NULL,
    effect TEXT NOT NULL,
    signed INTEGER NOT NULL,
    statement_sha256 TEXT,
    epoch INTEGER,
    at_ms INTEGER NOT NULL
) STRICT";

/// Longest decision id.
pub const DECISION_ID_MAX: usize = 128;

/// The offline-signed part of a request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    pub statement: String,
    pub signature: String,
}

/// A parsed operator request: the shared fields, the whole body (for the
/// action's own fields) and its digest.
#[derive(Clone, Debug)]
pub struct Request {
    pub decision: String,
    pub decision_sha256: Option<Digest>,
    pub reason: String,
    pub expect: Value,
    pub preview: bool,
    pub authorization: Option<Authorization>,
    pub body: Value,
    pub body_sha256: Digest,
}

fn opt_t(r: &[SqlValue], i: usize) -> Option<String> {
    match r.get(i) {
        Some(SqlValue::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

fn code_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.:".contains(&b))
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let b = hex::decode(s).ok()?;
    b.try_into().ok()
}

/// Canonical JSON: object keys sorted, no whitespace.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                write_canonical(&m[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// The body digest a statement signs: SHA-256 of the canonical JSON of
/// `body` without `preview` and `authorization`.
pub fn body_digest(body: &Value) -> Digest {
    let mut b = body.clone();
    if let Value::Object(m) = &mut b {
        m.remove("preview");
        m.remove("authorization");
    }
    Digest(Sha256::digest(canonical_json(&b).as_bytes()).into())
}

/// Parses an operator request body. Refusals are content-free codes.
pub fn parse(body: Value) -> Result<Request, &'static str> {
    let Value::Object(m) = &body else { return Err("bad_request") };
    let decision = m.get("decision").and_then(Value::as_str).ok_or("decision_missing")?.to_owned();
    if !code_ok(&decision, DECISION_ID_MAX) {
        return Err("decision_invalid");
    }
    let decision_sha256 = match m.get("decision_sha256") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(Digest(parse_hex32(s).ok_or("decision_sha256_invalid")?)),
        Some(_) => return Err("decision_sha256_invalid"),
    };
    let reason = m.get("reason").and_then(Value::as_str).ok_or("reason_missing")?.to_owned();
    if !code_ok(&reason, slop_ninja_protocol::submission::ABANDONMENT_CODE_MAX) {
        return Err("reason_invalid");
    }
    let expect = m.get("expect").cloned().unwrap_or(Value::Null);
    let preview = match m.get("preview") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("bad_request"),
    };
    let authorization = match m.get("authorization") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value::<Authorization>(v.clone()).map_err(|_| "authorization_invalid")?),
    };
    let body_sha256 = body_digest(&body);
    Ok(Request { decision, decision_sha256, reason, expect, preview, authorization, body, body_sha256 })
}

/// Where a request is addressed: what a statement must name.
#[derive(Clone, Debug)]
pub struct Address<'a> {
    pub service: &'a str,
    pub authority: &'a str,
    pub store: &'a str,
    pub chain: &'a ChainBinding,
    /// The subnet registration block, when this service knows it.
    pub registration: Option<u64>,
}

/// Checks the request's offline authorization for `action`. `signed`: the
/// action needs one. Returns the verified statement's digest (`None` for a
/// token-level action sent without one). An authorization sent with a
/// token-level action is still checked, so a bad one is never ignored.
pub fn authorize(
    req: &Request,
    action: &str,
    signed: bool,
    key: Option<&OperatorKey>,
    to: &Address<'_>,
) -> Result<Option<Digest>, &'static str> {
    let Some(a) = &req.authorization else {
        return if signed { Err("authorization_required") } else { Ok(None) };
    };
    let key = key.ok_or("operator_key_unconfigured")?;
    key.verify(a.statement.as_bytes(), &a.signature).map_err(|_| "authorization_invalid")?;
    let s = Statement::parse(&a.statement).map_err(|_| "authorization_invalid")?;
    if s.action != action
        || s.decision != req.decision
        || s.decision_sha256 != req.decision_sha256
        || s.body_sha256 != req.body_sha256
    {
        return Err("authorization_mismatch");
    }
    let registration_ok = match (s.registration, to.registration) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    if s.service != to.service
        || s.authority != to.authority
        || s.store != to.store
        || s.genesis != to.chain.genesis
        || s.netuid != to.chain.netuid
        || s.mechanism != to.chain.mechanism
        || !registration_ok
    {
        return Err("authorization_misaddressed");
    }
    Ok(Some(s.digest().map_err(|_| "authorization_invalid")?))
}

/// Reads the configured operator key (`OPERATOR_DECISION_KEY`): `None` when
/// unset, `Err` when set but not a key.
pub fn configured_key(value: Option<&str>) -> Result<Option<OperatorKey>, &'static str> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => OperatorKey::parse(v).map(Some).map_err(|_| "operator_key_invalid"),
    }
}

/// What `OPERATOR_DECISION_KEY` holds, for the status routes and the
/// deploy probe: `configured`, `unconfigured` (unset: every signed decision
/// refuses `operator_key_unconfigured`, except the coordinator restart,
/// which is token-level until a key is configured) or `invalid`.
pub fn key_state(value: Option<&str>) -> &'static str {
    match configured_key(value) {
        Ok(Some(_)) => "configured",
        Ok(None) => "unconfigured",
        Err(_) => "invalid",
    }
}

/// What the action log knows of a decision id.
#[derive(Clone, Debug, PartialEq)]
pub enum Prior {
    /// Not seen.
    New,
    /// The same id and body: the stored effect, returned as is.
    Replay(Value),
    /// The same id with another body.
    Reused,
}

/// The log's record of `req`'s decision id, for the replay rule.
pub fn prior(sql: &impl Sql, req: &Request) -> Result<Prior, StoreError> {
    let rows = sql.exec("SELECT body_sha256, effect FROM operator_actions WHERE decision = ?", &[req.decision.as_str().into()])?;
    let Some(r) = rows.first() else { return Ok(Prior::New) };
    if text(r, 0)? != req.body_sha256.hex() {
        return Ok(Prior::Reused);
    }
    Ok(Prior::Replay(serde_json::from_str(text(r, 1)?).map_err(|_| StoreError("operator action effect"))?))
}

/// Appends the action to the log (same transaction as its effect); its
/// sequence number. Never called for a preview.
pub fn record(
    sql: &impl Sql,
    req: &Request,
    action: &str,
    statement: Option<&Digest>,
    effect: &Value,
    epoch: Option<u64>,
    now_ms: u64,
) -> Result<u64, StoreError> {
    let next = sql
        .exec("SELECT COALESCE(MAX(seq), 0) + 1 FROM operator_actions", &[])?
        .first()
        .and_then(|r| r.first().cloned())
        .and_then(|v| if let SqlValue::Integer(n) = v { Some(n as u64) } else { None })
        .ok_or(StoreError("operator action seq"))?;
    sql.exec(
        "INSERT INTO operator_actions (seq, decision, decision_sha256, body_sha256, action, reason, expect, effect,
            signed, statement_sha256, epoch, at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            bind_u64(next)?,
            req.decision.as_str().into(),
            req.decision_sha256.map_or(SqlValue::Null, |d| d.hex().into()),
            req.body_sha256.hex().into(),
            action.into(),
            req.reason.as_str().into(),
            canonical_json(&req.expect).into(),
            canonical_json(effect).into(),
            SqlValue::Integer(i64::from(statement.is_some())),
            statement.map_or(SqlValue::Null, |d| d.hex().into()),
            epoch.map_or(Ok(SqlValue::Null), bind_u64)?,
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(next)
}

/// One logged action (`/operator/actions`).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ActionRow {
    pub seq: u64,
    pub decision: String,
    pub decision_sha256: Option<String>,
    pub body_sha256: String,
    pub action: String,
    pub reason: String,
    pub expect: Value,
    pub effect: Value,
    pub signed: bool,
    pub statement_sha256: Option<String>,
    pub epoch: Option<u64>,
    pub at_ms: u64,
}

/// The logged actions from sequence `from`, oldest first, at most `limit`.
pub fn list(sql: &impl Sql, from: u64, limit: u64) -> Result<Vec<ActionRow>, StoreError> {
    let rows = sql.exec(
        "SELECT seq, decision, decision_sha256, body_sha256, action, reason, expect, effect, signed, statement_sha256,
            epoch, at_ms FROM operator_actions WHERE seq >= ? ORDER BY seq LIMIT ?",
        &[bind_u64(from)?, bind_u64(limit.min(500))?],
    )?;
    let u = |r: &[SqlValue], i: usize| -> Result<u64, StoreError> {
        match r.get(i) {
            Some(SqlValue::Integer(n)) if *n >= 0 => Ok(*n as u64),
            _ => Err(StoreError("operator action integer")),
        }
    };
    let opt_u = |r: &[SqlValue], i: usize| match r.get(i) {
        Some(SqlValue::Integer(n)) if *n >= 0 => Some(*n as u64),
        _ => None,
    };
    let json = |s: &str| serde_json::from_str(s).map_err(|_| StoreError("operator action json"));
    rows.iter()
        .map(|r| {
            Ok(ActionRow {
                seq: u(r, 0)?,
                decision: text(r, 1)?.to_owned(),
                decision_sha256: opt_t(r, 2),
                body_sha256: text(r, 3)?.to_owned(),
                action: text(r, 4)?.to_owned(),
                reason: text(r, 5)?.to_owned(),
                expect: json(text(r, 6)?)?,
                effect: json(text(r, 7)?)?,
                signed: u(r, 8)? != 0,
                statement_sha256: opt_t(r, 9),
                epoch: opt_u(r, 10),
                at_ms: u(r, 11)?,
            })
        })
        .collect()
}

/// The newest sequence number and the count of logged actions (`/status`).
pub fn newest(sql: &impl Sql) -> Result<(Option<u64>, u64), StoreError> {
    let rows = sql.exec("SELECT MAX(seq), COUNT(*) FROM operator_actions", &[])?;
    let r = rows.first().ok_or(StoreError("operator actions"))?;
    let max = match r.first() {
        Some(SqlValue::Integer(n)) => Some(*n as u64),
        _ => None,
    };
    let count = match r.get(1) {
        Some(SqlValue::Integer(n)) => *n as u64,
        _ => 0,
    };
    Ok((max, count))
}

/// The outcome of the shared steps before an action's own checks.
pub enum Begin {
    /// Go on with the action's checks and effect.
    Proceed(Option<Digest>),
    /// Answer with this (a replay's stored effect).
    Answer(Value),
}

/// The shared steps every action runs first: the replay rule, then the
/// authorization. A replay is answered before the signature is checked
/// again, so a recorded action stays answerable after a key rotation.
pub fn begin(
    sql: &impl Sql,
    req: &Request,
    action: &str,
    signed: bool,
    key: Option<&OperatorKey>,
    to: &Address<'_>,
) -> Result<Result<Begin, &'static str>, StoreError> {
    match prior(sql, req)? {
        Prior::Replay(v) if !req.preview => return Ok(Ok(Begin::Answer(v))),
        Prior::Replay(_) => {}
        Prior::Reused => return Ok(Err("decision_reused")),
        Prior::New => {}
    }
    Ok(authorize(req, action, signed, key, to).map(Begin::Proceed))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{Sqlite, tx};
    use slop_ninja_protocol::operator::sign_sshsig;
    use slop_ninja_protocol::SigningKey;

    fn chain() -> ChainBinding {
        ChainBinding { genesis: slop_ninja_protocol::TESTNET_GENESIS, netuid: 601, mechanism: 0 }
    }

    fn db() -> Sqlite {
        let db = Sqlite::open();
        db.exec(ACTIONS_TABLE, &[]).unwrap();
        db
    }

    pub(crate) fn signed_body(key: &SigningKey, action: &str, to: &Address<'_>, mut body: Value) -> Value {
        let decision = body["decision"].as_str().unwrap().to_owned();
        let st = Statement {
            action: action.into(),
            decision,
            decision_sha256: None,
            body_sha256: body_digest(&body),
            service: to.service.into(),
            authority: to.authority.into(),
            store: to.store.into(),
            genesis: to.chain.genesis,
            netuid: to.chain.netuid,
            mechanism: to.chain.mechanism,
            registration: None,
        };
        let text = st.to_text().unwrap();
        body["authorization"] = serde_json::json!({"statement": text, "signature": sign_sshsig(key, text.as_bytes())});
        body
    }

    #[test]
    fn body_digest_ignores_preview_and_authorization_and_key_order() {
        let a = serde_json::json!({"decision": "d1", "reason": "operator_decision", "label": "x", "expect": {"phase": "queued"}});
        let b = serde_json::json!({"expect": {"phase": "queued"}, "label": "x", "preview": true, "reason": "operator_decision",
            "authorization": {"statement": "s", "signature": "t"}, "decision": "d1"});
        assert_eq!(body_digest(&a), body_digest(&b));
        assert_eq!(
            canonical_json(&a),
            r#"{"decision":"d1","expect":{"phase":"queued"},"label":"x","reason":"operator_decision"}"#
        );
    }

    #[test]
    fn signatures_bind_body_and_address() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let public = OperatorKey::from_bytes(key.verifying_key().to_bytes()).unwrap();
        let c = chain();
        let to = Address { service: "eval", authority: "r1", store: "main", chain: &c, registration: None };
        let body = serde_json::json!({"decision": "d1", "reason": "operator_decision", "label": "c5"});
        let signed = signed_body(&key, "eval.abandon", &to, body.clone());
        let req = parse(signed.clone()).unwrap();
        assert!(authorize(&req, "eval.abandon", true, Some(&public), &to).unwrap().is_some());
        // A preview carries the same signature.
        let mut preview = signed.clone();
        preview["preview"] = Value::Bool(true);
        assert!(authorize(&parse(preview).unwrap(), "eval.abandon", true, Some(&public), &to).is_ok());
        // Only the token: refused.
        assert_eq!(authorize(&parse(body.clone()).unwrap(), "eval.abandon", true, Some(&public), &to).err(), Some("authorization_required"));
        // The statement over another body.
        let mut other = signed.clone();
        other["label"] = "c6".into();
        assert_eq!(authorize(&parse(other).unwrap(), "eval.abandon", true, Some(&public), &to).err(), Some("authorization_mismatch"));
        // Another action.
        assert_eq!(authorize(&req, "eval.closure_decline", true, Some(&public), &to).err(), Some("authorization_mismatch"));
        // Addressed to another store, authority or chain.
        let to2 = Address { authority: "r2", ..to.clone() };
        assert_eq!(authorize(&req, "eval.abandon", true, Some(&public), &to2).err(), Some("authorization_misaddressed"));
        let c3 = ChainBinding { netuid: 602, ..c.clone() };
        let to3 = Address { chain: &c3, ..to.clone() };
        assert_eq!(authorize(&req, "eval.abandon", true, Some(&public), &to3).err(), Some("authorization_misaddressed"));
        // Signed by another key (for example the authorization key's seed).
        let other_key = SigningKey::from_bytes(&[6; 32]);
        let forged = signed_body(&other_key, "eval.abandon", &to, body.clone());
        assert_eq!(authorize(&parse(forged).unwrap(), "eval.abandon", true, Some(&public), &to).err(), Some("authorization_invalid"));
        // No key configured: a signed action cannot pass.
        assert_eq!(authorize(&req, "eval.abandon", true, None, &to).err(), Some("operator_key_unconfigured"));
        // A token-level action without a signature passes.
        assert_eq!(authorize(&parse(body).unwrap(), "eval.release_objects", false, None, &to), Ok(None));
    }

    #[test]
    fn replays_and_reuse() {
        let db = db();
        let req = parse(serde_json::json!({"decision": "d1", "reason": "operator_decision", "label": "c5"})).unwrap();
        let c = chain();
        let to = Address { service: "eval", authority: "r1", store: "main", chain: &c, registration: None };
        assert!(matches!(tx(&db, |s| begin(s, &req, "eval.abandon", false, None, &to)).unwrap(), Ok(Begin::Proceed(None))));
        let seq = tx(&db, |s| record(s, &req, "eval.abandon", None, &serde_json::json!({"abandoned": true}), Some(9), 1)).unwrap();
        assert_eq!(seq, 1);
        match tx(&db, |s| begin(s, &req, "eval.abandon", false, None, &to)).unwrap() {
            Ok(Begin::Answer(v)) => assert_eq!(v, serde_json::json!({"abandoned": true})),
            _ => panic!("replay"),
        }
        let other = parse(serde_json::json!({"decision": "d1", "reason": "operator_decision", "label": "c6"})).unwrap();
        assert_eq!(tx(&db, |s| begin(s, &other, "eval.abandon", false, None, &to)).unwrap().err(), Some("decision_reused"));
        let second = parse(serde_json::json!({"decision": "d2", "reason": "operator_decision", "label": "c6"})).unwrap();
        assert_eq!(tx(&db, |s| record(s, &second, "eval.abandon", None, &Value::Null, None, 2)).unwrap(), 2);
        let rows = tx(&db, |s| list(s, 0, 10)).unwrap();
        assert_eq!(rows.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(tx(&db, newest).unwrap(), (Some(2), 2));
    }

    #[test]
    fn requests_refuse_bad_shapes() {
        assert_eq!(parse(serde_json::json!({"reason": "x"})).err(), Some("decision_missing"));
        assert_eq!(parse(serde_json::json!({"decision": "D 1", "reason": "x"})).err(), Some("decision_invalid"));
        assert_eq!(parse(serde_json::json!({"decision": "d", "reason": "Free text"})).err(), Some("reason_invalid"));
        assert_eq!(
            parse(serde_json::json!({"decision": "d", "reason": "x", "authorization": {"statement": "s"}})).err(),
            Some("authorization_invalid")
        );
        assert_eq!(configured_key(Some("  ")), Ok(None));
        assert!(configured_key(Some("ssh-rsa AAAA")).is_err());
    }
}
