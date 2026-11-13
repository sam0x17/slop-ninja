//! The account pages' own form actions: sign out, sessions, export and
//! deletion. Export and deletion need a fresh step-up; outside the window
//! the reader is sent to step-up mode first.

use serde_json::{Value, json};
use slop_ninja_account_wire::{DeleteAccountForm, EmptyForm, SessionRevokeForm};
use slop_ninja_detect_page::{PurgeMode, RefusalCode};

use crate::config::Config;
use crate::delete::{self, Actor};
use crate::host::{DbError, Host, Row};
use crate::http::{self, Req, Resp};
use crate::journal;
use crate::sessions::{self, Session};

fn page_error(cfg: &Config, page: &str, code: RefusalCode) -> Resp {
    Resp::form_error(&cfg.origins.site, page, code)
}

fn signed_out(cfg: &Config) -> Resp {
    Resp::redirect(303, &cfg.origins.site, "/").with_cookie(sessions::clear_cookie())
}

/// `POST /auth/logout`.
pub async fn logout<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    if http::form(&req.body, EmptyForm::FIELDS).is_err() {
        return page_error(cfg, "/account/", RefusalCode::MalformedRequest);
    }
    if let Ok(Some(s)) = sessions::current(host, req).await {
        let _ = host.all(crate::stmt!("DELETE FROM sessions WHERE token_h = ?", s.token_h)).await;
        crate::metrics::session(host, "revoked");
    }
    signed_out(cfg)
}

async fn session_or_signin<H: Host>(cfg: &Config, host: &H, req: &Req) -> Result<Session, Resp> {
    match sessions::current(host, req).await {
        Ok(Some(s)) => Ok(s),
        _ => Err(page_error(cfg, "/signin/", RefusalCode::Session)),
    }
}

/// `POST /account/sessions/revoke`.
pub async fn revoke<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    let reference =
        match http::form(&req.body, SessionRevokeForm::FIELDS).ok().and_then(|f| f.get("session_ref").cloned()) {
            Some(r) => r,
            None => return page_error(cfg, "/account/sessions/", RefusalCode::MalformedRequest),
        };
    let s = match session_or_signin(cfg, host, req).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Ok(rows) = host.all(crate::stmt!("SELECT token_h FROM sessions WHERE user_id = ?", s.user_id.0)).await else {
        return page_error(cfg, "/account/sessions/", RefusalCode::Internal);
    };
    let Some(token_h) = rows.iter().filter_map(|r| r.b32("token_h")).find(|h| cfg.keys.session_ref(h) == reference)
    else {
        return page_error(cfg, "/account/sessions/", RefusalCode::NotFound);
    };
    let _ =
        host.all(crate::stmt!("DELETE FROM sessions WHERE token_h = ? AND user_id = ?", token_h, s.user_id.0)).await;
    crate::metrics::session(host, "revoked");
    if token_h == s.token_h { signed_out(cfg) } else { Resp::redirect(303, &cfg.origins.site, "/account/sessions/") }
}

/// `POST /account/sessions/revoke-all`: "Sign out everywhere".
pub async fn revoke_all<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    if http::form(&req.body, EmptyForm::FIELDS).is_err() {
        return page_error(cfg, "/account/sessions/", RefusalCode::MalformedRequest);
    }
    let s = match session_or_signin(cfg, host, req).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let _ = host.all(sessions::revoke_all(&s.user_id)).await;
    crate::metrics::session(host, "revoked");
    signed_out(cfg)
}

const STEP_UP: &str = "/signin/?step_up=1&return_to=%2Faccount%2F";

fn pick(rows: &[Row], cols: &[&str]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            let mut o = serde_json::Map::new();
            for c in cols {
                let v = match r.get(c) {
                    Some(crate::host::Val::Int(i)) => json!(i),
                    Some(crate::host::Val::Text(t)) => json!(t),
                    Some(crate::host::Val::Blob(b)) => json!(hex::encode(b)),
                    _ => Value::Null,
                };
                o.insert((*c).to_owned(), v);
            }
            Value::Object(o)
        })
        .collect()
}

async fn export_doc<H: Host>(cfg: &Config, host: &H, s: &Session) -> Result<Value, DbError> {
    let u = s.user_id.0;
    let user = host.all(crate::stmt!("SELECT created_at, state FROM users WHERE user_id = ?", u)).await?;
    let identities = host
        .all(crate::stmt!(
            "SELECT provider, subject, email, email_verified, handle, created_at, last_used_at FROM identities \
             WHERE user_id = ? ORDER BY identity_id",
            u
        ))
        .await?;
    let sessions = host
        .all(crate::stmt!(
            "SELECT created_at, last_seen_day, ua_family FROM sessions WHERE user_id = ? ORDER BY created_at",
            u
        ))
        .await?;
    let saved: Vec<Value> = crate::owned::entries(cfg, host, &s.user_id)
        .await?
        .into_iter()
        .map(|e| {
            json!({
                "slug": e.slug,
                "kind": e.summary.kind.as_str(),
                "band": e.summary.band.map(|b| b.as_str()),
                "words": e.summary.words,
                "created_at": e.summary.created_at,
                "expires_at": e.summary.expires_at,
                "owner": e.is_owner,
            })
        })
        .collect();
    let payments = host
        .all(crate::stmt!(
            "SELECT payment_id, kind, cash, tokens_total, tokens_left, amount_cents, currency, state, created_at \
             FROM payments WHERE user_id = ?",
            u
        ))
        .await?;
    let periods = host
        .all(crate::stmt!(
            "SELECT period_id, starts_at, ends_at, allowance_granted, used, api_cap_granted, api_used, cash, state \
             FROM plan_periods WHERE user_id = ?",
            u
        ))
        .await?;
    let holds = host
        .all(crate::stmt!(
            "SELECT hold_id, channel, tokens, state, created_at, updated_at FROM holds WHERE user_id = ?",
            u
        ))
        .await?;
    let debits = host
        .all(crate::stmt!(
            "SELECT hold_id, src, tokens, cash, package_pub, receipt_id, channel, at FROM debits WHERE user_id = ?",
            u
        ))
        .await?;
    Ok(json!({
        "schema": "slop_ninja_account_export_v1",
        "exported_at": host.now_ms(),
        "account": pick(&user, &["created_at", "state"]).into_iter().next(),
        "identities": pick(&identities, &["provider", "subject", "email", "email_verified", "handle", "created_at", "last_used_at"]),
        "sessions": pick(&sessions, &["created_at", "last_seen_day", "ua_family"]),
        "saved_detections": saved,
        "ledger": {
            "payments": pick(&payments, &["payment_id", "kind", "cash", "tokens_total", "tokens_left", "amount_cents", "currency", "state", "created_at"]),
            "plan_periods": pick(&periods, &["period_id", "starts_at", "ends_at", "allowance_granted", "used", "api_cap_granted", "api_used", "cash", "state"]),
            "holds": pick(&holds, &["hold_id", "channel", "tokens", "state", "created_at", "updated_at"]),
            "debits": pick(&debits, &["hold_id", "src", "tokens", "cash", "package_pub", "receipt_id", "channel", "at"]),
        },
        "note": "Texts are not included. Each saved page holds your own copy, which you can open from its link; a link that was never saved is not held by SlopNinja, so it cannot be exported.",
    }))
}

/// `POST /account/export`: a JSON attachment of the account's data.
pub async fn export<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    if http::form(&req.body, EmptyForm::FIELDS).is_err() {
        return page_error(cfg, "/account/", RefusalCode::MalformedRequest);
    }
    let s = match session_or_signin(cfg, host, req).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !s.stepped_up(cfg, host.now_ms()) {
        return Resp::redirect(303, &cfg.origins.site, STEP_UP);
    }
    match export_doc(cfg, host, &s).await {
        Ok(doc) => {
            Resp::json(200, &doc).with_header("content-disposition", "attachment; filename=\"slopninja-account.json\"")
        }
        Err(_) => page_error(cfg, "/account/", RefusalCode::Internal),
    }
}

/// `POST /account/delete`.
pub async fn delete_self<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    let form = http::form(&req.body, DeleteAccountForm::FIELDS).ok().and_then(|f| {
        Some(DeleteAccountForm { confirm: f.get("confirm")?.clone(), mode: PurgeMode::parse(f.get("mode")?)? })
    });
    let Some(form) = form else { return page_error(cfg, "/account/delete/", RefusalCode::MalformedRequest) };
    let s = match session_or_signin(cfg, host, req).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !s.stepped_up(cfg, host.now_ms()) {
        return Resp::redirect(303, &cfg.origins.site, "/signin/?step_up=1&return_to=%2Faccount%2Fdelete%2F");
    }
    if !form.confirmed() {
        return page_error(cfg, "/account/delete/", RefusalCode::ConfirmWord);
    }
    let by = Actor { actor: journal::ACTOR_USER, reason: None, signature: None, nonce_h: None, extra: vec![] };
    match delete::delete_account(cfg, host, &s.user_id, form.mode, by).await {
        Ok(_) => signed_out(cfg),
        Err(c) => page_error(cfg, "/account/delete/", c),
    }
}
