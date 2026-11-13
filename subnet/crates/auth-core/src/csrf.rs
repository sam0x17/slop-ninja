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
//! The host's own origin is `SITE_ORIGIN` or `SECRET_ORIGIN` from config
//! ([`Origins::of`] the request's [`HostRole`]), never read from `Host`. A
//! request from the secret host to a site-host `POST` is same-site but not
//! same-origin, so it is refused; that closes the gap `SameSite` leaves
//! between the two hosts.
//!
//! Scope ([`applies`], [`check_route`]): the forwarded route table's `csrf`
//! column ([`RouteId`]), which marks every browser route the website or the
//! secret host forwards with `X-SN-Route`, the account Worker's own browser
//! routes among them. The signed op routes, the paid API and webhooks are
//! not browser requests and are unmarked; binding calls to `/internal/v1/*`
//! are not forwarded routes at all. Each of those has its own
//! authentication.
//!
//! Every refusal maps to one [`RefusalCode`] ([`CsrfRefusal::code`]), the
//! code the route answers with and the `reason` of its `auth` metrics point.

use slop_ninja_detect_page::{HostRole, Origin, Origins, RefusalCode, RouteId};

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
    /// The refusal code: `method` (405) for a method no route takes,
    /// `csrf` (403) for the Fetch Metadata and `Origin` rule.
    #[must_use]
    pub const fn code(self) -> RefusalCode {
        match self {
            Self::Method => RefusalCode::Method,
            Self::FetchSite | Self::Origin => RefusalCode::Csrf,
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
    let own = |o: &str| o == host_origin.as_str();
    match sec_fetch_site {
        Some("same-origin") => match origin {
            None => Ok(()),
            Some(o) if own(o) => Ok(()),
            Some(_) => Err(CsrfRefusal::Origin),
        },
        Some(_) => Err(CsrfRefusal::FetchSite),
        None => match origin {
            Some(o) if own(o) => Ok(()),
            _ => Err(CsrfRefusal::Origin),
        },
    }
}

/// Whether [`check`] applies to a forwarded route: its table row's `csrf`
/// column.
#[must_use]
pub const fn applies(route: RouteId) -> bool {
    route.spec().csrf
}

/// [`check`] for a forwarded route that arrived on `host`, against that
/// host's configured origin, when it [`applies`] to the route; otherwise
/// passes.
pub fn check_route(
    route: RouteId,
    host: HostRole,
    origins: &Origins,
    method: &str,
    sec_fetch_site: Option<&str>,
    origin: Option<&str>,
) -> Result<(), CsrfRefusal> {
    if applies(route) {
        check(method, sec_fetch_site, origin, origins.of(host))
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

/// The refusal for a body of another type: `content_type` (415).
pub const CONTENT_TYPE_REFUSAL: RefusalCode = RefusalCode::ContentType;

/// Whether a `Content-Type` header is the route's type. Parameters after
/// `;` (a `charset`) are allowed; anything else, including `text/plain`
/// and `multipart/form-data`, is refused ([`CONTENT_TYPE_REFUSAL`]), so a
/// cross-origin page cannot send a JSON body without a preflight that never
/// succeeds (no CORS headers are ever sent).
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
