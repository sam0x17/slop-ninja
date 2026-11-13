//! Seals: a free check's result, encrypted under a day key, on the final line
//! of its answer.
//!
//! A seal lets the website render the unstored result again
//! (`/detect/result`), make a secret link from it (`/share`, once per seal),
//! or quote a paid check from it, without scoring the text again and without
//! the answer carrying any text-derived value in the clear.
//!
//! - **Format:** `base64url(v ‖ nonce96 ‖ AES-256-GCM(K(day), nonce, json))`,
//!   no padding, with version byte [`SEAL_V`] and the purpose's AAD
//!   ([`Purpose::aad`]): `slop_ninja_detect_seal_v1` for a result seal,
//!   `slop_ninja_detect_quote_v1` for a quote seal. A seal of one purpose
//!   never opens as the other.
//! - **Keys:** `K(day) = HKDF-SHA256(ikm = DETECT_STORE_SECRET, salt = none,
//!   info = "seal" ‖ day as 8 bytes big-endian)`, where `day` is the UTC day
//!   number (`ms / 86,400,000`) and the secret is its 32 bytes, written as 64
//!   lowercase hex digits ([`parse_secret`]): the store's other keys derive
//!   from the same bytes under their own `info`.
//! - **Validity:** a seal opens under today's key or yesterday's (24 to 48
//!   hours), and its payload must name the day of the key that opened it.
//! - **Result payload** ([`ResultPayload`]): `{day, text_sha256, scored_end,
//!   cut, package_load, policy_v, result_full, issued_ms}`. `text_sha256` is
//!   the SHA-256 of the exact string the free cut ran on (lowercase hex), and
//!   `scored_end` the scored part's end in UTF-16 units of that string, so a
//!   holder of the text can check it and find the scored part.
//! - **Single use** for `/share` is the store's: [`seal_nonce_h`],
//!   `SHA-256("seal_nonce_v1" ‖ nonce)`, is the value its unique column
//!   keeps. Renders and quotes reuse a seal.
//!
//! Nothing here reads a clock or a random source: the caller passes the day,
//! the time and the nonce, so every rule is tested natively.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::answer::ResultFull;

/// The seal format's version byte.
pub const SEAL_V: u8 = 1;
/// The AES-GCM nonce: 96 random bits per seal.
pub const NONCE_BYTES: usize = 12;
/// The AES-GCM tag.
pub const TAG_BYTES: usize = 16;
/// The store secret: 32 bytes.
pub const SECRET_BYTES: usize = 32;
/// One UTC day.
pub const DAY_MS: u64 = 86_400_000;
/// The result policy the free check applies (`long-v1`: the free cut, the
/// length bins and the bands of the detect-long rules).
pub const POLICY_V: &str = "long-v1";

/// What a seal is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A free check's result (`/score`).
    Result,
    /// A paid check's quote (`/paid/quote`).
    Quote,
}

impl Purpose {
    pub const fn aad(self) -> &'static [u8] {
        match self {
            Purpose::Result => b"slop_ninja_detect_seal_v1",
            Purpose::Quote => b"slop_ninja_detect_quote_v1",
        }
    }
}

/// Why a seal was not made or not opened. Every open failure answers
/// `seal_invalid`; none says why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealError {
    /// Not base64url, the wrong version, or too short.
    Malformed,
    /// No key of the window opens it (forged, edited, expired, another
    /// purpose), or its payload is not the purpose's or names another day.
    Invalid,
    /// The text given is not the one the seal binds.
    TextMismatch,
}

/// The day number of `ms`.
pub const fn day_of(ms: u64) -> u64 {
    ms / DAY_MS
}

/// The store secret's bytes from its 64 lowercase hex digits; `None` for any
/// other form.
pub fn parse_secret(hex_digits: &str) -> Option<[u8; SECRET_BYTES]> {
    if hex_digits.len() != 2 * SECRET_BYTES
        || !hex_digits.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; SECRET_BYTES];
    hex::decode_to_slice(hex_digits, &mut out).ok()?;
    Some(out)
}

/// `K(day)` from the store secret.
pub fn day_key(secret: &[u8; SECRET_BYTES], day: u64) -> [u8; 32] {
    let d = day.to_be_bytes();
    let info = [b's', b'e', b'a', b'l', d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]];
    let mut key = [0u8; 32];
    // 32 bytes is far under HKDF's 255 × 32 limit, so expand cannot fail.
    let _ = Hkdf::<Sha256>::new(None, secret).expand(&info, &mut key);
    key
}

/// Seals `payload` under `K(day)` with `nonce`.
pub fn seal<T: Serialize>(
    secret: &[u8; SECRET_BYTES],
    purpose: Purpose,
    day: u64,
    nonce: [u8; NONCE_BYTES],
    payload: &T,
) -> Result<String, SealError> {
    let key = day_key(secret, day);
    let json = serde_json::to_vec(payload).map_err(|_| SealError::Malformed)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SealError::Malformed)?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &json, aad: purpose.aad() })
        .map_err(|_| SealError::Malformed)?;
    let mut raw = Vec::with_capacity(1 + NONCE_BYTES + ct.len());
    raw.push(SEAL_V);
    raw.extend_from_slice(&nonce);
    raw.extend_from_slice(&ct);
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

/// A payload that names its day.
pub trait Dated {
    fn day(&self) -> u64;
}

/// An opened seal: its payload and its nonce.
#[derive(Debug, PartialEq)]
pub struct Opened<T> {
    pub payload: T,
    pub nonce: [u8; NONCE_BYTES],
}

impl<T> Opened<T> {
    /// The value `/share` keeps to use a seal once.
    pub fn nonce_h(&self) -> [u8; 32] {
        seal_nonce_h(&self.nonce)
    }
}

/// Opens a seal of `purpose` at `now_ms`: today's key, then yesterday's.
pub fn open<T: DeserializeOwned + Dated>(
    secret: &[u8; SECRET_BYTES],
    purpose: Purpose,
    now_ms: u64,
    sealed: &str,
) -> Result<Opened<T>, SealError> {
    let raw = URL_SAFE_NO_PAD.decode(sealed.as_bytes()).map_err(|_| SealError::Malformed)?;
    if raw.len() < 1 + NONCE_BYTES + TAG_BYTES || raw.first() != Some(&SEAL_V) {
        return Err(SealError::Malformed);
    }
    let (nonce_b, ct) =
        raw.get(1..).ok_or(SealError::Malformed)?.split_at_checked(NONCE_BYTES).ok_or(SealError::Malformed)?;
    let nonce: [u8; NONCE_BYTES] = nonce_b.try_into().map_err(|_| SealError::Malformed)?;
    let today = day_of(now_ms);
    for day in [Some(today), today.checked_sub(1)].into_iter().flatten() {
        let key = day_key(secret, day);
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SealError::Malformed)?;
        let Ok(json) = cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: ct, aad: purpose.aad() }) else {
            continue;
        };
        let payload: T = serde_json::from_slice(&json).map_err(|_| SealError::Invalid)?;
        if payload.day() != day {
            return Err(SealError::Invalid);
        }
        return Ok(Opened { payload, nonce });
    }
    Err(SealError::Invalid)
}

/// `seal_nonce_h = SHA-256("seal_nonce_v1" ‖ nonce)`, in the store's family
/// of domain-prefixed hashes (`slug_v1`, `manage_v1`).
pub fn seal_nonce_h(nonce: &[u8; NONCE_BYTES]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"seal_nonce_v1");
    h.update(nonce);
    h.finalize().into()
}

/// The lowercase hex SHA-256 of `text`'s UTF-8 bytes.
pub fn text_sha256(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// A free check's sealed result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultPayload {
    pub day: u64,
    /// SHA-256 (hex) of the exact string the free cut ran on.
    pub text_sha256: String,
    /// The scored part's end, in UTF-16 units of that string.
    pub scored_end: usize,
    /// How the scored part was chosen (`none`, `first_1000_words`,
    /// `token_limit`).
    pub cut: String,
    /// The files manifest digest the scorer loaded.
    pub package_load: String,
    /// The result policy ([`POLICY_V`]).
    pub policy_v: String,
    /// The result as the answer line carried it.
    pub result_full: ResultFull,
    pub issued_ms: u64,
}

impl Dated for ResultPayload {
    fn day(&self) -> u64 {
        self.day
    }
}

/// Opens a result seal and checks that it binds `text`.
pub fn open_result(
    secret: &[u8; SECRET_BYTES],
    now_ms: u64,
    sealed: &str,
    text: &str,
) -> Result<Opened<ResultPayload>, SealError> {
    let o = open::<ResultPayload>(secret, Purpose::Result, now_ms, sealed)?;
    if day_of(o.payload.issued_ms) != o.payload.day {
        return Err(SealError::Invalid);
    }
    if o.payload.text_sha256 != text_sha256(text) {
        return Err(SealError::TextMismatch);
    }
    Ok(o)
}

/// The base64url length (no padding) of a seal whose payload JSON is
/// `json_bytes` long.
pub const fn sealed_len(json_bytes: usize) -> usize {
    ((1 + NONCE_BYTES + json_bytes + TAG_BYTES) * 4).div_ceil(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::{PackageLine, Span};

    const SECRET: &[u8; SECRET_BYTES] = b"seal test bytes, not a real key.";
    const T0: u64 = 20_405 * DAY_MS + 3_600_000;
    const TEXT: &str = "A sealed text of a few words, scored whole.";

    fn result() -> ResultFull {
        ResultFull {
            band: "strong".into(),
            confidence_tenths: Some(973),
            confidence_text: Some("97.3%".into()),
            share_text: Some("about 34%".into()),
            words: 200,
            total_words: 200,
            cut: "none".into(),
            scored_end: 43,
            tokens: 300,
            length: "150_450".into(),
            spans: Some(vec![Span { start: 0, end: 40, words: 30, confidence: Some(92) }]),
            spans_note: None,
            markers: vec![],
            invisible: false,
            package: PackageLine { digest: "d".repeat(64), publication_pending: true },
        }
    }

    fn payload(now: u64, text: &str) -> ResultPayload {
        ResultPayload {
            day: day_of(now),
            text_sha256: text_sha256(text),
            scored_end: text.encode_utf16().count(),
            cut: "none".into(),
            package_load: "a".repeat(64),
            policy_v: POLICY_V.into(),
            result_full: result(),
            issued_ms: now,
        }
    }

    fn sealed(now: u64, nonce: u8) -> String {
        seal(SECRET, Purpose::Result, day_of(now), [nonce; NONCE_BYTES], &payload(now, TEXT)).unwrap()
    }

    #[test]
    fn a_seal_opens_today_and_tomorrow_and_binds_its_text() {
        let s = sealed(T0, 7);
        let o = open_result(SECRET, T0 + 1, &s, TEXT).unwrap();
        assert_eq!(o.payload, payload(T0, TEXT));
        // The next UTC day: yesterday's key still opens it.
        assert!(open_result(SECRET, T0 + DAY_MS, &s, TEXT).is_ok());
        // The day after that: expired.
        assert_eq!(open_result(SECRET, T0 + 2 * DAY_MS, &s, TEXT), Err(SealError::Invalid));
        // A clock behind the seal's day: no key of the window opens it.
        assert_eq!(open_result(SECRET, T0 - DAY_MS, &s, TEXT), Err(SealError::Invalid));
    }

    #[test]
    fn an_edited_text_does_not_match() {
        let s = sealed(T0, 7);
        assert_eq!(
            open_result(SECRET, T0, &s, "A sealed text of a few words, scored whole!"),
            Err(SealError::TextMismatch)
        );
        assert_eq!(open_result(SECRET, T0, &s, &format!("{TEXT} ")), Err(SealError::TextMismatch));
    }

    #[test]
    fn forgeries_are_refused() {
        let s = sealed(T0, 7);
        let raw = URL_SAFE_NO_PAD.decode(&s).unwrap();
        // Every byte flipped, one at a time: version, nonce, ciphertext, tag.
        for i in 0..raw.len() {
            let mut b = raw.clone();
            b[i] ^= 0x01;
            let r = open_result(SECRET, T0, &URL_SAFE_NO_PAD.encode(&b), TEXT);
            assert!(matches!(r, Err(SealError::Invalid | SealError::Malformed)), "byte {i}: {r:?}");
        }
        // Truncated, extended, not base64url, empty.
        let short = URL_SAFE_NO_PAD.encode(&raw[..raw.len() - 1]);
        assert_eq!(open_result(SECRET, T0, &short, TEXT), Err(SealError::Invalid));
        let mut long = raw.clone();
        long.push(0);
        assert_eq!(open_result(SECRET, T0, &URL_SAFE_NO_PAD.encode(&long), TEXT), Err(SealError::Invalid));
        for bad in ["", "!!!!", "AQ", &format!("{s}=")] {
            assert_eq!(open_result(SECRET, T0, bad, TEXT), Err(SealError::Malformed), "{bad}");
        }
        // Another secret.
        let other = b"other test bytes, not a key too.";
        assert_eq!(open_result(other, T0, &s, TEXT), Err(SealError::Invalid));
    }

    #[test]
    fn another_purposes_seal_never_opens() {
        let p = payload(T0, TEXT);
        let quote = seal(SECRET, Purpose::Quote, day_of(T0), [7; NONCE_BYTES], &p).unwrap();
        assert_eq!(open_result(SECRET, T0, &quote, TEXT), Err(SealError::Invalid));
        let s = sealed(T0, 7);
        assert_eq!(open::<ResultPayload>(SECRET, Purpose::Quote, T0, &s), Err(SealError::Invalid));
    }

    #[test]
    fn the_payload_names_the_keys_day() {
        // Sealed under today's key but naming yesterday: refused.
        let mut p = payload(T0, TEXT);
        p.day -= 1;
        let s = seal(SECRET, Purpose::Result, day_of(T0), [1; NONCE_BYTES], &p).unwrap();
        assert_eq!(open_result(SECRET, T0, &s, TEXT), Err(SealError::Invalid));
        // An issue time outside the named day: refused.
        let mut p = payload(T0, TEXT);
        p.issued_ms = T0 + DAY_MS;
        let s = seal(SECRET, Purpose::Result, day_of(T0), [1; NONCE_BYTES], &p).unwrap();
        assert_eq!(open_result(SECRET, T0, &s, TEXT), Err(SealError::Invalid));
        // A payload of another shape.
        #[derive(Serialize)]
        struct Other {
            day: u64,
        }
        let s = seal(SECRET, Purpose::Result, day_of(T0), [1; NONCE_BYTES], &Other { day: day_of(T0) }).unwrap();
        assert_eq!(open_result(SECRET, T0, &s, TEXT), Err(SealError::Invalid));
    }

    /// Replay: each seal has its own nonce hash, the same however often it is
    /// opened, so the store can take it once.
    #[test]
    fn replays_share_one_nonce_hash() {
        let (a, b) = (sealed(T0, 7), sealed(T0, 8));
        let (oa1, oa2) = (open_result(SECRET, T0, &a, TEXT).unwrap(), open_result(SECRET, T0 + 5, &a, TEXT).unwrap());
        let ob = open_result(SECRET, T0, &b, TEXT).unwrap();
        assert_eq!(oa1.nonce_h(), oa2.nonce_h());
        assert_ne!(oa1.nonce_h(), ob.nonce_h());
        assert_ne!(a, b, "a fresh nonce makes a fresh seal of the same result");
    }

    #[test]
    fn keys_differ_by_day_and_the_secret_is_64_hex_digits() {
        assert_ne!(day_key(SECRET, 1), day_key(SECRET, 2));
        let digits = "3c".repeat(SECRET_BYTES);
        assert_eq!(parse_secret(&digits), Some([0x3c; SECRET_BYTES]));
        for bad in [
            digits[..62].to_owned(),
            format!("{digits}0"),
            digits.to_uppercase(),
            format!(" {}", &digits[1..]),
            "zz".repeat(32),
        ] {
            assert_eq!(parse_secret(&bad), None, "{bad}");
        }
        // The nonce hash, written out.
        let mut want = Sha256::new();
        want.update(b"seal_nonce_v1");
        want.update([7u8; NONCE_BYTES]);
        assert_eq!(seal_nonce_h(&[7; NONCE_BYTES]), <[u8; 32]>::from(want.finalize()));
        // The derivation, written out: HKDF-SHA256, no salt, info "seal" and
        // the day as 8 big-endian bytes.
        let mut want = [0u8; 32];
        let mut info = b"seal".to_vec();
        info.extend_from_slice(&20_405u64.to_be_bytes());
        Hkdf::<Sha256>::new(None, SECRET).expand(&info, &mut want).unwrap();
        assert_eq!(day_key(SECRET, 20_405), want);
    }

    #[test]
    fn the_seal_carries_no_text_derived_value_in_the_clear() {
        let s = sealed(T0, 7);
        let hash = text_sha256(TEXT);
        assert!(!s.contains(&hash[..16]));
        let raw = URL_SAFE_NO_PAD.decode(&s).unwrap();
        assert!(!raw.windows(8).any(|w| w == &hex::decode(&hash).unwrap()[..8]));
        assert!(!String::from_utf8_lossy(&raw).contains("sealed text"));
        assert_eq!(s.len(), sealed_len(serde_json::to_vec(&payload(T0, TEXT)).unwrap().len()));
    }
}
