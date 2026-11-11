//! Content-free feedback receipts and the ledger that accepts each once.
//!
//! Each result carries a receipt: a random 128-bit id, the band, the length
//! bin and an HMAC-SHA-256 tag over those three under a key held only in the
//! memory of the `DetectFeedback` object. "Is this wrong?" sends the receipt
//! and the button, nothing else: no text and no hash of it. The ledger
//! accepts a receipt once, so a solved scan yields at most one vote, and the
//! band cannot be forged. Feedback has its own object, never the admission
//! gate, so a flood of feedback requests (which need no challenge) loads
//! only that object. When the object restarts, or its key changes at a new
//! UTC day or when its used-id set reaches its memory share, older receipts
//! stop verifying, which only loses votes.

use crate::policy::{Band, LengthBin};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeSet;

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

/// A receipt's parts, checked for shape only (no key): the id, band, length
/// bin and tag. The Worker drops a receipt that fails this before any object
/// call.
pub fn parse(receipt: &str) -> Option<([u8; 16], Band, LengthBin, [u8; TAG])> {
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
    Some((id, band, len, got))
}

/// A verified receipt's id, band and length bin.
pub fn verify(key: &[u8; 32], receipt: &str) -> Option<([u8; 16], Band, LengthBin)> {
    let (id, band, len, got) = parse(receipt)?;
    let want = tag(key, &id, band, len)?;
    let diff = want.iter().zip(got.iter()).fold(0u8, |a, (x, y)| a | (x ^ y));
    (diff == 0).then_some((id, band, len))
}

/// Bytes one used id holds in the ledger: 16 bytes of key in a B-tree node
/// of at most 11 keys that splits half full, the node's links, and the
/// allocator's header, rounded up.
pub const USED_ID_BYTES: usize = 64;

/// The `DetectFeedback` object's memory: today's key and the ids already
/// voted under it. Nothing is persisted.
#[derive(Debug)]
pub struct Ledger {
    day: u64,
    key: [u8; 32],
    used: BTreeSet<[u8; 16]>,
    max_used: usize,
}

impl Ledger {
    /// A ledger for UTC day `day` whose used-id set holds at most `max_used`
    /// ids (its memory share over [`USED_ID_BYTES`]).
    pub fn new(day: u64, key: [u8; 32], max_used: usize) -> Ledger {
        Ledger { day, key, used: BTreeSet::new(), max_used }
    }

    /// Whether the ledger needs a new key before its next call: a new UTC
    /// day, or a full used-id set (starting over then only loses the votes
    /// of receipts issued under the old key, and nothing is refused).
    pub fn stale(&self, day: u64) -> bool {
        day != self.day || self.used.len() >= self.max_used
    }

    /// Starts over with a new key for `day`.
    pub fn renew(&mut self, day: u64, key: [u8; 32]) {
        self.day = day;
        self.key = key;
        self.used = BTreeSet::new();
    }

    /// A receipt for one scored result.
    pub fn issue(&self, id: &[u8; 16], band: Band, len: LengthBin) -> Option<String> {
        issue(&self.key, id, band, len)
    }

    /// Accepts a valid receipt once: its band and length bin.
    pub fn vote(&mut self, receipt: &str) -> Option<(Band, LengthBin)> {
        let (id, band, len) = verify(&self.key, receipt)?;
        self.used.insert(id).then_some((band, len))
    }

    pub fn used(&self) -> usize {
        self.used.len()
    }
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
    fn the_ledger_accepts_each_receipt_once_and_stays_within_its_share() {
        let mut l = Ledger::new(1, [1u8; 32], 2);
        let a = l.issue(&[1u8; 16], Band::Weak, LengthBin::Longer).unwrap();
        let b = l.issue(&[2u8; 16], Band::Unclear, LengthBin::Measured).unwrap();
        assert_eq!(l.vote(&a), Some((Band::Weak, LengthBin::Longer)));
        assert_eq!(l.vote(&a), None, "a receipt counts once");
        assert_eq!(l.vote("junk"), None);
        assert!(!l.stale(1));
        assert_eq!(l.vote(&b), Some((Band::Unclear, LengthBin::Measured)));
        // Full: the next call starts over with a new key.
        assert!(l.stale(1));
        l.renew(1, [2u8; 32]);
        assert_eq!(l.used(), 0);
        let c = l.issue(&[3u8; 16], Band::Strong, LengthBin::Measured).unwrap();
        assert_eq!(l.vote(&a), None, "older receipts stop verifying");
        assert_eq!(l.vote(&c), Some((Band::Strong, LengthBin::Measured)));
        // A new UTC day starts over too.
        assert!(l.stale(2));
    }

    #[test]
    fn parse_checks_shape_only() {
        let r = issue(&[7u8; 32], &[3u8; 16], Band::Strong, LengthBin::Measured).unwrap();
        assert!(parse(&r).is_some());
        assert!(parse(&r.replace(".strong.", ".weak.")).is_some(), "shape only: the tag is checked by the ledger");
        for bad in ["", "x", "a.b.c.d", &format!("{r}.extra"), &r[1..]] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    /// Short-text results carry their bin, and the longest receipt fits the
    /// feedback body bound.
    #[test]
    fn short_receipts_verify_and_fit_the_feedback_body() {
        let key = [7u8; 32];
        let bands = [Band::VeryStrong, Band::Strong, Band::Unclear, Band::Weak, Band::NoStrongSigns];
        let bins = [LengthBin::Short24, LengthBin::Short50, LengthBin::Short100, LengthBin::Measured, LengthBin::Longer];
        for band in bands {
            for len in bins {
                let r = issue(&key, &[5u8; 16], band, len).unwrap();
                assert_eq!(verify(&key, &r), Some(([5u8; 16], band, len)));
                for claim in [Claim::WroteIt, Claim::WasAi] {
                    let body = format!(r#"{{"receipt":"{r}","claim":"{}"}}"#, claim.code());
                    assert!(body.len() <= crate::routes::FEEDBACK_BODY_BYTES, "{body}");
                }
            }
        }
        let r = issue(&key, &[5u8; 16], Band::NoStrongSigns, LengthBin::Short24).unwrap();
        assert_eq!(verify(&key, &r.replace(".24_49.", ".50_99.")), None);
    }

    #[test]
    fn claims_round_trip() {
        for c in [Claim::WroteIt, Claim::WasAi] {
            assert_eq!(Claim::from_code(c.code()), Some(c));
        }
        assert_eq!(Claim::from_code("maybe"), None);
    }
}
