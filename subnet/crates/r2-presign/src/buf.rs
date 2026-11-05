//! A fixed-capacity byte buffer that never allocates. Writing past its
//! capacity fails and leaves the buffer marked overflowed, so a too-long
//! value is refused rather than cut.

use core::fmt;

/// `N` bytes of inline storage holding valid UTF-8 written through
/// [`fmt::Write`] or [`Buf::push`].
#[derive(Clone)]
pub struct Buf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> Default for Buf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Buf<N> {
    pub const fn new() -> Self {
        Self { bytes: [0; N], len: 0, overflowed: false }
    }

    /// Appends `s`, or marks the buffer overflowed and returns an error.
    pub fn push(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).filter(|&e| e <= N);
        match end {
            Some(end) if !self.overflowed => {
                self.bytes[self.len..end].copy_from_slice(s.as_bytes());
                self.len = end;
                Ok(())
            }
            _ => {
                self.overflowed = true;
                Err(fmt::Error)
            }
        }
    }

    pub fn as_str(&self) -> &str {
        // Only whole `&str` values are ever copied in.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s)
    }
}

impl<const N: usize> fmt::Debug for Buf<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> PartialEq<str> for Buf<N> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

/// Lowercase hex of `bytes`, written to `out`.
pub fn write_hex(out: &mut impl fmt::Write, bytes: &[u8]) -> fmt::Result {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        let pair = [HEX[(b >> 4) as usize], HEX[(b & 15) as usize]];
        out.write_str(core::str::from_utf8(&pair).map_err(|_| fmt::Error)?)?;
    }
    Ok(())
}

/// Lowercase hex of a 32-byte digest.
pub fn hex32(bytes: &[u8; 32]) -> Buf<64> {
    let mut out = Buf::new();
    let _ = write_hex(&mut out, bytes);
    out
}

/// Parses exactly 64 lowercase hex digits.
pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    let b = text.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (digit(b[2 * i])? << 4) | digit(b[2 * i + 1])?;
    }
    Some(out)
}

/// Standard base64 (with padding) of a 32-byte digest: 44 characters, the
/// form S3 takes in `x-amz-checksum-sha256`.
pub fn base64_32(bytes: &[u8; 32]) -> Buf<44> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = [0u8; 44];
    let mut o = 0;
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out[o] = ALPHABET[(n >> 18) as usize & 63];
        out[o + 1] = ALPHABET[(n >> 12) as usize & 63];
        out[o + 2] = if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] } else { b'=' };
        out[o + 3] = if chunk.len() > 2 { ALPHABET[n as usize & 63] } else { b'=' };
        o += 4;
    }
    let mut buf = Buf::new();
    let _ = buf.push(core::str::from_utf8(&out).unwrap_or(""));
    buf
}

/// Decimal of `n`.
pub fn decimal(n: u64) -> Buf<20> {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut v = n;
    loop {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let mut out = Buf::new();
    let _ = out.push(core::str::from_utf8(&digits[i..]).unwrap_or("0"));
    out
}
