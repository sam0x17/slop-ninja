//! What a proven identity does under its flow's intent. OAuth callbacks,
//! the wallet verify and the email confirm prove control of an identity;
//! this module decides what follows, the same way for every method.
//!
//! - **`signin`** resolves `(provider, subject)`. A known identity of an
//!   active account signs in; an account that is not active gets no session
//!   (`account_unavailable`). An unknown identity creates an account, except
//!   that a Google or GitHub identity whose verified address is already an
//!   email identity's never links silently: a pending row of kind
//!   `email_in_use` waits under a fresh flow binding on this browser, and
//!   only a mail to that address, confirmed in this browser, attaches it. X
//!   never links by email.
//! - **`link`** needs the session that started the flow, still live and the
//!   same user's. The identity waits in a pending row (kind `link`, or
//!   `merge` when another active account owns it) under a fresh flow
//!   binding, for the explicit connect POST. An identity of an account that
//!   is not active cannot be connected (`link_unavailable`).
//! - **`step_up`** needs the same session and an identity that already
//!   belongs to its user; the session is rotated with `auth_time = now`. A
//!   foreign or unknown identity refuses (`step_up_identity`) and changes
//!   nothing.
//!
//! Every refusal issues no session, creates no user and attaches nothing.

use slop_ninja_account_wire::{Intent, PendingKind, Provider, UserState};
use slop_ninja_auth_core::cookie;
use slop_ninja_auth_core::email_addr::EmailNorm;
use slop_ninja_auth_core::token::Token;
use slop_ninja_detect_page::{RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Stmt, Val, guard};
use crate::http::Req;
use crate::sessions::{self, IdentityRef, Session};
use crate::store::{self, Switches, USER_ACTIVE};

/// An identity whose control a flow has just proven.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proven {
    pub provider: Provider,
    /// `email_norm`, Google `sub`, GitHub numeric id, X user id, or the
    /// sr25519 public key in hex.
    pub subject: String,
    /// What the holder's own pages show: the typed address of an email
    /// identity, the verified address of a Google or GitHub one, X's
    /// confirmed address.
    pub email: Option<String>,
    pub email_verified: bool,
    /// GitHub login, X username, SS58 address (prefix 42).
    pub handle: Option<String>,
    /// Google and GitHub only: the verified address the no-silent-link rule
    /// checks against the email identities.
    pub link_email: Option<EmailNorm>,
}

/// What the flow recorded at its start.
#[derive(Clone, Debug)]
pub struct FlowFacts {
    pub intent: Intent,
    /// `link`, `step_up`: the account and session that started the flow.
    pub user_id: Option<UserId>,
    pub session_h: Option<[u8; 32]>,
    /// The sanitized destination stored at the start.
    pub return_to: String,
}

/// The outcome of a proven identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    /// A session was issued (sign-in) or rotated (step-up); go to `next`.
    Session { set_cookies: Vec<String>, next: String, identity_id: i64, user_id: UserId },
    /// A pending link or merge waits for `/account/link/confirm`.
    Pending { set_cookie: String },
    /// An `email_in_use` row waits; go to `/signin/?e=email_in_use`.
    EmailInUse { set_cookie: String },
    /// The identity already belongs to this account.
    AlreadyLinked,
}

fn db(_: DbError) -> RefusalCode {
    RefusalCode::Internal
}

/// A fresh flow binding: its cookie and its hash.
pub fn fresh_binding<H: Host>(cfg: &Config, host: &H) -> (String, [u8; 32]) {
    let t = Token::from_random(host.random());
    (cookie::set(cookie::FLOW, &t, cfg.policy.flow_ttl_s.value), t.hash())
}

/// The request's session, when it is the one the flow recorded.
pub async fn flow_session<H: Host>(
    host: &H,
    req: &Req,
    flow: &FlowFacts,
    wrong: RefusalCode,
) -> Result<Session, RefusalCode> {
    let s = sessions::current(host, req).await.map_err(db)?.ok_or(wrong)?;
    let same = match (flow.user_id, flow.session_h) {
        (Some(u), Some(h)) => u == s.user_id && slop_ninja_auth_core::ct_eq(&h, &s.token_h),
        _ => false,
    };
    if same { Ok(s) } else { Err(wrong) }
}

/// Applies `proven` under `flow`. `wrong` is the refusal when the request
/// does not carry the flow's session (`link_wrong_browser` for an email
/// link, `session` otherwise).
pub async fn apply<H: Host>(
    cfg: &Config,
    host: &H,
    req: &Req,
    sw: &Switches,
    proven: &Proven,
    flow: &FlowFacts,
    wrong: RefusalCode,
) -> Result<Done, RefusalCode> {
    match flow.intent {
        Intent::Signin => sign_in(cfg, host, req, sw, proven, &flow.return_to).await,
        Intent::Link => {
            let s = flow_session(host, req, flow, wrong).await?;
            link(cfg, host, sw, proven, &s, &flow.return_to).await
        }
        Intent::StepUp => {
            let s = flow_session(host, req, flow, wrong).await?;
            step_up(cfg, host, proven, &s, &flow.return_to).await
        }
    }
}

/// The identity's refreshed fields at a sign-in (the X handle and the
/// providers' addresses change; an email identity keeps its typed form).
fn refresh(proven: &Proven, identity_id: i64, now: u64) -> Stmt {
    if proven.provider == Provider::Email {
        crate::stmt!("UPDATE identities SET last_used_at = ? WHERE identity_id = ?", now, identity_id)
    } else {
        crate::stmt!(
            "UPDATE identities SET email = ?, email_verified = ?, handle = ?, last_used_at = ? WHERE identity_id = ?",
            proven.email.clone(),
            proven.email_verified,
            proven.handle.clone(),
            now,
            identity_id
        )
    }
}

/// `signin`: sign in, create, or hold an `email_in_use` row.
pub async fn sign_in<H: Host>(
    cfg: &Config,
    host: &H,
    req: &Req,
    sw: &Switches,
    proven: &Proven,
    return_to: &str,
) -> Result<Done, RefusalCode> {
    let ua = sessions::ua_family(req.header("user-agent"));
    let now = host.now_ms();
    // Two passes: a concurrent first sign-in of the same identity makes the
    // create fail on UNIQUE (provider, subject), and the second pass signs
    // in to the account it created.
    for _ in 0..2 {
        if let Some(k) = store::identity(host, proven.provider, &proven.subject).await.map_err(db)? {
            if k.state != UserState::Active {
                return Err(RefusalCode::AccountUnavailable);
            }
            let issued = sessions::issue(cfg, host, &k.user_id, IdentityRef::Id(k.identity_id), ua.clone());
            let stmts = vec![
                guard(USER_ACTIVE, vec![Val::from(k.user_id.0)]),
                refresh(proven, k.identity_id, now),
                issued.stmt,
            ];
            return match host.batch(stmts).await {
                Ok(_) => {
                    crate::metrics::session(host, "issued");
                    Ok(Done::Session {
                        set_cookies: vec![issued.set_cookie, cookie::clear(cookie::FLOW)],
                        next: return_to.to_owned(),
                        identity_id: k.identity_id,
                        user_id: k.user_id,
                    })
                }
                Err(DbError::Guard) => Err(RefusalCode::AccountUnavailable),
                Err(e) => Err(db(e)),
            };
        }
        if let Some(addr) = &proven.link_email
            && let Some(owner) = store::identity(host, Provider::Email, addr.as_str()).await.map_err(db)?
        {
            if owner.state != UserState::Active {
                return Err(RefusalCode::AccountUnavailable);
            }
            let set_cookie =
                pending(cfg, host, PendingKind::EmailInUse, &owner.user_id, None, proven, None, return_to).await?;
            return Ok(Done::EmailInUse { set_cookie });
        }
        if sw.read_only() {
            return Err(RefusalCode::AccountsReadOnly);
        }
        let user = UserId(host.random());
        let issued =
            sessions::issue(cfg, host, &user, IdentityRef::Subject(proven.provider, &proven.subject), ua.clone());
        let stmts = vec![
            crate::stmt!("INSERT INTO users (user_id, state, created_at) VALUES (?, 'active', ?)", user.0, now),
            crate::stmt!(
                "INSERT INTO identities (user_id, provider, subject, email, email_verified, handle, created_at, \
                 last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING identity_id",
                user.0,
                proven.provider.as_str(),
                proven.subject.clone(),
                proven.email.clone(),
                proven.email_verified,
                proven.handle.clone(),
                now,
                now
            ),
            issued.stmt,
        ];
        match host.batch(stmts).await {
            Ok(rows) => {
                let identity_id = rows
                    .get(1)
                    .and_then(|r| r.first())
                    .and_then(|r| r.int("identity_id"))
                    .ok_or(RefusalCode::Internal)?;
                crate::metrics::session(host, "issued");
                return Ok(Done::Session {
                    set_cookies: vec![issued.set_cookie, cookie::clear(cookie::FLOW)],
                    next: return_to.to_owned(),
                    identity_id,
                    user_id: user,
                });
            }
            Err(DbError::Constraint) => continue,
            Err(e) => return Err(db(e)),
        }
    }
    Err(RefusalCode::Internal)
}

/// `link`, inside the flow's session `s`.
pub async fn link<H: Host>(
    cfg: &Config,
    host: &H,
    sw: &Switches,
    proven: &Proven,
    s: &Session,
    return_to: &str,
) -> Result<Done, RefusalCode> {
    if sw.read_only() {
        return Err(RefusalCode::AccountsReadOnly);
    }
    let (kind, from) = match store::identity(host, proven.provider, &proven.subject).await.map_err(db)? {
        None => (PendingKind::Link, None),
        Some(k) if k.user_id == s.user_id => return Ok(Done::AlreadyLinked),
        Some(k) if k.state == UserState::Active => (PendingKind::Merge, Some(k.user_id)),
        Some(_) => return Err(RefusalCode::LinkUnavailable),
    };
    let set_cookie = pending(cfg, host, kind, &s.user_id, Some(s.token_h), proven, from.as_ref(), return_to).await?;
    Ok(Done::Pending { set_cookie })
}

/// `step_up`, inside the flow's session `s`.
pub async fn step_up<H: Host>(
    cfg: &Config,
    host: &H,
    proven: &Proven,
    s: &Session,
    return_to: &str,
) -> Result<Done, RefusalCode> {
    let Some(k) = store::identity(host, proven.provider, &proven.subject).await.map_err(db)? else {
        return Err(RefusalCode::StepUpIdentity);
    };
    if k.user_id != s.user_id {
        return Err(RefusalCode::StepUpIdentity);
    }
    let rotated = sessions::rotate(cfg, host, s);
    let stmts = vec![
        guard(USER_ACTIVE, vec![Val::from(s.user_id.0)]),
        crate::stmt!("UPDATE identities SET last_used_at = ? WHERE identity_id = ?", host.now_ms(), k.identity_id),
        rotated.stmt,
    ];
    match host.batch(stmts).await {
        Ok(_) => {
            crate::metrics::session(host, "rotated");
            Ok(Done::Session {
                set_cookies: vec![rotated.set_cookie, cookie::clear(cookie::FLOW)],
                next: return_to.to_owned(),
                identity_id: s.identity_id,
                user_id: s.user_id,
            })
        }
        Err(DbError::Guard) => Err(RefusalCode::AccountUnavailable),
        Err(e) => Err(db(e)),
    }
}

/// Writes a pending row under a fresh flow binding; answers its cookie.
#[allow(clippy::too_many_arguments)]
async fn pending<H: Host>(
    cfg: &Config,
    host: &H,
    kind: PendingKind,
    user: &UserId,
    session_h: Option<[u8; 32]>,
    proven: &Proven,
    from: Option<&UserId>,
    return_to: &str,
) -> Result<String, RefusalCode> {
    let now = host.now_ms();
    let (set_cookie, binding_h) = fresh_binding(cfg, host);
    let email = if kind == PendingKind::EmailInUse {
        proven.link_email.as_ref().map(|e| e.as_str().to_owned())
    } else {
        proven.email.clone()
    };
    let stmts = vec![
        store::prune("pending_links", now, cfg.policy.flow_sweep.value.per_insert),
        guard(USER_ACTIVE, vec![Val::from(user.0)]),
        crate::stmt!(
            "INSERT INTO pending_links (binding_h, kind, user_id, session_h, provider, subject, email, email_verified, \
             handle, from_user_id, return_to, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            binding_h,
            kind.as_str(),
            user.0,
            session_h,
            proven.provider.as_str(),
            proven.subject.clone(),
            email,
            proven.email_verified,
            proven.handle.clone(),
            from.map(|f| f.0),
            return_to,
            now,
            now + cfg.policy.flow_ttl_s.value * 1000
        ),
    ];
    match host.batch(stmts).await {
        Ok(_) => Ok(set_cookie),
        Err(DbError::Guard) => Err(RefusalCode::LinkUnavailable),
        Err(e) => Err(db(e)),
    }
}
