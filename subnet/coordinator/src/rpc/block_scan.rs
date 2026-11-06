//! A streamed `chain_getBlock` reply, matched against one extrinsic.
//!
//! The signing journal reads a block body only to learn whether, and at
//! which index, it holds one attempt's exact bytes ([`crate::signing`]'s
//! inclusion step). A body has no size the journal can bound ahead: a block
//! that enacts a runtime upgrade carries the whole runtime code as one
//! extrinsic (testnet block 8380857, the 486 -> 488 upgrade: 5,740,192 reply
//! bytes, one extrinsic of 5,734,672 hex characters, the journal's own
//! commit beside it). So the reply is never held: the Worker feeds its
//! chunks to a [`BlockScan`] as they arrive, which keeps a fixed amount of
//! state (the target hex, a few small captures and the reply schema's
//! container path), compares every element of `extrinsics` with the target
//! byte by byte, and answers only the element count and the index of the
//! match ([`BlockMatch`]). Memory does not grow with the reply; its time is
//! bounded by the call's platform timeout.
//!
//! The scan accepts exactly what `serde_json` plus the former whole-body
//! checks accepted for this call, in any key order (the node sends
//! `extrinsics` before `header`): a JSON-RPC 2.0 envelope with the request's
//! `id`; an `error` member is [`RpcError::Remote`]; a `null` result is
//! [`RpcError::NotServed`]; `result.block.header.number` must be the
//! requested height ([`RpcError::Inconsistent`] otherwise); every extrinsic
//! is `0x`-prefixed hex of even length. A reply that ends before its root
//! object closes is [`RpcError::Malformed`], never "not in this block": only
//! a complete, well-formed body can answer that the bytes are absent.

use super::{RpcError, hex_number_str};
use serde::{Deserialize, Serialize};

/// What a scanned block body says about one extrinsic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockMatch {
    /// Elements of the block's `extrinsics`.
    pub count: u32,
    /// The first element equal to the target (hex, case-insensitive).
    pub index: Option<u32>,
}

/// The deepest container of a `chain_getBlock` reply (`SignedBlock` JSON):
/// root object, `result`, `block`, `header`, `digest`, `logs` (six levels;
/// `result.justifications` nests three: an array of `[engine, bytes]`
/// pairs). A deeper reply is not this call's shape.
const MAX_DEPTH: usize = 6;
/// The longest member name the scan routes on (`extrinsics`).
const KEY_BYTES: usize = 10;
/// The longest scalar it captures: a header `number` (`0x` and up to 16
/// hex digits, `hex_number`'s bound) or an `id` (a `u64`, 20 digits).
const CAPTURE_BYTES: usize = 20;

/// Which member of the reply schema a value fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Root,
    Jsonrpc,
    Id,
    Error,
    Result,
    Block,
    Header,
    Number,
    Extrinsics,
    Extrinsic,
    /// Anything else: checked for structure, otherwise skipped.
    Other,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    array: bool,
    slot: Slot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lex {
    /// A value for `slot` is next (after whitespace).
    Value(Slot),
    /// Just after `{`: a member name or `}`.
    ObjectStart,
    /// After `,` in an object: a member name.
    Key,
    /// Inside a member name ([`escape_step`] state).
    InKey { escape: u8 },
    /// After a member name: `:`.
    Colon,
    /// Just after `[`: a value or `]`.
    ArrayStart,
    /// After a value inside a container: `,` or its close.
    After,
    /// Inside a string value for `slot` ([`escape_step`] state).
    InString { slot: Slot, escape: u8 },
    /// Inside a number for `slot`.
    InNumber { slot: Slot, at: Num },
    /// Inside `true`, `false` or `null` for `slot`, `pos` bytes in.
    InWord { slot: Slot, word: &'static [u8], pos: u8 },
    /// The root object closed: only whitespace may follow.
    Done,
}

/// The incremental matcher; see the module documentation.
pub struct BlockScan {
    id: u64,
    number: u64,
    /// The target, lowercase, `0x` included.
    target: Vec<u8>,
    lex: Lex,
    stack: [Frame; MAX_DEPTH],
    depth: usize,
    key: [u8; KEY_BYTES],
    key_len: usize,
    /// The member name was longer than [`KEY_BYTES`] or held an escape.
    key_other: bool,
    capture: [u8; CAPTURE_BYTES],
    capture_len: usize,
    capture_over: bool,
    // The current extrinsic string: its length so far and whether it still
    // equals the target's prefix.
    ext_len: usize,
    ext_same: bool,
    // Members seen.
    jsonrpc_ok: Option<bool>,
    id_seen: Option<Option<u64>>,
    error: bool,
    result: Option<bool>,
    block: bool,
    header: bool,
    header_number: Option<Result<u64, RpcError>>,
    extrinsics: bool,
    count: u32,
    index: Option<u32>,
    failed: Option<RpcError>,
}

/// Where a JSON number is (RFC 8259 grammar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Num {
    Minus,
    Zero,
    Int,
    Dot,
    Frac,
    E,
    ESign,
    Exp,
}

impl Num {
    fn start(b: u8) -> Option<Self> {
        match b {
            b'-' => Some(Num::Minus),
            b'0' => Some(Num::Zero),
            b'1'..=b'9' => Some(Num::Int),
            _ => None,
        }
    }

    /// The state after `b`, or `None` where the number cannot go on.
    fn step(self, b: u8) -> Option<Self> {
        use Num::*;
        match (self, b) {
            (Minus, b'0') => Some(Zero),
            (Minus, b'1'..=b'9') | (Int, b'0'..=b'9') => Some(Int),
            (Zero | Int, b'.') => Some(Dot),
            (Dot | Frac, b'0'..=b'9') => Some(Frac),
            (Zero | Int | Frac, b'e' | b'E') => Some(E),
            (E, b'+' | b'-') => Some(ESign),
            (E | ESign | Exp, b'0'..=b'9') => Some(Exp),
            _ => None,
        }
    }

    fn complete(self) -> bool {
        matches!(self, Num::Zero | Num::Int | Num::Frac | Num::Exp)
    }
}

/// One byte after a backslash or inside `\uXXXX`, from escape state `e`
/// (`0`: none; `1`: just after the backslash; `10 + k`: `k` hex digits of a
/// `\u` escape still to come). The caller handles `e == 0`.
fn escape_step(e: u8, b: u8) -> Result<u8, RpcError> {
    match e {
        1 => match b {
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => Ok(0),
            b'u' => Ok(14),
            _ => Err(RpcError::Malformed),
        },
        11..=14 if b.is_ascii_hexdigit() => Ok(if e == 11 { 0 } else { e - 1 }),
        _ => Err(RpcError::Malformed),
    }
}

fn whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

impl BlockScan {
    /// A scan of the reply to request `id` for block `number`, looking for
    /// `target` (an attempt's extrinsic hex).
    pub fn new(id: u64, number: u64, target: &str) -> Self {
        Self {
            id,
            number,
            target: target.as_bytes().to_ascii_lowercase(),
            lex: Lex::Value(Slot::Root),
            stack: [Frame { array: false, slot: Slot::Other }; MAX_DEPTH],
            depth: 0,
            key: [0; KEY_BYTES],
            key_len: 0,
            key_other: false,
            capture: [0; CAPTURE_BYTES],
            capture_len: 0,
            capture_over: false,
            ext_len: 0,
            ext_same: true,
            jsonrpc_ok: None,
            id_seen: None,
            error: false,
            result: None,
            block: false,
            header: false,
            header_number: None,
            extrinsics: false,
            count: 0,
            index: None,
            failed: None,
        }
    }

    /// Feeds the next bytes of the reply. `false` once the reply cannot be
    /// this call's shape: the caller may stop reading ([`Self::finish`]
    /// answers why).
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        if self.failed.is_some() {
            return false;
        }
        for &b in bytes {
            if let Err(e) = self.byte(b) {
                self.failed = Some(e);
                return false;
            }
        }
        true
    }

    /// The answer once the whole reply was fed.
    pub fn finish(self) -> Result<BlockMatch, RpcError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.lex != Lex::Done {
            return Err(RpcError::Malformed);
        }
        if self.jsonrpc_ok != Some(true) || self.id_seen != Some(Some(self.id)) {
            return Err(RpcError::Malformed);
        }
        if self.error {
            return Err(RpcError::Remote);
        }
        match self.result {
            None => return Err(RpcError::Malformed),
            Some(false) => return Err(RpcError::NotServed),
            Some(true) => {}
        }
        if !self.block || !self.header || !self.extrinsics {
            return Err(RpcError::Malformed);
        }
        match self.header_number {
            Some(Ok(n)) if n == self.number => {}
            Some(Ok(_)) => return Err(RpcError::Inconsistent),
            Some(Err(e)) => return Err(e),
            None => return Err(RpcError::Malformed),
        }
        Ok(BlockMatch {
            count: self.count,
            index: self.index,
        })
    }

    fn parent(&self) -> Option<Frame> {
        self.depth.checked_sub(1).and_then(|i| self.stack.get(i).copied())
    }

    /// The slot of the member named by the captured key, inside `parent`.
    /// A member the scan routes on may appear once.
    fn member(&mut self, parent: Slot) -> Result<Slot, RpcError> {
        let key = if self.key_other { &[][..] } else { self.key.get(..self.key_len).unwrap_or(&[]) };
        let slot = match (parent, key) {
            (Slot::Root, b"jsonrpc") => Slot::Jsonrpc,
            (Slot::Root, b"id") => Slot::Id,
            (Slot::Root, b"error") => Slot::Error,
            (Slot::Root, b"result") => Slot::Result,
            (Slot::Result, b"block") => Slot::Block,
            (Slot::Block, b"header") => Slot::Header,
            (Slot::Block, b"extrinsics") => Slot::Extrinsics,
            (Slot::Header, b"number") => Slot::Number,
            _ => Slot::Other,
        };
        let seen = match slot {
            Slot::Jsonrpc => self.jsonrpc_ok.is_some(),
            Slot::Id => self.id_seen.is_some(),
            Slot::Error => self.error,
            Slot::Result => self.result.is_some(),
            Slot::Block => self.block,
            Slot::Header => self.header,
            Slot::Number => self.header_number.is_some(),
            Slot::Extrinsics => self.extrinsics,
            _ => false,
        };
        if seen {
            return Err(RpcError::Malformed);
        }
        if slot == Slot::Error {
            self.error = true;
        }
        Ok(slot)
    }

    fn push(&mut self, array: bool, slot: Slot) -> Result<(), RpcError> {
        let frame = self.stack.get_mut(self.depth).ok_or(RpcError::Malformed)?;
        *frame = Frame { array, slot };
        self.depth += 1;
        Ok(())
    }

    /// A value of `slot` ended: the lexer continues in its container.
    fn ended(&mut self) {
        self.lex = if self.depth == 0 { Lex::Done } else { Lex::After };
    }

    fn start_capture(&mut self) {
        self.capture_len = 0;
        self.capture_over = false;
    }

    fn captured(&mut self, b: u8) {
        match self.capture.get_mut(self.capture_len) {
            Some(slot) => {
                *slot = b;
                self.capture_len += 1;
            }
            None => self.capture_over = true,
        }
    }

    fn capture_str(&self) -> Option<&str> {
        if self.capture_over {
            return None;
        }
        std::str::from_utf8(self.capture.get(..self.capture_len)?).ok()
    }

    fn value(&mut self, slot: Slot, b: u8) -> Result<(), RpcError> {
        // What each routed member may be.
        let object_only = matches!(slot, Slot::Root | Slot::Block | Slot::Header);
        match b {
            b'{' => {
                if !(object_only || matches!(slot, Slot::Result | Slot::Error | Slot::Other)) {
                    return Err(RpcError::Malformed);
                }
                match slot {
                    Slot::Result => self.result = Some(true),
                    Slot::Block => self.block = true,
                    Slot::Header => self.header = true,
                    _ => {}
                }
                self.push(false, slot)?;
                self.lex = Lex::ObjectStart;
            }
            b'[' => {
                if !matches!(slot, Slot::Extrinsics | Slot::Error | Slot::Other) {
                    return Err(RpcError::Malformed);
                }
                if slot == Slot::Extrinsics {
                    self.extrinsics = true;
                }
                self.push(true, slot)?;
                self.lex = Lex::ArrayStart;
            }
            b'"' => {
                match slot {
                    Slot::Jsonrpc | Slot::Number => self.start_capture(),
                    Slot::Extrinsic => {
                        self.ext_len = 0;
                        self.ext_same = true;
                    }
                    Slot::Error | Slot::Other => {}
                    _ => return Err(RpcError::Malformed),
                }
                self.lex = Lex::InString { slot, escape: 0 };
            }
            b't' | b'f' | b'n' => {
                // `null` is the only literal a result may be.
                if !(matches!(slot, Slot::Error | Slot::Other) || (slot == Slot::Result && b == b'n')) {
                    return Err(RpcError::Malformed);
                }
                let word: &'static [u8] = match b {
                    b't' => b"true",
                    b'f' => b"false",
                    _ => b"null",
                };
                self.lex = Lex::InWord { slot, word, pos: 1 };
            }
            _ => {
                let at = Num::start(b).ok_or(RpcError::Malformed)?;
                if !matches!(slot, Slot::Id | Slot::Error | Slot::Other) {
                    return Err(RpcError::Malformed);
                }
                self.start_capture();
                self.captured(b);
                self.lex = Lex::InNumber { slot, at };
            }
        }
        Ok(())
    }

    fn string_byte(&mut self, slot: Slot, escape: u8, b: u8) -> Result<(), RpcError> {
        if b < 0x20 {
            return Err(RpcError::Malformed);
        }
        if escape != 0 {
            // Only skipped strings reach here: the routed ones refuse `\`.
            self.lex = Lex::InString { slot, escape: escape_step(escape, b)? };
            return Ok(());
        }
        match (slot, b) {
            (Slot::Extrinsic, b'"') => {
                // `0x` and an even number of hex digits (`hex_string`).
                if self.ext_len < 2 || !self.ext_len.is_multiple_of(2) {
                    return Err(RpcError::Malformed);
                }
                if self.ext_same && self.ext_len == self.target.len() && self.index.is_none() {
                    self.index = Some(self.count);
                }
                self.count = self.count.checked_add(1).ok_or(RpcError::Malformed)?;
                self.ended();
            }
            (Slot::Extrinsic, _) => {
                let ok = match self.ext_len {
                    0 => b == b'0',
                    1 => b == b'x',
                    _ => b.is_ascii_hexdigit(),
                };
                if !ok {
                    return Err(RpcError::Malformed);
                }
                if self.ext_same {
                    self.ext_same = self.target.get(self.ext_len) == Some(&b.to_ascii_lowercase());
                }
                self.ext_len = self.ext_len.saturating_add(1);
            }
            (Slot::Jsonrpc | Slot::Number, b'"') => {
                match slot {
                    Slot::Jsonrpc => self.jsonrpc_ok = Some(self.capture_str() == Some("2.0")),
                    _ => {
                        self.header_number =
                            Some(self.capture_str().ok_or(RpcError::Malformed).and_then(hex_number_str))
                    }
                }
                self.ended();
            }
            (Slot::Jsonrpc | Slot::Number, b'\\') => return Err(RpcError::Malformed),
            (Slot::Jsonrpc | Slot::Number, _) => self.captured(b),
            (_, b'"') => self.ended(),
            (_, b'\\') => self.lex = Lex::InString { slot, escape: 1 },
            _ => {}
        }
        Ok(())
    }

    /// A number ended (at the byte after it, which its container reads).
    fn number_end(&mut self, slot: Slot) {
        if slot == Slot::Id {
            // `as_u64`: plain digits that fit.
            let id = self
                .capture_str()
                .filter(|t| t.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|t| t.parse::<u64>().ok());
            self.id_seen = Some(id);
        }
        self.ended();
    }

    /// `true`, `false` or `null` ended at its last byte.
    fn word_end(&mut self, slot: Slot, word: &'static [u8]) {
        if slot == Slot::Result && word == b"null" {
            self.result = Some(false);
        }
        self.ended();
    }

    fn close(&mut self, array: bool) -> Result<(), RpcError> {
        match self.parent() {
            Some(f) if f.array == array => {
                self.depth -= 1;
                self.ended();
                Ok(())
            }
            _ => Err(RpcError::Malformed),
        }
    }

    fn byte(&mut self, b: u8) -> Result<(), RpcError> {
        match self.lex {
            Lex::InString { slot, escape } => return self.string_byte(slot, escape, b),
            Lex::InKey { escape } => {
                if b < 0x20 {
                    return Err(RpcError::Malformed);
                }
                if escape != 0 {
                    self.lex = Lex::InKey { escape: escape_step(escape, b)? };
                } else if b == b'"' {
                    self.lex = Lex::Colon;
                } else if b == b'\\' {
                    self.key_other = true;
                    self.lex = Lex::InKey { escape: 1 };
                } else if let Some(k) = self.key.get_mut(self.key_len) {
                    *k = b;
                    self.key_len += 1;
                } else {
                    self.key_other = true;
                }
                return Ok(());
            }
            Lex::InWord { slot, word, pos } => {
                if word.get(usize::from(pos)) != Some(&b) {
                    return Err(RpcError::Malformed);
                }
                if usize::from(pos) + 1 == word.len() {
                    self.word_end(slot, word);
                } else {
                    self.lex = Lex::InWord { slot, word, pos: pos + 1 };
                }
                return Ok(());
            }
            Lex::InNumber { slot, at } => {
                if let Some(next) = at.step(b) {
                    self.captured(b);
                    self.lex = Lex::InNumber { slot, at: next };
                    return Ok(());
                }
                if !at.complete() {
                    return Err(RpcError::Malformed);
                }
                // The byte after the number is read in its container's state.
                self.number_end(slot);
            }
            _ => {}
        }
        if whitespace(b) {
            return Ok(());
        }
        match self.lex {
            Lex::Value(slot) => self.value(slot, b),
            Lex::ObjectStart | Lex::Key => match b {
                b'"' => {
                    self.key_len = 0;
                    self.key_other = false;
                    self.lex = Lex::InKey { escape: 0 };
                    Ok(())
                }
                b'}' if self.lex == Lex::ObjectStart => self.close(false),
                _ => Err(RpcError::Malformed),
            },
            Lex::Colon => {
                if b != b':' {
                    return Err(RpcError::Malformed);
                }
                let parent = self.parent().ok_or(RpcError::Malformed)?;
                let slot = match parent.slot {
                    Slot::Root | Slot::Result | Slot::Block | Slot::Header => self.member(parent.slot)?,
                    _ => Slot::Other,
                };
                self.lex = Lex::Value(slot);
                Ok(())
            }
            Lex::ArrayStart if b == b']' => self.close(true),
            Lex::ArrayStart => {
                let parent = self.parent().ok_or(RpcError::Malformed)?;
                let slot = if parent.slot == Slot::Extrinsics { Slot::Extrinsic } else { Slot::Other };
                self.value(slot, b)
            }
            Lex::After => {
                let parent = self.parent().ok_or(RpcError::Malformed)?;
                match (b, parent.array) {
                    (b',', false) => {
                        self.lex = Lex::Key;
                        Ok(())
                    }
                    (b',', true) => {
                        let slot = if parent.slot == Slot::Extrinsics { Slot::Extrinsic } else { Slot::Other };
                        self.lex = Lex::Value(slot);
                        Ok(())
                    }
                    (b'}', false) => self.close(false),
                    (b']', true) => self.close(true),
                    _ => Err(RpcError::Malformed),
                }
            }
            Lex::Done => Err(RpcError::Malformed),
            Lex::InString { .. } | Lex::InKey { .. } | Lex::InNumber { .. } | Lex::InWord { .. } => {
                Err(RpcError::Malformed)
            }
        }
    }
}

/// Scans a whole reply held in memory (tests and native callers).
pub fn scan_block(id: u64, body: &[u8], number: u64, target: &str) -> Result<BlockMatch, RpcError> {
    let mut scan = BlockScan::new(id, number, target);
    scan.feed(body);
    scan.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The former whole-body reading (`serde_json` and the old checks), plus
    /// the journal's case-insensitive position match: the reference every
    /// scan must agree with.
    fn reference(id: u64, body: &[u8], number: u64, target: &str) -> Result<BlockMatch, RpcError> {
        let value: Value = serde_json::from_slice(body).map_err(|_| RpcError::Malformed)?;
        let object = value.as_object().ok_or(RpcError::Malformed)?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || object.get("id").and_then(Value::as_u64) != Some(id)
        {
            return Err(RpcError::Malformed);
        }
        if object.contains_key("error") {
            return Err(RpcError::Remote);
        }
        let v = object.get("result").ok_or(RpcError::Malformed)?;
        if v.is_null() {
            return Err(RpcError::NotServed);
        }
        let block = v.get("block").ok_or(RpcError::Malformed)?;
        let header = block.get("header").ok_or(RpcError::Malformed)?;
        let n = header.get("number").and_then(Value::as_str).ok_or(RpcError::Malformed)?;
        if hex_number_str(n)? != number {
            return Err(RpcError::Inconsistent);
        }
        let list = block.get("extrinsics").and_then(Value::as_array).ok_or(RpcError::Malformed)?;
        let mut index = None;
        for (i, x) in list.iter().enumerate() {
            let s = x.as_str().ok_or(RpcError::Malformed)?;
            let raw = s.strip_prefix("0x").ok_or(RpcError::Malformed)?;
            if raw.len() % 2 != 0 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(RpcError::Malformed);
            }
            if index.is_none() && s.eq_ignore_ascii_case(target) {
                index = Some(i as u32);
            }
        }
        Ok(BlockMatch { count: list.len() as u32, index })
    }

    fn scan_chunks(body: &[u8], chunk: usize, number: u64, target: &str) -> Result<BlockMatch, RpcError> {
        let mut scan = BlockScan::new(14, number, target);
        for part in body.chunks(chunk) {
            if !scan.feed(part) {
                break;
            }
        }
        scan.finish()
    }

    /// Deterministic synthetic hex of `chars` characters, `0x` included.
    fn hex_of(chars: usize, seed: u8) -> String {
        let mut s = String::with_capacity(chars);
        s.push_str("0x");
        let digits = b"0123456789abcdef";
        let mut x = u32::from(seed) | 1;
        while s.len() < chars {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            s.push(char::from(digits[(x >> 16) as usize % 16]));
        }
        s
    }

    /// A reply shaped like testnet block 8380857 (the 486 -> 488 upgrade):
    /// `extrinsics` before `header`, the extrinsics' hex lengths as measured
    /// (`[24, 2386, 400, 400, 400, 400, 5734672, 832]`: the runtime code at
    /// index 6, the journal's commit at index 7), a four-log digest and
    /// `justifications: null`. Synthetic bytes, not the live block.
    fn upgrade_block(number: u64, ours: &str, at_seven: bool) -> Vec<u8> {
        let lengths = [24usize, 2386, 400, 400, 400, 400, 5_734_672];
        let mut ext: Vec<String> = lengths.iter().enumerate().map(|(i, &l)| hex_of(l, i as u8)).collect();
        ext.push(if at_seven { ours.to_owned() } else { hex_of(ours.len(), 99) });
        let list = ext.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(",");
        format!(
            concat!(
                r#"{{"jsonrpc":"2.0","result":{{"block":{{"extrinsics":[{list}],"header":{{"digest":{{"logs":["#,
                r#""0x066175726120e813e90800000000","0x08","{l3}","{l4}"]}},"extrinsicsRoot":"{root}","number":"0x{n:x}","#,
                r#""parentHash":"{root}","stateRoot":"{root}"}}}},"justifications":null}},"id":14}}"#
            ),
            list = list,
            l3 = hex_of(80, 7),
            l4 = hex_of(140, 8),
            root = hex_of(66, 9),
            n = number,
        )
        .into_bytes()
    }

    #[test]
    fn the_upgrade_block_8380857_is_matched_without_holding_it() {
        let ours = hex_of(832, 42);
        let body = upgrade_block(8_380_857, &ours, true);
        // About the measured reply (5,740,192 bytes), past the cap a whole
        // body read is refused at: the stall of 2026-11-05/06.
        assert!(body.len() > 5_739_000 && body.len() < 5_742_000, "{}", body.len());
        assert!(body.len() > crate::rpc::MAX_LARGE_RESPONSE_BYTES);
        assert_eq!(crate::rpc::parse_large_hex(14, &body), Err(RpcError::TooLarge));
        let expected = BlockMatch { count: 8, index: Some(7) };
        assert_eq!(reference(14, &body, 8_380_857, &ours), Ok(expected));
        for chunk in [body.len(), 65_536, 16_384, 7_919, 4_096] {
            assert_eq!(scan_chunks(&body, chunk, 8_380_857, &ours), Ok(expected), "chunk {chunk}");
        }
        // Upper-case bytes and an upper-case target still match.
        assert_eq!(
            scan_chunks(&body, 65_536, 8_380_857, &ours.to_ascii_uppercase().replacen("0X", "0x", 1)),
            Ok(expected)
        );
        // Absent bytes: the whole body answers "not here", with its count.
        let other = upgrade_block(8_380_857, &ours, false);
        assert_eq!(scan_chunks(&other, 65_536, 8_380_857, &ours), Ok(BlockMatch { count: 8, index: None }));
        // The height is checked even though the header comes last.
        assert_eq!(scan_chunks(&body, 65_536, 8_380_858, &ours), Err(RpcError::Inconsistent));
        // The scan's own state is fixed: the target and small captures.
        assert!(std::mem::size_of::<BlockScan>() < 512);
    }

    #[test]
    fn a_reply_cut_short_is_never_an_answer() {
        let ours = hex_of(64, 1);
        let body = format!(
            r#"{{"jsonrpc":"2.0","result":{{"block":{{"extrinsics":["0x0400","{ours}"],"header":{{"number":"0x10","digest":{{"logs":[]}}}}}},"justifications":null}},"id":14}}"#
        );
        let body = body.as_bytes();
        assert_eq!(scan_chunks(body, 1, 16, &ours), Ok(BlockMatch { count: 2, index: Some(1) }));
        for cut in 0..body.len() {
            let r = scan_chunks(&body[..cut], 3, 16, &ours);
            assert!(r.is_err(), "prefix {cut} answered {r:?}");
            assert_ne!(r, Err(RpcError::NotServed));
        }
    }

    #[test]
    fn the_scan_agrees_with_the_whole_body_reading() {
        let t = "0xabcd";
        let cases: Vec<String> = vec![
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0x0400","0xABCD"]},"justifications":null},"id":14}"#.into(),
            r#" { "id" : 14 , "result" : { "justifications" : [ [ "0x01" , "0x02" ] ] , "block" : { "extrinsics" : [ ] , "header" : { "number" : "0x10" } } } , "jsonrpc" : "2.0" } "#.into(),
            r#"{"jsonrpc":"2.0","result":null,"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","error":{"code":4003,"message":"UnknownBlock \"x\"","data":[1,2.5e3,true,null]},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0x0400","0xabc"]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0400"]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":[1]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"}}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"extrinsics":[]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x11"},"extrinsics":[]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":16},"extrinsics":[]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":[]}},"id":15}"#.into(),
            r#"{"jsonrpc":"1.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":[]}},"id":14}"#.into(),
            r#"{"result":{"block":{"header":{"number":"0x10"},"extrinsics":[]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":[]}},"id":14} x"#.into(),
            r#"[{"jsonrpc":"2.0","id":14}]"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10","x":{"a":"\\\"}"}},"extrinsics":["0xabcd"]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0xabcd","0xabcd"]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0xabcdab"]}},"id":14}"#.into(),
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0xab"]}},"id":14}"#.into(),
        ];
        for c in &cases {
            let b = c.as_bytes();
            assert_eq!(scan_block(14, b, 16, t), reference(14, b, 16, t), "{c}");
        }
        // Duplicate routed members (`serde_json` keeps the last) and a reply
        // deeper than the schema are refused outright.
        for c in [
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":[],"extrinsics":["0xabcd"]}},"id":14}"#,
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x11","number":"0x10"},"extrinsics":[]}},"id":14}"#,
            r#"{"jsonrpc":"2.0","id":13,"result":null,"id":14}"#,
            r#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10","d":{"l":[[["0x"]]]}},"extrinsics":[]}},"id":14}"#,
        ] {
            assert_eq!(scan_block(14, c.as_bytes(), 16, t), Err(RpcError::Malformed), "{c}");
        }
    }

    #[test]
    fn a_scan_never_answers_what_the_whole_body_reading_would_not() {
        // Random byte edits of a small reply: whenever the scan answers, the
        // reference answers the same.
        let base = br#"{"jsonrpc":"2.0","result":{"block":{"extrinsics":["0x0400","0xabcd"],"header":{"number":"0x10","digest":{"logs":["0x08"]}}},"justifications":null},"id":14}"#;
        let alphabet = br#"{}[]:,"\ 0x1aAbcdn-e."#;
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut answered = 0;
        for _ in 0..20_000 {
            let mut body = base.to_vec();
            for _ in 0..1 + next() % 3 {
                let at = (next() as usize) % body.len();
                match next() % 3 {
                    0 => body[at] = alphabet[(next() as usize) % alphabet.len()],
                    1 => {
                        body.remove(at);
                    }
                    _ => body.insert(at, alphabet[(next() as usize) % alphabet.len()]),
                }
            }
            let got = scan_block(14, &body, 16, "0xabcd");
            if let Ok(m) = got {
                answered += 1;
                assert_eq!(reference(14, &body, 16, "0xabcd"), Ok(m), "{}", String::from_utf8_lossy(&body));
            }
        }
        assert!(answered > 100, "{answered}");
    }
}
