//! The contract between Workers: the `X-SN-*` headers, the forwarded route
//! ids and the internal routes.
//!
//! Every outside request enters through the website or the secret-host
//! Worker. Both strip every incoming header whose name begins `x-sn-`
//! ([`is_sn_header`]) and add:
//! - `X-SN-Host`: the authority of the configured origin the request arrived
//!   on ([`crate::Origins::sn_host`]);
//! - `X-SN-Route`: the [`RouteId`] of the forwarder's allow-list entry.
//!
//! On the few routes that act for an account ([`RouteId::takes_principal`])
//! the website first asks the account Worker for the session and forwards it
//! as `X-SN-Principal: v1;<user_id hex32>;<auth_time ms>;<flags hex>`
//! ([`Principal`]), or omits the header for an anonymous request. The secret
//! host never sets it.
//!
//! Internal routes ([`InternalRoute`]) are binding-only. A callee refuses any
//! internal request that carries `X-SN-Route` or `X-SN-Host`
//! ([`internal_request_allowed`]): only the forwarders set those headers, and
//! they never forward an `/internal/` path, so a forwarded request can never
//! reach an internal route.

use crate::codes::HostRole;
use crate::ids::UserId;

/// The prefix the forwarders strip from every incoming header.
pub const SN_PREFIX: &str = "x-sn-";
pub const X_SN_HOST: &str = "x-sn-host";
pub const X_SN_ROUTE: &str = "x-sn-route";
pub const X_SN_PRINCIPAL: &str = "x-sn-principal";

/// Whether a header name (any case) is an `x-sn-` header.
pub fn is_sn_header(name: &str) -> bool {
    name.len() >= SN_PREFIX.len() && name.as_bytes()[..SN_PREFIX.len()].eq_ignore_ascii_case(SN_PREFIX.as_bytes())
}

/// Whether an internal request may proceed: it carries neither `X-SN-Route`
/// nor `X-SN-Host`.
pub fn internal_request_allowed(has_sn_route: bool, has_sn_host: bool) -> bool {
    !has_sn_route && !has_sn_host
}

/// The Worker a route reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callee {
    Detect,
    Account,
}

/// The hosts a forwarded route may arrive on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hosts {
    Site,
    Secret,
    Both,
}

impl Hosts {
    pub fn admits(self, role: HostRole) -> bool {
        matches!((self, role), (Hosts::Both, _) | (Hosts::Site, HostRole::Site) | (Hosts::Secret, HostRole::Secret))
    }
}

/// One forwarded route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteSpec {
    pub id: RouteId,
    pub method: &'static str,
    /// The path the callee serves (`<p>` stands for a provider, `<ref>` for
    /// an avatar reference, `<processor>` for a payment processor).
    pub path: &'static str,
    pub callee: Callee,
    pub hosts: Hosts,
    /// The website forwards `X-SN-Principal` (with `X-SN-Host` the site).
    pub principal: bool,
    /// `auth-core::csrf::check` applies: every forwarded browser POST,
    /// never the signed op routes, the paid API or webhooks.
    pub csrf: bool,
}

macro_rules! routes {
    ($($var:ident = $s:literal, $method:literal, $path:literal, $callee:ident, $hosts:ident, $principal:literal, $csrf:literal;)+) => {
        /// A forwarded route id, the value of `X-SN-Route`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum RouteId { $($var,)+ }

        impl RouteId {
            pub const ALL: &'static [RouteId] = &[$(RouteId::$var,)+];

            pub const fn as_str(self) -> &'static str {
                match self { $(RouteId::$var => $s,)+ }
            }

            pub fn parse(s: &str) -> Option<RouteId> {
                match s { $($s => Some(RouteId::$var),)+ _ => None }
            }

            /// The route's table row.
            pub const fn spec(self) -> RouteSpec {
                match self {
                    $(RouteId::$var => RouteSpec {
                        id: RouteId::$var,
                        method: $method,
                        path: $path,
                        callee: Callee::$callee,
                        hosts: Hosts::$hosts,
                        principal: $principal,
                        csrf: $csrf,
                    },)+
                }
            }
        }
    };
}

routes! {
    // detect
    Score = "score", "POST", "/api/detect/v1/score", Detect, Site, false, true;
    Warm = "warm", "POST", "/api/detect/v1/warm", Detect, Site, false, true;
    Share = "share", "POST", "/api/detect/v1/share", Detect, Site, true, true;
    Url = "url", "POST", "/api/detect/v1/url", Detect, Site, false, true;
    Rerun = "rerun", "POST", "/api/detect/v1/rerun", Detect, Site, false, true;
    State = "state", "POST", "/api/detect/v1/state", Detect, Site, false, true;
    Feedback = "feedback", "POST", "/api/detect/v1/feedback", Detect, Both, false, true;
    PaidQuote = "paid_quote", "POST", "/api/detect/v1/paid/quote", Detect, Site, true, true;
    PaidStart = "paid_start", "POST", "/api/detect/v1/paid/start", Detect, Site, true, true;
    Removal = "removal", "POST", "/api/detect/v1/removal", Detect, Site, true, true;
    RemovalStatus = "removal_status", "POST", "/api/detect/v1/removal/status", Detect, Both, false, true;
    Manage = "manage", "POST", "/api/detect/v1/manage", Detect, Secret, false, true;
    Job = "job", "POST", "/api/detect/v1/job", Detect, Secret, false, true;
    DetectOp = "op", "POST", "/api/detect/v1/op", Detect, Site, false, false;
    PaidApi = "paid_api", "POST", "/api/v1/detect", Detect, Site, false, false;
    // account
    EmailStart = "email_start", "POST", "/api/account/v1/email/start", Account, Site, false, true;
    EmailPeek = "email_peek", "POST", "/api/account/v1/email/peek", Account, Site, false, true;
    EmailConfirm = "email_confirm", "POST", "/api/account/v1/email/confirm", Account, Site, false, true;
    WalletChallenge = "wallet_challenge", "POST", "/api/account/v1/wallet/challenge", Account, Site, false, true;
    WalletVerify = "wallet_verify", "POST", "/api/account/v1/wallet/verify", Account, Site, false, true;
    Me = "me", "GET", "/api/account/v1/me", Account, Site, false, false;
    Avatar = "avatar", "GET", "/api/account/v1/avatar/<ref>", Account, Site, false, false;
    AccountOp = "account_op", "POST", "/api/account/v1/op", Account, Site, false, false;
    AuthStart = "auth_start", "GET", "/auth/<p>/start", Account, Site, false, false;
    AuthCallback = "auth_callback", "GET", "/auth/<p>/callback", Account, Site, false, false;
    Logout = "logout", "POST", "/auth/logout", Account, Site, false, true;
    SessionsRevoke = "sessions_revoke", "POST", "/account/sessions/revoke", Account, Site, false, true;
    SessionsRevokeAll = "sessions_revoke_all", "POST", "/account/sessions/revoke-all", Account, Site, false, true;
    // Connect, merge or cancel: a merge runs only here, through a pending
    // row of kind `merge` (there is no separate merge route).
    LinkConfirm = "link_confirm", "POST", "/account/link/confirm", Account, Site, false, true;
    Unlink = "unlink", "POST", "/account/unlink", Account, Site, false, true;
    Export = "export", "POST", "/account/export", Account, Site, false, true;
    DeleteAccount = "delete_account", "POST", "/account/delete", Account, Site, false, true;
    BillingWebhook = "billing_webhook", "POST", "/api/billing/v1/webhook/<processor>", Account, Site, false, false;
}

impl RouteId {
    /// Whether the detect Worker accepts `X-SN-Principal` on this route.
    pub fn takes_principal(self) -> bool {
        self.spec().principal
    }
}

/// A binding-only route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InternalRoute {
    // detect
    Page,
    Preview,
    Card,
    States,
    Owner,
    Examples,
    Example,
    Sitemap,
    Removals,
    // account
    Session,
    View,
    OwnedAdd,
    OwnedGet,
    OwnedRemove,
    OwnedPrune,
    /// One page of the account ids (the operator's `owner_match`).
    Users,
    BillingPeek,
    BillingHold,
    BillingCommit,
    BillingRelease,
}

impl InternalRoute {
    pub const ALL: &'static [InternalRoute] = &[
        InternalRoute::Page,
        InternalRoute::Preview,
        InternalRoute::Card,
        InternalRoute::States,
        InternalRoute::Owner,
        InternalRoute::Examples,
        InternalRoute::Example,
        InternalRoute::Sitemap,
        InternalRoute::Removals,
        InternalRoute::Session,
        InternalRoute::View,
        InternalRoute::OwnedAdd,
        InternalRoute::OwnedGet,
        InternalRoute::OwnedRemove,
        InternalRoute::OwnedPrune,
        InternalRoute::Users,
        InternalRoute::BillingPeek,
        InternalRoute::BillingHold,
        InternalRoute::BillingCommit,
        InternalRoute::BillingRelease,
    ];

    /// `(method, path)`; the sitemap shard path ends in `<n>`.
    pub const fn method_path(self) -> (&'static str, &'static str) {
        match self {
            InternalRoute::Page => ("POST", "/internal/v1/page"),
            InternalRoute::Preview => ("POST", "/internal/v1/preview"),
            InternalRoute::Card => ("POST", "/internal/v1/card"),
            InternalRoute::States => ("POST", "/internal/v1/states"),
            InternalRoute::Owner => ("POST", "/internal/v1/owner"),
            InternalRoute::Examples => ("GET", "/internal/v1/examples"),
            InternalRoute::Example => ("POST", "/internal/v1/example"),
            InternalRoute::Sitemap => ("GET", "/internal/v1/sitemap/<n>"),
            InternalRoute::Removals => ("GET", "/internal/v1/removals"),
            InternalRoute::Session => ("POST", "/internal/v1/session"),
            InternalRoute::View => ("POST", "/internal/v1/view"),
            InternalRoute::OwnedAdd => ("POST", "/internal/v1/owned/add"),
            InternalRoute::OwnedGet => ("POST", "/internal/v1/owned/get"),
            InternalRoute::OwnedRemove => ("POST", "/internal/v1/owned/remove"),
            InternalRoute::OwnedPrune => ("POST", "/internal/v1/owned/prune"),
            InternalRoute::Users => ("POST", "/internal/v1/users"),
            InternalRoute::BillingPeek => ("POST", "/internal/v1/billing/peek"),
            InternalRoute::BillingHold => ("POST", "/internal/v1/billing/hold"),
            InternalRoute::BillingCommit => ("POST", "/internal/v1/billing/commit"),
            InternalRoute::BillingRelease => ("POST", "/internal/v1/billing/release"),
        }
    }

    pub const fn callee(self) -> Callee {
        match self {
            InternalRoute::Page
            | InternalRoute::Preview
            | InternalRoute::Card
            | InternalRoute::States
            | InternalRoute::Owner
            | InternalRoute::Examples
            | InternalRoute::Example
            | InternalRoute::Sitemap
            | InternalRoute::Removals => Callee::Detect,
            _ => Callee::Account,
        }
    }

    /// The only internal route that takes `X-SN-Principal`.
    pub fn takes_principal(self) -> bool {
        self == InternalRoute::Owner
    }
}

/// `X-SN-Principal` flag bits (the user's flags, set by operator ops).
pub mod flags {
    /// A paid-path tester (`set_flag {paid_preview}`): sees paid checks while
    /// `DETECT_PAID` is off for the public.
    pub const PAID_PREVIEW: u32 = 1;
}

/// The signed-in account a forwarded request acts for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub user_id: UserId,
    /// When the session last proved a sign-in method (ms).
    pub auth_time_ms: u64,
    /// [`flags`] bits.
    pub flags: u32,
}

/// The header's version tag.
pub const PRINCIPAL_V: &str = "v1";

impl Principal {
    /// `v1;<user_id hex32>;<auth_time ms>;<flags hex>`.
    pub fn encode(&self) -> String {
        format!("{PRINCIPAL_V};{};{};{:x}", self.user_id.hex(), self.auth_time_ms, self.flags)
    }

    /// The exact form [`Principal::encode`] writes: decimal without leading
    /// zeros, lowercase hex flags of 1 to 8 digits without leading zeros.
    pub fn decode(s: &str) -> Option<Principal> {
        let mut parts = s.split(';');
        let (v, u, t, f) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || v != PRINCIPAL_V {
            return None;
        }
        let user_id = UserId::parse(u)?;
        let dec_ok = !t.is_empty()
            && t.len() <= 20
            && t.bytes().all(|b| b.is_ascii_digit())
            && (t == "0" || !t.starts_with('0'));
        if !dec_ok {
            return None;
        }
        let auth_time_ms = t.parse().ok()?;
        let hex_ok = !f.is_empty()
            && f.len() <= 8
            && f.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && (f == "0" || !f.starts_with('0'));
        if !hex_ok {
            return None;
        }
        let flags = u32::from_str_radix(f, 16).ok()?;
        Some(Principal { user_id, auth_time_ms, flags })
    }

    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag == flag
    }
}
