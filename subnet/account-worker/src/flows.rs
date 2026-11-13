//! OAuth sign-in: `GET /auth/<p>/start` and `GET /auth/<p>/callback` for
//! Google, GitHub and X, and the X avatar.
//!
//! **Start.** A configured, switched-on method writes a flow row (`state`
//! hash, the flow cookie's binding, the provider, the intent, the PKCE
//! verifier, Google's nonce, the sanitized `return_to`, a `flow_ttl`
//! expiry) and answers 302 to the provider's authorize endpoint with S256.
//! `link` and `step_up` first need a session (and `link` a fresh step-up);
//! without one nothing is written.
//!
//! **Callback.** The row is taken by `state` (`DELETE … RETURNING`, so a
//! replay finds nothing) and checked against the flow cookie and the path's
//! provider. The code is exchanged at once, in this request: X's code must
//! be redeemed within 30 seconds of approval, so there is no queue, and
//! every provider call has its own platform-enforced budget
//! ([`PROVIDER_CALL_MS`]). Tokens are used once and dropped. Then the proven
//! identity goes through [`crate::signin::apply`].
//!
//! **X's avatar** (X requires the handle, the avatar and the X logo after
//! an X sign-in). Fetched once per sign-in, only from exactly
//! `https://pbs.twimg.com/…`, with no redirect followed and the body cut at
//! 64 KiB; its type comes from the bytes. Each stored fetch draws a new
//! random reference, the only id the avatar route takes. Anything else
//! stores nothing and drops the old image.

use slop_ninja_account_wire::{Intent, Provider};
use slop_ninja_auth_core::oidc;
use slop_ninja_auth_core::pkce::Verifier;
use slop_ninja_auth_core::providers::{
    self, AVATAR_MAX, AuthorizeParams, ExchangeParams, HttpRequest, OAuthProvider, PROFILE_BODY_MAX, ProviderError,
    TOKEN_BODY_MAX,
};
use slop_ninja_auth_core::return_to::{ReturnTo, sanitize_or_default};
use slop_ninja_auth_core::state::{CallbackQuery, Flow, FlowFacts as StateFacts, check_flow};
use slop_ninja_auth_core::token::Token;
use slop_ninja_auth_core::{cookie, email_addr::EmailAddr};
use slop_ninja_detect_page::{RefusalCode, UserId};

use crate::config::Config;
use crate::host::{Host, NetError, OutRequest};
use crate::http::{Req, Resp};
use crate::sessions;
use crate::signin::{self, Done, FlowFacts, Proven};
use crate::store::{self, Switches};

/// Every provider call's budget, ms (the platform aborts the call and its
/// body read). Three calls at most fit X's 30-second code life.
pub const PROVIDER_CALL_MS: u64 = 9_000;
/// The avatar fetch's budget, ms.
pub const AVATAR_MS: u64 = 5_000;

const SIGNIN: &str = "/signin/";

fn refuse(cfg: &Config, code: RefusalCode) -> Resp {
    Resp::form_error(&cfg.origins.site, SIGNIN, code)
}

/// `GET /auth/<p>/start?return_to=&intent=`.
pub async fn start<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches, p: OAuthProvider) -> Resp {
    let method = p.method().as_str();
    let Some(client) = cfg.client(p) else { return refuse(cfg, RefusalCode::MethodOff) };
    if !sw.method_on(p.method()) {
        return refuse(cfg, RefusalCode::MethodOff);
    }
    let q = req.query_pairs().unwrap_or_default();
    let intent = match q.get("intent") {
        None => Intent::Signin,
        Some(i) => match Intent::parse(i) {
            Some(i) => i,
            None => return refuse(cfg, RefusalCode::State),
        },
    };
    let now = host.now_ms();
    let (user_id, session_h) = if intent.is_signed_in() {
        let Ok(Some(s)) = sessions::current(host, req).await else { return refuse(cfg, RefusalCode::Session) };
        if intent == Intent::Link {
            if sw.read_only() {
                return Resp::form_error(&cfg.origins.site, "/account/", RefusalCode::AccountsReadOnly);
            }
            if !s.stepped_up(cfg, now) {
                return Resp::redirect(303, &cfg.origins.site, "/signin/?step_up=1&return_to=%2Faccount%2F");
            }
        }
        (Some(s.user_id), Some(s.token_h))
    } else {
        (None, None)
    };
    let return_to = sanitize_or_default(
        q.get("return_to").map(String::as_str),
        &cfg.origins.site,
        &cfg.policy,
        intent.is_signed_in(),
    );
    // The browser's flow binding: the one it has, or a new one.
    let binding = cookie::token(req.cookie(), cookie::FLOW).unwrap_or_else(|| Token::from_random(host.random()));
    let state = Token::from_random(host.random());
    let verifier = Verifier::from_random(host.random());
    let nonce = p.spec().sends_nonce.then(|| Token::from_random(host.random()).encode());
    let stmts = vec![
        store::prune("auth_flows", now, cfg.policy.flow_sweep.value.per_insert),
        crate::stmt!(
            "INSERT INTO auth_flows (state_h, binding_h, provider, intent, user_id, session_h, pkce_verifier, nonce, \
             return_to, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            state.hash(),
            binding.hash(),
            p.as_str(),
            intent.as_str(),
            user_id.map(|u| u.0),
            session_h,
            verifier.as_str(),
            nonce.clone(),
            return_to.as_str(),
            now,
            now + cfg.policy.flow_ttl_s.value * 1000
        ),
    ];
    if host.batch(stmts).await.is_err() {
        crate::metrics::error(host, "d1");
        return refuse(cfg, RefusalCode::Internal);
    }
    let redirect_uri = p.redirect_uri(&cfg.origins.site);
    let url = providers::authorize_url(
        p.spec(),
        &cfg.endpoints(p),
        &AuthorizeParams {
            client_id: &client.id,
            redirect_uri: &redirect_uri,
            state: &state.encode(),
            nonce: nonce.as_deref().unwrap_or(""),
            code_challenge: &verifier.challenge(),
        },
    );
    crate::metrics::auth(host, method, "start", None);
    Resp::redirect_to(url).with_cookie(cookie::set(cookie::FLOW, &binding, cfg.policy.flow_ttl_s.value))
}

/// The flow row a callback took.
struct Taken {
    facts: StateFacts,
    flow: FlowFacts,
    verifier: String,
    nonce: Option<String>,
}

/// `GET /auth/<p>/callback?code=&state=`.
pub async fn callback<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches, p: OAuthProvider) -> Resp {
    let method = p.method().as_str();
    match run_callback(cfg, host, req, sw, p).await {
        Ok(resp) => {
            crate::metrics::auth(host, method, "redeemed", None);
            resp
        }
        Err(code) => {
            crate::metrics::auth(host, method, "failed", Some(code));
            refuse(cfg, code)
        }
    }
}

async fn run_callback<H: Host>(
    cfg: &Config,
    host: &H,
    req: &Req,
    sw: &Switches,
    p: OAuthProvider,
) -> Result<Resp, RefusalCode> {
    let query = CallbackQuery::parse(&req.query).map_err(|r| r.code(Flow::OAuth))?;
    let row = host
        .first(crate::stmt!(
            "DELETE FROM auth_flows WHERE state_h = ? RETURNING binding_h, provider, intent, user_id, session_h, \
             pkce_verifier, nonce, return_to, expires_at",
            query.state.hash()
        ))
        .await
        .map_err(|_| RefusalCode::Internal)?;
    let taken = row.and_then(|r| {
        let intent = Intent::parse(r.text("intent")?)?;
        Some(Taken {
            facts: StateFacts {
                provider: OAuthProvider::parse(r.text("provider")?)?,
                binding_h: r.b32("binding_h")?,
                expires_at_ms: r.u64("expires_at")?,
            },
            flow: FlowFacts {
                intent,
                user_id: r.b16("user_id").map(UserId),
                session_h: r.b32("session_h"),
                return_to: ReturnTo::from_stored(
                    r.text("return_to")?,
                    &cfg.origins.site,
                    &cfg.policy,
                    intent.is_signed_in(),
                )
                .as_str()
                .to_owned(),
            },
            verifier: r.text("pkce_verifier")?.to_owned(),
            nonce: r.text("nonce").map(str::to_owned),
        })
    });
    let now = host.now_ms();
    check_flow(taken.as_ref().map(|t| &t.facts), cookie::get(req.cookie(), cookie::FLOW), p, now)
        .map_err(|r| r.code(Flow::OAuth))?;
    let taken = taken.ok_or(RefusalCode::State)?;
    let code = query.outcome.map_err(|d| d.code())?;
    if !sw.method_on(p.method()) {
        return Err(RefusalCode::MethodOff);
    }
    let client = cfg.client(p).ok_or(RefusalCode::MethodOff)?;
    let (proven, avatar_url) = exchange(cfg, host, p, client, &code, &taken).await.inspect_err(|e| {
        if p == OAuthProvider::X && *e == RefusalCode::MethodOff {
            crate::metrics::alert(host, slop_ninja_detect_page::ae::account::X_REFUSED);
        }
    })?;
    let done = signin::apply(cfg, host, req, sw, &proven, &taken.flow, RefusalCode::Session).await?;
    Ok(match done {
        Done::Session { set_cookies, next, identity_id, .. } => {
            if p == OAuthProvider::X && taken.flow.intent == Intent::Signin {
                store_avatar(cfg, host, identity_id, avatar_url.as_deref()).await;
            }
            let next = ReturnTo::from_stored(&next, &cfg.origins.site, &cfg.policy, taken.flow.intent.is_signed_in());
            Resp::redirect(303, &cfg.origins.site, next.as_str()).with_cookies(set_cookies)
        }
        Done::Pending { set_cookie } => {
            Resp::redirect(303, &cfg.origins.site, slop_ninja_account_wire::LINK_CONFIRM_PATH).with_cookie(set_cookie)
        }
        Done::EmailInUse { set_cookie } => {
            Resp::form_error(&cfg.origins.site, SIGNIN, RefusalCode::EmailInUse).with_cookie(set_cookie)
        }
        Done::AlreadyLinked => {
            Resp::redirect(303, &cfg.origins.site, "/account/").with_cookie(cookie::clear(cookie::FLOW))
        }
    })
}

fn out(req: HttpRequest, max_body: usize) -> OutRequest {
    OutRequest {
        method: req.method,
        url: req.url,
        headers: req.headers,
        body: req.body,
        timeout_ms: PROVIDER_CALL_MS,
        max_body,
    }
}

/// One provider call: its status and body, or the refusal.
async fn call<H: Host>(host: &H, req: HttpRequest, max: usize) -> Result<(u16, Vec<u8>), RefusalCode> {
    match host.http(&out(req, max)).await {
        Ok(r) => match r.body {
            Some(b) => Ok((r.status, b)),
            None => Err(ProviderError::TooLarge.code()),
        },
        Err(NetError::Timeout | NetError::Failed) => Err(RefusalCode::Provider),
    }
}

fn norm(e: &Option<EmailAddr>) -> Option<String> {
    e.as_ref().map(|a| a.norm.as_str().to_owned())
}

/// The code exchange and profile reads: the proven identity, and X's
/// `profile_image_url`.
async fn exchange<H: Host>(
    cfg: &Config,
    host: &H,
    p: OAuthProvider,
    client: &crate::config::Client,
    code: &str,
    taken: &Taken,
) -> Result<(Proven, Option<String>), RefusalCode> {
    let spec = p.spec();
    let endpoints = cfg.endpoints(p);
    let redirect_uri = p.redirect_uri(&cfg.origins.site);
    let token_req = providers::token_request(
        spec,
        &endpoints,
        &ExchangeParams {
            client_id: &client.id,
            client_secret: &client.secret,
            code,
            redirect_uri: &redirect_uri,
            code_verifier: &taken.verifier,
        },
    );
    let (status, body) = call(host, token_req, TOKEN_BODY_MAX).await?;
    let token = providers::parse_token_response(spec, status, &body).map_err(|e| e.code())?;
    match p {
        OAuthProvider::Google => {
            let id_token = token.id_token.as_deref().ok_or(RefusalCode::Provider)?;
            let nonce = taken.nonce.as_deref().ok_or(RefusalCode::State)?;
            let id = oidc::check(
                id_token,
                &oidc::Expect {
                    issuers: &providers::GOOGLE_ISSUERS,
                    client_id: &client.id,
                    nonce,
                    now_s: host.now_ms() / 1000,
                },
            )
            .map_err(|e| e.code())?;
            Ok((
                Proven {
                    provider: Provider::Google,
                    subject: id.sub,
                    email: norm(&id.verified_email),
                    email_verified: id.verified_email.is_some(),
                    handle: None,
                    link_email: id.verified_email.map(|e| e.norm),
                },
                None,
            ))
        }
        OAuthProvider::GitHub => {
            let user_url = endpoints.user.as_deref().ok_or(RefusalCode::Provider)?;
            let (s, b) = call(host, providers::api_get(spec, user_url, &token.access_token), PROFILE_BODY_MAX).await?;
            let user = providers::parse_github_user(s, &b).map_err(|e| e.code())?;
            let emails_url = endpoints.emails.as_deref().ok_or(RefusalCode::Provider)?;
            let (s, b) =
                call(host, providers::api_get(spec, emails_url, &token.access_token), PROFILE_BODY_MAX).await?;
            let email = providers::parse_github_emails(s, &b).map_err(|e| e.code())?;
            Ok((
                Proven {
                    provider: Provider::Github,
                    subject: user.id.to_string(),
                    email: norm(&email),
                    email_verified: email.is_some(),
                    handle: Some(user.login),
                    link_email: email.map(|e| e.norm),
                },
                None,
            ))
        }
        OAuthProvider::X => {
            let me_url = endpoints.user.as_deref().ok_or(RefusalCode::Provider)?;
            let (s, b) = call(host, providers::api_get(spec, me_url, &token.access_token), PROFILE_BODY_MAX).await?;
            let me = providers::parse_x_me(s, &b).map_err(|e| e.code())?;
            Ok((
                Proven {
                    provider: Provider::X,
                    subject: me.id,
                    email: norm(&me.confirmed_email),
                    email_verified: me.confirmed_email.is_some(),
                    handle: Some(me.username),
                    // X never links by email.
                    link_email: None,
                },
                me.profile_image_url,
            ))
        }
    }
}

/// The URL the avatar is fetched from: X's own, or in a dev build the same
/// path under the auth stub.
fn avatar_fetch_url(cfg: &Config, u: &url::Url) -> String {
    #[cfg(feature = "dev")]
    if let Some(stub) = &cfg.stub_origin {
        let rest = &u.as_str()["https://".len()..];
        return format!("{}/{rest}", stub.as_str());
    }
    let _ = cfg;
    u.as_str().to_owned()
}

/// Fetches and stores X's avatar for `identity_id`, or drops the stored one.
pub async fn store_avatar<H: Host>(cfg: &Config, host: &H, identity_id: i64, profile_image_url: Option<&str>) {
    let mut fetched = None;
    if let Some(u) = profile_image_url.and_then(providers::x_avatar_url) {
        let req = OutRequest {
            method: "GET",
            url: avatar_fetch_url(cfg, &u),
            headers: vec![("User-Agent", providers::USER_AGENT.to_owned()), ("Accept", "image/*".to_owned())],
            body: String::new(),
            timeout_ms: AVATAR_MS,
            max_body: AVATAR_MAX,
        };
        if let Ok(r) = host.http(&req).await
            && r.status == 200
            && let Some(bytes) = r.body
            && let Some(ty) = providers::sniff_avatar(&bytes)
        {
            fetched = Some((ty, bytes));
        }
    }
    let stmt = match fetched {
        Some((ty, bytes)) => {
            let r: [u8; 16] = host.random();
            crate::stmt!(
                "INSERT INTO avatars (identity_id, ref, content_type, bytes, fetched_at) VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT (identity_id) DO UPDATE SET ref = excluded.ref, content_type = excluded.content_type, \
                 bytes = excluded.bytes, fetched_at = excluded.fetched_at",
                identity_id,
                r,
                ty.as_str(),
                bytes,
                host.now_ms()
            )
        }
        None => crate::stmt!("DELETE FROM avatars WHERE identity_id = ?", identity_id),
    };
    if host.all(stmt).await.is_err() {
        crate::metrics::error(host, "d1");
    }
}

/// `GET /api/account/v1/avatar/<ref>`: the session's own identity's image,
/// else a 404 identical to an unknown reference.
pub async fn avatar<H: Host>(host: &H, req: &Req, reference: &str) -> Resp {
    let not_found = || Resp::refusal(RefusalCode::NotFound);
    let Some(r) = crate::hex16(reference) else { return not_found() };
    let Ok(Some(s)) = sessions::current(host, req).await else { return not_found() };
    let row = host
        .first(crate::stmt!(
            "SELECT a.content_type, a.bytes FROM avatars a JOIN identities i ON i.identity_id = a.identity_id \
             WHERE a.ref = ? AND i.user_id = ?",
            r,
            s.user_id.0
        ))
        .await;
    let Ok(Some(row)) = row else { return not_found() };
    let (Some(ct), Some(bytes)) = (row.text("content_type"), row.blob("bytes")) else { return not_found() };
    let Some(ty) = slop_ninja_account_wire::AvatarType::parse(ct) else { return not_found() };
    Resp { status: 200, headers: Vec::new(), body: bytes.to_vec() }
        .with_header("content-type", ty.as_str())
        .with_header("cache-control", "private, max-age=3600")
        .with_header("x-content-type-options", "nosniff")
        .with_header("content-security-policy", "default-src 'none'; sandbox")
        .with_header("cross-origin-resource-policy", "same-origin")
        .with_header("referrer-policy", "no-referrer")
}

#[cfg(test)]
mod tests {
    #[test]
    fn three_provider_calls_fit_the_code_life() {
        const { assert!(3 * super::PROVIDER_CALL_MS < 30_000) };
    }
}
