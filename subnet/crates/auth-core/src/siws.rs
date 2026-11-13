//! Bittensor wallet sign-in: the message and its sr25519 verification.
//!
//! The server builds the message, stores its exact bytes with the challenge
//! row, and later verifies a signature over those stored bytes only. A
//! signature is accepted over the raw bytes or over the `<Bytes>…</Bytes>`
//! wrapping a browser extension's `signRaw` applies, and over nothing else;
//! each form is checked with `rtao_core::keys::verify_strict`, which does
//! no wrapping retry of its own.
//!
//! The extension message uses the Sign-In-with-Substrate layout, so wallets
//! that recognise it show a sign-in and check its domain line against the
//! requesting page:
//!
//! ```text
//! <site host> wants you to sign in with your Bittensor account:
//! <ss58, prefix 42>
//!
//! Sign in to SlopNinja. This is not a transaction and costs nothing.
//!
//! URI: <site origin>/signin/
//! Version: 1
//! Nonce: <26 Crockford base32 characters>
//! Issued At: <RFC 3339, milliseconds, Z>
//! Expiration Time: <issued + the challenge lifetime>
//! ```
//!
//! The command-line message is one line, because multi-line shell arguments
//! are easy to get wrong:
//! `<site host> sign-in for <ss58>; nonce <n>; issued <t>; expires <t>`.
//!
//! `<site host>` (with its port when not the default) and `<site origin>` come
//! from the configured site origin. Neither form can parse as any other
//! SlopNinja signed message, each of which starts with its own
//! `slop_ninja_…` prefix.

use rtao_core::keys::{self, CRYPTO_SR25519};
use slop_ninja_account_wire::WalletMode;
use slop_ninja_detect_page::{Origin, RefusalCode};

use crate::state::{Refusal, check_binding};
use crate::time::rfc3339_ms;

/// The statement line.
pub const STATEMENT: &str = "Sign in to SlopNinja. This is not a transaction and costs nothing.";
/// The `Version` line's value.
pub const VERSION: &str = "1";
/// The SS58 prefix messages render addresses with (Bittensor's generic
/// Substrate format).
pub const SS58_PREFIX: u16 = 42;
/// The longest address text accepted (an SS58 address with a two-byte
/// prefix is 49 characters).
pub const ADDRESS_MAX: usize = 64;
/// The Crockford base32 alphabet.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A 128-bit challenge nonce, written as 26 Crockford base32 characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nonce([u8; 16]);

impl Nonce {
    /// From 16 bytes drawn by the caller from a CSPRNG.
    #[must_use]
    pub const fn from_random(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The 26-character form (big-endian, two leading zero bits).
    #[must_use]
    pub fn encode(&self) -> String {
        let n = u128::from_be_bytes(self.0);
        (0..26)
            .rev()
            .map(|i| CROCKFORD[((n >> (5 * i)) & 31) as usize] as char)
            .collect()
    }
}

/// Why a wallet request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiwsError {
    /// The address does not decode as SS58.
    Address,
    /// The signature is not 64 bytes of hex.
    SignatureFormat,
    /// The signature does not verify over the issued bytes, raw or wrapped.
    BadSignature,
}

impl SiwsError {
    /// The refusal code: `wallet_type` (400, "This sign-in needs an sr25519
    /// (Substrate) account.") for an address that is not SS58, `signature`
    /// (403) for a signature that is not 64 bytes of hex or does not verify.
    #[must_use]
    pub const fn code(self) -> RefusalCode {
        match self {
            Self::Address => RefusalCode::WalletType,
            Self::SignatureFormat | Self::BadSignature => RefusalCode::Signature,
        }
    }
}

/// Decodes an SS58 address with any prefix the SS58 registry knows, and
/// returns the 32-byte public key: the identity is the key, not the text.
pub fn decode_address(address: &str) -> Result<[u8; 32], SiwsError> {
    if address.is_empty() || address.len() > ADDRESS_MAX || !address.is_ascii() {
        return Err(SiwsError::Address);
    }
    keys::public_key_from_ss58(address).map_err(|_| SiwsError::Address)
}

/// The key's address under prefix 42.
#[must_use]
pub fn address_42(public_key: &[u8; 32]) -> String {
    keys::ss58_from_public(*public_key, SS58_PREFIX)
}

/// What a challenge message names.
#[derive(Clone, Copy, Debug)]
pub struct Challenge<'a> {
    /// The configured site origin.
    pub site: &'a Origin,
    /// The signer's public key.
    pub public_key: &'a [u8; 32],
    /// The nonce.
    pub nonce: &'a Nonce,
    /// Issue time (Unix milliseconds).
    pub issued_at_ms: u64,
    /// Expiry (Unix milliseconds).
    pub expires_at_ms: u64,
}

/// The exact message text for `mode`; the challenge row stores its bytes.
#[must_use]
pub fn message(mode: WalletMode, c: &Challenge<'_>) -> String {
    let address = address_42(c.public_key);
    let nonce = c.nonce.encode();
    let issued = rfc3339_ms(c.issued_at_ms);
    let expires = rfc3339_ms(c.expires_at_ms);
    let domain = c.site.authority();
    match mode {
        WalletMode::Extension => format!(
            "{domain} wants you to sign in with your Bittensor account:\n{address}\n\n{STATEMENT}\n\nURI: {}/signin/\nVersion: {VERSION}\nNonce: {nonce}\nIssued At: {issued}\nExpiration Time: {expires}",
            c.site.as_str()
        ),
        WalletMode::Cli => {
            format!(
                "{domain} sign-in for {address}; nonce {nonce}; issued {issued}; expires {expires}"
            )
        }
    }
}

/// Parses a signature in hex, with or without `0x`, either case: exactly 64
/// bytes.
pub fn parse_signature(text: &str) -> Result<[u8; 64], SiwsError> {
    let text = text.trim_matches(|c: char| c.is_ascii_whitespace());
    let hex_part = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if hex_part.len() != 128 {
        return Err(SiwsError::SignatureFormat);
    }
    let mut out = [0u8; 64];
    hex::decode_to_slice(hex_part, &mut out).map_err(|_| SiwsError::SignatureFormat)?;
    Ok(out)
}

/// Which form verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedForm {
    /// The issued bytes as they are (the command-line tools).
    Raw,
    /// `<Bytes>` + the issued bytes + `</Bytes>` (browser extensions).
    Wrapped,
}

/// Verifies `signature` by `public_key` over the stored `message` bytes:
/// raw first, then wrapped, and nothing else.
pub fn verify(
    message: &[u8],
    signature: &[u8; 64],
    public_key: &[u8; 32],
) -> Result<SignedForm, SiwsError> {
    let address = address_42(public_key);
    let check = |bytes: &[u8]| {
        keys::verify_strict(bytes, signature, &address, CRYPTO_SR25519).unwrap_or(false)
    };
    if check(message) {
        return Ok(SignedForm::Raw);
    }
    let mut wrapped = Vec::with_capacity(message.len() + 15);
    wrapped.extend_from_slice(b"<Bytes>");
    wrapped.extend_from_slice(message);
    wrapped.extend_from_slice(b"</Bytes>");
    if check(&wrapped) {
        return Ok(SignedForm::Wrapped);
    }
    Err(SiwsError::BadSignature)
}

/// What the verify's `DELETE … RETURNING` took from the challenge row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChallengeFacts {
    /// `SHA-256` of the flow cookie the challenge was issued to.
    pub binding_h: [u8; 32],
    /// When the challenge expires (Unix milliseconds).
    pub expires_at_ms: u64,
}

/// The verify's decision on the row it took: one must have been taken (so
/// a reused challenge id finds nothing), the request's flow cookie must be
/// the one the challenge was issued to, and it must not have expired. A
/// refusal answers [`Refusal::code`] with [`crate::state::Flow::WalletChallenge`].
pub fn check_challenge(
    taken: Option<&ChallengeFacts>,
    flow_cookie: Option<&str>,
    now_ms: u64,
) -> Result<(), Refusal> {
    let row = taken.ok_or(Refusal::Unknown)?;
    check_binding(&row.binding_h, flow_cookie)?;
    if now_ms >= row.expires_at_ms {
        return Err(Refusal::Expired);
    }
    Ok(())
}
