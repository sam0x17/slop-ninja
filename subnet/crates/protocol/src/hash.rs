use core::fmt;

use sha2::{Digest as _, Sha256};

use crate::Result;
use crate::codec::{Canonical, Decoder, Encoder};

/// A 32-byte SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl Canonical for Digest {
    fn encode_to(&self, e: &mut Encoder) {
        e.raw(&self.0);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.bytes32()?))
    }
}

/// `SHA-256(u32_le(len(domain)) || domain || payload)`.
///
/// Every protocol digest names its domain, so equal bytes hashed for different
/// purposes never collide.
pub fn domain_hash(domain: &str, payload: &[u8]) -> Digest {
    let mut hash = Sha256::new();
    hash.update((domain.len() as u32).to_le_bytes());
    hash.update(domain.as_bytes());
    hash.update(payload);
    Digest(hash.finalize().into())
}

/// Plain SHA-256 of exact stored bytes, as an object store reports it.
pub fn object_sha256(bytes: &[u8]) -> Digest {
    Digest(Sha256::digest(bytes).into())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 15)] as char);
    }
    out
}
