//! The Worker's one entry: routes a request to its handler.
//!
//! - **Internal routes** (`/internal/v1/*`, binding calls from the website
//!   and the detect Worker): refused when they carry `X-SN-Route` or
//!   `X-SN-Host`, which only the public forwarders set. No CSRF check
//!   applies to them (they are not browser requests).
//! - **Forwarded routes**: the website forwards each with `X-SN-Route` (a
//!   route of the account contract's table) and `X-SN-Host` (the site
//!   host's authority, from config). The path and method must be the
//!   route's; the CSRF rule (`auth-core::csrf`, Fetch Metadata plus
//!   `Origin` against the configured site origin) runs first on every route
//!   whose table row marks it, then the content type.
//! - Anything else is a 404.

use slop_ninja_auth_core::csrf::{self, BodyType};
use slop_ninja_auth_core::providers::OAuthProvider;
use slop_ninja_detect_page::sn::{self, Callee, InternalRoute};
use slop_ninja_detect_page::{HostRole, RefusalCode, RouteId};

use crate::config::{Config, ConfigError};
use crate::host::Host;
use crate::http::{Req, Resp};
use crate::store::Switches;

/// Which arena footprint a request takes (`memory.rs`): read in JS from the
/// path before any Rust allocation, so this mirrors that table.
pub fn is_list_path(path: &str) -> bool {
    matches!(path, "/internal/v1/view" | "/account/export" | "/api/account/v1/op")
}

/// Matches `path` against a route pattern; `Some(param)` on a match, with
/// the `<…>` segment when the pattern has one.
pub fn match_path<'a>(pattern: &str, path: &'a str) -> Option<Option<&'a str>> {
    let Some((head, rest)) = pattern.split_once('<') else {
        return (pattern == path).then_some(None);
    };
    let tail = rest.split_once('>').map_or("", |(_, t)| t);
    let mid = path.strip_prefix(head)?.strip_suffix(tail)?;
    (!mid.is_empty() && !mid.contains('/')).then_some(Some(mid))
}

fn internal_route(method: &str, path: &str) -> Option<InternalRoute> {
    InternalRoute::ALL
        .iter()
        .copied()
        .filter(|r| r.callee() == Callee::Account)
        .find(|r| r.method_path() == (method, path))
}

fn body_ok(req: &Req, want: BodyType) -> bool {
    csrf::content_type_ok(req.header("content-type"), want)
}

/// Handles one request.
pub async fn handle<H: Host>(cfg: &Result<Config, ConfigError>, host: &H, req: &Req) -> Resp {
    let Ok(cfg) = cfg else {
        crate::metrics::error(host, "config");
        return Resp::refusal(RefusalCode::NotConfigured);
    };
    if req.path.starts_with("/internal/") {
        return internal(cfg, host, req).await;
    }
    forwarded(cfg, host, req).await
}

async fn internal<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    if !sn::internal_request_allowed(req.has_header(sn::X_SN_ROUTE), req.has_header(sn::X_SN_HOST)) {
        return Resp::refusal(RefusalCode::NotFound);
    }
    let Some(route) = internal_route(&req.method, &req.path) else { return Resp::refusal(RefusalCode::NotFound) };
    if route != InternalRoute::Session && !body_ok(req, BodyType::Json) {
        return Resp::refusal(csrf::CONTENT_TYPE_REFUSAL);
    }
    let sw = Switches::load(cfg, host).await;
    match route {
        InternalRoute::Session => crate::views::session(cfg, host, req, &sw).await,
        InternalRoute::View => crate::views::view(cfg, host, req, &sw).await,
        InternalRoute::OwnedAdd => crate::owned::add(cfg, host, req, &sw).await,
        InternalRoute::OwnedGet => crate::owned::get(cfg, host, req).await,
        InternalRoute::OwnedRemove => crate::owned::remove(host, req).await,
        InternalRoute::OwnedPrune => crate::owned::prune(host, req).await,
        InternalRoute::BillingPeek => crate::ledger::peek(cfg, host, req, &sw).await,
        InternalRoute::BillingHold => crate::ledger::hold(host, req, &sw).await,
        InternalRoute::BillingCommit => crate::ledger::commit(host, req).await,
        InternalRoute::BillingRelease => crate::ledger::release(host, req).await,
        InternalRoute::BillingExpire => crate::ledger::expire(host, req).await,
        InternalRoute::Users => crate::users::users(host, req).await,
        // The detect Worker's internal routes never parse as this Worker's.
        _ => Resp::refusal(RefusalCode::NotFound),
    }
}

async fn forwarded<H: Host>(cfg: &Config, host: &H, req: &Req) -> Resp {
    let not_found = || Resp::refusal(RefusalCode::NotFound);
    let Some(route) = req.header(sn::X_SN_ROUTE).and_then(RouteId::parse) else { return not_found() };
    let spec = route.spec();
    if spec.callee != Callee::Account {
        return not_found();
    }
    match req.header(sn::X_SN_HOST).and_then(|h| cfg.origins.role_of_sn_host(h)) {
        Some(role) if spec.hosts.admits(role) && role == HostRole::Site => {}
        _ => return not_found(),
    }
    let Some(param) = match_path(spec.path, &req.path) else { return not_found() };
    if req.method != spec.method {
        return Resp::refusal(RefusalCode::Method).with_header("allow", spec.method);
    }
    if let Err(r) = csrf::check_route(
        route,
        HostRole::Site,
        &cfg.origins,
        &req.method,
        req.header("sec-fetch-site"),
        req.header("origin"),
    ) {
        return Resp::refusal(r.code());
    }
    use RouteId as R;
    let form = matches!(
        route,
        R::Logout
            | R::SessionsRevoke
            | R::SessionsRevokeAll
            | R::LinkConfirm
            | R::Unlink
            | R::Export
            | R::DeleteAccount
    );
    let json = matches!(route, R::EmailPeek | R::EmailConfirm | R::WalletChallenge | R::WalletVerify | R::AccountOp);
    if (form && !body_ok(req, BodyType::Form)) || (json && !body_ok(req, BodyType::Json)) {
        return Resp::refusal(csrf::CONTENT_TYPE_REFUSAL);
    }
    if route == R::AccountOp {
        return crate::ops::op(cfg, host, req).await;
    }
    let sw = Switches::load(cfg, host).await;
    match route {
        R::EmailStart => crate::email::start(cfg, host, req, &sw).await,
        R::EmailPeek => crate::email::peek(host, req, &sw).await,
        R::EmailConfirm => crate::email::confirm(cfg, host, req, &sw).await,
        R::WalletChallenge => crate::wallet::challenge(cfg, host, req, &sw).await,
        R::WalletVerify => crate::wallet::verify(cfg, host, req, &sw).await,
        R::Me => crate::views::me(host, req, &sw).await,
        R::Avatar => crate::flows::avatar(host, req, param.unwrap_or("")).await,
        R::AuthStart | R::AuthCallback => {
            let Some(p) = param.and_then(OAuthProvider::parse) else { return not_found() };
            if route == R::AuthStart {
                crate::flows::start(cfg, host, req, &sw, p).await
            } else {
                crate::flows::callback(cfg, host, req, &sw, p).await
            }
        }
        R::Logout => crate::account::logout(cfg, host, req).await,
        R::SessionsRevoke => crate::account::revoke(cfg, host, req).await,
        R::SessionsRevokeAll => crate::account::revoke_all(cfg, host, req).await,
        R::LinkConfirm => crate::link::confirm(cfg, host, req, &sw).await,
        R::Unlink => crate::link::unlink(cfg, host, req).await,
        R::Export => crate::account::export(cfg, host, req).await,
        R::DeleteAccount => crate::account::delete_self(cfg, host, req).await,
        // The processor webhook comes with payments.
        R::BillingWebhook => Resp::refusal(RefusalCode::NotConfigured),
        _ => not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_take_one_segment() {
        assert_eq!(match_path("/auth/<p>/start", "/auth/google/start"), Some(Some("google")));
        assert_eq!(match_path("/auth/<p>/start", "/auth//start"), None);
        assert_eq!(match_path("/auth/<p>/start", "/auth/a/b/start"), None);
        assert_eq!(match_path("/api/account/v1/avatar/<ref>", "/api/account/v1/avatar/00ff"), Some(Some("00ff")));
        assert_eq!(match_path("/auth/logout", "/auth/logout"), Some(None));
        assert_eq!(match_path("/auth/logout", "/auth/logout/"), None);
    }

    #[test]
    fn every_account_route_has_a_handler_path() {
        for r in RouteId::ALL.iter().filter(|r| r.spec().callee == Callee::Account) {
            assert!(r.spec().path.starts_with('/'), "{r:?}");
        }
        assert!(internal_route("POST", "/internal/v1/session").is_some());
        assert!(internal_route("POST", "/internal/v1/page").is_none(), "a detect route is not ours");
    }
}
