//! Email magic links: `email/start`, `email/peek` and `email/confirm`.
//!
//! **Start** always answers the same (303 to `/signin/check-email/`, or 202
//! for JSON), whether or not anything is sent, so it never says whether an
//! account or an address exists. Nothing is sent when: the address does
//! not pass `email_addr`; the intent needs a session it lacks; the address
//! is suppressed; this browser asked for a link to it less than
//! `email_spacing` ago; or a quota bucket is empty. Sign-in mail (the
//! `signin`, `step_up` and `attach_pending` kinds) draws on the address's
//! `email` bucket; connect mail (`link`) draws on the address's own
//! `email_link` bucket and the requesting account's `link_mail` bucket, so
//! another account's connect requests never spend the holder's sign-in
//! budget. The token row is upserted per address **and browser**, so another
//! browser's requests never void or delay a link.
//!
//! **Peek** is read-only. **Confirm** spends the token first, then applies
//! its intent's browser rule: `signin` works in any browser; `link` and
//! `step_up` only in the session that asked; `attach_pending` only in the
//! browser that ran the provider sign-in.

use slop_ninja_account_wire::{
    EmailPeekAnswer, EmailStartRequest, EmailTokenRequest, Intent, LINK_CONFIRM_PATH, MailIntent, NextAnswer, Provider,
    UserState,
};
use slop_ninja_auth_core::email_addr::{self, EmailNorm};
use slop_ninja_auth_core::mail::{self, MailKind, SendFailure};
use slop_ninja_auth_core::policy::ceiling_alert;
use slop_ninja_auth_core::providers::OAuthProvider;
use slop_ninja_auth_core::return_to::{ReturnTo, sanitize_or_default};
use slop_ninja_auth_core::state::{Flow, check_binding, check_email_token};
use slop_ninja_auth_core::token::Token;
use slop_ninja_auth_core::{cookie, csrf};
use slop_ninja_detect_page::{RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Row, Val, guard};
use crate::http::{self, Req, Resp};
use crate::sessions::{self, IdentityRef};
use crate::signin::{self, Done, FlowFacts, Proven};
use crate::store::{self, Switches, USER_ACTIVE};

const METHOD: &str = "email";
const CHECK_EMAIL: &str = "/signin/check-email/";

/// The one answer of `email/start`.
fn same_answer(cfg: &Config, form: bool, set_cookie: Option<String>) -> Resp {
    let r = if form {
        Resp::redirect(303, &cfg.origins.site, CHECK_EMAIL)
    } else {
        Resp::json(202, &serde_json::json!({}))
    };
    match set_cookie {
        Some(c) => r.with_cookie(c),
        None => r,
    }
}

fn refuse(cfg: &Config, form: bool, code: RefusalCode) -> Resp {
    if form { Resp::form_error(&cfg.origins.site, "/signin/", code) } else { Resp::refusal(code) }
}

/// The start request, from a form or JSON.
fn start_request(req: &Req) -> Result<(EmailStartRequest, bool), RefusalCode> {
    let ct = req.header("content-type");
    if csrf::content_type_ok(ct, csrf::BodyType::Form) {
        let f = http::form(&req.body, &["email", "return_to", "intent", "identity_id"])?;
        let intent = match f.get("intent").map(String::as_str) {
            None | Some("") => None,
            Some(i) => Some(MailIntent::parse(i).ok_or(RefusalCode::MalformedRequest)?),
        };
        let identity_id = match f.get("identity_id").map(String::as_str) {
            None | Some("") => None,
            Some(i) => Some(i.parse().map_err(|_| RefusalCode::MalformedRequest)?),
        };
        Ok((
            EmailStartRequest {
                email: f.get("email").cloned(),
                return_to: f.get("return_to").cloned(),
                intent,
                identity_id,
            },
            true,
        ))
    } else if csrf::content_type_ok(ct, csrf::BodyType::Json) {
        Ok((http::json(&req.body)?, false))
    } else {
        Err(csrf::CONTENT_TYPE_REFUSAL)
    }
}

/// Who a mail goes to and what its token row holds.
struct Plan {
    to: EmailNorm,
    typed: String,
    kind: MailKind,
    user_id: Option<UserId>,
    session_h: Option<[u8; 32]>,
    return_to: String,
    /// `attach_pending`: the pending row's expiry caps the link's.
    expires_cap: Option<u64>,
}

/// `POST /api/account/v1/email/start`.
pub async fn start<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    let (body, form) = match start_request(req) {
        Ok(v) => v,
        Err(code) => {
            let form = csrf::content_type_ok(req.header("content-type"), csrf::BodyType::Form);
            return refuse(cfg, form, code);
        }
    };
    if !sw.method_on(Provider::Email) {
        return refuse(cfg, form, RefusalCode::MethodOff);
    }
    let Ok(mail_cfg) = &cfg.mail else { return refuse(cfg, form, RefusalCode::NotConfigured) };
    // The browser's binding: the one it has, or a new one set now.
    let (binding, set_cookie) = match cookie::token(req.cookie(), cookie::FLOW) {
        Some(t) => (t, None),
        None => {
            let t = Token::from_random(host.random());
            let c = cookie::set(cookie::FLOW, &t, cfg.policy.flow_ttl_s.value);
            (t, Some(c))
        }
    };
    let binding_h = binding.hash();
    let intent = body.intent.unwrap_or(MailIntent::Signin);
    crate::metrics::auth(host, METHOD, "start", None);
    let plan = match plan(cfg, host, req, sw, &body, intent, &binding_h).await {
        Ok(Some(p)) => p,
        Ok(None) => return same_answer(cfg, form, set_cookie),
        Err(code) => {
            crate::metrics::auth(host, METHOD, "failed", Some(code));
            return same_answer(cfg, form, set_cookie);
        }
    };
    send(cfg, host, mail_cfg, &plan, &binding_h).await;
    same_answer(cfg, form, set_cookie)
}

/// Decides the recipient; `Ok(None)` sends nothing (the same answer).
async fn plan<H: Host>(
    cfg: &Config,
    host: &H,
    req: &Req,
    sw: &Switches,
    body: &EmailStartRequest,
    intent: MailIntent,
    binding_h: &[u8; 32],
) -> Result<Option<Plan>, RefusalCode> {
    let now = host.now_ms();
    let return_to = |signed_in: bool| {
        sanitize_or_default(body.return_to.as_deref(), &cfg.origins.site, &cfg.policy, signed_in).as_str().to_owned()
    };
    let typed = |raw: Option<&str>| -> Result<(EmailNorm, String), RefusalCode> {
        let a = email_addr::parse(raw.unwrap_or("")).map_err(|e| e.code())?;
        Ok((a.norm, a.typed))
    };
    Ok(Some(match intent {
        MailIntent::Signin => {
            let (to, typed) = typed(body.email.as_deref())?;
            Plan {
                to,
                typed,
                kind: MailKind::Signin,
                user_id: None,
                session_h: None,
                return_to: return_to(false),
                expires_cap: None,
            }
        }
        MailIntent::Link => {
            let Some(s) = sessions::current(host, req).await.map_err(|_| RefusalCode::Internal)? else {
                return Ok(None);
            };
            if !s.stepped_up(cfg, now) || sw.read_only() {
                return Ok(None);
            }
            let (to, typed) = typed(body.email.as_deref())?;
            Plan {
                to,
                typed,
                kind: MailKind::Link,
                user_id: Some(s.user_id),
                session_h: Some(s.token_h),
                return_to: return_to(true),
                expires_cap: None,
            }
        }
        MailIntent::StepUp => {
            let Some(s) = sessions::current(host, req).await.map_err(|_| RefusalCode::Internal)? else {
                return Ok(None);
            };
            let Some(id) = body.identity_id else { return Ok(None) };
            let row = host
                .first(crate::stmt!(
                    "SELECT subject, email FROM identities WHERE identity_id = ? AND user_id = ? AND provider = 'email'",
                    id,
                    s.user_id.0
                ))
                .await
                .map_err(|_| RefusalCode::Internal)?;
            let Some(row) = row else { return Ok(None) };
            let to = row.text("subject").and_then(EmailNorm::from_stored).ok_or(RefusalCode::Internal)?;
            let typed = row.text("email").unwrap_or(to.as_str()).to_owned();
            Plan {
                to,
                typed,
                kind: MailKind::StepUp,
                user_id: Some(s.user_id),
                session_h: Some(s.token_h),
                return_to: return_to(true),
                expires_cap: None,
            }
        }
        MailIntent::AttachPending => {
            if !req.cookie().is_some_and(|c| cookie::get(Some(c), cookie::FLOW).is_some()) {
                return Ok(None);
            }
            let row = host
                .first(crate::stmt!(
                    "SELECT provider, email, return_to, expires_at FROM pending_links \
                     WHERE binding_h = ? AND kind = 'email_in_use' AND expires_at > ?",
                    *binding_h,
                    now
                ))
                .await
                .map_err(|_| RefusalCode::Internal)?;
            let Some(row) = row else { return Ok(None) };
            let provider = row.text("provider").and_then(OAuthProvider::parse).ok_or(RefusalCode::Internal)?;
            let to = row.text("email").and_then(EmailNorm::from_stored).ok_or(RefusalCode::Internal)?;
            Plan {
                typed: to.as_str().to_owned(),
                to,
                kind: MailKind::AttachPending { provider },
                user_id: None,
                session_h: None,
                return_to: row.text("return_to").unwrap_or(signin_default()).to_owned(),
                expires_cap: row.u64("expires_at"),
            }
        }
        // Created only by an accepted operator op, never by a request.
        MailIntent::Recover => return Ok(None),
    }))
}

fn signin_default() -> &'static str {
    slop_ninja_account_wire::SIGNED_OUT_RETURN
}

/// Spacing, suppression, quota, the token row and the send.
async fn send<H: Host>(cfg: &Config, host: &H, mail_cfg: &mail::MailConfig, plan: &Plan, binding_h: &[u8; 32]) {
    let now = host.now_ms();
    let addr_h = plan.to.keyed_hash(cfg.keys.k_addr());
    let spacing_ms = cfg.policy.email_spacing_s.value * 1000;
    let pre = host
        .first(crate::stmt!(
            "SELECT (SELECT COUNT(*) FROM email_suppress WHERE addr_h = ?) AS suppressed, \
             (SELECT COUNT(*) FROM email_tokens WHERE addr_h = ? AND binding_h = ? AND created_at > ?) AS recent",
            addr_h,
            addr_h,
            *binding_h,
            now.saturating_sub(spacing_ms)
        ))
        .await;
    let Ok(Some(pre)) = pre else {
        crate::metrics::error(host, "d1");
        return;
    };
    if pre.int("suppressed").unwrap_or(1) > 0 || pre.int("recent").unwrap_or(1) > 0 {
        return;
    }
    // The buckets: a connect mail takes the account's own quota first, so an
    // account at its quota never spends the address's connect budget.
    let Ok(mut buckets) = plan.kind.buckets(&addr_h, plan.user_id) else { return };
    buckets.sort_by_key(|b| b.kind != slop_ninja_account_wire::BucketKind::LinkMail);
    for b in &buckets {
        let Some(size) = cfg.policy.bucket(b.kind) else { return };
        match host.quota_take(&cfg.keys.quota_name(&b.name()), size).await {
            Ok(t) if t.taken => {
                if t.emptied
                    && let Some(rule) = ceiling_alert(b.kind)
                {
                    crate::metrics::alert(host, rule);
                }
            }
            Ok(_) => {
                crate::metrics::auth(host, METHOD, "failed", Some(RefusalCode::Quota));
                return;
            }
            Err(_) => {
                crate::metrics::error(host, "quota");
                return;
            }
        }
    }
    let mut expires = now + cfg.policy.email_link_ttl_s.value * 1000;
    if let Some(cap) = plan.expires_cap {
        expires = expires.min(cap);
    }
    let token = Token::from_random(host.random());
    let stmts = vec![
        store::prune("email_tokens", now, cfg.policy.flow_sweep.value.per_insert),
        crate::stmt!(
            "INSERT INTO email_tokens (token_h, addr_h, email_norm, email_typed, intent, user_id, session_h, binding_h, \
             return_to, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (addr_h, binding_h) DO UPDATE SET token_h = excluded.token_h, email_norm = excluded.email_norm, \
             email_typed = excluded.email_typed, intent = excluded.intent, user_id = excluded.user_id, \
             session_h = excluded.session_h, return_to = excluded.return_to, created_at = excluded.created_at, \
             expires_at = excluded.expires_at WHERE email_tokens.created_at <= ? RETURNING token_h",
            token.hash(),
            addr_h,
            plan.to.as_str(),
            plan.typed.clone(),
            plan.kind.intent().as_str(),
            plan.user_id.map(|u| u.0),
            plan.session_h,
            *binding_h,
            plan.return_to.clone(),
            now,
            expires,
            now.saturating_sub(spacing_ms)
        ),
    ];
    let Ok(rows) = host.batch(stmts).await else {
        crate::metrics::error(host, "d1");
        return;
    };
    if rows.get(1).is_none_or(Vec::is_empty) {
        // A concurrent request from this browser wrote it first.
        return;
    }
    let minutes = expires.saturating_sub(now) / 60_000;
    let message = mail::compose(mail_cfg, plan.kind, &plan.to, &token, minutes);
    match host.send_mail(&message).await {
        Ok(()) => {
            crate::metrics::mail(host, "sent");
            crate::metrics::auth(host, METHOD, "sent", None);
        }
        Err(f) => {
            crate::metrics::mail(host, f.as_str());
            if let Some(rule) = f.alert() {
                crate::metrics::alert(host, rule);
            }
            if f == SendFailure::Suppressed {
                let _ = host
                    .all(crate::stmt!(
                        "INSERT INTO email_suppress (addr_h, reason, at) VALUES (?, 'recipient_suppressed', ?) \
                         ON CONFLICT (addr_h) DO NOTHING",
                        addr_h,
                        now
                    ))
                    .await;
            }
        }
    }
}

/// A token row.
struct TokenRow {
    email_norm: String,
    email_typed: String,
    intent: MailIntent,
    user_id: Option<UserId>,
    session_h: Option<[u8; 32]>,
    binding_h: [u8; 32],
    return_to: String,
    expires_at: u64,
}

fn token_row(r: &Row) -> Option<TokenRow> {
    Some(TokenRow {
        email_norm: r.text("email_norm")?.to_owned(),
        email_typed: r.text("email_typed")?.to_owned(),
        intent: MailIntent::parse(r.text("intent")?)?,
        user_id: r.b16("user_id").map(UserId),
        session_h: r.b32("session_h"),
        binding_h: r.b32("binding_h")?,
        return_to: r.text("return_to")?.to_owned(),
        expires_at: r.u64("expires_at")?,
    })
}

const TOKEN_COLS: &str = "email_norm, email_typed, intent, user_id, session_h, binding_h, return_to, expires_at";

fn token_of(req: &Req) -> Result<Token, RefusalCode> {
    if !csrf::content_type_ok(req.header("content-type"), csrf::BodyType::Json) {
        return Err(csrf::CONTENT_TYPE_REFUSAL);
    }
    let body: EmailTokenRequest = http::json(&req.body)?;
    // A malformed token is one that was never issued.
    Token::parse(&body.token).ok_or(RefusalCode::Expired)
}

/// The intent's browser rule.
async fn browser_ok<H: Host>(host: &H, req: &Req, t: &TokenRow) -> Result<(), RefusalCode> {
    match t.intent {
        MailIntent::Signin | MailIntent::Recover => Ok(()),
        MailIntent::Link | MailIntent::StepUp => {
            let flow = FlowFacts {
                intent: Intent::Link,
                user_id: t.user_id,
                session_h: t.session_h,
                return_to: String::new(),
            };
            signin::flow_session(host, req, &flow, RefusalCode::LinkWrongBrowser).await.map(|_| ())
        }
        MailIntent::AttachPending => {
            check_binding(&t.binding_h, cookie::get(req.cookie(), cookie::FLOW)).map_err(|r| r.code(Flow::EmailLink))
        }
    }
}

/// `POST /api/account/v1/email/peek`: read-only.
pub async fn peek<H: Host>(host: &H, req: &Req, sw: &Switches) -> Resp {
    if !sw.method_on(Provider::Email) {
        return Resp::refusal(RefusalCode::MethodOff);
    }
    let token = match token_of(req) {
        Ok(t) => t,
        Err(c) => return Resp::refusal(c),
    };
    let row = host
        .first(crate::stmt!(
            &format!("SELECT {TOKEN_COLS} FROM email_tokens WHERE token_h = ? AND expires_at > ?"),
            token.hash(),
            host.now_ms()
        ))
        .await;
    let t = match row {
        Ok(Some(r)) => match token_row(&r) {
            Some(t) => t,
            None => return Resp::refusal(RefusalCode::Internal),
        },
        Ok(None) => return Resp::refusal(RefusalCode::Expired),
        Err(_) => return Resp::refusal(RefusalCode::Internal),
    };
    if let Err(c) = browser_ok(host, req, &t).await {
        return Resp::refusal(c);
    }
    let provider = if t.intent == MailIntent::AttachPending {
        host.first(crate::stmt!(
            "SELECT provider FROM pending_links WHERE binding_h = ? AND kind = 'email_in_use'",
            t.binding_h
        ))
        .await
        .ok()
        .flatten()
        .and_then(|r| Provider::parse(r.text("provider")?))
    } else {
        None
    };
    Resp::json(200, &EmailPeekAnswer { intent: t.intent, address: t.email_typed, provider })
}

/// `POST /api/account/v1/email/confirm`.
pub async fn confirm<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    if !sw.method_on(Provider::Email) {
        return Resp::refusal(RefusalCode::MethodOff);
    }
    match run_confirm(cfg, host, req, sw).await {
        Ok(r) => {
            crate::metrics::auth(host, METHOD, "redeemed", None);
            r
        }
        Err(c) => {
            crate::metrics::auth(host, METHOD, "failed", Some(c));
            Resp::refusal(c)
        }
    }
}

fn next(return_to: &str, cookies: Vec<String>) -> Resp {
    Resp::json(200, &NextAnswer { return_to: return_to.to_owned() }).with_cookies(cookies)
}

async fn run_confirm<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Result<Resp, RefusalCode> {
    let token = token_of(req)?;
    let now = host.now_ms();
    // Spent here, whether or not the rest succeeds.
    let row = host
        .first(crate::stmt!(
            &format!("DELETE FROM email_tokens WHERE token_h = ? AND expires_at > ? RETURNING {TOKEN_COLS}"),
            token.hash(),
            now
        ))
        .await
        .map_err(|_| RefusalCode::Internal)?;
    let t = row.as_ref().and_then(token_row);
    check_email_token(t.as_ref().map(|t| t.expires_at), now).map_err(|r| r.code(Flow::EmailLink))?;
    let t = t.ok_or(RefusalCode::Expired)?;
    browser_ok(host, req, &t).await?;
    let to = EmailNorm::from_stored(&t.email_norm).ok_or(RefusalCode::Internal)?;
    let proven = Proven {
        provider: Provider::Email,
        subject: to.as_str().to_owned(),
        email: Some(t.email_typed.clone()),
        email_verified: true,
        handle: None,
        link_email: None,
    };
    let signed_in = t.intent.is_signed_in();
    let return_to = ReturnTo::from_stored(&t.return_to, &cfg.origins.site, &cfg.policy, signed_in).as_str().to_owned();
    match t.intent {
        MailIntent::Signin => match signin::sign_in(cfg, host, req, sw, &proven, &return_to).await? {
            Done::Session { set_cookies, next: n, .. } => Ok(next(&n, set_cookies)),
            _ => Err(RefusalCode::Internal),
        },
        MailIntent::Link | MailIntent::StepUp => {
            let intent = if t.intent == MailIntent::Link { Intent::Link } else { Intent::StepUp };
            let flow = FlowFacts { intent, user_id: t.user_id, session_h: t.session_h, return_to: return_to.clone() };
            match signin::apply(cfg, host, req, sw, &proven, &flow, RefusalCode::LinkWrongBrowser).await? {
                Done::Session { set_cookies, next: n, .. } => Ok(next(&n, set_cookies)),
                Done::Pending { set_cookie } => Ok(next(LINK_CONFIRM_PATH, vec![set_cookie])),
                Done::AlreadyLinked => Ok(next(LINK_CONFIRM_PATH, vec![])),
                Done::EmailInUse { .. } => Err(RefusalCode::Internal),
            }
        }
        MailIntent::AttachPending => attach_pending(cfg, host, req, sw, &t, &to).await,
        MailIntent::Recover => recover(cfg, host, req, &t, &proven, &return_to).await,
    }
}

/// The `email_in_use` attach: this browser's pending row's identity joins
/// the account that owns the address, which is signed in.
async fn attach_pending<H: Host>(
    cfg: &Config,
    host: &H,
    req: &Req,
    sw: &Switches,
    t: &TokenRow,
    to: &EmailNorm,
) -> Result<Resp, RefusalCode> {
    let now = host.now_ms();
    let p = host
        .first(crate::stmt!(
            "SELECT user_id, provider, subject, email, email_verified, handle, return_to FROM pending_links \
             WHERE binding_h = ? AND kind = 'email_in_use' AND expires_at > ?",
            t.binding_h,
            now
        ))
        .await
        .map_err(|_| RefusalCode::Internal)?
        .ok_or(RefusalCode::Expired)?;
    let user = UserId(p.b16("user_id").ok_or(RefusalCode::Internal)?);
    let provider = p.text("provider").and_then(Provider::parse).ok_or(RefusalCode::Internal)?;
    let subject = p.text("subject").ok_or(RefusalCode::Internal)?.to_owned();
    if sw.read_only() {
        return Err(RefusalCode::AccountsReadOnly);
    }
    // The address must still be this account's, and the provider identity
    // still unknown; the account must be active.
    match store::identity(host, Provider::Email, to.as_str()).await.map_err(|_| RefusalCode::Internal)? {
        Some(k) if k.user_id == user && k.state == UserState::Active => {}
        _ => return Err(RefusalCode::LinkUnavailable),
    }
    if store::identity(host, provider, &subject).await.map_err(|_| RefusalCode::Internal)?.is_some() {
        return Err(RefusalCode::LinkUnavailable);
    }
    let ua = sessions::ua_family(req.header("user-agent"));
    let issued = sessions::issue(cfg, host, &user, IdentityRef::Subject(provider, &subject), ua);
    let stmts = vec![
        guard(USER_ACTIVE, vec![Val::from(user.0)]),
        guard(
            "EXISTS (SELECT 1 FROM pending_links WHERE binding_h = ? AND kind = 'email_in_use' AND user_id = ? AND expires_at > ?)",
            vec![Val::from(t.binding_h), Val::from(user.0), Val::from(now)],
        ),
        crate::stmt!(
            "INSERT INTO identities (user_id, provider, subject, email, email_verified, handle, consent_at, created_at, \
             last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            user.0,
            provider.as_str(),
            subject.clone(),
            p.text("email").map(str::to_owned),
            p.int("email_verified").unwrap_or(0) != 0,
            p.text("handle").map(str::to_owned),
            now,
            now,
            now
        ),
        issued.stmt,
        crate::stmt!("DELETE FROM pending_links WHERE binding_h = ?", t.binding_h),
    ];
    match host.batch(stmts).await {
        Ok(_) => {}
        Err(DbError::Guard | DbError::Constraint) => return Err(RefusalCode::LinkUnavailable),
        Err(_) => return Err(RefusalCode::Internal),
    }
    crate::metrics::session(host, "issued");
    let rt = ReturnTo::from_stored(p.text("return_to").unwrap_or(""), &cfg.origins.site, &cfg.policy, false);
    Ok(next(rt.as_str(), vec![issued.set_cookie, cookie::clear(cookie::FLOW)]))
}

/// `recover` (only from an accepted `attach_email` op, refused at launch):
/// the still-unknown address joins the token's account, which is signed in.
async fn recover<H: Host>(
    cfg: &Config,
    host: &H,
    req: &Req,
    t: &TokenRow,
    proven: &Proven,
    return_to: &str,
) -> Result<Resp, RefusalCode> {
    let user = t.user_id.ok_or(RefusalCode::Internal)?;
    if store::identity(host, Provider::Email, &proven.subject).await.map_err(|_| RefusalCode::Internal)?.is_some() {
        return Err(RefusalCode::LinkUnavailable);
    }
    let now = host.now_ms();
    let ua = sessions::ua_family(req.header("user-agent"));
    let issued = sessions::issue(cfg, host, &user, IdentityRef::Subject(Provider::Email, &proven.subject), ua);
    let stmts = vec![
        guard(USER_ACTIVE, vec![Val::from(user.0)]),
        crate::stmt!(
            "INSERT INTO identities (user_id, provider, subject, email, email_verified, created_at, last_used_at) \
             VALUES (?, 'email', ?, ?, 1, ?, ?)",
            user.0,
            proven.subject.clone(),
            proven.email.clone(),
            now,
            now
        ),
        issued.stmt,
    ];
    host.batch(stmts).await.map_err(|_| RefusalCode::LinkUnavailable)?;
    Ok(next(return_to, vec![issued.set_cookie, cookie::clear(cookie::FLOW)]))
}
