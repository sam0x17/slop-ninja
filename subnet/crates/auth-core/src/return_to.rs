//! Where a sign-in returns to.
//!
//! [`sanitize`] is used by every start, callback and confirm:
//!
//! 1. the raw value is refused if it holds `\`, any C0 or C1 control, DEL or
//!    any white space, or if its percent-decoded form holds `\`, a control or
//!    white space, starts with `//` or is not UTF-8;
//! 2. it is parsed with the `url` crate, joined to the configured site
//!    origin's root, and the result must have that origin, no user name or
//!    password, and no fragment;
//! 3. the path must be one of the policy's exact paths (`/`) or start with
//!    one of its prefixes (`auth-policy/v1` `return_to_paths`: the pages a
//!    sign-in starts from);
//! 4. only the re-serialized path and query are kept, at most the policy's
//!    `return_to_max` bytes (a longer raw or re-serialized value is refused,
//!    not cut). That value is stored; the raw value never is.
//!
//! A refused value becomes `/account/` for a signed-in flow and `/detect/`
//! otherwise ([`sanitize_or_default`]; these are the account contract's
//! [`slop_ninja_account_wire::SIGNED_IN_RETURN`] and
//! [`slop_ninja_account_wire::SIGNED_OUT_RETURN`], and a flow is signed in
//! when its intent
//! [`is_signed_in`](slop_ninja_account_wire::Intent::is_signed_in)). Every
//! redirect is built by
//! [`ReturnTo::location`]: the fixed configured origin followed by the stored
//! path, so even a value that slipped past the parser could not leave the
//! origin.

use percent_encoding::percent_decode_str;
use url::{Position, Url};

use slop_ninja_detect_page::Origin;

use crate::policy::AuthPolicy;

/// A sanitized destination: a same-origin path and query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnTo(String);

/// Why a raw value was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Longer than `return_to_max`.
    TooLong,
    /// A backslash, control or white space, raw or percent-decoded, or a
    /// decoded form that starts with `//` or is not UTF-8.
    Characters,
    /// Does not parse against the site origin.
    Unparsable,
    /// Resolves to another origin, or carries user info or a fragment.
    Origin,
    /// The path is not one of the allowed destinations.
    Path,
}

/// The default for a signed-in flow.
pub const SIGNED_IN_DEFAULT: &str = slop_ninja_account_wire::SIGNED_IN_RETURN;
/// The default otherwise.
pub const SIGNED_OUT_DEFAULT: &str = slop_ninja_account_wire::SIGNED_OUT_RETURN;

impl ReturnTo {
    /// The stored path and query.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The default destination.
    #[must_use]
    pub fn default_for(signed_in: bool) -> Self {
        Self(
            if signed_in {
                SIGNED_IN_DEFAULT
            } else {
                SIGNED_OUT_DEFAULT
            }
            .to_owned(),
        )
    }

    /// The absolute `Location`: the configured origin plus the stored path.
    #[must_use]
    pub fn location(&self, site: &Origin) -> String {
        site.url(&self.0)
    }

    /// The value percent-encoded as one query parameter value (the sign-in
    /// page's start links carry it this way).
    #[must_use]
    pub fn as_query_value(&self) -> String {
        url::form_urlencoded::byte_serialize(self.0.as_bytes()).collect()
    }

    /// Re-checks a value read back from storage; anything that would not
    /// pass [`sanitize`] unchanged becomes the default.
    #[must_use]
    pub fn from_stored(stored: &str, site: &Origin, policy: &AuthPolicy, signed_in: bool) -> Self {
        match sanitize(stored, site, policy) {
            Ok(r) if r.0 == stored => r,
            _ => Self::default_for(signed_in),
        }
    }
}

/// [`sanitize`], with a refused or absent value replaced by the default.
#[must_use]
pub fn sanitize_or_default(
    raw: Option<&str>,
    site: &Origin,
    policy: &AuthPolicy,
    signed_in: bool,
) -> ReturnTo {
    raw.and_then(|r| sanitize(r, site, policy).ok())
        .unwrap_or_else(|| ReturnTo::default_for(signed_in))
}

fn bad_char(c: char) -> bool {
    c == '\\' || c.is_control() || c.is_whitespace()
}

/// Sanitizes a raw `return_to` (see the module text).
pub fn sanitize(raw: &str, site: &Origin, policy: &AuthPolicy) -> Result<ReturnTo, Refused> {
    let max = policy.return_to_max.value;
    if raw.len() > max {
        return Err(Refused::TooLong);
    }
    if raw.is_empty() {
        return Err(Refused::Path);
    }
    if raw.chars().any(bad_char) {
        return Err(Refused::Characters);
    }
    let decoded = percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| Refused::Characters)?;
    if decoded.chars().any(bad_char) || decoded.starts_with("//") {
        return Err(Refused::Characters);
    }
    let base = Url::parse(site.as_str()).map_err(|_| Refused::Unparsable)?;
    let url = base.join(raw).map_err(|_| Refused::Unparsable)?;
    if url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Refused::Origin);
    }
    let path = url.path();
    let paths = &policy.return_to_paths.value;
    let allowed = paths.exact.iter().any(|p| p == path)
        || paths.prefixes.iter().any(|p| path.starts_with(p.as_str()));
    if !allowed {
        return Err(Refused::Path);
    }
    let kept = &url[Position::BeforePath..Position::AfterQuery];
    if kept.len() > max {
        return Err(Refused::TooLong);
    }
    if !kept.starts_with('/') || kept.starts_with("//") {
        return Err(Refused::Path);
    }
    Ok(ReturnTo(kept.to_owned()))
}
