//! The scorer's token count for a document, compiled in.
//!
//! The scorer (`inference/a-container-serve`, tokenizers 0.22.2 with onig)
//! frames a document as `[CLS]` + tokens + `[SEP]` under the pinned
//! `tokenizer.json` and answers a document over 2,048 tokens
//! `unsupported_length` for every artifact. The eval Worker cannot run that
//! library, so this crate counts with the same rules over a table derived
//! from the pinned file (`table/scorer-tokenizer-v1.bin`, built and checked
//! against the scorer's library by `reference/`):
//!
//! 1. **Raw added tokens** (`normalized: false`, the special tokens) are split
//!    out of the raw text, leftmost-longest; `lstrip` takes the whitespace
//!    before the match with it.
//! 2. Each remaining piece is normalized **NFC** by the scorer's own
//!    normalization crate and version.
//! 3. **Normalized added tokens** (whitespace runs, placeholders) are split
//!    out of each normalized piece, leftmost-longest.
//! 4. Each remaining piece is **pre-tokenized** by the byte-level split
//!    `'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+`,
//!    with the letter, number and whitespace classes read off the scorer's
//!    own pre-tokenizer for every code point.
//! 5. Each pre-token's bytes are merged by **BPE** in the scorer's order
//!    (lowest merge rank first, then leftmost position).
//!
//! Every added-token match is one token, every merged symbol one token, and
//! the frame adds two. Nothing is truncated.
//!
//! **Memory.** The table is static and read in place. A count holds one
//! symbol buffer and one merge queue sized for the longest pre-token seen so
//! far (12 and 24 bytes per byte of it, grown by doubling) and, for a piece
//! that is not already NFC, its normalized copy. Each of those allocations
//! is first offered to the caller's `admit` with its exact size and
//! alignment (the eval Worker admits them as arena blocks); a refusal ends
//! the count with [`Refused`].
//!
//! No global state, no unsafe code, no panics on any input.
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use unicode_normalization_alignments::{IsNormalized, UnicodeNormalization, is_nfc_quick};

/// The table's identifier: its layout and the counting rules above.
pub const TABLE_ID: &str = "slop_ninja_scorer_tokenizer_table_v1";

/// SHA-256 of the pinned `tokenizer.json` the table was derived from
/// (`artifact-check` `spec::TOKENIZER_SHA256`; the table header repeats it).
pub const SOURCE_TOKENIZER_SHA256: &str =
    "6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30";

/// SHA-256 of [`TABLE`] (checked by a test).
pub const TABLE_SHA256: &str = "018a069d786d8ba964061117839e1c938c12b2abfb8c3985a52dcce6999a64c6";

/// The derived table (layout in [`Table::parse`]).
pub static TABLE: &[u8] = include_bytes!("../table/scorer-tokenizer-v1.bin");

const MAGIC: &[u8; 8] = b"SNTOK001";
const HEADER: usize = 76;
const BYTE_IDS: usize = 512;
const MERGE_RECORD: usize = 8;
const RANGE_RECORD: usize = 8;
const NONE: u32 = u32::MAX;
/// The `lstrip` flag of an added token.
const LSTRIP: u8 = 1;

/// The caller refused a scratch allocation, or the table is malformed (a
/// build defect the crate's tests exclude).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// `admit` refused a block of this many bytes.
    Scratch(usize),
    /// [`TABLE`] does not parse.
    Table,
}

/// Offers an allocation of `(bytes, align)` about to be made; `false` refuses it.
pub type Admit<'a> = &'a mut dyn FnMut(usize, usize) -> bool;

/// The exact token count of `text` including the frame.
pub fn count(text: &str, admit: Admit<'_>) -> Result<u64, Refused> {
    let t = Table::parse(TABLE).ok_or(Refused::Table)?;
    run(&t, text, u64::MAX, admit).map(|n| n.unwrap_or(u64::MAX))
}

/// Whether `text`, framed, has at most `limit` tokens. Every token covers at
/// least one byte of the normalized text, so a text already in NFC whose
/// length plus the frame is within `limit` fits without being tokenized;
/// otherwise the count stops as soon as it passes `limit`.
pub fn fits(text: &str, limit: u64, admit: Admit<'_>) -> Result<bool, Refused> {
    let t = Table::parse(TABLE).ok_or(Refused::Table)?;
    let bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
    if bytes.saturating_add(t.specials) <= limit && is_nfc_quick(text.chars()) == IsNormalized::Yes
    {
        return Ok(true);
    }
    run(&t, text, limit, admit).map(|n| n.is_some())
}

/// The frame's token count (`[CLS]` and `[SEP]`).
pub fn frame_tokens() -> Result<u64, Refused> {
    Table::parse(TABLE)
        .map(|t| t.specials)
        .ok_or(Refused::Table)
}

// ------------------------------------------------------------------ table

/// The derived table, read in place. Layout (little-endian):
///
/// | offset | field |
/// |---|---|
/// | 0 | magic `SNTOK001` |
/// | 8 | SHA-256 of the source `tokenizer.json` |
/// | 40 | frame tokens (u32) |
/// | 44 | merges (u32 count) |
/// | 48, 52 | raw added tokens: count, section bytes |
/// | 56, 60 | normalized added tokens: count, section bytes |
/// | 64, 68, 72 | letter, number, whitespace ranges (u32 counts) |
/// | 76 | byte -> id, 256 x u16 (`0xffff`: not in the vocabulary) |
///
/// then the merges (key `left << 16 | right` u32, rank u16, new id u16,
/// sorted by key), the raw and normalized added tokens (each `len` u16,
/// flags u8, content), and the class ranges (`lo`, `hi` inclusive, u32 each,
/// sorted). Every other code point is in the remaining class
/// (`[^\s\p{L}\p{N}]`).
pub struct Table<'a> {
    pub source_sha256: &'a [u8],
    specials: u64,
    byte_ids: &'a [u8],
    merges: &'a [u8],
    raw: AddedList<'a>,
    norm: AddedList<'a>,
    letters: &'a [u8],
    numbers: &'a [u8],
    spaces: &'a [u8],
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([
        *s.first()?,
        *s.get(1)?,
        *s.get(2)?,
        *s.get(3)?,
    ]))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

fn usize_at(b: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32_at(b, at)?).ok()
}

impl<'a> Table<'a> {
    /// Parses `b` (no allocation); `None` when it is not a whole table.
    pub fn parse(b: &'a [u8]) -> Option<Table<'a>> {
        if b.get(..8)? != MAGIC {
            return None;
        }
        let source_sha256 = b.get(8..40)?;
        let specials = u64::from(u32_at(b, 40)?);
        let merges_n = usize_at(b, 44)?;
        let (raw_n, raw_len) = (usize_at(b, 48)?, usize_at(b, 52)?);
        let (norm_n, norm_len) = (usize_at(b, 56)?, usize_at(b, 60)?);
        let (l_n, n_n, s_n) = (usize_at(b, 64)?, usize_at(b, 68)?, usize_at(b, 72)?);
        let mut at = HEADER;
        let mut take = |len: usize| -> Option<&'a [u8]> {
            let s = b.get(at..at.checked_add(len)?)?;
            at += len;
            Some(s)
        };
        let byte_ids = take(BYTE_IDS)?;
        let merges = take(merges_n.checked_mul(MERGE_RECORD)?)?;
        let raw = AddedList::new(take(raw_len)?, raw_n)?;
        let norm = AddedList::new(take(norm_len)?, norm_n)?;
        let letters = take(l_n.checked_mul(RANGE_RECORD)?)?;
        let numbers = take(n_n.checked_mul(RANGE_RECORD)?)?;
        let spaces = take(s_n.checked_mul(RANGE_RECORD)?)?;
        if at != b.len() {
            return None;
        }
        Some(Table {
            source_sha256,
            specials,
            byte_ids,
            merges,
            raw,
            norm,
            letters,
            numbers,
            spaces,
        })
    }

    fn byte_id(&self, byte: u8) -> u32 {
        match u16_at(self.byte_ids, usize::from(byte) * 2) {
            Some(0xffff) | None => NONE,
            Some(id) => u32::from(id),
        }
    }

    /// The merge of `(left, right)`: `(rank, new id)`.
    fn merge(&self, left: u32, right: u32) -> Option<(u32, u32)> {
        if left > 0xffff || right > 0xffff {
            return None;
        }
        let key = (left << 16) | right;
        let (mut lo, mut hi) = (0usize, self.merges.len() / MERGE_RECORD);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = mid * MERGE_RECORD;
            let k = u32_at(self.merges, at)?;
            if k < key {
                lo = mid + 1;
            } else if k > key {
                hi = mid;
            } else {
                return Some((
                    u32::from(u16_at(self.merges, at + 4)?),
                    u32::from(u16_at(self.merges, at + 6)?),
                ));
            }
        }
        None
    }

    fn class(&self, c: char) -> Class {
        let c = u32::from(c);
        if in_ranges(self.letters, c) {
            Class::Letter
        } else if in_ranges(self.numbers, c) {
            Class::Number
        } else if in_ranges(self.spaces, c) {
            Class::Space
        } else {
            Class::Other
        }
    }
}

fn in_ranges(r: &[u8], c: u32) -> bool {
    let (mut lo, mut hi) = (0usize, r.len() / RANGE_RECORD);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (Some(a), Some(b)) = (
            u32_at(r, mid * RANGE_RECORD),
            u32_at(r, mid * RANGE_RECORD + 4),
        ) else {
            return false;
        };
        if c < a {
            hi = mid;
        } else if c > b {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

/// One list of added tokens, with the set of their first bytes.
struct AddedList<'a> {
    section: &'a [u8],
    first: [bool; 256],
}

impl<'a> AddedList<'a> {
    fn new(section: &'a [u8], n: usize) -> Option<AddedList<'a>> {
        let mut first = [false; 256];
        let mut at = 0usize;
        for _ in 0..n {
            let len = usize::from(u16_at(section, at)?);
            let content = section.get(at + 3..at + 3 + len)?;
            *first.get_mut(usize::from(*content.first()?))? = true;
            at += 3 + len;
        }
        (at == section.len()).then_some(AddedList { section, first })
    }

    /// The longest token matching at the start of `s`: `(length, flags)`.
    fn longest_at(&self, s: &[u8]) -> Option<(usize, u8)> {
        if !self
            .first
            .get(usize::from(*s.first()?))
            .copied()
            .unwrap_or(false)
        {
            return None;
        }
        let mut best: Option<(usize, u8)> = None;
        let mut at = 0usize;
        while at < self.section.len() {
            let len = usize::from(u16_at(self.section, at)?);
            let flags = *self.section.get(at + 2)?;
            let content = self.section.get(at + 3..at + 3 + len)?;
            if s.starts_with(content) && best.is_none_or(|(l, _)| len > l) {
                best = Some((len, flags));
            }
            at += 3 + len;
        }
        best
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Letter,
    Number,
    Space,
    Other,
}

// ---------------------------------------------------------------- counting

/// One split of a sentence by an added-token list: the pieces between
/// matches (`Some`) and the matches (`None`), in order.
///
/// Matches are leftmost-longest and do not overlap. An `lstrip` match takes
/// the whitespace (`char::is_whitespace`, the scorer's `\s*$`) at the end of
/// the piece before it, but never text an earlier match took.
fn split_added<'s>(
    s: &'s str,
    list: &AddedList<'_>,
    mut f: impl FnMut(Option<&'s str>) -> Result<bool, Refused>,
) -> Result<bool, Refused> {
    let b = s.as_bytes();
    let (mut from, mut p) = (0usize, 0usize);
    while p < b.len() {
        let Some((len, flags)) = b.get(p..).and_then(|rest| list.longest_at(rest)) else {
            p += 1;
            continue;
        };
        let mut start = p;
        if flags & LSTRIP != 0 {
            let before = s.get(..p).unwrap_or("");
            let stripped = before.trim_end_matches(char::is_whitespace).len();
            start = stripped.max(from);
        }
        if from < start && !f(Some(s.get(from..start).ok_or(Refused::Table)?))? {
            return Ok(false);
        }
        if !f(None)? {
            return Ok(false);
        }
        p += len;
        from = p;
    }
    if from < b.len() && !f(Some(s.get(from..).ok_or(Refused::Table)?))? {
        return Ok(false);
    }
    Ok(true)
}

/// The byte length of the next pre-token of `s` at `p` (a char boundary
/// before the end), under the byte-level split's alternatives in order.
fn next_pretoken(t: &Table<'_>, s: &str, p: usize) -> usize {
    let rest = s.get(p..).unwrap_or("");
    let mut chars = rest.chars();
    let Some(c0) = chars.next() else { return 0 };
    if c0 == '\'' {
        let r = rest.get(1..).unwrap_or("");
        for k in ["s", "t", "re", "ve", "m", "ll", "d"] {
            if r.starts_with(k) {
                return 1 + k.len();
            }
        }
    }
    // ` ?\p{L}+`, ` ?\p{N}+`, ` ?[^\s\p{L}\p{N}]+`
    let run = |from: usize, class: Class| -> usize {
        let mut end = from;
        for c in rest.get(from..).unwrap_or("").chars() {
            if t.class(c) != class {
                break;
            }
            end += c.len_utf8();
        }
        end
    };
    let c1 = chars.next();
    if c0 == ' '
        && let Some(c1) = c1
        && t.class(c1) != Class::Space
    {
        return run(1, t.class(c1));
    }
    let k0 = t.class(c0);
    if k0 != Class::Space {
        return run(0, k0);
    }
    // `\s+(?!\S)`, then `\s+`.
    let mut end = 0usize;
    let mut last = 0usize;
    let mut n = 0usize;
    for c in rest.chars() {
        if t.class(c) != Class::Space {
            break;
        }
        last = c.len_utf8();
        end += last;
        n += 1;
    }
    if end == rest.len() || n < 2 {
        end
    } else {
        end - last
    }
}

/// A merge queue entry: rank, then position (both ascending), then the new id.
fn entry(rank: u32, pos: usize, new_id: u32) -> Reverse<u64> {
    Reverse(
        (u64::from(rank) << 48) | ((pos as u64 & 0xffff_ffff) << 16) | u64::from(new_id & 0xffff),
    )
}

#[derive(Clone, Copy)]
struct Sym {
    c: u32,
    prev: u32,
    next: u32,
}

/// The count's reusable scratch.
struct Work {
    sym: Vec<Sym>,
    queue: Vec<Reverse<u64>>,
}

impl Work {
    /// Room for a pre-token of `n` bytes: `n` symbols and at most `3n`
    /// queue entries (`n - 1` pairs, then at most two per merge).
    fn reserve(&mut self, n: usize, admit: Admit<'_>) -> Result<(), Refused> {
        if n <= self.sym.capacity() {
            return Ok(());
        }
        let cap = n.max(self.sym.capacity().saturating_mul(2)).max(64);
        let sym_bytes = cap
            .checked_mul(size_of::<Sym>())
            .ok_or(Refused::Scratch(usize::MAX))?;
        let queue_bytes = cap
            .checked_mul(3 * size_of::<Reverse<u64>>())
            .ok_or(Refused::Scratch(usize::MAX))?;
        self.sym = Vec::new();
        self.queue = Vec::new();
        if !admit(sym_bytes, align_of::<Sym>()) {
            return Err(Refused::Scratch(sym_bytes));
        }
        self.sym = Vec::with_capacity(cap);
        if !admit(queue_bytes, align_of::<Reverse<u64>>()) {
            return Err(Refused::Scratch(queue_bytes));
        }
        self.queue = Vec::with_capacity(cap * 3);
        Ok(())
    }

    /// The BPE token count of one pre-token.
    fn bpe(&mut self, t: &Table<'_>, piece: &[u8], admit: Admit<'_>) -> Result<u64, Refused> {
        self.reserve(piece.len(), admit)?;
        self.sym.clear();
        for &byte in piece {
            let c = t.byte_id(byte);
            if c != NONE {
                let i = self.sym.len() as u32;
                self.sym.push(Sym {
                    c,
                    prev: i.wrapping_sub(1),
                    next: i + 1,
                });
            }
        }
        let m = self.sym.len();
        if let Some(last) = self.sym.last_mut() {
            last.next = NONE;
        }
        if m < 2 {
            return Ok(m as u64);
        }
        let mut queue = BinaryHeap::from(std::mem::take(&mut self.queue));
        queue.clear();
        for (i, w) in self.sym.windows(2).enumerate() {
            if let [a, b] = w
                && let Some((rank, new_id)) = t.merge(a.c, b.c)
            {
                queue.push(entry(rank, i, new_id));
            }
        }
        let mut alive = m;
        while let Some(Reverse(k)) = queue.pop() {
            let pos = ((k >> 16) & 0xffff_ffff) as usize;
            let new_id = (k & 0xffff) as u32;
            let Some(&here) = self.sym.get(pos) else {
                continue;
            };
            if here.c == NONE || here.next == NONE {
                continue;
            }
            let next_pos = here.next as usize;
            let Some(&right) = self.sym.get(next_pos) else {
                continue;
            };
            if t.merge(here.c, right.c).is_none_or(|(_, id)| id != new_id) {
                continue;
            }
            if let Some(s) = self.sym.get_mut(pos) {
                s.c = new_id;
                s.next = right.next;
            }
            if let Some(s) = self.sym.get_mut(next_pos) {
                s.c = NONE;
            }
            alive -= 1;
            if right.next != NONE
                && let Some(s) = self.sym.get_mut(right.next as usize)
            {
                s.prev = pos as u32;
            }
            if here.prev != NONE
                && let Some(prev) = self.sym.get(here.prev as usize)
                && let Some((rank, id)) = t.merge(prev.c, new_id)
            {
                queue.push(entry(rank, here.prev as usize, id));
            }
            if right.next != NONE
                && let Some(next) = self.sym.get(right.next as usize)
                && let Some((rank, id)) = t.merge(new_id, next.c)
            {
                queue.push(entry(rank, pos, id));
            }
        }
        self.queue = queue.into_vec();
        Ok(alive as u64)
    }
}

/// NFC of `s` by the scorer's normalizer, borrowed when `s` already is.
fn nfc<'s>(s: &'s str, admit: Admit<'_>) -> Result<Cow<'s, str>, Refused> {
    if is_nfc_quick(s.chars()) == IsNormalized::Yes {
        return Ok(Cow::Borrowed(s));
    }
    let len: usize = s.nfc().map(|(c, _)| c.len_utf8()).sum();
    if !admit(len, 1) {
        return Err(Refused::Scratch(len));
    }
    let mut out = String::with_capacity(len);
    out.extend(s.nfc().map(|(c, _)| c));
    Ok(Cow::Owned(out))
}

/// The framed count of `text`, or `None` once it passes `cap`.
fn run(t: &Table<'_>, text: &str, cap: u64, admit: Admit<'_>) -> Result<Option<u64>, Refused> {
    let mut n = t.specials;
    if n > cap {
        return Ok(None);
    }
    let mut work = Work {
        sym: Vec::new(),
        queue: Vec::new(),
    };
    let whole = split_added(text, &t.raw, |piece| {
        let Some(piece) = piece else {
            n += 1;
            return Ok(n <= cap);
        };
        let normalized = nfc(piece, &mut *admit)?;
        split_added(&normalized, &t.norm, |piece| {
            let Some(piece) = piece else {
                n += 1;
                return Ok(n <= cap);
            };
            let mut p = 0usize;
            while p < piece.len() {
                let len = next_pretoken(t, piece, p);
                if len == 0 {
                    return Err(Refused::Table);
                }
                let bytes = piece.as_bytes().get(p..p + len).ok_or(Refused::Table)?;
                n += work.bpe(t, bytes, &mut *admit)?;
                if n > cap {
                    return Ok(false);
                }
                p += len;
            }
            Ok(true)
        })
    })?;
    Ok(whole.then_some(n))
}

#[cfg(test)]
mod tests;
