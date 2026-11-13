//! CSRF and origin rules: Fetch Metadata plus `Origin`, against origins from
//! config.
//!
//! [`check`] is one function, called first in every route it applies to:
//!
//! - `GET` and `HEAD` pass: no state-changing route is a `GET` (an OAuth
//!   start only sets a flow cookie and redirects; a callback is bound by
//!   `state` and the flow cookie);
//! - `POST` must carry `Sec-Fetch-Site: same-origin`; when it does and also
//!   carries an `Origin`, that origin must be the host's own. Without
//!   `Sec-Fetch-Site`, `Origin` must equal the host's own origin exactly. A
//!   missing `Origin` or `Origin: null` is then refused;
//! - any other method is refused.
//!
//! The host's own origin is `SITE_ORIGIN` or `SECRET_ORIGIN` from config,
//! chosen by the host the request was made to, never read from `Host`. A
//! request from the secret host to a site-host `POST` is same-site but not
//! same-origin, so it is refused; that closes the gap `SameSite` leaves
//! between the two hosts.
//!
//! Scope ([`applies`]): requests that carry `X-SN-Route` (browser routes the
//! website forwards) and the account Worker's own browser routes. Binding
//! calls to `/internal/v1/*`, signed operator ops (`/op`), the paid API and
//! webhooks are not browser requests and have their own authentication.

use crate::origin::Origin;

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrfRefusal {
    /// A method other than `GET`, `HEAD` or `POST`.
    Method,
    /// `Sec-Fetch-Site` is present and not `same-origin`.
    FetchSite,
    /// `Origin` is missing (without Fetch Metadata), `null`, or not the
    /// host's own.
    Origin,
}

impl CsrfRefusal {
    /// The journal reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Method => "csrf_method",
            Self::FetchSite => "csrf_fetch_site",
            Self::Origin => "csrf_origin",
        }
    }
}

/// The decision for one request.
///
/// `sec_fetch_site` and `origin` are the request's header values as
/// received (the website forwards both unchanged); `host_origin` is the
/// configured origin of the host the request was made to.
pub fn check(
    method: &str,
    sec_fetch_site: Option<&str>,
    origin: Option<&str>,
    host_origin: &Origin,
) -> Result<(), CsrfRefusal> {
    match method {
        "GET" | "HEAD" => return Ok(()),
        "POST" => {}
        _ => return Err(CsrfRefusal::Method),
    }
    match sec_fetch_site {
        Some("same-origin") => match origin {
            None => Ok(()),
            Some(o) if host_origin.is(o) => Ok(()),
            Some(_) => Err(CsrfRefusal::Origin),
        },
        Some(_) => Err(CsrfRefusal::FetchSite),
        None => match origin {
            Some(o) if host_origin.is(o) => Ok(()),
            _ => Err(CsrfRefusal::Origin),
        },
    }
}

/// The kinds of request a Worker serves, for the CSRF scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestClass {
    /// A browser route forwarded by the website (carries `X-SN-Route`).
    ForwardedBrowser,
    /// One of the account Worker's own browser routes.
    AccountBrowser,
    /// A service-binding call to `/internal/v1/*`.
    Internal,
    /// A signed operator op (`/op`).
    Op,
    /// The paid API (bearer keys).
    Api,
    /// A webhook (its own signature).
    Webhook,
}

/// Whether [`check`] applies to a request class.
#[must_use]
pub const fn applies(class: RequestClass) -> bool {
    matches!(
        class,
        RequestClass::ForwardedBrowser | RequestClass::AccountBrowser
    )
}

/// [`check`] when it [`applies`] to `class`; otherwise passes.
pub fn check_for(
    class: RequestClass,
    method: &str,
    sec_fetch_site: Option<&str>,
    origin: Option<&str>,
    host_origin: &Origin,
) -> Result<(), CsrfRefusal> {
    if applies(class) {
        check(method, sec_fetch_site, origin, host_origin)
    } else {
        Ok(())
    }
}

/// A body type a route accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyType {
    /// `application/json` (JSON APIs).
    Json,
    /// `application/x-www-form-urlencoded` (form endpoints).
    Form,
}

/// Whether a `Content-Type` header is the route's type. Parameters after
/// `;` (a `charset`) are allowed; anything else, including `text/plain`
/// and `multipart/form-data`, is refused, so a cross-origin page cannot send
/// a JSON body without a preflight that never succeeds (no CORS headers are
/// ever sent).
#[must_use]
pub fn content_type_ok(content_type: Option<&str>, want: BodyType) -> bool {
    let Some(ct) = content_type else {
        return false;
    };
    let essence = ct.split(';').next().unwrap_or("").trim_matches(' ');
    let want = match want {
        BodyType::Json => "application/json",
        BodyType::Form => "application/x-www-form-urlencoded",
    };
    essence.eq_ignore_ascii_case(want)
}
