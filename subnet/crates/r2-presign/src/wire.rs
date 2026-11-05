//! The signer Worker's internal messages: fixed-size binary records, so
//! both sides check a length before decoding anything and nothing is
//! parsed from text.
//!
//! A request is `[version, n]` then `n` entries of [`ENTRY_LEN`] bytes; a
//! reply is `[version, n]` then `n` reply entries, each at most
//! [`REPLY_ENTRY_MAX`] bytes. `n` is at most the caller's bound (the
//! signer's is one header plus the largest file table of any compiled
//! admission spec), so one `urls` call is one signer request.

use crate::copy::{CopyKind, CopySpec, MAX_COPY_URL_LEN};
use crate::presign::MAX_URL_LEN;

pub const VERSION: u8 = 1;

/// What one entry asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// The header object of `(cohort, commitment)`, bound to `sha256` and `len`.
    Header = 1,
    /// The file object `sha256` of `(cohort, commitment)`.
    File = 2,
    /// The upload-path canary: `commitment` is the nonce, and `sha256` and
    /// `len` must be the fixed canary body's.
    Canary = 3,
}

impl Op {
    fn from_u8(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Header),
            2 => Some(Self::File),
            3 => Some(Self::Canary),
            _ => None,
        }
    }
}

/// One mint request entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub op: Op,
    pub cohort: u64,
    pub commitment: [u8; 32],
    pub sha256: [u8; 32],
    pub len: u64,
    pub valid_secs: u32,
}

/// `op, cohort, commitment, sha256, len, valid_secs`.
pub const ENTRY_LEN: usize = 1 + 8 + 32 + 32 + 8 + 4;

/// The exact length of a request of `n` entries.
pub const fn request_len(n: usize) -> usize {
    2 + n * ENTRY_LEN
}

impl Entry {
    pub fn encode(&self, out: &mut [u8; ENTRY_LEN]) {
        out[0] = self.op as u8;
        out[1..9].copy_from_slice(&self.cohort.to_le_bytes());
        out[9..41].copy_from_slice(&self.commitment);
        out[41..73].copy_from_slice(&self.sha256);
        out[73..81].copy_from_slice(&self.len.to_le_bytes());
        out[81..85].copy_from_slice(&self.valid_secs.to_le_bytes());
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != ENTRY_LEN {
            return None;
        }
        let arr = |r: core::ops::Range<usize>| -> [u8; 32] {
            let mut a = [0u8; 32];
            a.copy_from_slice(&b[r]);
            a
        };
        Some(Self {
            op: Op::from_u8(b[0])?,
            cohort: u64::from_le_bytes(b[1..9].try_into().ok()?),
            commitment: arr(9..41),
            sha256: arr(41..73),
            len: u64::from_le_bytes(b[73..81].try_into().ok()?),
            valid_secs: u32::from_le_bytes(b[81..85].try_into().ok()?),
        })
    }
}

/// Writes a request of `entries` into `out`, returning its length, or
/// `None` when `out` is too small.
pub fn encode_request(entries: &[Entry], out: &mut [u8]) -> Option<usize> {
    let n = u8::try_from(entries.len()).ok()?;
    let total = request_len(entries.len());
    let out = out.get_mut(..total)?;
    out[0] = VERSION;
    out[1] = n;
    for (i, e) in entries.iter().enumerate() {
        let slot: &mut [u8; ENTRY_LEN] = (&mut out[2 + i * ENTRY_LEN..2 + (i + 1) * ENTRY_LEN]).try_into().ok()?;
        e.encode(slot);
    }
    Some(total)
}

/// The entries of a request whose length was already checked against
/// [`request_len`] of at most `max_entries`; `None` for anything malformed.
pub fn decode_request(b: &[u8], max_entries: usize) -> Option<impl Iterator<Item = Option<Entry>> + '_> {
    if b.len() < 2 || b[0] != VERSION {
        return None;
    }
    let n = usize::from(b[1]);
    if n == 0 || n > max_entries || b.len() != request_len(n) {
        return None;
    }
    Some(b[2..].as_chunks::<ENTRY_LEN>().0.iter().map(|c| Entry::decode(c)))
}

/// A reply entry's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    /// Length zero, above the signer's bound, or above R2's single PUT.
    Length = 1,
    /// Validity outside 1 s to 7 days.
    Validity = 2,
    /// A host, bucket or key outside its format (the signer's configuration).
    Format = 3,
    /// The signer's clock is outside SigV4's range.
    Clock = 4,
    /// An entry that did not decode.
    Malformed = 5,
    /// A canary entry whose length or digest is not the fixed canary body's.
    Canary = 6,
}

impl Status {
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Ok,
            1 => Self::Length,
            2 => Self::Validity,
            3 => Self::Format,
            4 => Self::Clock,
            5 => Self::Malformed,
            6 => Self::Canary,
            _ => return None,
        })
    }
}

/// `status, signed_at, valid_secs, content_length, checksum, url_len, url`.
pub const REPLY_FIXED_LEN: usize = 1 + 8 + 4 + 8 + 44 + 2;
pub const REPLY_ENTRY_MAX: usize = REPLY_FIXED_LEN + MAX_URL_LEN;
const _: () = assert!(MAX_URL_LEN <= u16::MAX as usize);

/// The largest reply of `n` entries.
pub const fn reply_max(n: usize) -> usize {
    2 + n * REPLY_ENTRY_MAX
}

/// One decoded reply entry, borrowing the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyEntry<'a> {
    pub status: Status,
    pub signed_at: u64,
    pub valid_secs: u32,
    pub content_length: u64,
    pub checksum: &'a str,
    pub url: &'a str,
}

/// Appends one reply entry at `out[at..]`, returning the new end.
pub fn encode_reply_entry(out: &mut [u8], at: usize, e: &ReplyEntry<'_>) -> Option<usize> {
    let url = e.url.as_bytes();
    let end = at.checked_add(REPLY_FIXED_LEN)?.checked_add(url.len())?;
    let slot = out.get_mut(at..end)?;
    if e.checksum.len() > 44 || url.len() > MAX_URL_LEN {
        return None;
    }
    slot[0] = e.status as u8;
    slot[1..9].copy_from_slice(&e.signed_at.to_le_bytes());
    slot[9..13].copy_from_slice(&e.valid_secs.to_le_bytes());
    slot[13..21].copy_from_slice(&e.content_length.to_le_bytes());
    slot[21..65].fill(b' ');
    slot[21..21 + e.checksum.len()].copy_from_slice(e.checksum.as_bytes());
    slot[65..67].copy_from_slice(&(url.len() as u16).to_le_bytes());
    slot[67..].copy_from_slice(url);
    Some(end)
}

/// Writes the reply header for `n` entries.
pub fn encode_reply_head(out: &mut [u8], n: u8) -> Option<usize> {
    let head = out.get_mut(..2)?;
    head[0] = VERSION;
    head[1] = n;
    Some(2)
}

/// Decodes a reply of exactly `expected` entries, refusing trailing bytes.
pub fn decode_reply(b: &[u8], expected: usize) -> Option<ReplyIter<'_>> {
    if b.len() < 2 || b[0] != VERSION || usize::from(b[1]) != expected || b.len() > reply_max(expected) {
        return None;
    }
    // Validate the whole reply first, so a consumer never acts on a prefix.
    let mut it = ReplyIter { rest: &b[2..], left: expected };
    for _ in 0..expected {
        it.next()??;
    }
    if !it.rest.is_empty() {
        return None;
    }
    Some(ReplyIter { rest: &b[2..], left: expected })
}

pub struct ReplyIter<'a> {
    rest: &'a [u8],
    left: usize,
}

impl<'a> Iterator for ReplyIter<'a> {
    type Item = Option<ReplyEntry<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        Some((|| {
            let b = self.rest;
            if b.len() < REPLY_FIXED_LEN {
                return None;
            }
            let url_len = usize::from(u16::from_le_bytes(b[65..67].try_into().ok()?));
            if url_len > MAX_URL_LEN || b.len() < REPLY_FIXED_LEN + url_len {
                return None;
            }
            let checksum = core::str::from_utf8(&b[21..65]).ok()?.trim_end_matches(' ');
            let url = core::str::from_utf8(&b[67..67 + url_len]).ok()?;
            let e = ReplyEntry {
                status: Status::from_u8(b[0])?,
                signed_at: u64::from_le_bytes(b[1..9].try_into().ok()?),
                valid_secs: u32::from_le_bytes(b[9..13].try_into().ok()?),
                content_length: u64::from_le_bytes(b[13..21].try_into().ok()?),
                checksum,
                url,
            };
            self.rest = &b[REPLY_FIXED_LEN + url_len..];
            Some(e)
        })())
    }
}

// ------------------------------------------------------------------ copy

/// Version of the copy messages (publication at scoring start).
pub const COPY_VERSION: u8 = 1;

/// `version, kind, cohort, commitment, sha256, etag, valid_secs`.
pub const COPY_REQUEST_LEN: usize = 1 + 1 + 8 + 32 + 32 + 16 + 4;

fn copy_kind(b: u8) -> Option<CopyKind> {
    match b {
        1 => Some(CopyKind::Header),
        2 => Some(CopyKind::File),
        _ => None,
    }
}

/// The copy request for `spec`, valid for `valid_secs`.
pub fn encode_copy_request(spec: &CopySpec, valid_secs: u32) -> [u8; COPY_REQUEST_LEN] {
    let mut out = [0u8; COPY_REQUEST_LEN];
    out[0] = COPY_VERSION;
    out[1] = match spec.kind {
        CopyKind::Header => 1,
        CopyKind::File => 2,
    };
    out[2..10].copy_from_slice(&spec.cohort.to_le_bytes());
    out[10..42].copy_from_slice(&spec.commitment);
    out[42..74].copy_from_slice(&spec.sha256);
    out[74..90].copy_from_slice(&spec.etag);
    out[90..94].copy_from_slice(&valid_secs.to_le_bytes());
    out
}

/// A copy request of exactly [`COPY_REQUEST_LEN`] bytes; `None` for
/// anything else.
pub fn decode_copy_request(b: &[u8]) -> Option<(CopySpec, u32)> {
    if b.len() != COPY_REQUEST_LEN || b[0] != COPY_VERSION {
        return None;
    }
    let mut commitment = [0u8; 32];
    commitment.copy_from_slice(&b[10..42]);
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&b[42..74]);
    let mut etag = [0u8; 16];
    etag.copy_from_slice(&b[74..90]);
    let spec = CopySpec {
        cohort: u64::from_le_bytes(b[2..10].try_into().ok()?),
        commitment,
        kind: copy_kind(b[1])?,
        sha256,
        etag,
    };
    Some((spec, u32::from_le_bytes(b[90..94].try_into().ok()?)))
}

/// `status, signed_at, valid_secs, url_len, url`.
pub const COPY_REPLY_FIXED_LEN: usize = 1 + 8 + 4 + 2;
/// The largest copy reply.
pub const COPY_REPLY_MAX: usize = COPY_REPLY_FIXED_LEN + MAX_COPY_URL_LEN;
const _: () = assert!(MAX_COPY_URL_LEN <= u16::MAX as usize);

/// One copy reply, borrowing its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyReply<'a> {
    pub status: Status,
    pub signed_at: u64,
    pub valid_secs: u32,
    pub url: &'a str,
}

/// Writes `r` into `out`, returning its length.
pub fn encode_copy_reply(out: &mut [u8], r: &CopyReply<'_>) -> Option<usize> {
    let url = r.url.as_bytes();
    if url.len() > MAX_COPY_URL_LEN {
        return None;
    }
    let end = COPY_REPLY_FIXED_LEN + url.len();
    let slot = out.get_mut(..end)?;
    slot[0] = r.status as u8;
    slot[1..9].copy_from_slice(&r.signed_at.to_le_bytes());
    slot[9..13].copy_from_slice(&r.valid_secs.to_le_bytes());
    slot[13..15].copy_from_slice(&(url.len() as u16).to_le_bytes());
    slot[15..].copy_from_slice(url);
    Some(end)
}

/// Decodes a whole copy reply, refusing trailing bytes.
pub fn decode_copy_reply(b: &[u8]) -> Option<CopyReply<'_>> {
    if b.len() < COPY_REPLY_FIXED_LEN || b.len() > COPY_REPLY_MAX {
        return None;
    }
    let url_len = usize::from(u16::from_le_bytes(b[13..15].try_into().ok()?));
    if b.len() != COPY_REPLY_FIXED_LEN + url_len {
        return None;
    }
    Some(CopyReply {
        status: Status::from_u8(b[0])?,
        signed_at: u64::from_le_bytes(b[1..9].try_into().ok()?),
        valid_secs: u32::from_le_bytes(b[9..13].try_into().ok()?),
        url: core::str::from_utf8(&b[15..]).ok()?,
    })
}
