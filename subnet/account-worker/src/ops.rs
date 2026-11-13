//! Signed account ops (`POST /api/account/v1/op`).
//!
//! The body is the bare signed-op wire object. The Worker verifies it under
//! its own environment's operator key (`DETECT_OP_KEY`, `OP_ENV`), for the
//! `account` target only, inside the op's validity window, then applies the
//! op and its journal row in one batch: the journal's `nonce_h UNIQUE` makes
//! a reused nonce fail the whole batch. Every op answers `{ok, op, seq, …}`
//! or a refusal; every journal row is content-free (an address an op names
//! appears only as `HMAC(K_journal, email_norm)`, and `x_signin_purge`
//! keeps counts, never an X id, handle or address).
//!
//! | Op | Args |
//! |---|---|
//! | `disable_account`, `restore_account`, `revoke_sessions` | `user_id`, `reason` |
//! | `delete_account` | `user_id`, `basis` (`legal` or `abuse`: the operator's own recorded decision, never a bare claim of ownership), `mode`?, `reason` |
//! | `merge_accounts` | `from_user_id`, `to_user_id`, `reason` (moves money too: the operator's decision) |
//! | `undo_merge` | `seq` (the merge's journal row), `reason` |
//! | `set_flag` | `user_id`, `flag` (`paid_preview`), `value`, `reason` |
//! | `set_switch` | `name` (`auth.*`, `ACCOUNTS`, `BILLING`), `state`, `reason` |
//! | `reset_quota` | `kind`, `key` (an address, a user id or a key prefix), `reason` |
//! | `unsuppress` | `address`, `reason` (the address leaves our suppression list; Cloudflare's own is the operator's console step) |
//! | `x_signin_purge` | `dry_run`, `reason` |
//! | `attach_email` | refused at launch (`no_accepted_evidence`), journaled |
//!
//! `grant_credit`, `grant_plan` and `void_grant` belong to the ledger's
//! build and answer `not_configured` here without spending the nonce.

use serde_json::{Map, Value, json};
use slop_ninja_account_wire::{Bucket, BucketKind};
use slop_ninja_auth_core::email_addr;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::switches::{self, Owner};
use slop_ninja_detect_page::{IdH, PurgeMode, RefusalCode, UserId};
use slop_ninja_signed_op::{Target, Verified, Verifier};

use crate::config::Config;
use crate::delete::{self, Actor};
use crate::host::{DbError, Host, Stmt, Val, guard};
use crate::http::{Req, Resp};
use crate::journal::{self, Entry};
use crate::keys::NONCE_LEN;
use crate::sessions;

/// The longest op body read.
pub const OP_BODY_MAX: usize = 16 * 1024;
/// The longest `reason`.
pub const REASON_MAX: usize = 1000;
/// X identities one `x_signin_purge` call handles: each page fits one
/// invocation and one batch; `detect-op` repeats the op until its dry run
/// counts zero.
pub const X_PURGE_PAGE: usize = 25;
/// Ids per statement when an undo moves rows back (D1 binds at most 100).
const IDS_PER_STMT: usize = 90;

/// One verified op being applied.
struct Ctx<'a> {
    v: &'a Verified,
    reason: String,
    now: u64,
}

impl Ctx<'_> {
    fn args(&self) -> &Map<String, Value> {
        &self.v.op.args
    }

    fn entry(&self, op: String, user: Option<[u8; 32]>) -> Stmt {
        Entry {
            at: self.now,
            actor: &self.v.actor(),
            op,
            user_h: user,
            reason: Some(&self.reason),
            signature: Some(self.v.sig),
            nonce_h: Some(self.v.nonce_h()),
        }
        .stmt()
    }

    fn str(&self, k: &str) -> Result<&str, RefusalCode> {
        self.args().get(k).and_then(Value::as_str).ok_or(RefusalCode::OpMalformed)
    }

    fn user(&self, k: &str) -> Result<UserId, RefusalCode> {
        UserId::parse(self.str(k)?).ok_or(RefusalCode::OpMalformed)
    }

    fn bool(&self, k: &str) -> Result<bool, RefusalCode> {
        self.args().get(k).and_then(Value::as_bool).ok_or(RefusalCode::OpMalformed)
    }

    /// Only these keys (and `reason`) may appear.
    fn only(&self, keys: &[&str]) -> Result<(), RefusalCode> {
        if self.args().keys().all(|k| k == "reason" || keys.contains(&k.as_str())) {
            Ok(())
        } else {
            Err(RefusalCode::OpMalformed)
        }
    }
}

fn ok(op: &str, seq: u64, extra: Map<String, Value>) -> Resp {
    let mut o = journal::args([("ok", json!(true)), ("op", json!(op)), ("seq", json!(seq))]);
    o.extend(extra);
    Resp::json(200, &Value::Object(o))
}

fn seq_of(rows: &[Vec<crate::host::Row>], at: usize) -> u64 {
    rows.get(at).and_then(|r| r.first()).and_then(|r| r.u64("seq")).unwrap_or(0)
}

fn db_code(e: DbError) -> RefusalCode {
    match e {
        DbError::Guard => RefusalCode::NotFound,
        DbError::Constraint => RefusalCode::OpReusedNonce,
        DbError::Unavailable => RefusalCode::Internal,
    }
}

/// `POST /api/account/v1/op`.
pub async fn op<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    let Some(key) = cfg.op_key else { return Resp::refusal(RefusalCode::NotConfigured) };
    if req.body.len() > OP_BODY_MAX {
        return Resp::refusal(RefusalCode::TooLarge);
    }
    let now = host.now_ms();
    let v = match Verifier::new(key, cfg.op_env, &[Target::Account]).verify(&req.body, now) {
        Ok(v) => v,
        Err(r) => return Resp::refusal(RefusalCode::parse(r.code()).unwrap_or(RefusalCode::OpMalformed)),
    };
    let name = v.op.op.clone();
    if matches!(name.as_str(), "grant_credit" | "grant_plan" | "void_grant") {
        return Resp::refusal(RefusalCode::NotConfigured);
    }
    match host.first(crate::stmt!("SELECT seq FROM journal WHERE nonce_h = ?", v.nonce_h())).await {
        Ok(None) => {}
        Ok(Some(_)) => return Resp::refusal(RefusalCode::OpReusedNonce),
        Err(_) => return Resp::refusal(RefusalCode::Internal),
    }
    let reason = match v.op.args.get("reason").and_then(Value::as_str) {
        Some(r) if !r.trim().is_empty() && r.len() <= REASON_MAX => r.to_owned(),
        _ => return Resp::refusal(RefusalCode::OpMalformed),
    };
    let c = Ctx { v: &v, reason, now };
    let r = match name.as_str() {
        "disable_account" => disable(cfg, host, &c).await,
        "restore_account" => restore(cfg, host, &c).await,
        "revoke_sessions" => revoke(cfg, host, &c).await,
        "delete_account" => delete_op(cfg, host, &c).await,
        "merge_accounts" => merge_op(cfg, host, &c).await,
        "undo_merge" => undo_merge(cfg, host, &c).await,
        "set_flag" => set_flag(cfg, host, &c).await,
        "set_switch" => set_switch(host, &c).await,
        "reset_quota" => reset_quota(cfg, host, &c).await,
        "unsuppress" => unsuppress(cfg, host, &c).await,
        "x_signin_purge" => x_signin_purge(host, &c).await,
        "attach_email" => attach_email(host, &c).await,
        _ => Err(RefusalCode::OpUnknown),
    };
    r.unwrap_or_else(Resp::refusal)
}

async fn disable<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["user_id"])?;
    let u = c.user("user_id")?;
    let stmts = vec![
        guard("EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state = 'active')", vec![Val::from(u.0)]),
        crate::stmt!("UPDATE users SET state = 'locked' WHERE user_id = ?", u.0),
        sessions::revoke_all(&u),
        c.entry(
            journal::op_text("disable_account", journal::args([("user_id", json!(u.hex()))])),
            Some(cfg.keys.user_h(&u)),
        ),
    ];
    let rows = host.batch(stmts).await.map_err(db_code)?;
    Ok(ok("disable_account", seq_of(&rows, 3), Map::new()))
}

async fn restore<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["user_id"])?;
    let u = c.user("user_id")?;
    let stmts = vec![
        guard("EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state = 'locked')", vec![Val::from(u.0)]),
        crate::stmt!("UPDATE users SET state = 'active' WHERE user_id = ?", u.0),
        c.entry(
            journal::op_text("restore_account", journal::args([("user_id", json!(u.hex()))])),
            Some(cfg.keys.user_h(&u)),
        ),
    ];
    let rows = host.batch(stmts).await.map_err(db_code)?;
    Ok(ok("restore_account", seq_of(&rows, 2), Map::new()))
}

async fn revoke<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["user_id"])?;
    let u = c.user("user_id")?;
    let stmts = vec![
        crate::stmt!("DELETE FROM sessions WHERE user_id = ? RETURNING user_id", u.0),
        c.entry(
            journal::op_text("revoke_sessions", journal::args([("user_id", json!(u.hex()))])),
            Some(cfg.keys.user_h(&u)),
        ),
    ];
    let rows = host.batch(stmts).await.map_err(db_code)?;
    let n = rows.first().map_or(0, Vec::len);
    Ok(ok("revoke_sessions", seq_of(&rows, 1), journal::args([("sessions", json!(n))])))
}

async fn delete_op<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["user_id", "basis", "mode"])?;
    let u = c.user("user_id")?;
    let basis = c.str("basis")?;
    if !matches!(basis, "legal" | "abuse") {
        return Err(RefusalCode::OpMalformed);
    }
    let mode = match c.args().get("mode") {
        None => PurgeMode::Delete,
        Some(m) => m.as_str().and_then(PurgeMode::parse).ok_or(RefusalCode::OpMalformed)?,
    };
    let actor = c.v.actor();
    let by = Actor {
        actor: &actor,
        reason: Some(&c.reason),
        signature: Some(c.v.sig),
        nonce_h: Some(c.v.nonce_h()),
        extra: vec![("basis", json!(basis))],
    };
    let seq = delete::delete_account(cfg, host, &u, mode, by).await?;
    Ok(ok("delete_account", seq, Map::new()))
}

async fn merge_op<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["from_user_id", "to_user_id"])?;
    let (from, to) = (c.user("from_user_id")?, c.user("to_user_id")?);
    if from == to {
        return Err(RefusalCode::OpMalformed);
    }
    let alive = "EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state != 'deleted')";
    let mut stmts = vec![guard(alive, vec![Val::from(from.0)]), guard(alive, vec![Val::from(to.0)])];
    let plan = crate::link::merge_plan(cfg, host, &to, &from, c.now).await?;
    stmts.extend(plan.stmts);
    stmts.push(c.entry(journal::op_text("merge_accounts", plan.journal_args), Some(cfg.keys.user_h(&to))));
    let at = stmts.len() - 1;
    let rows = host.batch(stmts).await.map_err(db_code)?;
    let rekeyed = crate::link::rekey(host, &from, &to).await;
    Ok(ok("merge_accounts", seq_of(&rows, at), journal::args([("owner_rekey_sent", json!(rekeyed))])))
}

fn id_list(v: &Value, k: &str) -> Vec<i64> {
    v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default()
}

fn moves_back(table: &str, col: &str, ids: &[i64], from: &UserId, to: &UserId) -> Vec<Stmt> {
    ids.chunks(IDS_PER_STMT)
        .map(|chunk| {
            let marks = vec!["?"; chunk.len()].join(", ");
            let mut args = vec![Val::from(from.0), Val::from(to.0)];
            args.extend(chunk.iter().map(|&i| Val::from(i)));
            Stmt::new(format!("UPDATE {table} SET user_id = ? WHERE user_id = ? AND {col} IN ({marks})"), args)
        })
        .collect()
}

async fn undo_merge<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["seq"])?;
    let seq = c.args().get("seq").and_then(Value::as_u64).ok_or(RefusalCode::OpMalformed)?;
    let row = host
        .first(crate::stmt!("SELECT op FROM journal WHERE seq = ?", seq))
        .await
        .map_err(|_| RefusalCode::Internal)?
        .ok_or(RefusalCode::NotFound)?;
    let op: Value = serde_json::from_str(row.text("op").unwrap_or("")).map_err(|_| RefusalCode::NotFound)?;
    if !matches!(op.get("op").and_then(Value::as_str), Some("merge" | "merge_accounts")) {
        return Err(RefusalCode::NotFound);
    }
    let a = op.get("args").cloned().unwrap_or(Value::Null);
    let user = |k: &str| a.get(k).and_then(Value::as_str).and_then(UserId::parse).ok_or(RefusalCode::NotFound);
    let (from, to) = (user("from_user_id")?, user("to_user_id")?);
    let identities = id_list(&a, "identity_ids");
    let entries = id_list(&a, "entry_ids");
    let mut stmts = vec![
        guard(
            "EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state = 'deleted' AND merged_into = ?)",
            vec![Val::from(from.0), Val::from(to.0)],
        ),
        crate::stmt!(
            "UPDATE users SET state = 'active', merged_into = NULL, deleted_at = NULL WHERE user_id = ?",
            from.0
        ),
    ];
    stmts.extend(moves_back("identities", "identity_id", &identities, &from, &to));
    for chunk in identities.chunks(IDS_PER_STMT) {
        let marks = vec!["?"; chunk.len()].join(", ");
        stmts.push(Stmt::new(
            format!("DELETE FROM sessions WHERE identity_id IN ({marks})"),
            chunk.iter().map(|&i| Val::from(i)).collect(),
        ));
    }
    for e in &entries {
        let r = host
            .first(crate::stmt!("SELECT det_id_h, enc_ref FROM owned WHERE entry_id = ? AND user_id = ?", *e, to.0))
            .await
            .map_err(|_| RefusalCode::Internal)?;
        let Some(r) = r else { continue };
        let (Some(det), Some(sealed)) = (r.b32("det_id_h"), r.blob("enc_ref")) else { continue };
        let det = IdH(det);
        let Some(value) = cfg.keys.open_owned(&to, &det, sealed) else { continue };
        let resealed = cfg.keys.seal_owned(&from, &det, &value, host.random::<NONCE_LEN>());
        stmts.push(crate::stmt!(
            "UPDATE owned SET user_id = ?, enc_ref = ? WHERE entry_id = ? AND user_id = ?",
            from.0,
            resealed,
            *e,
            to.0
        ));
    }
    stmts.extend(moves_back("payments", "payment_id", &id_list(&a, "payment_ids"), &from, &to));
    stmts.extend(moves_back("plan_periods", "period_id", &id_list(&a, "period_ids"), &from, &to));
    stmts.push(
        c.entry(journal::op_text("undo_merge", journal::args([("seq", json!(seq))])), Some(cfg.keys.user_h(&from))),
    );
    let at = stmts.len() - 1;
    let rows = host.batch(stmts).await.map_err(db_code)?;
    // The detect Worker's owner hashes stay with the account the merge kept:
    // a re-key back would also move that account's own pages. Holds and
    // debits stay where the merge put them.
    Ok(ok(
        "undo_merge",
        seq_of(&rows, at),
        journal::args([("detect_owner_unchanged", json!(true)), ("holds_debits_unchanged", json!(true))]),
    ))
}

async fn set_flag<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["user_id", "flag", "value"])?;
    let u = c.user("user_id")?;
    let bit = match c.str("flag")? {
        "paid_preview" => slop_ninja_detect_page::sn::flags::PAID_PREVIEW,
        _ => return Err(RefusalCode::OpMalformed),
    };
    let on = c.bool("value")?;
    let set = if on {
        crate::stmt!("UPDATE users SET flags = flags | ? WHERE user_id = ?", bit, u.0)
    } else {
        crate::stmt!("UPDATE users SET flags = flags & ~? WHERE user_id = ?", bit, u.0)
    };
    let stmts = vec![
        guard("EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state != 'deleted')", vec![Val::from(u.0)]),
        set,
        c.entry(
            journal::op_text(
                "set_flag",
                journal::args([("user_id", json!(u.hex())), ("flag", json!("paid_preview")), ("value", json!(on))]),
            ),
            Some(cfg.keys.user_h(&u)),
        ),
    ];
    let rows = host.batch(stmts).await.map_err(db_code)?;
    Ok(ok("set_flag", seq_of(&rows, 2), Map::new()))
}

async fn set_switch<H: Host>(host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["name", "state"])?;
    let (name, state) = (c.str("name")?, c.str("state")?);
    let sw = switches::switch(name).filter(|s| s.owner == Owner::Account).ok_or(RefusalCode::OpMalformed)?;
    if !sw.states.contains(&state) {
        return Err(RefusalCode::OpMalformed);
    }
    let stmts = vec![
        c.entry(journal::op_text("set_switch", journal::args([("name", json!(name)), ("state", json!(state))])), None),
        crate::stmt!(
            "INSERT INTO switches (name, state, at, journal_seq) VALUES (?, ?, ?, (SELECT MAX(seq) FROM journal)) \
             ON CONFLICT (name) DO UPDATE SET state = excluded.state, at = excluded.at, journal_seq = excluded.journal_seq",
            name,
            state,
            c.now
        ),
    ];
    let rows = host.batch(stmts).await.map_err(db_code)?;
    Ok(ok("set_switch", seq_of(&rows, 0), Map::new()))
}

async fn reset_quota<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["kind", "key"])?;
    let kind = BucketKind::parse(c.str("kind")?).ok_or(RefusalCode::OpMalformed)?;
    let key = c.str("key")?;
    let (bucket_key, journaled) = match kind {
        BucketKind::Email | BucketKind::EmailLink => {
            let a = email_addr::parse(key).map_err(|_| RefusalCode::OpMalformed)?;
            (hex::encode(a.norm.keyed_hash(cfg.keys.k_addr())), cfg.keys.journal_addr(a.norm.as_str()))
        }
        BucketKind::LinkMail | BucketKind::Quote | BucketKind::Checkout => {
            let u = UserId::parse(key).ok_or(RefusalCode::OpMalformed)?;
            (u.hex(), u.hex())
        }
        BucketKind::Api => {
            if key.is_empty() || key.len() > 64 || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(RefusalCode::OpMalformed);
            }
            (key.to_owned(), key.to_owned())
        }
    };
    let rows = host
        .batch(vec![c.entry(
            journal::op_text("reset_quota", journal::args([("kind", json!(kind.as_str())), ("key", json!(journaled))])),
            None,
        )])
        .await
        .map_err(db_code)?;
    let bucket = Bucket { kind, key: bucket_key };
    host.quota_reset(&cfg.keys.quota_name(&bucket.name())).await.map_err(|_| RefusalCode::Unavailable)?;
    Ok(ok("reset_quota", seq_of(&rows, 0), Map::new()))
}

async fn unsuppress<H: Host>(cfg: &Config, host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["address"])?;
    let a = email_addr::parse(c.str("address")?).map_err(|_| RefusalCode::OpMalformed)?;
    let addr_h = a.norm.keyed_hash(cfg.keys.k_addr());
    let stmts = vec![
        crate::stmt!("DELETE FROM email_suppress WHERE addr_h = ? RETURNING addr_h", addr_h),
        c.entry(
            journal::op_text("unsuppress", journal::args([("address", json!(cfg.keys.journal_addr(a.norm.as_str())))])),
            None,
        ),
    ];
    let rows = host.batch(stmts).await.map_err(db_code)?;
    let removed = rows.first().is_some_and(|r| !r.is_empty());
    Ok(ok("unsuppress", seq_of(&rows, 1), journal::args([("removed", json!(removed))])))
}

async fn attach_email<H: Host>(host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    // Refused at launch: no evidence is accepted yet. The attempt is
    // journaled with nothing from its arguments.
    let rows = host
        .batch(vec![c.entry(
            journal::op_text_result(
                "attach_email",
                Map::new(),
                Some(journal::args([("refused", json!("no_accepted_evidence"))])),
            ),
            None,
        )])
        .await
        .map_err(db_code)?;
    let _ = seq_of(&rows, 0);
    Err(RefusalCode::NoAcceptedEvidence)
}

/// The counts of what X sign-in stored.
async fn x_counts<H: Host>(host: &H) -> Result<Map<String, Value>, RefusalCode> {
    let r = host
        .first(Stmt::new(
            "SELECT (SELECT COUNT(*) FROM identities WHERE provider = 'x') AS identities, \
             (SELECT COUNT(*) FROM avatars a JOIN identities i ON i.identity_id = a.identity_id WHERE i.provider = 'x') AS avatars, \
             (SELECT COUNT(*) FROM sessions s JOIN identities i ON i.identity_id = s.identity_id WHERE i.provider = 'x') AS sessions, \
             (SELECT COUNT(DISTINCT i.user_id) FROM identities i WHERE i.provider = 'x' AND NOT EXISTS \
               (SELECT 1 FROM identities j WHERE j.user_id = i.user_id AND j.provider != 'x')) AS x_only_accounts, \
             (SELECT COUNT(*) FROM pending_links WHERE provider = 'x') AS pending",
            vec![],
        ))
        .await
        .map_err(|_| RefusalCode::Internal)?
        .ok_or(RefusalCode::Internal)?;
    Ok(["identities", "avatars", "sessions", "x_only_accounts", "pending"]
        .into_iter()
        .map(|k| (k.to_owned(), json!(r.u64(k).unwrap_or(0))))
        .collect())
}

/// `x_signin_purge {dry_run}`: one page of X identities.
async fn x_signin_purge<H: Host>(host: &H, c: &Ctx<'_>) -> Result<Resp, RefusalCode> {
    c.only(&["dry_run"])?;
    let dry_run = c.bool("dry_run")?;
    let args = journal::args([("dry_run", json!(dry_run))]);
    if dry_run {
        let counts = x_counts(host).await?;
        let rows = host
            .batch(vec![c.entry(journal::op_text_result("x_signin_purge", args, Some(counts.clone())), None)])
            .await
            .map_err(db_code)?;
        let mut extra = counts;
        extra.insert("dry_run".into(), json!(true));
        return Ok(ok("x_signin_purge", seq_of(&rows, 0), extra));
    }
    let page = host
        .all(crate::stmt!(
            "SELECT i.identity_id, i.user_id, (SELECT COUNT(*) FROM identities j WHERE j.user_id = i.user_id \
             AND j.provider != 'x') AS others FROM identities i WHERE i.provider = 'x' ORDER BY i.identity_id LIMIT ?",
            X_PURGE_PAGE as u64
        ))
        .await
        .map_err(|_| RefusalCode::Internal)?;
    let mut stmts = Vec::new();
    let (mut identities, mut x_only) = (0u64, Vec::<UserId>::new());
    for r in &page {
        let (Some(id), Some(u)) = (r.int("identity_id"), r.b16("user_id").map(UserId)) else { continue };
        if r.int("others").unwrap_or(0) == 0 {
            if !x_only.contains(&u) {
                x_only.push(u);
            }
            continue;
        }
        identities += 1;
        stmts.push(crate::stmt!("DELETE FROM avatars WHERE identity_id = ?", id));
        stmts.push(crate::stmt!("DELETE FROM sessions WHERE identity_id = ?", id));
        stmts.push(crate::stmt!("DELETE FROM identities WHERE identity_id = ? AND provider = 'x'", id));
    }
    // An account left with no other method is deleted as a holder's own
    // deletion is, with its saved pages released as anonymous links (the
    // build default). The detect Worker's purge message goes first.
    for u in &x_only {
        let msg = QueueMessage::new(Job::OwnerPurge { user_id: *u, mode: PurgeMode::Release });
        if host.enqueue(&msg).await.is_err() {
            crate::metrics::error(host, "queue");
            return Err(RefusalCode::Unavailable);
        }
        let n = host
            .first(crate::stmt!("SELECT COUNT(*) AS n FROM identities WHERE user_id = ? AND provider = 'x'", u.0))
            .await
            .map_err(|_| RefusalCode::Internal)?
            .and_then(|r| r.u64("n"))
            .unwrap_or(0);
        identities += n;
        stmts.extend(delete::statements(u, c.now));
    }
    let before = x_counts(host).await?;
    stmts.push(crate::stmt!("DELETE FROM pending_links WHERE provider = 'x'"));
    stmts.push(crate::stmt!("DELETE FROM auth_flows WHERE provider = 'x'"));
    let mut result = journal::args([("identities", json!(identities)), ("accounts_deleted", json!(x_only.len()))]);
    let journal_at = stmts.len();
    stmts.push(c.entry(journal::op_text_result("x_signin_purge", args, Some(result.clone())), None));
    let rows = host.batch(stmts).await.map_err(db_code)?;
    let after = x_counts(host).await?;
    let gone = |k: &str| {
        before
            .get(k)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_sub(after.get(k).and_then(Value::as_u64).unwrap_or(0))
    };
    result.insert("avatars".into(), json!(gone("avatars")));
    result.insert("sessions".into(), json!(gone("sessions")));
    result.insert("remaining".into(), after.get("identities").cloned().unwrap_or(json!(0)));
    result.insert("dry_run".into(), json!(false));
    Ok(ok("x_signin_purge", seq_of(&rows, journal_at), result))
}
