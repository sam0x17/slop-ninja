//! Sessions: issue, check, renew, rotate and revoke.
//!
//! The cookie `__Host-sn_session` carries a 256-bit token; the store holds
//! only its SHA-256 (`sessions.token_h`). A session is live while both
//! `idle_expires_at` and `abs_expires_at` are ahead and its user is
//! `active`. Renewal (at most once a UTC day) keeps the token and extends
//! the idle limit; rotation (every sign-in, step-up and identity change)
//! replaces the token in the same row, so a flow that recorded the old hash
//! no longer matches it. No session row holds an address, and only the
//! browser and OS family of the user agent.

use slop_ninja_account_wire::{Provider, SessionAnswer};
use slop_ninja_auth_core::cookie;
use slop_ninja_auth_core::token::Token;
use slop_ninja_detect_page::UserId;

use crate::config::Config;
use crate::host::{DbError, Host, Row, Stmt, Val};
use crate::http::{Req, utc_day};

/// A live session, read through its cookie.
#[derive(Clone, Debug)]
pub struct Session {
    pub token: Token,
    pub token_h: [u8; 32],
    pub user_id: UserId,
    pub identity_id: i64,
    pub auth_time: u64,
    pub last_seen_day: String,
    pub idle_expires_at: u64,
    pub abs_expires_at: u64,
    pub flags: u32,
    /// The identity the session signed in with.
    pub method: Provider,
    pub label: String,
}

impl Session {
    /// Inside the step-up window (`now - auth_time <= step_up_window`).
    pub fn stepped_up(&self, cfg: &Config, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.auth_time) <= cfg.policy.step_up_window_s.value.saturating_mul(1000)
    }

    pub fn answer(&self, billing: slop_ninja_detect_page::BillingState) -> SessionAnswer {
        SessionAnswer {
            user_id: self.user_id,
            auth_time: self.auth_time,
            flags: self.flags,
            label: self.label.clone(),
            method: self.method,
            billing,
        }
    }
}

const SELECT: &str = "SELECT s.token_h, s.user_id, s.identity_id, s.auth_time, s.last_seen_day, s.idle_expires_at, \
     s.abs_expires_at, u.flags, i.provider, i.email, i.handle \
     FROM sessions s JOIN users u ON u.user_id = s.user_id JOIN identities i ON i.identity_id = s.identity_id \
     WHERE s.token_h = ? AND u.state = 'active' AND s.idle_expires_at > ? AND s.abs_expires_at > ? \
     AND i.user_id = s.user_id";

fn from_row(token: Token, r: &Row) -> Option<Session> {
    Some(Session {
        token,
        token_h: r.b32("token_h")?,
        user_id: UserId(r.b16("user_id")?),
        identity_id: r.int("identity_id")?,
        auth_time: r.u64("auth_time")?,
        last_seen_day: r.text("last_seen_day")?.to_owned(),
        idle_expires_at: r.u64("idle_expires_at")?,
        abs_expires_at: r.u64("abs_expires_at")?,
        flags: u32::try_from(r.int("flags")?).ok()?,
        method: Provider::parse(r.text("provider")?)?,
        label: crate::store::row_label(r),
    })
}

/// The request's live session, if any.
pub async fn current<H: Host>(host: &H, req: &Req) -> Result<Option<Session>, DbError> {
    let Some(token) = cookie::token(req.cookie(), cookie::SESSION) else { return Ok(None) };
    let now = host.now_ms();
    let row = host.first(crate::stmt!(SELECT, token.hash(), now, now)).await?;
    Ok(row.and_then(|r| from_row(token, &r)))
}

/// The cookie's `Max-Age`: the idle limit, never past the absolute expiry.
fn max_age_s(idle_expires_at: u64, abs_expires_at: u64, now_ms: u64) -> u64 {
    idle_expires_at.min(abs_expires_at).saturating_sub(now_ms) / 1000
}

/// A new session's token, its insert and its cookie.
pub struct Issued {
    pub token: Token,
    pub stmt: Stmt,
    pub set_cookie: String,
}

/// The identity a new session signs in with: a known row, or the one the
/// same batch has just inserted.
#[derive(Clone, Copy, Debug)]
pub enum IdentityRef<'a> {
    Id(i64),
    Subject(Provider, &'a str),
}

/// Issues a session for `user` signed in with `identity`.
pub fn issue<H: Host>(
    cfg: &Config,
    host: &H,
    user: &UserId,
    identity: IdentityRef<'_>,
    ua_family: Option<String>,
) -> Issued {
    let now = host.now_ms();
    let token = Token::from_random(host.random());
    let idle = now + cfg.policy.session_idle_s.value * 1000;
    let abs = now + cfg.policy.session_absolute_s.value * 1000;
    let mut args = vec![
        Val::from(token.hash()),
        Val::from(user.0),
        Val::from(now),
        Val::from(utc_day(now)),
        Val::from(idle.min(abs)),
        Val::from(abs),
        Val::from(now),
        Val::from(ua_family),
    ];
    let head = "INSERT INTO sessions (token_h, user_id, created_at, last_seen_day, idle_expires_at, abs_expires_at, \
         auth_time, ua_family, identity_id) SELECT ?, ?, ?, ?, ?, ?, ?, ?, ";
    let sql = match identity {
        IdentityRef::Id(id) => {
            args.push(Val::from(id));
            format!("{head}?")
        }
        IdentityRef::Subject(p, subject) => {
            args.push(Val::from(p.as_str()));
            args.push(Val::from(subject));
            args.push(Val::from(user.0));
            format!("{head}identity_id FROM identities WHERE provider = ? AND subject = ? AND user_id = ?")
        }
    };
    let set_cookie = cookie::set(cookie::SESSION, &token, max_age_s(idle, abs, now));
    Issued { token, stmt: Stmt::new(sql, args), set_cookie }
}

/// Rotates `old` to a new token with `auth_time = now` (step-up, a link
/// confirm): the same row, so its creation time and identity stay.
pub fn rotate<H: Host>(cfg: &Config, host: &H, old: &Session) -> Issued {
    let now = host.now_ms();
    let token = Token::from_random(host.random());
    let idle = (now + cfg.policy.session_idle_s.value * 1000).min(old.abs_expires_at);
    let stmt = crate::stmt!(
        "UPDATE sessions SET token_h = ?, auth_time = ?, last_seen_day = ?, idle_expires_at = ? WHERE token_h = ?",
        token.hash(),
        now,
        utc_day(now),
        idle,
        old.token_h
    );
    let set_cookie = cookie::set(cookie::SESSION, &token, max_age_s(idle, old.abs_expires_at, now));
    Issued { token, stmt, set_cookie }
}

/// Renews a session whose `last_seen_day` is not today: the same token, a
/// new idle limit and `Max-Age`. Answers the refreshed `Set-Cookie`.
pub async fn renew_if_due<H: Host>(cfg: &Config, host: &H, s: &Session) -> Option<String> {
    let now = host.now_ms();
    let today = utc_day(now);
    if s.last_seen_day == today {
        return None;
    }
    let idle = (now + cfg.policy.session_idle_s.value * 1000).min(s.abs_expires_at);
    let ok = host
        .all(crate::stmt!(
            "UPDATE sessions SET last_seen_day = ?, idle_expires_at = ? WHERE token_h = ?",
            today,
            idle,
            s.token_h
        ))
        .await
        .is_ok();
    if !ok {
        return None;
    }
    crate::metrics::session(host, "renewed");
    Some(cookie::set(cookie::SESSION, &s.token, max_age_s(idle, s.abs_expires_at, now)))
}

/// Deletes every session of `user`.
pub fn revoke_all(user: &UserId) -> Stmt {
    crate::stmt!("DELETE FROM sessions WHERE user_id = ?", user.0)
}

/// The `Set-Cookie` that signs a browser out.
pub fn clear_cookie() -> String {
    cookie::clear(cookie::SESSION)
}

/// The browser and OS family of a `User-Agent`, from a fixed list (never
/// the string itself).
pub fn ua_family(ua: Option<&str>) -> Option<String> {
    let ua = ua?;
    if ua.len() > 1024 {
        return None;
    }
    let browser = [
        ("Edg/", "Edge"),
        ("OPR/", "Opera"),
        ("Firefox/", "Firefox"),
        ("FxiOS/", "Firefox"),
        ("CriOS/", "Chrome"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map_or("Other browser", |(_, v)| v);
    let os = [
        ("iPhone", "iOS"),
        ("iPad", "iPadOS"),
        ("Android", "Android"),
        ("CrOS", "ChromeOS"),
        ("Windows", "Windows"),
        ("Mac OS X", "macOS"),
        ("Macintosh", "macOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map_or("other OS", |(_, v)| v);
    Some(format!("{browser} on {os}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agents_reduce_to_a_family() {
        let ff = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
        assert_eq!(ua_family(Some(ff)).as_deref(), Some("Firefox on Linux"));
        let safari =
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605 Version/17.0 Mobile Safari/604.1";
        assert_eq!(ua_family(Some(safari)).as_deref(), Some("Safari on iOS"));
        assert_eq!(ua_family(Some("curl/8")).as_deref(), Some("Other browser on other OS"));
        assert_eq!(ua_family(None), None);
    }

    #[test]
    fn the_cookie_never_outlives_the_absolute_limit() {
        assert_eq!(max_age_s(10_000, 5_000, 0), 5);
        assert_eq!(max_age_s(3_000, 5_000, 1_000), 2);
    }
}
