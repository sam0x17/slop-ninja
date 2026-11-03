//! Zero-copy building blocks (design §4.1).
//!
//! - [`Sink`]: where encoded bytes go. [`Count`] gives an exact
//!   `encoded_len`, [`Hash`] a streaming SHA-256, [`ExactBuf`] a buffer whose
//!   capacity is fixed at admission, [`ChunkSink`] a fixed chunk buffer that
//!   feeds a stream, and [`Tee`] two of them in one pass.
//! - [`Enc`]: the canonical encoding (fixed-width little-endian integers,
//!   `u32` length prefixes) written to any sink, so a record is encoded once
//!   to count it and once to hash and store it, with no intermediate `Vec`.
//! - [`Ingest`]: a bounded body reader. It checks the declared length against
//!   the schema bound before allocating, takes chunks into an admitted
//!   buffer, and refuses overflow and truncation. Once the buffer is full it
//!   probes for one more byte: a longer stream is refused, never truncated.
//! - [`BoundedVec`]: a vector whose capacity is admitted once and never grows.
//! - [`Recheck`]: pass B of a two-pass signed write. It re-hashes the record
//!   as it streams and writes the tail (key and signature fixed by pass A)
//!   only if the digest still matches; on a mismatch it aborts the stream.
//!
//! A platform stream with a declared length (`FixedLengthStream`) is never
//! left short: a writer that stops short of its length, with neither
//! `close()` nor `abort()`, leaves the consumer (an R2 put) pending forever
//! (local workerd 2026-10-23, run `rereview/runs/fls1`: still pending after
//! 3 s; `close()` rejects it in 5 ms, `abort()` in 15 ms). So every refusal
//! of a [`ChunkSink`], and a `Recheck` mismatch, calls the sink's abort,
//! which the host binds to `writer.abort(reason)`; the host then awaits the
//! put's rejection inside the scope, bounded by an `AbortSignal` timeout.
//!
//! Every constructor that allocates takes the scope ([`ScopeRef`]) it allocates in and
//! admits the block first; refusals never allocate. The `unscoped`
//! constructors exist for builds without the arena allocator (native tools,
//! the default-allocator drill) and make the same exact-size allocation.
//!
//! The kind of a refusal is chosen by its cause, which only the caller knows
//! ([`Kinds`]): network input that overflows its bound is `Invalid` and a
//! truncated network read is `Retry`; a plan-derived overflow is `Capacity`;
//! an encoder whose output disagrees with its declared length is a `Defect`.

use core::fmt;
use core::mem::MaybeUninit;

use crate::scope::{Reason, Refusal, RefusalKind, ScopeRef};

/// The refusal kind for each I/O failure, chosen by the caller from the
/// buffer's cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kinds {
    /// Bytes past an exact capacity or declared length.
    pub overflow: RefusalKind,
    /// Fewer bytes than declared.
    pub truncated: RefusalKind,
    /// A declared length (or a stream) above its schema or route bound.
    pub bound: RefusalKind,
}

impl Kinds {
    /// Input from the network or a peer: an over-long body is a client error
    /// (4xx), a short read may succeed on redelivery.
    pub const NETWORK: Kinds = Kinds { overflow: RefusalKind::Invalid, truncated: RefusalKind::Retry, bound: RefusalKind::Invalid };
    /// Sizes derived from a plan: a plan that outgrows the build's bounds is a
    /// durable `arena_capacity` pause; a short total is a defect.
    pub const PLAN: Kinds = Kinds { overflow: RefusalKind::Capacity, truncated: RefusalKind::Defect, bound: RefusalKind::Capacity };
    /// Our own encoder writing a declared total: any mismatch is a defect.
    pub const ENCODER: Kinds = Kinds { overflow: RefusalKind::Defect, truncated: RefusalKind::Defect, bound: RefusalKind::Defect };
}

/// Destination of encoded bytes.
pub trait Sink {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal>;

    /// Abandons whatever platform stream the sink feeds, so its consumer
    /// rejects instead of waiting for bytes that will never come. In-memory
    /// sinks have nothing to abort.
    fn abort(&mut self, _why: Refusal) {}
}

impl<S: Sink + ?Sized> Sink for &mut S {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        (**self).put(bytes)
    }
    fn abort(&mut self, why: Refusal) {
        (**self).abort(why)
    }
}

fn overflow(k: Kinds, route: u8, need: usize, have: usize) -> Refusal {
    Refusal::new(route, k.overflow, Reason::Overflow, need, have)
}

fn truncated(k: Kinds, route: u8, need: usize, have: usize) -> Refusal {
    Refusal::new(route, k.truncated, Reason::Truncated, need, have)
}

/// The allocator failed after admission held the block's pages: an
/// accounting defect, whatever the buffer's cause.
fn alloc_refused(route: u8, need: usize) -> Refusal {
    Refusal::new(route, RefusalKind::Defect, Reason::Allocator, need, 0)
}

/// Counts bytes: the exact encoded length of whatever is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub bytes: u64,
}

impl Sink for Count {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(())
    }
}

/// Streaming SHA-256.
#[cfg(feature = "sha2")]
#[derive(Clone, Default)]
pub struct Hash(sha2::Sha256);

#[cfg(feature = "sha2")]
impl Hash {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn finish(self) -> [u8; 32] {
        use sha2::Digest;
        self.0.finalize().into()
    }
}

#[cfg(feature = "sha2")]
impl Sink for Hash {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        use sha2::Digest;
        self.0.update(bytes);
        Ok(())
    }
}

/// Writes to both sinks in one pass.
pub struct Tee<A, B>(pub A, pub B);

impl<A: Sink, B: Sink> Sink for Tee<A, B> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        self.0.put(bytes)?;
        self.1.put(bytes)
    }
    fn abort(&mut self, why: Refusal) {
        self.0.abort(why);
        self.1.abort(why);
    }
}

/// A fixed stack buffer for fixed-size records.
pub struct Stack<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Default for Stack<N> {
    fn default() -> Self {
        Self { buf: [0; N], len: 0 }
    }
}

impl<const N: usize> Stack<N> {
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> Sink for Stack<N> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        let end = self.len.saturating_add(bytes.len());
        if end > N {
            return Err(overflow(Kinds::ENCODER, 0, end, N));
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

/// A byte buffer whose capacity is fixed when it is admitted. Writes past it
/// are refused; it never reallocates.
pub struct ExactBuf {
    v: Vec<u8>,
    route: u8,
    kinds: Kinds,
}

impl ExactBuf {
    /// Admits `len` bytes in `scope` and allocates exactly that. `kinds`
    /// classes overflow and truncation by the buffer's cause.
    pub fn admit(scope: &ScopeRef, len: usize, kinds: Kinds) -> Result<Self, Refusal> {
        let route = scope.route().id;
        scope.admit_block(len, 1)?;
        let mut v = Vec::new();
        scope.enter(|| v.try_reserve_exact(len)).map_err(|_| alloc_refused(route, len))?;
        Self::checked(v, len, route, kinds)
    }

    /// The same exact allocation under whatever allocator is installed.
    pub fn unscoped(len: usize, kinds: Kinds) -> Result<Self, Refusal> {
        let mut v = Vec::new();
        v.try_reserve_exact(len).map_err(|_| alloc_refused(0, len))?;
        Self::checked(v, len, 0, kinds)
    }

    fn checked(v: Vec<u8>, len: usize, route: u8, kinds: Kinds) -> Result<Self, Refusal> {
        // A capacity other than the request counts as a refusal (design §2.4).
        if v.capacity() != len {
            return Err(alloc_refused(route, len));
        }
        Ok(Self { v, route, kinds })
    }

    pub fn kinds(&self) -> Kinds {
        self.kinds
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.v
    }
    pub fn len(&self) -> usize {
        self.v.len()
    }
    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.v.capacity()
    }
    pub fn remaining(&self) -> usize {
        self.v.capacity() - self.v.len()
    }
    pub fn is_full(&self) -> bool {
        self.remaining() == 0
    }
    /// Empties the buffer for reuse (a chunk buffer between chunks).
    pub fn clear(&mut self) {
        self.v.clear();
    }
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.v.as_mut_ptr()
    }

    /// The unfilled tail, for a copy straight from a platform buffer
    /// (`Uint8Array::copy_to`).
    pub fn spare(&mut self) -> &mut [MaybeUninit<u8>] {
        self.v.spare_capacity_mut()
    }

    /// Marks `n` more bytes of [`spare`](Self::spare) as written.
    ///
    /// # Safety
    /// The first `n` bytes of the spare tail must have been initialised.
    pub unsafe fn advance(&mut self, n: usize) -> Result<(), Refusal> {
        if n > self.remaining() {
            return Err(overflow(self.kinds, self.route, self.len().saturating_add(n), self.capacity()));
        }
        unsafe { self.v.set_len(self.v.len() + n) };
        Ok(())
    }

    /// The contents, which must fill the capacity exactly.
    pub fn finish(&self) -> Result<&[u8], Refusal> {
        if !self.is_full() {
            return Err(truncated(self.kinds, self.route, self.capacity(), self.len()));
        }
        Ok(&self.v)
    }
}

impl Sink for ExactBuf {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        if bytes.len() > self.remaining() {
            return Err(overflow(self.kinds, self.route, self.len().saturating_add(bytes.len()), self.capacity()));
        }
        // Within the admitted capacity: `extend_from_slice` cannot allocate.
        self.v.extend_from_slice(bytes);
        Ok(())
    }
}

/// A bounded body reader.
pub struct Ingest {
    buf: ExactBuf,
    declared: Option<usize>,
    bound: usize,
    kinds: Kinds,
}

impl Ingest {
    /// Checks `declared` (a `Content-Length` or object size) against `bound`
    /// (the schema or route limit) before allocating anything, then admits
    /// exactly `declared`, or `bound` when no length is declared.
    pub fn admit(scope: &ScopeRef, declared: Option<usize>, bound: usize, kinds: Kinds) -> Result<Self, Refusal> {
        let len = Self::len_for(scope.route().id, declared, bound, kinds)?;
        Ok(Self { buf: ExactBuf::admit(scope, len, kinds)?, declared, bound, kinds })
    }

    pub fn unscoped(declared: Option<usize>, bound: usize, kinds: Kinds) -> Result<Self, Refusal> {
        let len = Self::len_for(0, declared, bound, kinds)?;
        Ok(Self { buf: ExactBuf::unscoped(len, kinds)?, declared, bound, kinds })
    }

    fn len_for(route: u8, declared: Option<usize>, bound: usize, kinds: Kinds) -> Result<usize, Refusal> {
        match declared {
            Some(d) if d > bound => Err(Refusal::new(route, kinds.bound, Reason::Bound, d, bound)),
            Some(d) => Ok(d),
            None => Ok(bound),
        }
    }

    /// The refusal for input past the buffer: `Overflow` past a declared
    /// length, `Bound` past the schema bound of an undeclared stream.
    fn excess(&self, extra: usize) -> Refusal {
        let have = self.buf.capacity();
        let need = have.saturating_add(extra);
        match self.declared {
            Some(_) => Refusal::new(self.buf.route, self.kinds.overflow, Reason::Overflow, need, have),
            None => Refusal::new(self.buf.route, self.kinds.bound, Reason::Bound, need, self.bound),
        }
    }

    /// Takes one chunk; refuses one past the declared length or bound. On
    /// refusal the caller cancels the JS stream.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), Refusal> {
        if chunk.len() > self.buf.remaining() {
            return Err(self.excess(chunk.len() - self.buf.remaining()));
        }
        self.buf.put(chunk)
    }

    /// Pulls chunks with `read(dst) -> written` until a read returns 0.
    /// `read` must initialise the first `written` bytes. Once the buffer is
    /// full it reads once more into a one-byte stack probe: any byte there
    /// means the stream is longer than its declared length or bound, and the
    /// body is refused (never accepted truncated).
    pub fn pull(&mut self, mut read: impl FnMut(&mut [MaybeUninit<u8>]) -> usize) -> Result<(), Refusal> {
        loop {
            if self.buf.is_full() {
                let mut probe = [MaybeUninit::<u8>::uninit(); 1];
                let n = read(&mut probe);
                if n > 0 {
                    return Err(self.excess(n));
                }
                return Ok(());
            }
            let n = read(self.buf.spare());
            if n == 0 {
                return Ok(());
            }
            // SAFETY: `read` initialised `n` bytes of the spare tail; `advance`
            // refuses `n` beyond it.
            unsafe { self.buf.advance(n)? };
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The body; refuses one shorter than its declared length.
    pub fn finish(self) -> Result<ExactBuf, Refusal> {
        if let Some(d) = self.declared
            && self.buf.len() != d
        {
            return Err(truncated(self.kinds, self.buf.route, d, self.buf.len()));
        }
        Ok(self.buf)
    }
}

/// Streams a declared total through one fixed chunk buffer: each full chunk
/// goes to `write` (a `FixedLengthStream` writer, an R2 part) and the buffer
/// is reused. Refuses bytes past the total and a short total at `finish`.
///
/// Every refusal (overflow, a failed write, a short total, or dropping the
/// sink unfinished after an early return) first calls `abort` once, which the
/// host binds to the platform writer's `abort(reason)`: the stream is never
/// left short of its declared length, which would leave the put pending.
pub struct ChunkSink<W, A>
where
    W: FnMut(&[u8]) -> Result<(), Refusal>,
    A: FnMut(Refusal),
{
    chunk: ExactBuf,
    total: u64,
    written: u64,
    write: W,
    abort: A,
    done: bool,
}

impl<W, A> ChunkSink<W, A>
where
    W: FnMut(&[u8]) -> Result<(), Refusal>,
    A: FnMut(Refusal),
{
    pub fn new(chunk: ExactBuf, total: u64, write: W, abort: A) -> Self {
        Self { chunk, total, written: 0, write, abort, done: false }
    }

    fn fail(&mut self, why: Refusal) -> Refusal {
        if !self.done {
            self.done = true;
            (self.abort)(why);
        }
        why
    }

    /// Flushes the last chunk; refuses (and aborts the stream on) a short
    /// total, with the chunk buffer's kinds: a defect for an encoder writing
    /// a declared total.
    pub fn finish(mut self) -> Result<u64, Refusal> {
        if self.written != self.total {
            // Never flush a short tail; abort the stream so its consumer
            // rejects (a short stream left open never settles).
            let r = truncated(self.chunk.kinds, self.chunk.route, self.total as usize, self.written as usize);
            return Err(self.fail(r));
        }
        if !self.chunk.is_empty() {
            if let Err(r) = (self.write)(self.chunk.as_slice()) {
                return Err(self.fail(r));
            }
            self.chunk.clear();
        }
        self.done = true;
        Ok(self.written)
    }

    pub fn written(&self) -> u64 {
        self.written
    }
}

impl<W, A> Sink for ChunkSink<W, A>
where
    W: FnMut(&[u8]) -> Result<(), Refusal>,
    A: FnMut(Refusal),
{
    fn put(&mut self, mut bytes: &[u8]) -> Result<(), Refusal> {
        if self.done {
            return Err(Refusal::new(self.chunk.route, RefusalKind::Defect, Reason::Overflow, bytes.len(), 0));
        }
        let after = self.written.saturating_add(bytes.len() as u64);
        if after > self.total {
            let r = overflow(self.chunk.kinds, self.chunk.route, after as usize, self.total as usize);
            return Err(self.fail(r));
        }
        while !bytes.is_empty() {
            let n = bytes.len().min(self.chunk.remaining());
            self.chunk.put(&bytes[..n])?;
            bytes = &bytes[n..];
            if self.chunk.is_full() {
                if let Err(r) = (self.write)(self.chunk.as_slice()) {
                    return Err(self.fail(r));
                }
                self.chunk.clear();
            }
        }
        self.written = after;
        Ok(())
    }

    fn abort(&mut self, why: Refusal) {
        self.fail(why);
    }
}

impl<W, A> Drop for ChunkSink<W, A>
where
    W: FnMut(&[u8]) -> Result<(), Refusal>,
    A: FnMut(Refusal),
{
    fn drop(&mut self) {
        if !self.done {
            // Dropped unfinished (an early return): the stream is short.
            let r = truncated(self.chunk.kinds, self.chunk.route, self.total as usize, self.written as usize);
            self.fail(r);
        }
    }
}

/// Pass B of a two-pass signed write (design §4.2, the streamed package).
///
/// Pass A hashed the record and signed its digest. Pass B streams the same
/// record to storage: [`Recheck`] re-hashes every record byte it forwards
/// and writes the tail (the key and signature fixed by pass A) only if the
/// re-hash equals pass A's digest. On a mismatch (a receipt row changed
/// between the passes) it refuses with a `Defect` before the tail and aborts
/// the inner sink's stream ([`Sink::abort`]), so the platform put rejects and
/// nothing is stored: no envelope whose bytes disagree with its signature is
/// ever stored, and the put never waits on a short stream.
#[cfg(feature = "sha2")]
pub struct Recheck<S> {
    inner: S,
    hash: Hash,
    route: u8,
}

#[cfg(feature = "sha2")]
impl<S: Sink> Recheck<S> {
    pub fn new(inner: S, route: u8) -> Self {
        Self { inner, hash: Hash::new(), route }
    }

    /// Bytes outside the hashed record (the envelope header and length).
    pub fn raw(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        self.inner.put(bytes)
    }

    /// Checks the re-hash against pass A's digest, then writes `tail`.
    pub fn finish(mut self, expected: &[u8; 32], tail: &[u8]) -> Result<S, Refusal> {
        let got = self.hash.finish();
        if &got != expected {
            let r = Refusal::new(self.route, RefusalKind::Defect, Reason::Truncated, tail.len(), 0);
            self.inner.abort(r);
            return Err(r);
        }
        self.inner.put(tail)?;
        Ok(self.inner)
    }
}

#[cfg(feature = "sha2")]
impl<S: Sink> Sink for Recheck<S> {
    /// Record bytes: hashed and forwarded.
    fn put(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        self.hash.put(bytes)?;
        self.inner.put(bytes)
    }
    fn abort(&mut self, why: Refusal) {
        self.inner.abort(why)
    }
}

/// A vector whose capacity is admitted once and never grows.
pub struct BoundedVec<T> {
    v: Vec<T>,
    route: u8,
    kinds: Kinds,
}

impl<T> BoundedVec<T> {
    pub fn admit(scope: &ScopeRef, cap: usize, kinds: Kinds) -> Result<Self, Refusal> {
        let route = scope.route().id;
        let bytes = cap
            .checked_mul(size_of::<T>())
            .ok_or(Refusal::new(route, kinds.bound, Reason::Bound, usize::MAX, isize::MAX as usize))?;
        scope.admit_block(bytes, align_of::<T>())?;
        let mut v = Vec::new();
        scope.enter(|| v.try_reserve_exact(cap)).map_err(|_| alloc_refused(route, bytes))?;
        Self::checked(v, cap, route, kinds)
    }

    pub fn unscoped(cap: usize, kinds: Kinds) -> Result<Self, Refusal> {
        let mut v = Vec::new();
        v.try_reserve_exact(cap).map_err(|_| alloc_refused(0, cap.saturating_mul(size_of::<T>())))?;
        Self::checked(v, cap, 0, kinds)
    }

    fn checked(v: Vec<T>, cap: usize, route: u8, kinds: Kinds) -> Result<Self, Refusal> {
        if size_of::<T>() != 0 && v.capacity() != cap {
            return Err(alloc_refused(route, cap.saturating_mul(size_of::<T>())));
        }
        Ok(Self { v, route, kinds })
    }

    /// Refuses (and drops `t`) once the admitted capacity is used.
    pub fn try_push(&mut self, t: T) -> Result<(), Refusal> {
        if self.v.len() == self.v.capacity() {
            return Err(overflow(self.kinds, self.route, self.v.len().saturating_add(1), self.v.capacity()));
        }
        // alloc-ok: within the admitted capacity, `push` cannot allocate.
        self.v.push(t);
        Ok(())
    }

    pub fn as_slice(&self) -> &[T] {
        &self.v
    }
    pub fn len(&self) -> usize {
        self.v.len()
    }
    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.v.capacity()
    }
}

impl<T> core::ops::Deref for BoundedVec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.v
    }
}

/// Canonical encoder over any sink: fixed-width little-endian integers,
/// `u32` length-prefixed strings and byte strings.
pub struct Enc<S>(pub S);

impl<S: Sink> Enc<S> {
    pub fn raw(&mut self, b: &[u8]) -> Result<(), Refusal> {
        self.0.put(b)
    }
    pub fn u8(&mut self, v: u8) -> Result<(), Refusal> {
        self.0.put(&[v])
    }
    pub fn u16(&mut self, v: u16) -> Result<(), Refusal> {
        self.0.put(&v.to_le_bytes())
    }
    pub fn u32(&mut self, v: u32) -> Result<(), Refusal> {
        self.0.put(&v.to_le_bytes())
    }
    pub fn u64(&mut self, v: u64) -> Result<(), Refusal> {
        self.0.put(&v.to_le_bytes())
    }
    /// A `u32` length prefix; refuses lengths that do not fit.
    pub fn len_prefix(&mut self, len: usize) -> Result<(), Refusal> {
        let n = u32::try_from(len).map_err(|_| Refusal::new(0, RefusalKind::Defect, Reason::Bound, len, u32::MAX as usize))?;
        self.u32(n)
    }
    pub fn str(&mut self, s: &str) -> Result<(), Refusal> {
        self.len_prefix(s.len())?;
        self.raw(s.as_bytes())
    }
    pub fn bytes(&mut self, b: &[u8]) -> Result<(), Refusal> {
        self.len_prefix(b.len())?;
        self.raw(b)
    }
    pub fn into_inner(self) -> S {
        self.0
    }
}

/// Exact encoded length of `write`'s output (pass one of a two-pass encode).
pub fn encoded_len(write: impl FnOnce(&mut Enc<Count>) -> Result<(), Refusal>) -> Result<u64, Refusal> {
    let mut e = Enc(Count::default());
    write(&mut e)?;
    Ok(e.0.bytes)
}

/// A `fmt::Write` over a fixed byte buffer. Overflow is an error, never a
/// silent truncation.
pub struct FmtBuf<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> FmtBuf<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }
    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are ever copied in.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Write for FmtBuf<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.saturating_add(s.len());
        if end > self.buf.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Fixed-size hex without allocation.
pub mod hex {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    /// Decodes exactly 64 hex digits (either case) into 32 bytes.
    pub fn decode32(s: &[u8]) -> Option<[u8; 32]> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        let (pairs, _) = s.as_chunks::<2>();
        for (i, pair) in pairs.iter().enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(out)
    }

    pub fn encode32(d: &[u8; 32]) -> [u8; 64] {
        let mut out = [0u8; 64];
        for (i, b) in d.iter().enumerate() {
            out[2 * i] = DIGITS[(b >> 4) as usize];
            out[2 * i + 1] = DIGITS[(b & 15) as usize];
        }
        out
    }

    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
}
