//! The store's keys, hashes and document encryption (pure).
//!
//! - `DETECT_STORE_SECRET` (64 lowercase hex digits, a Worker secret) feeds
//!   HKDF-SHA256 with no salt and one `info` per key: `content` →
//!   `K_content`, `cases` → `K_cases`, `owner` → `K_owner`, and `seal` ‖
//!   day (8 bytes, big-endian) → `K_seal(day)`. `owner_h = HMAC(K_owner,
//!   "owner_v1" ‖ user_id)`: only this Worker holds the secret, so only it
//!   can derive an owner hash, and every other Worker passes the user id.
//! - Each slug pepper `DETECT_SLUG_PEPPER_V<n>` (64 lowercase hex digits)
//!   keys a secret page's id, `id_h = HMAC(pepper, "slug_v1" ‖ slug)`, and
//!   salts its document key, `K_doc = HKDF(ikm = slug, salt =
//!   "slop_ninja_detect_private_v1" ‖ pepper, info = "doc")`. New pages use
//!   the highest version; a page keeps the version it was made with
//!   (`detections.pepper_v`), so a pepper is never rotated for an existing
//!   page.
//! - A public page's id is `H("slug_v1" ‖ slug)`; a manage token's hash is
//!   `H("manage_v1" ‖ token)`; a seal's single use is `H("seal_nonce_v1" ‖
//!   nonce)`. `H` is SHA-256; every prefix is part of the store's schema
//!   version.
//! - A secret document is `nonce ‖ AES-256-GCM(K_doc, nonce, plaintext)`
//!   with the AAD [`DOC_AAD`] and a 96-bit nonce from the platform's CSPRNG.
//!   Each document is written once under its own key, so a nonce never
//!   repeats under a key.
//!
//! A dump of D1 and R2 without these secrets yields no plaintext, no secret
//! slug and no manage token: the slug is never stored, and a secret page's
//! id and key both need the pepper.
//!
//! Keys never appear in `Debug` output.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// A key or digest.
pub type Bytes32 = [u8; 32];

/// AES-GCM's nonce length.
pub const NONCE_BYTES: usize = 12;
/// AES-GCM's tag length.
pub const TAG_BYTES: usize = 16;
/// A secret document's associated data.
pub const DOC_AAD: &[u8] = b"slop_ninja_detect_doc_v1";
/// The salt prefix of a secret document's key.
pub const DOC_SALT: &[u8] = b"slop_ninja_detect_private_v1";
/// The domain prefix of public content keys (public runs only).
pub const CONTENT_DOMAIN: &[u8] = b"slop_ninja_detect_content_v1\0";
/// The var of a pepper version: `DETECT_SLUG_PEPPER_V1`, `_V2`, ...
pub const PEPPER_VAR_PREFIX: &str = "DETECT_SLUG_PEPPER_V";
/// The var of the store secret.
pub const STORE_SECRET_VAR: &str = "DETECT_STORE_SECRET";
/// The most pepper versions a deployment carries (each page read tries
/// every live version).
pub const MAX_PEPPERS: u32 = 8;

type HmacSha256 = Hmac<Sha256>;

/// `SHA-256(domain ‖ data)`.
pub fn h(domain: &[u8], data: &[u8]) -> Bytes32 {
    let mut d = Sha256::new();
    d.update(domain);
    d.update(data);
    d.finalize().into()
}

/// `HMAC-SHA-256(key, domain ‖ data)`.
pub fn hmac(key: &[u8], domain: &[u8], data: &[u8]) -> Bytes32 {
    // HMAC takes a key of any length.
    let Ok(mut m) = <HmacSha256 as Mac>::new_from_slice(key) else { return [0; 32] };
    m.update(domain);
    m.update(data);
    m.finalize().into_bytes().into()
}

/// HKDF-SHA256 to 32 bytes.
pub fn hkdf32(ikm: &[u8], salt: Option<&[u8]>, info: &[u8]) -> Bytes32 {
    let mut out = [0u8; 32];
    // 32 bytes is far under HKDF's 255 × 32 limit, so expand cannot fail.
    let _ = Hkdf::<Sha256>::new(salt, ikm).expand(info, &mut out);
    out
}

/// Exactly 64 lowercase hex digits.
pub fn parse_key_hex(s: &str) -> Option<Bytes32> {
    let b = s.trim().as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        let nib = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        *o = (nib(*b.get(2 * i)?)? << 4) | nib(*b.get(2 * i + 1)?)?;
    }
    Some(out)
}

/// A public page's id: `H("slug_v1" ‖ slug)`.
pub fn public_id_h(slug: &str) -> Bytes32 {
    h(b"slug_v1", slug.as_bytes())
}

/// A manage token's hash: `H("manage_v1" ‖ token)`.
pub fn manage_h(token: &str) -> Bytes32 {
    h(b"manage_v1", token.as_bytes())
}

/// A seal's single-use hash: `H("seal_nonce_v1" ‖ nonce)`.
pub fn seal_nonce_h(nonce: &[u8]) -> Bytes32 {
    h(b"seal_nonce_v1", nonce)
}

/// Constant-time equality of two digests.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The keys derived from `DETECT_STORE_SECRET`.
#[derive(Clone)]
pub struct StoreKeys {
    secret: Bytes32,
    content: Bytes32,
    cases: Bytes32,
    owner: Bytes32,
}

impl std::fmt::Debug for StoreKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKeys(…)")
    }
}

impl StoreKeys {
    /// From the secret's 64 hex digits; `None` for any other form.
    pub fn from_hex(secret_hex: &str) -> Option<StoreKeys> {
        parse_key_hex(secret_hex).map(|s| StoreKeys::derive(&s))
    }

    pub fn derive(secret: &Bytes32) -> StoreKeys {
        StoreKeys {
            secret: *secret,
            content: hkdf32(secret, None, b"content"),
            cases: hkdf32(secret, None, b"cases"),
            owner: hkdf32(secret, None, b"owner"),
        }
    }

    /// `K_seal(day)` for a UTC day number (ms / 86,400,000).
    pub fn seal_key(&self, day: u64) -> Bytes32 {
        let d = day.to_be_bytes();
        let info = [b's', b'e', b'a', b'l', d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]];
        hkdf32(&self.secret, None, &info)
    }

    /// `K_cases`, under which notices and quarantined copies are encrypted.
    pub fn cases_key(&self) -> &Bytes32 {
        &self.cases
    }

    /// `owner_h = HMAC(K_owner, "owner_v1" ‖ user_id)`.
    pub fn owner_h(&self, user_id: &[u8; 16]) -> Bytes32 {
        hmac(&self.owner, b"owner_v1", user_id)
    }

    /// A public run's content key: `HMAC(K_content, domain ‖ canon_v ‖ 0 ‖
    /// package_load ‖ 0 ‖ text)`. Keyed, so a dump of D1 alone cannot test
    /// whether a text was checked; public runs only.
    pub fn content_key(&self, canon_v: &str, package_load: &str, text: &str) -> Bytes32 {
        let Ok(mut m) = <HmacSha256 as Mac>::new_from_slice(&self.content) else { return [0; 32] };
        m.update(CONTENT_DOMAIN);
        m.update(canon_v.as_bytes());
        m.update(&[0]);
        m.update(package_load.as_bytes());
        m.update(&[0]);
        m.update(text.as_bytes());
        m.finalize().into_bytes().into()
    }
}

/// One slug pepper version.
#[derive(Clone)]
pub struct Pepper {
    pub v: u32,
    key: Bytes32,
}

impl std::fmt::Debug for Pepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pepper(v{})", self.v)
    }
}

impl Pepper {
    pub fn new(v: u32, key: Bytes32) -> Pepper {
        Pepper { v, key }
    }

    /// A secret page's id: `HMAC(pepper, "slug_v1" ‖ slug)`.
    pub fn id_h(&self, slug: &str) -> Bytes32 {
        hmac(&self.key, b"slug_v1", slug.as_bytes())
    }

    /// A secret page's document key.
    pub fn doc_key(&self, slug: &str) -> DocKey {
        let mut salt = Vec::with_capacity(DOC_SALT.len() + 32);
        salt.extend_from_slice(DOC_SALT);
        salt.extend_from_slice(&self.key);
        DocKey(hkdf32(slug.as_bytes(), Some(&salt), b"doc"))
    }
}

/// The deployment's pepper versions; new pages use the highest.
#[derive(Clone, Debug)]
pub struct Peppers {
    /// Ascending by version, at least one.
    list: Vec<Pepper>,
    /// The highest version.
    current: Pepper,
}

/// Why the pepper vars do not form a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PepperError {
    /// No `DETECT_SLUG_PEPPER_V1`.
    Missing,
    /// A var is not 64 lowercase hex digits.
    Form(u32),
}

impl Peppers {
    /// From `(version, var value)` pairs: version 1 must be present;
    /// versions run `1..=n` with no gap, at most [`MAX_PEPPERS`]; a gap ends
    /// the set (later versions are not read).
    pub fn from_vars(get: impl Fn(u32) -> Option<String>) -> Result<Peppers, PepperError> {
        let mut list = Vec::new();
        for v in 1..=MAX_PEPPERS {
            let Some(s) = get(v) else { break };
            let key = parse_key_hex(&s).ok_or(PepperError::Form(v))?;
            list.push(Pepper::new(v, key));
        }
        Peppers::new(list).ok_or(PepperError::Missing)
    }

    pub fn new(list: Vec<Pepper>) -> Option<Peppers> {
        let ok = list.iter().enumerate().all(|(i, p)| p.v == i as u32 + 1);
        let current = list.last().filter(|_| ok)?.clone();
        Some(Peppers { list, current })
    }

    /// The version new pages use.
    pub fn current(&self) -> &Pepper {
        &self.current
    }

    pub fn get(&self, v: u32) -> Option<&Pepper> {
        self.list.iter().find(|p| p.v == v)
    }

    /// Every live version, newest first: a page read tries each.
    pub fn all(&self) -> impl Iterator<Item = &Pepper> {
        self.list.iter().rev()
    }
}

/// A secret document's key.
pub struct DocKey(Bytes32);

impl std::fmt::Debug for DocKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DocKey(…)")
    }
}

/// A document that does not open (wrong key, edited bytes, short).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unopened;

impl DocKey {
    pub fn from_bytes(k: Bytes32) -> DocKey {
        DocKey(k)
    }

    /// `nonce ‖ ciphertext ‖ tag` of `plaintext` under this key.
    pub fn seal(&self, nonce: &[u8; NONCE_BYTES], plaintext: &[u8]) -> Vec<u8> {
        seal_with(&self.0, nonce, DOC_AAD, plaintext)
    }

    /// The plaintext of [`DocKey::seal`]'s output.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, Unopened> {
        open_with(&self.0, DOC_AAD, sealed)
    }

    /// `nonce ‖ ciphertext ‖ tag` under this key with another AAD: a paid
    /// job's objects, which never open as the page's document.
    pub fn seal_with_aad(&self, nonce: &[u8; NONCE_BYTES], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        seal_with(&self.0, nonce, aad, plaintext)
    }

    /// The plaintext of [`DocKey::seal_with_aad`]'s output.
    pub fn open_with_aad(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, Unopened> {
        open_with(&self.0, aad, sealed)
    }
}

/// `nonce ‖ AES-256-GCM(key, nonce, plaintext, aad)`.
pub fn seal_with(key: &Bytes32, nonce: &[u8; NONCE_BYTES], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes256Gcm::new_from_slice(key) else { return Vec::new() };
    let ct = cipher.encrypt(Nonce::from_slice(nonce), Payload { msg: plaintext, aad }).unwrap_or_default();
    let mut out = Vec::with_capacity(NONCE_BYTES + ct.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    out
}

/// Opens [`seal_with`]'s output.
pub fn open_with(key: &Bytes32, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, Unopened> {
    if sealed.len() < NONCE_BYTES + TAG_BYTES {
        return Err(Unopened);
    }
    let (nonce, ct) = sealed.split_at(NONCE_BYTES);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Unopened)?;
    cipher.decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad }).map_err(|_| Unopened)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexs(b: &[u8]) -> String {
        hex::encode(b)
    }

    /// RFC 5869 test case 1, through the same call the keys use.
    #[test]
    fn hkdf_matches_rfc_5869() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0u8..=0x0c).collect();
        let info: Vec<u8> = (0xf0u8..=0xf9).collect();
        let okm = hkdf32(&ikm, Some(&salt), &info);
        assert_eq!(hexs(&okm), "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf");
    }

    /// RFC 4231 test case 2 (key "Jefe"), with the domain as the message's
    /// first part.
    #[test]
    fn hmac_matches_rfc_4231() {
        let mac = hmac(b"Jefe", b"what do ya want ", b"for nothing?");
        assert_eq!(hexs(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn keys_are_separated_by_domain() {
        let k = StoreKeys::derive(&[7u8; 32]);
        let user = [1u8; 16];
        let owner = k.owner_h(&user);
        assert_ne!(owner, k.owner_h(&[2u8; 16]));
        assert_ne!(k.seal_key(20_000), k.seal_key(20_001));
        assert_ne!(k.seal_key(1), *k.cases_key());
        assert_ne!(k.content_key("canon_v1", "a", "bc"), k.content_key("canon_v1", "ab", "c"));
        // Another secret, another owner hash.
        assert_ne!(owner, StoreKeys::derive(&[8u8; 32]).owner_h(&user));
        assert_eq!(format!("{k:?}"), "StoreKeys(…)");
        assert_ne!(public_id_h("abcdefg"), manage_h("abcdefg"));
    }

    #[test]
    fn peppers_key_ids_and_documents() {
        let p1 = Pepper::new(1, [3u8; 32]);
        let p2 = Pepper::new(2, [4u8; 32]);
        let slug = "7hq2kd9m4xw8rt3vn6pb";
        assert_ne!(p1.id_h(slug), p2.id_h(slug));
        assert_ne!(p1.id_h(slug), public_id_h(slug));
        let doc = p1.doc_key(slug).seal(&[9u8; 12], b"{\"text\":\"x\"}");
        assert_eq!(&doc[..12], &[9u8; 12]);
        assert_eq!(p1.doc_key(slug).open(&doc).unwrap(), b"{\"text\":\"x\"}");
        assert_eq!(p2.doc_key(slug).open(&doc), Err(Unopened), "another pepper");
        assert_eq!(p1.doc_key("7hq2kd9m4xw8rt3vn6pc").open(&doc), Err(Unopened), "another slug");
        let mut edited = doc.clone();
        edited[20] ^= 1;
        assert_eq!(p1.doc_key(slug).open(&edited), Err(Unopened));
        assert_eq!(p1.doc_key(slug).open(&doc[..27]), Err(Unopened));
        // Another AAD does not open it either.
        assert!(
            open_with(&hkdf32(slug.as_bytes(), Some(&[DOC_SALT, &[3u8; 32]].concat()), b"doc"), b"x", &doc).is_err()
        );
        assert_eq!(format!("{p1:?}"), "Pepper(v1)");
    }

    #[test]
    fn pepper_vars_run_from_one_without_gaps() {
        let k = "ab".repeat(32);
        let set = Peppers::from_vars(|v| (v <= 2).then(|| k.clone())).unwrap();
        assert_eq!(set.current().v, 2);
        assert_eq!(set.all().map(|p| p.v).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(Peppers::from_vars(|_| None).unwrap_err(), PepperError::Missing);
        assert_eq!(Peppers::from_vars(|v| (v == 1).then(|| "AB".repeat(32))).unwrap_err(), PepperError::Form(1));
        // A gap ends the set.
        let set = Peppers::from_vars(|v| (v == 1 || v == 3).then(|| k.clone())).unwrap();
        assert_eq!(set.current().v, 1);
        assert!(Peppers::new(vec![Pepper::new(2, [0; 32])]).is_none());
        assert!(StoreKeys::from_hex(&"0".repeat(63)).is_none());
        assert!(StoreKeys::from_hex(&"0".repeat(64)).is_some());
    }

    #[test]
    fn ct_eq_compares_whole_values() {
        assert!(ct_eq(&[1, 2], &[1, 2]));
        assert!(!ct_eq(&[1, 2], &[1, 3]));
        assert!(!ct_eq(&[1, 2], &[1, 2, 3]));
    }
}
