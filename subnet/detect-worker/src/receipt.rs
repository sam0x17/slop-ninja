//! Content-free feedback receipts.
//!
//! Each result carries a receipt: a random 128-bit id, the band, the length
//! bin and an HMAC-SHA-256 tag over those three under the gate's in-memory
//! key. "Is this wrong?" sends the receipt and the button, nothing else: no
//! text and no hash of it. The gate accepts a receipt once, so a solved scan
//! yields at most one vote, and the band cannot be forged. The key lives
//! only in the gate object's memory; when the object restarts, older
//! receipts stop verifying, which only loses votes.

use crate::policy::{Band, LengthBin};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Domain separation for the tag.
const DOMAIN: &[u8] = b"slop_ninja_detect_feedback_v1\0";
/// Tag bytes kept (128 bits).
const TAG: usize = 16;

/// The button a reader pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// "I wrote this myself."
    WroteIt,
    /// "This was AI."
    WasAi,
}

impl Claim {
    pub fn code(self) -> &'static str {
        match self {
            Claim::WroteIt => "wrote_it",
            Claim::WasAi => "was_ai",
        }
    }

    pub fn from_code(s: &str) -> Option<Claim> {
        [Claim::WroteIt, Claim::WasAi].into_iter().find(|c| c.code() == s)
    }
}

fn tag(key: &[u8; 32], id: &[u8; 16], band: Band, len: LengthBin) -> Option<[u8; TAG]> {
    let mut m = HmacSha256::new_from_slice(key).ok()?;
    m.update(DOMAIN);
    m.update(id);
    m.update(band.code().as_bytes());
    m.update(b"\0");
    m.update(len.code().as_bytes());
    let full = m.finalize().into_bytes();
    let mut out = [0u8; TAG];
    out.copy_from_slice(full.get(..TAG)?);
    Some(out)
}

/// `<id hex>.<band>.<length bin>.<tag hex>`.
pub fn issue(key: &[u8; 32], id: &[u8; 16], band: Band, len: LengthBin) -> Option<String> {
    let t = tag(key, id, band, len)?;
    Some(format!("{}.{}.{}.{}", hex::encode(id), band.code(), len.code(), hex::encode(t)))
}

/// A verified receipt's id, band and length bin.
pub fn verify(key: &[u8; 32], receipt: &str) -> Option<([u8; 16], Band, LengthBin)> {
    let mut parts = receipt.split('.');
    let (id_hex, band, len, tag_hex) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || id_hex.len() != 32 || tag_hex.len() != 2 * TAG {
        return None;
    }
    let mut id = [0u8; 16];
    hex::decode_to_slice(id_hex, &mut id).ok()?;
    let (band, len) = (Band::from_code(band)?, LengthBin::from_code(len)?);
    let mut got = [0u8; TAG];
    hex::decode_to_slice(tag_hex, &mut got).ok()?;
    let want = tag(key, &id, band, len)?;
    let diff = want.iter().zip(got.iter()).fold(0u8, |a, (x, y)| a | (x ^ y));
    (diff == 0).then_some((id, band, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_verify_once_issued_and_resist_forging() {
        let key = [7u8; 32];
        let id = [3u8; 16];
        let r = issue(&key, &id, Band::Strong, LengthBin::Measured).unwrap();
        assert_eq!(verify(&key, &r), Some((id, Band::Strong, LengthBin::Measured)));
        let forged = r.replace(".strong.", ".weak.");
        assert_eq!(verify(&key, &forged), None);
        assert_eq!(verify(&[8u8; 32], &r), None);
        assert_eq!(verify(&key, "x"), None);
        assert_eq!(verify(&key, &format!("{r}.extra")), None);
        assert!(!r.contains("text"));
    }

    #[test]
    fn claims_round_trip() {
        for c in [Claim::WroteIt, Claim::WasAi] {
            assert_eq!(Claim::from_code(c.code()), Some(c));
        }
        assert_eq!(Claim::from_code("maybe"), None);
    }
}
