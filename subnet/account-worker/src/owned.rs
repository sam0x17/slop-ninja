//! "My detections": the internal saved-link routes.
//!
//! `owned/add` keeps a secret link for an account, sealed under that
//! account's key with the `id_h`, summary and ownership the detect Worker
//! produced or answered. It never keeps a public page's slug: a public page
//! is reachable by anyone and needs no saved link, and an X post's page must
//! leave nothing outside the rows its deletion reaches. `get`, `remove` and
//! `prune` touch only rows of the `user_id` they are given.

use slop_ninja_account_wire::{
    OwnedAddAnswer, OwnedAddRequest, OwnedEntry, OwnedEntryRequest, OwnedPruneRequest, OwnedRemoved,
};
use slop_ninja_detect_page::internal::MAX_STATES_PER_CALL;
use slop_ninja_detect_page::slug::{SECRET_LEN, is_canonical};
use slop_ninja_detect_page::{IdH, RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Row, Stmt, Val, guard};
use crate::http::{self, Req, Resp};
use crate::keys::{NONCE_LEN, OwnedRef};
use crate::store::{Switches, USER_ACTIVE};

/// The most entries one prune removes (D1 binds at most 100 values).
pub const MAX_PRUNE: usize = MAX_STATES_PER_CALL;

/// `POST /internal/v1/owned/add`.
pub async fn add<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    let body: OwnedAddRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    // Only a secret page's slug: never the public form.
    if !is_canonical(&body.slug, SECRET_LEN) {
        return Resp::refusal(RefusalCode::MalformedRequest);
    }
    if sw.read_only() {
        return Resp::refusal(RefusalCode::AccountsReadOnly);
    }
    let now = host.now_ms();
    let sealed = cfg.keys.seal_owned(
        &body.user_id,
        &body.id_h,
        &OwnedRef { slug: body.slug.clone(), summary: body.summary.clone() },
        host.random::<NONCE_LEN>(),
    );
    let stmts = vec![
        guard(USER_ACTIVE, vec![Val::from(body.user_id.0)]),
        crate::stmt!(
            "INSERT INTO owned (user_id, det_id_h, enc_ref, is_owner, created_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (user_id, det_id_h) DO NOTHING RETURNING entry_id",
            body.user_id.0,
            body.id_h.0,
            sealed,
            body.is_owner,
            now
        ),
        crate::stmt!(
            "UPDATE owned SET is_owner = MAX(is_owner, ?) WHERE user_id = ? AND det_id_h = ? RETURNING entry_id",
            body.is_owner,
            body.user_id.0,
            body.id_h.0
        ),
    ];
    match host.batch(stmts).await {
        Ok(rows) => {
            let added = rows.get(1).is_some_and(|r| !r.is_empty());
            let entry_id = rows.get(2).and_then(|r| r.first()).and_then(|r| r.u64("entry_id"));
            match entry_id {
                Some(entry_id) => Resp::json(200, &OwnedAddAnswer { entry_id, added }),
                None => Resp::refusal(RefusalCode::Internal),
            }
        }
        Err(DbError::Guard) => Resp::refusal(RefusalCode::AccountUnavailable),
        Err(_) => Resp::refusal(RefusalCode::SaveUnavailable),
    }
}

/// One stored entry, opened for its holder.
pub fn entry(cfg: &Config, uid: &UserId, r: &Row) -> Option<OwnedEntry> {
    let det = IdH(r.b32("det_id_h")?);
    let value = cfg.keys.open_owned(uid, &det, r.blob("enc_ref")?)?;
    Some(OwnedEntry {
        entry_id: r.u64("entry_id")?,
        slug: value.slug,
        id_h: det,
        is_owner: r.int("is_owner")? != 0,
        summary: value.summary,
    })
}

/// Every entry of `user`, newest first.
pub async fn entries<H: Host>(cfg: &Config, host: &H, user: &UserId) -> Result<Vec<OwnedEntry>, DbError> {
    let rows = host
        .all(crate::stmt!(
            "SELECT entry_id, det_id_h, enc_ref, is_owner FROM owned WHERE user_id = ? ORDER BY created_at DESC, entry_id DESC",
            user.0
        ))
        .await?;
    Ok(rows.iter().filter_map(|r| entry(cfg, user, r)).collect())
}

/// `POST /internal/v1/owned/get`.
pub async fn get<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    let body: OwnedEntryRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    let row = host
        .first(crate::stmt!(
            "SELECT entry_id, det_id_h, enc_ref, is_owner FROM owned WHERE entry_id = ? AND user_id = ?",
            body.entry_id,
            body.user_id.0
        ))
        .await;
    match row {
        Ok(Some(r)) => match entry(cfg, &body.user_id, &r) {
            Some(e) => Resp::json(200, &e),
            None => Resp::refusal(RefusalCode::NotFound),
        },
        Ok(None) => Resp::refusal(RefusalCode::NotFound),
        Err(_) => Resp::refusal(RefusalCode::Internal),
    }
}

/// `POST /internal/v1/owned/remove`.
pub async fn remove<H: Host>(host: &H, req: &Req) -> Resp {
    let body: OwnedEntryRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    let rows = host
        .all(crate::stmt!(
            "DELETE FROM owned WHERE entry_id = ? AND user_id = ? RETURNING entry_id",
            body.entry_id,
            body.user_id.0
        ))
        .await;
    match rows {
        Ok(r) => Resp::json(200, &OwnedRemoved { removed: r.len() as u32 }),
        Err(_) => Resp::refusal(RefusalCode::Internal),
    }
}

/// `POST /internal/v1/owned/prune`.
pub async fn prune<H: Host>(host: &H, req: &Req) -> Resp {
    let body: OwnedPruneRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    if body.entry_ids.len() > MAX_PRUNE {
        return Resp::refusal(RefusalCode::TooLarge);
    }
    if body.entry_ids.is_empty() {
        return Resp::json(200, &OwnedRemoved { removed: 0 });
    }
    let marks = vec!["?"; body.entry_ids.len()].join(", ");
    let mut args = vec![Val::from(body.user_id.0)];
    args.extend(body.entry_ids.iter().map(|&e| Val::from(e)));
    let stmt =
        Stmt::new(format!("DELETE FROM owned WHERE user_id = ? AND entry_id IN ({marks}) RETURNING entry_id"), args);
    match host.all(stmt).await {
        Ok(r) => Resp::json(200, &OwnedRemoved { removed: r.len() as u32 }),
        Err(_) => Resp::refusal(RefusalCode::Internal),
    }
}
