//! Keys derived from `ACCOUNT_STORE_SECRET`, and the saved-link seal.
//!
//! - `K_owned(user_id) = HKDF-SHA256(ikm = secret, salt = user_id, info =
//!   "owned_v1")`: each account's saved links are sealed under its own key;
//! - `K_addr`, `K_quota`, `K_journal`, `K_ref`: HKDF with the store's salt
//!   and their own `info`. `addr_h = HMAC(K_addr, email_norm)` keys spacing,
//!   quotas and suppression; the quota object's name is `HMAC(K_quota,
//!   "<kind>:<key>")`; the journal holds `HMAC(K_journal, …)` where it names
//!   an account or an address; a session's opaque reference on
//!   `/account/sessions/` is `HMAC(K_ref, token_h)`.
//!
//! A dump of the store without the secret opens no saved link (the dump
//! property): `owned.enc_ref` is `nonce ‖ AES-256-GCM`, with the row's user
//! and detection bound as associated data, so a sealed value moved to another
//! row does not open.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use slop_ninja_detect_page::internal::Summary;
use slop_ninja_detect_page::{IdH, UserId};

/// The shortest accepted secret, in bytes (it is drawn at random).
pub const SECRET_MIN: usize = 32;
const STORE_SALT: &[u8] = b"slop_ninja_account_store_v1";
const OWNED_INFO: &[u8] = b"owned_v1";
const OWNED_AAD: &[u8] = b"slop_ninja_owned_v1\0";
/// The AES-GCM nonce length.
pub const NONCE_LEN: usize = 12;

/// The derived keys.
#[derive(Clone)]
pub struct Keys {
    secret: Vec<u8>,
    addr: [u8; 32],
    quota: [u8; 32],
    journal: [u8; 32],
    session_ref: [u8; 32],
}

impl core::fmt::Debug for Keys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Keys(..)")
    }
}

fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    // 32 bytes is far below HKDF-SHA256's output limit.
    let _ = Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out);
    out
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts a key of any length");
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

/// What a saved link holds, sealed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedRef {
    pub slug: String,
    pub summary: Summary,
}

impl Keys {
    /// Derives every key; `None` for a secret shorter than [`SECRET_MIN`].
    pub fn derive(secret: &[u8]) -> Option<Keys> {
        if secret.len() < SECRET_MIN {
            return None;
        }
        Some(Keys {
            secret: secret.to_vec(),
            addr: hkdf(secret, STORE_SALT, b"addr_v1"),
            quota: hkdf(secret, STORE_SALT, b"quota_v1"),
            journal: hkdf(secret, STORE_SALT, b"journal_v1"),
            session_ref: hkdf(secret, STORE_SALT, b"session_ref_v1"),
        })
    }

    /// `K_addr`, for `EmailNorm::keyed_hash`.
    pub fn k_addr(&self) -> &[u8] {
        &self.addr
    }

    /// The quota object's name for a bucket name (`"<kind>:<key>"`), hex.
    pub fn quota_name(&self, bucket_name: &str) -> String {
        hex::encode(hmac(&self.quota, &[bucket_name.as_bytes()]))
    }

    /// `user_h` of the journal.
    pub fn user_h(&self, uid: &UserId) -> [u8; 32] {
        hmac(&self.journal, &[b"user\0", &uid.0])
    }

    /// The journal's stand-in for an address an op named.
    pub fn journal_addr(&self, email_norm: &str) -> String {
        hex::encode(hmac(&self.journal, &[b"addr\0", email_norm.as_bytes()]))
    }

    /// A session's opaque reference (32 hex digits).
    pub fn session_ref(&self, token_h: &[u8]) -> String {
        hex::encode(&hmac(&self.session_ref, &[token_h])[..16])
    }

    fn owned_cipher(&self, uid: &UserId) -> Aes256Gcm {
        let k = hkdf(&self.secret, &uid.0, OWNED_INFO);
        Aes256Gcm::new_from_slice(&k).expect("a 32-byte key")
    }

    fn owned_aad(uid: &UserId, det: &IdH) -> Vec<u8> {
        [OWNED_AAD, &uid.0, &det.0].concat()
    }

    /// Seals a saved link for `user`'s row of detection `det`.
    pub fn seal_owned(&self, uid: &UserId, det: &IdH, value: &OwnedRef, nonce: [u8; NONCE_LEN]) -> Vec<u8> {
        let plain = serde_json::to_vec(value).expect("a saved link always serializes");
        let aad = Self::owned_aad(uid, det);
        let ct = self
            .owned_cipher(uid)
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &aad })
            .expect("AES-GCM sealing does not fail for in-memory input");
        [nonce.as_slice(), &ct].concat()
    }

    /// Opens a saved link; `None` when it does not open under this row.
    pub fn open_owned(&self, uid: &UserId, det: &IdH, sealed: &[u8]) -> Option<OwnedRef> {
        if sealed.len() <= NONCE_LEN {
            return None;
        }
        let (nonce, ct) = sealed.split_at(NONCE_LEN);
        let aad = Self::owned_aad(uid, det);
        let plain = self.owned_cipher(uid).decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: &aad }).ok()?;
        serde_json::from_slice(&plain).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_detect_page::Kind;

    fn summary() -> Summary {
        Summary {
            kind: Kind::PrivateText,
            band: None,
            short: None,
            strong_sections: None,
            total_sections: None,
            words: 120,
            created_at: 1,
            expires_at: Some(2),
        }
    }

    #[test]
    fn a_seal_opens_only_under_its_own_row() {
        let k = Keys::derive(&[7; 32]).unwrap();
        let (a, b) = (UserId([1; 16]), UserId([2; 16]));
        let (d1, d2) = (IdH([3; 32]), IdH([4; 32]));
        let v = OwnedRef { slug: "s".repeat(20), summary: summary() };
        let sealed = k.seal_owned(&a, &d1, &v, [9; 12]);
        assert_eq!(k.open_owned(&a, &d1, &sealed), Some(v.clone()));
        assert_eq!(k.open_owned(&b, &d1, &sealed), None);
        assert_eq!(k.open_owned(&a, &d2, &sealed), None);
        let other = Keys::derive(&[8; 32]).unwrap();
        assert_eq!(other.open_owned(&a, &d1, &sealed), None);
        assert!(!sealed.windows(20).any(|w| w == v.slug.as_bytes()));
    }

    #[test]
    fn short_secrets_are_refused_and_keys_differ() {
        assert!(Keys::derive(&[1; 31]).is_none());
        let k = Keys::derive(&[1; 32]).unwrap();
        assert_ne!(k.addr, k.quota);
        assert_ne!(k.journal, k.session_ref);
        assert_eq!(k.session_ref(&[0; 32]).len(), 32);
        assert_eq!(format!("{k:?}"), "Keys(..)");
    }
}
