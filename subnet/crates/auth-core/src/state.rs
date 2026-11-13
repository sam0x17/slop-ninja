//! The OAuth `state` round trip and the single-use row checks.
//!
//! A start writes a flow row keyed by `SHA-256(state)` holding the provider,
//! `SHA-256(binding)` of the `__Host-sn_flow` cookie and an expiry. The
//! callback parses its query ([`CallbackQuery::parse`]), takes the row with
//! one `DELETE … RETURNING` (so a replayed `state` finds nothing), and passes
//! what it took to [`check_flow`] together with the request's flow cookie.
//! The same take-then-check shape serves the email link token
//! ([`check_email_token`]) and the wallet challenge (`siws`).
//!
//! The storage statement is the Worker's; these functions decide on what it
//! returned, so the refusals are tested without a database.

use crate::providers::Provider;
use crate::token::Token;

/// The longest callback query read. A provider's `code` is well under a
/// kilobyte (Google's about 75 characters, GitHub's 20, X's about 90) and
/// `state` is 43; the bound keeps a hostile query from being scanned at
/// length.
pub const CALLBACK_QUERY_MAX: usize = 4096;

/// The longest authorization `code` accepted.
pub const CODE_MAX: usize = 1024;

/// The parsed callback query.
#[derive(Debug, PartialEq, Eq)]
pub struct CallbackQuery {
    /// The `state` the start issued.
    pub state: Token,
    /// The authorization code, or the provider's refusal.
    pub outcome: Result<String, ProviderDenial>,
}

/// A provider's `error` parameter on the callback, mapped to a closed set
/// (the free-text `error_description` is never kept).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderDenial {
    /// `access_denied`: the reader declined at the consent screen.
    AccessDenied,
    /// Any other `error` value.
    Other,
}

/// Why a callback or a single-use token was refused. Each maps to a
/// content-free journal reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The query is too long, repeats a parameter or is not valid
    /// form encoding.
    MalformedQuery,
    /// No `state` parameter.
    MissingState,
    /// `state` is not a well-formed token.
    MalformedState,
    /// Neither `code` nor `error`, or a malformed `code`.
    MissingCode,
    /// No row was taken: an unknown, already-used (replayed) or swept token.
    Unknown,
    /// The request carries no flow cookie.
    MissingFlowCookie,
    /// The flow cookie is present but is not a well-formed token.
    MalformedFlowCookie,
    /// The flow cookie is not the one the flow was started with.
    BindingMismatch,
    /// The row belongs to another provider's flow (a mix-up attempt).
    ProviderMismatch,
    /// The row had expired when it was taken.
    Expired,
}

impl Refusal {
    /// The journal reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedQuery => "malformed_query",
            Self::MissingState => "missing_state",
            Self::MalformedState => "malformed_state",
            Self::MissingCode => "missing_code",
            Self::Unknown => "unknown",
            Self::MissingFlowCookie => "missing_flow_cookie",
            Self::MalformedFlowCookie => "malformed_flow_cookie",
            Self::BindingMismatch => "binding_mismatch",
            Self::ProviderMismatch => "provider_mismatch",
            Self::Expired => "expired",
        }
    }
}

impl CallbackQuery {
    /// Parses a callback's query string (without the leading `?`).
    ///
    /// Every parameter may appear at most once; a repeated `state`, `code`
    /// or `error` refuses the whole query rather than picking one.
    pub fn parse(query: &str) -> Result<Self, Refusal> {
        if query.len() > CALLBACK_QUERY_MAX {
            return Err(Refusal::MalformedQuery);
        }
        let (mut state, mut code, mut error) = (None, None, None);
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let slot = match key.as_ref() {
                "state" => &mut state,
                "code" => &mut code,
                "error" => &mut error,
                _ => continue,
            };
            if slot.replace(value.into_owned()).is_some() {
                return Err(Refusal::MalformedQuery);
            }
        }
        let state = state.ok_or(Refusal::MissingState)?;
        let state = Token::parse(&state).ok_or(Refusal::MalformedState)?;
        let outcome = match (code, error) {
            (_, Some(e)) => Err(if e == "access_denied" {
                ProviderDenial::AccessDenied
            } else {
                ProviderDenial::Other
            }),
            (Some(c), None) if is_code(&c) => Ok(c),
            _ => return Err(Refusal::MissingCode),
        };
        Ok(Self { state, outcome })
    }
}

fn is_code(c: &str) -> bool {
    !c.is_empty() && c.len() <= CODE_MAX && c.bytes().all(|b| b.is_ascii_graphic())
}

/// What the callback's `DELETE … RETURNING` took from the flow row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlowFacts {
    /// The provider the start redirected to.
    pub provider: Provider,
    /// `SHA-256` of the flow cookie the start set or found.
    pub binding_h: [u8; 32],
    /// When the row expires (Unix milliseconds).
    pub expires_at_ms: u64,
}

/// The callback's decision on the row it took.
///
/// `taken` is `None` when the statement returned nothing: the `state` was
/// never issued, was already used, or was swept. The provider in the callback
/// path must equal the row's, and the request's flow cookie must hash to the
/// row's binding.
pub fn check_flow(
    taken: Option<&FlowFacts>,
    flow_cookie: Option<&str>,
    callback_provider: Provider,
    now_ms: u64,
) -> Result<(), Refusal> {
    let row = taken.ok_or(Refusal::Unknown)?;
    check_binding(&row.binding_h, flow_cookie)?;
    if row.provider != callback_provider {
        return Err(Refusal::ProviderMismatch);
    }
    if now_ms >= row.expires_at_ms {
        return Err(Refusal::Expired);
    }
    Ok(())
}

/// The flow cookie must be present, well formed and hash to `binding_h`.
pub fn check_binding(binding_h: &[u8; 32], flow_cookie: Option<&str>) -> Result<(), Refusal> {
    let cookie = flow_cookie.ok_or(Refusal::MissingFlowCookie)?;
    let token = Token::parse(cookie).ok_or(Refusal::MalformedFlowCookie)?;
    if token.matches(binding_h) {
        Ok(())
    } else {
        Err(Refusal::BindingMismatch)
    }
}

/// The email confirm's decision on the token row it took
/// (`DELETE FROM email_tokens WHERE token_h = ? AND expires_at > ? RETURNING …`).
///
/// The token is spent whether or not the rest succeeds; the browser rules of
/// each intent are the account Worker's. Here: a row must have been taken,
/// and it must not have expired (repeated in code, so a statement that lost
/// its time filter still refuses).
pub fn check_email_token(taken_expires_at_ms: Option<u64>, now_ms: u64) -> Result<(), Refusal> {
    let expires = taken_expires_at_ms.ok_or(Refusal::Unknown)?;
    if now_ms >= expires {
        return Err(Refusal::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(n: u8) -> Token {
        Token::from_random([n; 32])
    }

    #[test]
    fn query_shapes() {
        let s = tok(1).encode();
        let q = CallbackQuery::parse(&format!("code=abc&state={s}")).unwrap();
        assert_eq!(q.state, tok(1));
        assert_eq!(q.outcome, Ok("abc".to_owned()));
        let q = CallbackQuery::parse(&format!("error=access_denied&state={s}")).unwrap();
        assert_eq!(q.outcome, Err(ProviderDenial::AccessDenied));
        let q = CallbackQuery::parse(&format!("error=server_error&error_description=x&state={s}"))
            .unwrap();
        assert_eq!(q.outcome, Err(ProviderDenial::Other));
        assert_eq!(CallbackQuery::parse("code=abc"), Err(Refusal::MissingState));
        assert_eq!(
            CallbackQuery::parse("code=abc&state=short"),
            Err(Refusal::MalformedState)
        );
        assert_eq!(
            CallbackQuery::parse(&format!("state={s}")),
            Err(Refusal::MissingCode)
        );
        assert_eq!(
            CallbackQuery::parse(&format!("code=a%20b&state={s}")),
            Err(Refusal::MissingCode)
        );
        assert_eq!(
            CallbackQuery::parse(&format!("code=a&code=b&state={s}")),
            Err(Refusal::MalformedQuery)
        );
        assert_eq!(
            CallbackQuery::parse(&format!("code=a&state={s}&state={}", tok(2).encode())),
            Err(Refusal::MalformedQuery)
        );
        assert_eq!(
            CallbackQuery::parse(&format!(
                "code={}&state={s}",
                "a".repeat(CALLBACK_QUERY_MAX)
            )),
            Err(Refusal::MalformedQuery)
        );
    }
}
