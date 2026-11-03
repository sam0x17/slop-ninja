//! Synthetic Workers handler for the arena drill.
//!
//! One invocation decodes a package-sized JSON receipt list and encodes the
//! receipt bundle and index root for it:
//!
//! 1. ingest the body through a bounded reader (declared length checked
//!    against the schema bound first, then pulled in 64 KiB chunks);
//! 2. an allocation-free pre-scan, then a borrowed, typed decode (every
//!    position through `deserialize_any`, static error messages, so errors
//!    stay inside the derived `CONTROL_FOOTPRINT`) into a vector admitted once
//!    from the declared count, which is accepted only if it is at most
//!    `input_len / MIN_ENTRY_JSON` (a schema-derived bound);
//! 3. one pass over the entries: each 151-byte receipt is encoded on the
//!    stack, hashed, and streamed into the bundle (exact-size buffer or 64 KiB
//!    chunks to the host) and the bundle digest; the index (55-byte header
//!    plus 69 bytes per entry) is streamed straight into its digest.
//!
//! Byte layouts mirror the protocol's `AttemptReceipt` (151 B, 155 B with its
//! chunk length prefix) and `ReceiptIndex` (55 + 69 B per entry); the values
//! are synthetic. The same code runs under the arena (`Ctx::Scoped`) and
//! under the default allocator (`Ctx::Unscoped`).

// The wasm32 stack is linear memory outside every arena: frames, futures and
// stack arrays stay below the measured bound (clippy.toml, design §5.1).
#![warn(clippy::large_stack_frames, clippy::large_futures, clippy::large_stack_arrays)]

use core::cell::Cell;
use core::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use slop_ninja_arena::io::{BoundedVec, ChunkSink, Enc, ExactBuf, Hash, Ingest, Kinds, Sink, Stack, Tee};
use slop_ninja_arena::{AsyncRoute, Footprint, Reason, Refusal, RefusalKind, Route, ScopeRef};

#[cfg(target_arch = "wasm32")]
mod wasm;

pub const SCHEMA: &str = "slop_ninja_arena_drill_package_v1";
pub const ROUTE: Route = Route::new(1, "drill/package");
/// The async variant awaits a host promise with its scope open. Its
/// abandonment deadline is the platform's `waitUntil` wall limit, 30 s
/// ([Q] LIMITS.md: "`waitUntil()` extends execution for up to 30 seconds").
pub const ABANDON_AFTER_MS: u64 = 30_000;
pub const ROUTE_ASYNC: AsyncRoute = AsyncRoute::new(2, "drill/package-async", ABANDON_AFTER_MS);
/// Stream chunk size. It trades CPU for memory and limits no input.
pub const CHUNK: usize = 64 * 1024;

pub const RECEIPT_LEN: usize = 151;
pub const BUNDLE_ENTRY: usize = 4 + RECEIPT_LEN;
pub const INDEX_HEADER: usize = 55;
pub const INDEX_ENTRY: usize = 69;

const ENTRY_KEYS: [&str; 5] = ["attempt", "assignment", "number", "outcome", "result_commitment"];
const TOP_KEYS: [&str; 5] = ["schema", "cohort_manifest", "artifact", "count", "receipts"];
const HEX_VALUES: usize = 3;
const NUM_VALUES: usize = 2;

const fn keys_len(keys: &[&str]) -> usize {
    let mut i = 0;
    let mut n = 0;
    while i < keys.len() {
        n += keys[i].len() + 3; // quotes and colon
        i += 1;
    }
    n
}

/// Fewest JSON bytes one receipt entry can take: every key once, 64-digit
/// hex strings, one-digit numbers, four commas, braces and one separator.
pub const MIN_ENTRY_JSON: usize = keys_len(&ENTRY_KEYS) + HEX_VALUES * 66 + NUM_VALUES + 4 + 2 + 1;
/// Most bytes one compact entry can take (numbers of up to three digits).
pub const MAX_ENTRY_JSON: usize = MIN_ENTRY_JSON + NUM_VALUES * 2;
/// Most bytes of the compact envelope around the entries.
pub const MAX_HEADER_JSON: usize = keys_len(&TOP_KEYS) + (SCHEMA.len() + 2) + 2 * 66 + 10 + 4 + 2 + 2;

/// Schema bound on a compact body declaring `receipts` entries.
pub const fn body_bound(receipts: usize) -> usize {
    MAX_HEADER_JSON.saturating_add(receipts.saturating_mul(MAX_ENTRY_JSON))
}

/// Most decimal digits a number in the schema has (`count` is a `u32`).
pub const MAX_DIGITS: usize = 10;

// --- Decode error footprint (finding 9) ---------------------------------
//
// The decoder allocates only on errors, and only a bounded amount:
// - the body is pre-scanned allocation-free and refused if it holds a
//   backslash (no schema string needs an escape, so serde_json never uses its
//   scratch buffer), a sign, fraction or exponent, or a digit run longer than
//   `MAX_DIGITS` outside strings;
// - every position is read with `deserialize_any`, so a type mismatch reaches
//   the visitor (never serde_json's `peek_invalid_type`, which embeds the
//   offending string in its message), and every visitor answers a string it
//   does not expect, and an unknown key, with a static message.
// What remains is serde_json's boxed error plus one message: static, or
// `invalid type: <kind>, expected <expecting>` where the kind is a fixed word
// (`map`, `sequence`, `boolean`, `null`) or an integer of at most 20 digits.

const EXPECTING: [&str; 6] = ["a drill package", "receipt entries", "a receipt entry", "64 hex digits", "an integer", "a field name"];
const MESSAGES: [&str; 8] = ["unknown field", "count", "refused", "more entries than admitted", "hex", "duplicate field", "out of range", "string not expected here"];

const fn max_len(xs: &[&str]) -> usize {
    let (mut i, mut m) = (0, 0);
    while i < xs.len() {
        if xs[i].len() > m {
            m = xs[i].len();
        }
        i += 1;
    }
    m
}

/// Longest message a decode error can carry: a static message, or
/// serde_json's `invalid type: integer `N`, expected E` (the longest
/// unexpected kind after the pre-scan) with N at most 20 digits.
pub const MAX_ERROR_MESSAGE: usize = {
    let invalid = "invalid type: ".len() + "integer ``".len() + 20 + ", expected ".len() + max_len(&EXPECTING);
    let stat = max_len(&MESSAGES);
    if invalid > stat { invalid } else { stat }
};
/// serde_json's boxed `ErrorImpl { code: ErrorCode, line, column }`: its
/// largest `code` variant is `Message(Box<str>)` (two words) plus a
/// discriminant, so five words; eight words bound it (the witness test
/// `decode_errors_fit_the_control_footprint` checks the measured peak).
pub const ERROR_BOX_BOUND: usize = size_of::<[usize; 8]>();
/// The route's control footprint: one boxed error and one message string
/// while it grows (doubling) and is shrunk into the box.
pub const CONTROL_FOOTPRINT: Footprint = Footprint::ZERO.alloc(ERROR_BOX_BOUND, 8).growth(MAX_ERROR_MESSAGE.next_power_of_two(), 1);
/// Control-path bytes for the route (allocations other than its admitted
/// buffers), derived above, never a hand-picked number.
pub const CONTROL_BYTES: usize = CONTROL_FOOTPRINT.bytes();

/// Where the handler allocates.
pub enum Ctx<'s> {
    Scoped(&'s ScopeRef),
    Unscoped,
}

impl Ctx<'_> {
    fn buf(&self, len: usize, kinds: Kinds) -> Result<ExactBuf, Refusal> {
        match self {
            Ctx::Scoped(s) => ExactBuf::admit(s, len, kinds),
            Ctx::Unscoped => ExactBuf::unscoped(len, kinds),
        }
    }
    fn vec<T>(&self, cap: usize) -> Result<BoundedVec<T>, Refusal> {
        match self {
            Ctx::Scoped(s) => BoundedVec::admit(s, cap, Kinds::NETWORK),
            Ctx::Unscoped => BoundedVec::unscoped(cap, Kinds::NETWORK),
        }
    }
    /// The bounded body reader for `len` declared bytes of `receipts` entries.
    pub fn ingest(&self, len: usize, receipts: usize) -> Result<Ingest, Refusal> {
        match self {
            Ctx::Scoped(s) => Ingest::admit(s, Some(len), body_bound(receipts), Kinds::NETWORK),
            Ctx::Unscoped => Ingest::unscoped(Some(len), body_bound(receipts), Kinds::NETWORK),
        }
    }
    fn route(&self) -> u8 {
        match self {
            Ctx::Scoped(s) => s.route().id,
            Ctx::Unscoped => ROUTE.id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub attempt: [u8; 32],
    pub assignment: [u8; 32],
    pub result_commitment: [u8; 32],
    pub number: u8,
    pub outcome: u8,
}

/// What an invocation produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub count: u32,
    pub bundle_len: u32,
    pub bundle_sha256: [u8; 32],
    pub index_len: u32,
    pub index_root: [u8; 32],
}

/// Why an invocation failed: a static code, never a heap value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    Refused(Refusal),
    Json,
    Schema,
    Order,
    Count,
    /// The pre-scan refused the body (escape, sign, fraction, exponent or an
    /// over-long number): a 4xx.
    Invalid,
}

impl Fail {
    pub fn code(&self) -> u32 {
        match self {
            Fail::Refused(r) => r.code(),
            Fail::Json => 0x1001,
            Fail::Schema => 0x1002,
            Fail::Order => 0x1003,
            Fail::Count => 0x1004,
            Fail::Invalid => 0x1005,
        }
    }
}

impl From<Refusal> for Fail {
    fn from(r: Refusal) -> Self {
        Fail::Refused(r)
    }
}

// ------------------------------------------------------------------ decode

/// Allocation-free pre-scan (see "Decode error footprint" above).
pub fn prescan(body: &[u8]) -> Result<(), Fail> {
    let (mut in_str, mut digits) = (false, 0usize);
    for &b in body {
        if b == b'\\' {
            return Err(Fail::Invalid);
        }
        if in_str {
            if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                digits = 0;
            }
            b'0'..=b'9' => {
                digits += 1;
                if digits > MAX_DIGITS {
                    return Err(Fail::Invalid);
                }
            }
            b'-' | b'+' | b'.' | b'e' | b'E' => return Err(Fail::Invalid),
            _ => digits = 0,
        }
    }
    Ok(())
}

/// A static-message error for a string where none is expected.
fn no_string<E: de::Error>() -> E {
    E::custom(MESSAGES[7])
}

struct Hex32([u8; 32]);

impl<'de> de::Deserialize<'de> for Hex32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Hex32;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(EXPECTING[3])
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Hex32, E> {
                slop_ninja_arena::io::hex::decode32(s.as_bytes()).map(Hex32).ok_or_else(|| E::custom(MESSAGES[4]))
            }
        }
        d.deserialize_any(V)
    }
}

/// A small integer field: numbers are range-checked with a static message,
/// strings are refused with a static message.
struct Small<const MAX: u64>(u64);

impl<'de, const MAX: u64> de::Deserialize<'de> for Small<MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<const MAX: u64>;
        impl<const MAX: u64> Visitor<'_> for V<MAX> {
            type Value = Small<MAX>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(EXPECTING[4])
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Small<MAX>, E> {
                if v > MAX { Err(E::custom(MESSAGES[6])) } else { Ok(Small(v)) }
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Small<MAX>, E> {
                Err(no_string())
            }
        }
        d.deserialize_any(V::<MAX>)
    }
}

/// The schema identifier: borrowed (the pre-scan guarantees no escape).
struct SchemaId<'a>(&'a str);

impl<'de> de::Deserialize<'de> for SchemaId<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = SchemaId<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(EXPECTING[5])
            }
            fn visit_borrowed_str<E: de::Error>(self, s: &'de str) -> Result<SchemaId<'de>, E> {
                Ok(SchemaId(s))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<SchemaId<'de>, E> {
                Err(no_string())
            }
        }
        d.deserialize_any(V)
    }
}

/// A field name mapped to its index in `keys`; anything else is a static
/// "unknown field" error (no `deny_unknown_fields` message that embeds the
/// key).
struct Key<const K: usize> {
    keys: &'static [&'static str; K],
}

impl<'de, const K: usize> DeserializeSeed<'de> for Key<K> {
    type Value = usize;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<usize, D::Error> {
        d.deserialize_identifier(self)
    }
}

impl<const K: usize> Visitor<'_> for Key<K> {
    type Value = usize;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXPECTING[5])
    }
    fn visit_str<E: de::Error>(self, s: &str) -> Result<usize, E> {
        self.keys.iter().position(|k| *k == s).ok_or_else(|| E::custom(MESSAGES[0]))
    }
}

struct EntryIn {
    attempt: [u8; 32],
    assignment: [u8; 32],
    number: u8,
    outcome: u8,
    result_commitment: [u8; 32],
}

impl<'de> de::Deserialize<'de> for EntryIn {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = EntryIn;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(EXPECTING[2])
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<EntryIn, E> {
                Err(no_string())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<EntryIn, A::Error> {
                type Fields = (Option<[u8; 32]>, Option<[u8; 32]>, Option<u8>, Option<u8>, Option<[u8; 32]>);
                let mut f: Fields = (None, None, None, None, None);
                while let Some(k) = m.next_key_seed(Key { keys: &ENTRY_KEYS })? {
                    let dup = match k {
                        0 => f.0.replace(m.next_value::<Hex32>()?.0).is_some(),
                        1 => f.1.replace(m.next_value::<Hex32>()?.0).is_some(),
                        2 => f.2.replace(m.next_value::<Small<255>>()?.0 as u8).is_some(),
                        3 => f.3.replace(m.next_value::<Small<255>>()?.0 as u8).is_some(),
                        _ => f.4.replace(m.next_value::<Hex32>()?.0).is_some(),
                    };
                    if dup {
                        return Err(de::Error::custom(MESSAGES[5]));
                    }
                }
                match f {
                    (Some(attempt), Some(assignment), Some(number), Some(outcome), Some(result_commitment)) => {
                        Ok(EntryIn { attempt, assignment, number, outcome, result_commitment })
                    }
                    _ => Err(de::Error::custom(MESSAGES[0])),
                }
            }
        }
        d.deserialize_any(V)
    }
}

struct Top<'a> {
    schema: Option<&'a str>,
    cohort_manifest: Option<[u8; 32]>,
    artifact: Option<[u8; 32]>,
    count: Option<u32>,
    entries: Option<BoundedVec<Entry>>,
}

struct TopVisitor<'c, 's> {
    cx: &'c Ctx<'s>,
    cap: usize,
    refused: &'c Cell<Option<Refusal>>,
}

impl<'de> Visitor<'de> for TopVisitor<'_, '_> {
    type Value = Top<'de>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXPECTING[0])
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Top<'de>, E> {
        Err(no_string())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Top<'de>, A::Error> {
        let mut top = Top { schema: None, cohort_manifest: None, artifact: None, count: None, entries: None };
        while let Some(key) = m.next_key_seed(Key { keys: &TOP_KEYS })? {
            match key {
                0 => top.schema = Some(m.next_value::<SchemaId<'de>>()?.0),
                1 => top.cohort_manifest = Some(m.next_value::<Hex32>()?.0),
                2 => top.artifact = Some(m.next_value::<Hex32>()?.0),
                3 => {
                    let n = m.next_value::<Small<{ u32::MAX as u64 }>>()?.0 as u32;
                    // A declared count is accepted only if the remaining bytes
                    // could hold that many entries.
                    if n as usize > self.cap {
                        let r = Refusal::new(self.cx.route(), RefusalKind::Invalid, Reason::Bound, n as usize, self.cap);
                        self.refused.set(Some(r));
                        return Err(de::Error::custom(MESSAGES[1]));
                    }
                    top.count = Some(n);
                }
                _ => {
                    if top.entries.is_some() {
                        return Err(de::Error::custom(MESSAGES[5]));
                    }
                    let cap = top.count.map_or(self.cap, |n| n as usize);
                    let mut v = self.cx.vec::<Entry>(cap).map_err(|r| {
                        self.refused.set(Some(r));
                        de::Error::custom(MESSAGES[2])
                    })?;
                    m.next_value_seed(Entries { v: &mut v, refused: self.refused })?;
                    top.entries = Some(v);
                }
            }
        }
        Ok(top)
    }
}

struct Entries<'v, 'c> {
    v: &'v mut BoundedVec<Entry>,
    refused: &'c Cell<Option<Refusal>>,
}

impl<'de> DeserializeSeed<'de> for Entries<'_, '_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Entries<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXPECTING[1])
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Err(no_string())
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
        while let Some(e) = seq.next_element::<EntryIn>()? {
            let entry = Entry {
                attempt: e.attempt,
                assignment: e.assignment,
                result_commitment: e.result_commitment,
                number: e.number,
                outcome: e.outcome,
            };
            if let Err(r) = self.v.try_push(entry) {
                self.refused.set(Some(r));
                return Err(de::Error::custom(MESSAGES[3]));
            }
        }
        Ok(())
    }
}

struct Package {
    cohort_manifest: [u8; 32],
    artifact: [u8; 32],
    entries: BoundedVec<Entry>,
}

/// Decodes a body into an admitted entry vector. The body is pre-scanned
/// first; every error is converted to a static code before returning, so its
/// heap (serde_json's boxed error, bounded by `CONTROL_FOOTPRINT`) is freed
/// inside the caller's scope.
fn decode(cx: &Ctx, body: &[u8]) -> Result<Package, Fail> {
    prescan(body)?;
    let refused = Cell::new(None);
    let cap = (body.len() + 1) / MIN_ENTRY_JSON;
    let mut de = serde_json::Deserializer::from_slice(body);
    let top = de.deserialize_any(TopVisitor { cx, cap, refused: &refused });
    let top = match top.and_then(|t| de.end().map(|()| t)) {
        Ok(t) => t,
        Err(_) => return Err(refused.get().map_or(Fail::Json, Fail::Refused)),
    };
    let (Some(schema), Some(cohort_manifest), Some(artifact), Some(count), Some(entries)) =
        (top.schema, top.cohort_manifest, top.artifact, top.count, top.entries)
    else {
        return Err(Fail::Schema);
    };
    if schema != SCHEMA {
        return Err(Fail::Schema);
    }
    if entries.len() != count as usize {
        return Err(Fail::Count);
    }
    Ok(Package { cohort_manifest, artifact, entries })
}

// ------------------------------------------------------------------ encode

fn receipt(e: &Entry, artifact: &[u8; 32]) -> Result<Stack<RECEIPT_LEN>, Refusal> {
    let mut enc = Enc(Stack::<RECEIPT_LEN>::default());
    enc.str("attempt-receipt")?;
    enc.u16(1)?;
    enc.raw(&e.attempt)?;
    enc.raw(&e.assignment)?;
    enc.raw(artifact)?;
    enc.u8(e.number)?;
    enc.u8(e.outcome)?;
    enc.raw(&e.result_commitment)?;
    Ok(enc.into_inner())
}

fn sha256(b: &[u8]) -> [u8; 32] {
    let mut h = Hash::new();
    let _ = h.put(b);
    h.finish()
}

/// One pass over the entries into the bundle sink and the index digest.
fn encode(pkg: &Package, bundle: &mut impl Sink) -> Result<(u32, [u8; 32]), Fail> {
    let count = u32::try_from(pkg.entries.len()).map_err(|_| Fail::Count)?;
    let mut index = Enc(Hash::new());
    index.str("receipt-index")?;
    index.u16(1)?;
    index.raw(&pkg.cohort_manifest)?;
    index.u32(count)?;
    let mut prev: Option<&[u8; 32]> = None;
    for e in pkg.entries.iter() {
        // Index order and uniqueness, checked incrementally.
        if prev.is_some_and(|p| p >= &e.attempt) {
            return Err(Fail::Order);
        }
        prev = Some(&e.attempt);
        let r = receipt(e, &pkg.artifact)?;
        let bytes = r.as_slice();
        let mut b = Enc(&mut *bundle);
        b.u32(RECEIPT_LEN as u32)?;
        b.raw(bytes)?;
        index.raw(&e.attempt)?;
        index.u8(e.outcome)?;
        index.raw(&sha256(bytes))?;
        index.u32(RECEIPT_LEN as u32)?;
    }
    let index_len = INDEX_HEADER + INDEX_ENTRY * pkg.entries.len();
    Ok((u32::try_from(index_len).map_err(|_| Fail::Count)?, index.into_inner().finish()))
}

/// The whole handler after ingest. `stream` sends the bundle in `CHUNK`-sized
/// pieces through `emit`; otherwise it is built in one exact buffer and
/// emitted once. `abort` abandons the streamed bundle (the host's
/// `writer.abort(reason)`) whenever the stream would otherwise stop short.
pub fn handle(
    cx: &Ctx,
    body: &[u8],
    stream: bool,
    emit: &mut dyn FnMut(&[u8]) -> Result<(), Refusal>,
    abort: &mut dyn FnMut(Refusal),
) -> Result<Output, Fail> {
    let pkg = decode(cx, body)?;
    let bundle_len = BUNDLE_ENTRY.checked_mul(pkg.entries.len()).ok_or(Fail::Count)?;
    let mut digest = Hash::new();
    let (index_len, index_root) = if stream {
        let chunk = cx.buf(CHUNK.min(bundle_len), Kinds::ENCODER)?;
        let mut sink = ChunkSink::new(chunk, bundle_len as u64, &mut *emit, &mut *abort);
        let r = encode(&pkg, &mut Tee(&mut sink, &mut digest))?;
        sink.finish()?;
        r
    } else {
        let mut buf = cx.buf(bundle_len, Kinds::ENCODER)?;
        let r = encode(&pkg, &mut Tee(&mut buf, &mut digest))?;
        emit(buf.finish()?)?;
        r
    };
    Ok(Output {
        count: pkg.entries.len() as u32,
        bundle_len: bundle_len as u32,
        bundle_sha256: digest.finish(),
        index_len,
        index_root,
    })
}
