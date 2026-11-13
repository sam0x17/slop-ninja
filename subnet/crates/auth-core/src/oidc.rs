//! Google ID token claim checks.
//!
//! The ID token comes straight from Google's token endpoint, over TLS and
//! authenticated with the client secret. Google's OIDC guide says such a
//! token can be trusted without a signature check, so none is made and no
//! JWKS is fetched. The claims are still checked, each refusal by name:
//!
//! - `iss` is one of the two forms Google uses ([`GOOGLE_ISSUERS`]);
//! - `aud` is the client id (and when `aud` lists several audiences, `azp`
//!   is the client id);
//! - `exp` is in the future;
//! - `nonce` equals the flow's nonce;
//! - `sub` is present: the identity key, never the email.
//!
//! The email counts as verified only when `email_verified` is true and the
//! address passes [`crate::email_addr::parse`].
//!
//! [`GOOGLE_ISSUERS`]: crate::providers::GOOGLE_ISSUERS

use serde::Deserialize;
use slop_ninja_detect_page::RefusalCode;

use crate::b64;
use crate::email_addr::{self, EmailAddr};
use crate::providers::ID_TOKEN_MAX;

/// What the checks compare against.
#[derive(Clone, Copy, Debug)]
pub struct Expect<'a> {
    /// Accepted `iss` values.
    pub issuers: &'a [&'a str],
    /// The client id.
    pub client_id: &'a str,
    /// The nonce the flow sent (its token text).
    pub nonce: &'a str,
    /// The current time (Unix seconds).
    pub now_s: u64,
}

/// A checked Google identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// `sub`: at most 255 ASCII characters, never reused by Google.
    pub sub: String,
    /// The address, only when `email_verified` is true.
    pub verified_email: Option<EmailAddr>,
}

/// Why an ID token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Not three base64url segments with a JSON payload, or too long.
    Malformed,
    /// `iss` missing or not an accepted issuer.
    Issuer,
    /// `aud` (or `azp`) is not the client id.
    Audience,
    /// `exp` missing or passed.
    Expired,
    /// `iat` more than [`IAT_FUTURE_SKEW_S`] ahead of the clock.
    IssuedInFuture,
    /// `nonce` missing or different.
    Nonce,
    /// `sub` missing or out of format.
    Subject,
}

impl ClaimError {
    /// The refusal code: `provider` (502) for every failed claim. The token
    /// came from the provider's own endpoint, so a failed claim is the
    /// provider answering out of form or a mixed-up flow, and the reader is
    /// asked to sign in again either way; the variant stays in the Worker.
    #[must_use]
    pub const fn code(self) -> RefusalCode {
        match self {
            Self::Malformed
            | Self::Issuer
            | Self::Audience
            | Self::Expired
            | Self::IssuedInFuture
            | Self::Nonce
            | Self::Subject => RefusalCode::Provider,
        }
    }
}

/// How far ahead of the Worker's clock `iat` may be (clock skew between
/// Google and Cloudflare).
pub const IAT_FUTURE_SKEW_S: u64 = 300;

#[derive(Deserialize)]
#[serde(untagged)]
enum Aud {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Flag {
    Bool(bool),
    Text(String),
}

#[derive(Deserialize)]
struct RawClaims {
    iss: Option<String>,
    aud: Option<Aud>,
    azp: Option<String>,
    exp: Option<u64>,
    iat: Option<u64>,
    nonce: Option<String>,
    sub: Option<String>,
    email: Option<String>,
    email_verified: Option<Flag>,
}

/// Decodes an ID token's payload and checks its claims.
pub fn check(id_token: &str, expect: &Expect<'_>) -> Result<Identity, ClaimError> {
    if id_token.len() > ID_TOKEN_MAX {
        return Err(ClaimError::Malformed);
    }
    let mut parts = id_token.split('.');
    let (Some(_header), Some(payload), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ClaimError::Malformed);
    };
    let payload = b64::decode(payload).ok_or(ClaimError::Malformed)?;
    let claims: RawClaims = serde_json::from_slice(&payload).map_err(|_| ClaimError::Malformed)?;
    check_claims(claims, expect)
}

fn check_claims(c: RawClaims, expect: &Expect<'_>) -> Result<Identity, ClaimError> {
    let iss = c.iss.ok_or(ClaimError::Issuer)?;
    if !expect.issuers.contains(&iss.as_str()) {
        return Err(ClaimError::Issuer);
    }
    match c.aud.ok_or(ClaimError::Audience)? {
        Aud::One(a) if a == expect.client_id => {}
        Aud::Many(list) if list.iter().any(|a| a == expect.client_id) => {
            if list.len() > 1 && c.azp.as_deref() != Some(expect.client_id) {
                return Err(ClaimError::Audience);
            }
        }
        _ => return Err(ClaimError::Audience),
    }
    if let Some(azp) = &c.azp
        && azp != expect.client_id
    {
        return Err(ClaimError::Audience);
    }
    let exp = c.exp.ok_or(ClaimError::Expired)?;
    if exp <= expect.now_s {
        return Err(ClaimError::Expired);
    }
    if let Some(iat) = c.iat
        && iat > expect.now_s.saturating_add(IAT_FUTURE_SKEW_S)
    {
        return Err(ClaimError::IssuedInFuture);
    }
    let nonce = c.nonce.ok_or(ClaimError::Nonce)?;
    if !crate::ct_eq(nonce.as_bytes(), expect.nonce.as_bytes()) {
        return Err(ClaimError::Nonce);
    }
    let sub = c.sub.ok_or(ClaimError::Subject)?;
    if !(1..=255).contains(&sub.len()) || !sub.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ClaimError::Subject);
    }
    let verified = matches!(c.email_verified, Some(Flag::Bool(true)))
        || matches!(&c.email_verified, Some(Flag::Text(t)) if t == "true");
    let verified_email = if verified {
        c.email.and_then(|e| email_addr::parse(&e).ok())
    } else {
        None
    };
    Ok(Identity {
        sub,
        verified_email,
    })
}

/// Builds an unsigned test token around a JSON payload (tests and the local
/// auth stub; the Worker never builds one).
#[cfg(any(test, feature = "dev"))]
#[must_use]
pub fn test_token(payload_json: &str) -> String {
    format!(
        "{}.{}.{}",
        b64::encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        b64::encode(payload_json.as_bytes()),
        b64::encode(b"not-checked")
    )
}
