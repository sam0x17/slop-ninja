//! Connecting, merging and removing sign-in methods.
//!
//! **Connect** (`POST /account/link/confirm`). Between a successful link
//! flow and this POST the resolved identity lives only in this browser's
//! pending row. The POST names nothing but its action: the row is found by
//! the flow cookie **and** the session (`binding_h`, `user_id`,
//! `session_h`), and only `link` and `merge` rows are read. Connect and
//! merge need a fresh step-up; every statement of the attach runs in one
//! batch behind guards that re-check the pending row, that both accounts
//! are `active` and that the identity is still unknown (connect) or still
//! the other account's (merge). The session is then rotated.
//!
//! **Merge** moves the other account's identities, saved links (re-sealed
//! under this account's key), credit, plan periods, holds and debits,
//! revokes its sessions and leaves it `deleted` with `merged_into`. It is
//! refused while both accounts hold credit or plans (the operator decides
//! those with `merge_accounts`). The detect Worker re-keys its owner hashes
//! from a queue message the account Worker sends once the merge committed.
//! The journal row lists the moved ids, so `undo_merge` can move them back.
//!
//! **Unlink** needs a fresh step-up, refuses the last method, deletes an X
//! identity's avatar with it, and revokes the sessions that signed in with
//! it.

use serde_json::{Value, json};
use slop_ninja_account_wire::{LinkAction, LinkConfirmForm, PendingKind, Provider, UnlinkForm};
use slop_ninja_auth_core::cookie;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::{IdH, RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Row, Stmt, Val, guard};
use crate::http::{self, Req, Resp};
use crate::journal::{self, Entry};
use crate::keys::NONCE_LEN;
use crate::sessions::{self, Session};
use crate::store::{self, Switches, USER_ACTIVE};

const CONFIRM_PAGE: &str = "/account/link/confirm";
const STEP_UP_FOR_CONFIRM: &str = "/signin/?step_up=1&return_to=%2Faccount%2Flink%2Fconfirm";
const STEP_UP_FOR_ACCOUNT: &str = "/signin/?step_up=1&return_to=%2Faccount%2F";

fn page_error(cfg: &Config, page: &str, code: RefusalCode) -> Resp {
    Resp::form_error(&cfg.origins.site, page, code)
}

/// This browser's pending row for session `s`, if any.
pub async fn pending_row<H: Host>(host: &H, req: &Req, s: &Session) -> Result<Option<([u8; 32], Row)>, DbError> {
    let Some(binding) = cookie::token(req.cookie(), cookie::FLOW) else { return Ok(None) };
    let binding_h = binding.hash();
    let row = host
        .first(crate::stmt!(
            "SELECT kind, provider, subject, email, email_verified, handle, from_user_id, return_to FROM pending_links \
             WHERE binding_h = ? AND user_id = ? AND session_h = ? AND expires_at > ? AND kind IN ('link','merge')",
            binding_h,
            s.user_id.0,
            s.token_h,
            host.now_ms()
        ))
        .await?;
    Ok(row.map(|r| (binding_h, r)))
}

/// The guard that the pending row is still this browser's and session's.
fn pending_guard(binding_h: &[u8; 32], s: &Session, now: u64) -> Stmt {
    guard(
        "EXISTS (SELECT 1 FROM pending_links WHERE binding_h = ? AND user_id = ? AND session_h = ? AND expires_at > ? \
         AND kind IN ('link','merge'))",
        vec![Val::from(*binding_h), Val::from(s.user_id.0), Val::from(s.token_h), Val::from(now)],
    )
}

/// `POST /account/link/confirm`.
pub async fn confirm<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    let action = match http::form(&req.body, LinkConfirmForm::FIELDS)
        .ok()
        .and_then(|f| f.get("action").and_then(|a| LinkAction::parse(a)))
    {
        Some(a) => a,
        None => return page_error(cfg, CONFIRM_PAGE, RefusalCode::MalformedRequest),
    };
    let Ok(Some(s)) = sessions::current(host, req).await else {
        return page_error(cfg, "/signin/", RefusalCode::Session);
    };
    let Ok(Some((binding_h, p))) = pending_row(host, req, &s).await else {
        return page_error(cfg, CONFIRM_PAGE, RefusalCode::NothingPending);
    };
    let now = host.now_ms();
    if action == LinkAction::Cancel {
        let _ = host
            .all(crate::stmt!(
                "DELETE FROM pending_links WHERE binding_h = ? AND user_id = ? AND session_h = ? AND kind IN ('link','merge')",
                binding_h,
                s.user_id.0,
                s.token_h
            ))
            .await;
        return Resp::redirect(303, &cfg.origins.site, "/account/").with_cookie(cookie::clear(cookie::FLOW));
    }
    if !s.stepped_up(cfg, now) {
        return Resp::redirect(303, &cfg.origins.site, STEP_UP_FOR_CONFIRM);
    }
    if sw.read_only() {
        return page_error(cfg, CONFIRM_PAGE, RefusalCode::AccountsReadOnly);
    }
    let kind = p.text("kind").and_then(PendingKind::parse);
    let result = match (action, kind) {
        (LinkAction::Connect, Some(PendingKind::Link)) => connect(cfg, host, &s, &binding_h, &p).await,
        (LinkAction::Merge, Some(PendingKind::Merge)) => merge_pending(cfg, host, &s, &binding_h, &p).await,
        _ => Err(RefusalCode::NothingPending),
    };
    match result {
        Ok(set_cookie) => {
            let rt = slop_ninja_auth_core::return_to::ReturnTo::from_stored(
                p.text("return_to").unwrap_or(""),
                &cfg.origins.site,
                &cfg.policy,
                true,
            );
            Resp::redirect(303, &cfg.origins.site, rt.as_str()).with_cookies([set_cookie, cookie::clear(cookie::FLOW)])
        }
        Err(code) => page_error(cfg, CONFIRM_PAGE, code),
    }
}

/// Attaches a pending `link` row's identity; answers the rotated cookie.
async fn connect<H: Host>(
    cfg: &Config,
    host: &H,
    s: &Session,
    binding_h: &[u8; 32],
    p: &Row,
) -> Result<String, RefusalCode> {
    let now = host.now_ms();
    let provider = p.text("provider").and_then(Provider::parse).ok_or(RefusalCode::Internal)?;
    let rotated = sessions::rotate(cfg, host, s);
    let stmts = vec![
        pending_guard(binding_h, s, now),
        guard(USER_ACTIVE, vec![Val::from(s.user_id.0)]),
        crate::stmt!(
            "INSERT INTO identities (user_id, provider, subject, email, email_verified, handle, consent_at, created_at, \
             last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            s.user_id.0,
            provider.as_str(),
            p.text("subject").map(str::to_owned),
            p.text("email").map(str::to_owned),
            p.int("email_verified").unwrap_or(0) != 0,
            p.text("handle").map(str::to_owned),
            now,
            now,
            now
        ),
        crate::stmt!("DELETE FROM pending_links WHERE binding_h = ?", *binding_h),
        rotated.stmt,
    ];
    match host.batch(stmts).await {
        Ok(_) => {
            crate::metrics::session(host, "rotated");
            Ok(rotated.set_cookie)
        }
        Err(DbError::Guard | DbError::Constraint) => Err(RefusalCode::LinkUnavailable),
        Err(_) => Err(RefusalCode::Internal),
    }
}

/// Merges the account owning a pending `merge` row's identity into `s`'s.
async fn merge_pending<H: Host>(
    cfg: &Config,
    host: &H,
    s: &Session,
    binding_h: &[u8; 32],
    p: &Row,
) -> Result<String, RefusalCode> {
    let now = host.now_ms();
    let from = UserId(p.b16("from_user_id").ok_or(RefusalCode::Internal)?);
    let provider = p.text("provider").and_then(Provider::parse).ok_or(RefusalCode::Internal)?;
    let subject = p.text("subject").ok_or(RefusalCode::Internal)?.to_owned();
    if store::both_hold_money(host, &s.user_id, &from).await.map_err(|_| RefusalCode::Internal)? {
        return Err(RefusalCode::MergeNeedsOperator);
    }
    let rotated = sessions::rotate(cfg, host, s);
    let mut pre = vec![
        pending_guard(binding_h, s, now),
        guard(USER_ACTIVE, vec![Val::from(s.user_id.0)]),
        // The other account is active (only the operator merges a locked
        // one) and still owns the identity this browser proved.
        guard(USER_ACTIVE, vec![Val::from(from.0)]),
        guard(
            "EXISTS (SELECT 1 FROM identities WHERE provider = ? AND subject = ? AND user_id = ?)",
            vec![Val::from(provider.as_str()), Val::from(subject), Val::from(from.0)],
        ),
    ];
    let plan = merge_plan(cfg, host, &s.user_id, &from, now).await?;
    pre.extend(plan.stmts);
    pre.push(crate::stmt!("DELETE FROM pending_links WHERE binding_h = ?", *binding_h));
    pre.push(rotated.stmt);
    pre.push(
        Entry {
            at: now,
            actor: journal::ACTOR_USER,
            op: journal::op_text("merge", plan.journal_args),
            user_h: Some(cfg.keys.user_h(&s.user_id)),
            reason: None,
            signature: None,
            nonce_h: None,
        }
        .stmt(),
    );
    match host.batch(pre).await {
        Ok(_) => {}
        Err(DbError::Guard | DbError::Constraint) => return Err(RefusalCode::LinkUnavailable),
        Err(_) => return Err(RefusalCode::Internal),
    }
    rekey(host, &from, &s.user_id).await;
    crate::metrics::session(host, "rotated");
    Ok(rotated.set_cookie)
}

/// Sends `owner_rekey {from, to}` after a merge committed (retried once).
pub async fn rekey<H: Host>(host: &H, from: &UserId, to: &UserId) -> bool {
    let msg = QueueMessage::new(Job::OwnerRekey { from_user_id: *from, to_user_id: *to });
    for _ in 0..2 {
        if host.enqueue(&msg).await.is_ok() {
            return true;
        }
    }
    crate::metrics::error(host, "queue");
    false
}

/// The statements that merge `from` into `to`, and the moved ids.
pub struct MergePlan {
    pub stmts: Vec<Stmt>,
    pub journal_args: serde_json::Map<String, Value>,
}

fn ids(rows: &[Row], col: &str) -> Vec<i64> {
    rows.iter().filter_map(|r| r.int(col)).collect()
}

/// Reads what `from` holds and builds the merge's statements. Every move is
/// keyed by `user_id = from`, so a row that changed hands meanwhile is not
/// touched.
pub async fn merge_plan<H: Host>(
    cfg: &Config,
    host: &H,
    to: &UserId,
    from: &UserId,
    now: u64,
) -> Result<MergePlan, RefusalCode> {
    let db = |_| RefusalCode::Internal;
    let identities =
        host.all(crate::stmt!("SELECT identity_id FROM identities WHERE user_id = ?", from.0)).await.map_err(db)?;
    let theirs = host
        .all(crate::stmt!("SELECT entry_id, det_id_h, enc_ref, is_owner FROM owned WHERE user_id = ?", from.0))
        .await
        .map_err(db)?;
    let ours =
        host.all(crate::stmt!("SELECT entry_id, det_id_h FROM owned WHERE user_id = ?", to.0)).await.map_err(db)?;
    let payments =
        host.all(crate::stmt!("SELECT payment_id FROM payments WHERE user_id = ?", from.0)).await.map_err(db)?;
    let periods =
        host.all(crate::stmt!("SELECT period_id FROM plan_periods WHERE user_id = ?", from.0)).await.map_err(db)?;
    let mut stmts = vec![crate::stmt!("UPDATE identities SET user_id = ? WHERE user_id = ?", to.0, from.0)];
    let mut moved_entries = Vec::new();
    for r in &theirs {
        let (Some(entry_id), Some(det), Some(sealed)) = (r.int("entry_id"), r.b32("det_id_h"), r.blob("enc_ref"))
        else {
            continue;
        };
        let det = IdH(det);
        let is_owner = r.int("is_owner").unwrap_or(0);
        if let Some(mine) = ours.iter().find(|o| o.b32("det_id_h") == Some(det.0)).and_then(|o| o.int("entry_id")) {
            stmts.push(crate::stmt!("UPDATE owned SET is_owner = MAX(is_owner, ?) WHERE entry_id = ?", is_owner, mine));
            stmts.push(crate::stmt!("DELETE FROM owned WHERE entry_id = ? AND user_id = ?", entry_id, from.0));
            continue;
        }
        let Some(value) = cfg.keys.open_owned(from, &det, sealed) else { continue };
        let resealed = cfg.keys.seal_owned(to, &det, &value, host.random::<NONCE_LEN>());
        stmts.push(crate::stmt!(
            "UPDATE owned SET user_id = ?, enc_ref = ? WHERE entry_id = ? AND user_id = ?",
            to.0,
            resealed,
            entry_id,
            from.0
        ));
        moved_entries.push(entry_id);
    }
    for table in ["payments", "plan_periods", "holds", "debits"] {
        stmts.push(Stmt::new(
            format!("UPDATE {table} SET user_id = ? WHERE user_id = ?"),
            vec![Val::from(to.0), Val::from(from.0)],
        ));
    }
    stmts.extend([
        sessions::revoke_all(from),
        crate::stmt!("DELETE FROM pending_links WHERE user_id = ? OR from_user_id = ?", from.0, from.0),
        crate::stmt!("DELETE FROM auth_flows WHERE user_id = ?", from.0),
        crate::stmt!("DELETE FROM email_tokens WHERE user_id = ?", from.0),
        crate::stmt!("DELETE FROM wallet_chals WHERE user_id = ?", from.0),
        crate::stmt!(
            "UPDATE users SET state = 'deleted', merged_into = ?, deleted_at = ?, flags = 0 WHERE user_id = ?",
            to.0,
            now,
            from.0
        ),
    ]);
    let journal_args = journal::args([
        ("from_user_id", json!(from.hex())),
        ("to_user_id", json!(to.hex())),
        ("identity_ids", json!(ids(&identities, "identity_id"))),
        ("entry_ids", json!(moved_entries)),
        ("payment_ids", json!(ids(&payments, "payment_id"))),
        ("period_ids", json!(ids(&periods, "period_id"))),
    ]);
    Ok(MergePlan { stmts, journal_args })
}

/// `POST /account/unlink`.
pub async fn unlink<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    let id: Option<i64> =
        http::form(&req.body, UnlinkForm::FIELDS).ok().and_then(|f| f.get("identity_id").and_then(|v| v.parse().ok()));
    let Some(id) = id else { return page_error(cfg, "/account/", RefusalCode::MalformedRequest) };
    let Ok(Some(s)) = sessions::current(host, req).await else {
        return page_error(cfg, "/signin/", RefusalCode::Session);
    };
    let now = host.now_ms();
    if !s.stepped_up(cfg, now) {
        return Resp::redirect(303, &cfg.origins.site, STEP_UP_FOR_ACCOUNT);
    }
    let owns = host
        .first(crate::stmt!("SELECT provider FROM identities WHERE identity_id = ? AND user_id = ?", id, s.user_id.0))
        .await;
    let Ok(Some(_)) = owns else { return page_error(cfg, "/account/", RefusalCode::NotFound) };
    let signs_out = s.identity_id == id;
    let mut stmts = vec![
        guard("(SELECT COUNT(*) FROM identities WHERE user_id = ?) >= 2", vec![Val::from(s.user_id.0)]),
        crate::stmt!("DELETE FROM avatars WHERE identity_id = ?", id),
        crate::stmt!("DELETE FROM sessions WHERE identity_id = ? AND user_id = ?", id, s.user_id.0),
        crate::stmt!("DELETE FROM identities WHERE identity_id = ? AND user_id = ?", id, s.user_id.0),
        Entry {
            at: now,
            actor: journal::ACTOR_USER,
            op: journal::op_text("unlink", journal::args([("identity_id", json!(id))])),
            user_h: Some(cfg.keys.user_h(&s.user_id)),
            reason: None,
            signature: None,
            nonce_h: None,
        }
        .stmt(),
    ];
    let rotated = (!signs_out).then(|| sessions::rotate(cfg, host, &s));
    if let Some(r) = &rotated {
        stmts.push(r.stmt.clone());
    }
    match host.batch(stmts).await {
        Ok(_) => {}
        Err(DbError::Guard) => return page_error(cfg, "/account/", RefusalCode::LastMethod),
        Err(_) => return page_error(cfg, "/account/", RefusalCode::Internal),
    }
    match rotated {
        Some(r) => Resp::redirect(303, &cfg.origins.site, "/account/").with_cookie(r.set_cookie),
        None => Resp::redirect(303, &cfg.origins.site, "/").with_cookie(sessions::clear_cookie()),
    }
}
