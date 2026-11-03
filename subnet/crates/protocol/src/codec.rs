//! Canonical binary encoding.
//!
//! Integers are fixed-width little-endian. `bool` is one byte, 0 or 1.
//! Strings and byte strings carry a `u32` length prefix; strings must be UTF-8.
//! Sequences carry a `u32` item count. `Option` is a tag byte, 0 (none) or 1
//! (some) followed by the value. Enums are a `u8` tag followed by their fields.
//! Decoding is strict: every value has exactly one encoding, and records reject
//! trailing bytes, so `encode(decode(bytes)) == bytes` for every accepted input.

use crate::{Error, Result};

/// Upper bound on any length-prefixed string.
pub const MAX_STRING_LEN: u32 = 4096;
/// Upper bound on any sequence's item count.
pub const MAX_SEQUENCE_LEN: u32 = 1 << 20;
/// Largest allocation a sequence decoder reserves before its items decode.
pub const MAX_PREALLOC_BYTES: usize = 64 << 10;

#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.raw(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.raw(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.raw(&v.to_le_bytes());
    }
    pub fn u128(&mut self, v: u128) {
        self.raw(&v.to_le_bytes());
    }
    /// Length is checked at record validation; encoding a longer value is a bug.
    pub fn len_prefix(&mut self, len: usize) {
        let len = u32::try_from(len).expect("validated length fits u32");
        self.u32(len);
    }
    pub fn str(&mut self, s: &str) {
        self.len_prefix(s.len());
        self.raw(s.as_bytes());
    }
}

pub struct Decoder<'a> {
    input: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self { input }
    }
    pub fn remaining(&self) -> usize {
        self.input.len()
    }
    pub fn finish(self) -> Result<()> {
        if self.input.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingBytes)
        }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.input.len() < n {
            return Err(Error::UnexpectedEnd);
        }
        let (head, tail) = self.input.split_at(n);
        self.input = tail;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("exact length"))
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub fn u128(&mut self) -> Result<u128> {
        Ok(u128::from_le_bytes(self.array()?))
    }
    pub fn bytes32(&mut self) -> Result<[u8; 32]> {
        self.array()
    }
    pub fn bytes64(&mut self) -> Result<[u8; 64]> {
        self.array()
    }
    pub fn len_prefix(&mut self, field: &'static str, max: u32) -> Result<usize> {
        let len = self.u32()?;
        if len > max {
            return Err(Error::LengthLimit {
                field,
                len: len.into(),
            });
        }
        Ok(len as usize)
    }
    pub fn str(&mut self) -> Result<String> {
        let len = self.len_prefix("string", MAX_STRING_LEN)?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::InvalidUtf8)
    }
}

/// A value with exactly one canonical encoding.
pub trait Canonical: Sized {
    fn encode_to(&self, e: &mut Encoder);
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self>;
}

macro_rules! canonical_int {
    ($($t:ident),*) => {$(
        impl Canonical for $t {
            fn encode_to(&self, e: &mut Encoder) {
                e.$t(*self);
            }
            fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
                d.$t()
            }
        }
    )*};
}
canonical_int!(u8, u16, u32, u64, u128);

impl Canonical for bool {
    fn encode_to(&self, e: &mut Encoder) {
        e.u8(u8::from(*self));
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Error::InvalidBool(other)),
        }
    }
}

impl Canonical for String {
    fn encode_to(&self, e: &mut Encoder) {
        e.str(self);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        d.str()
    }
}

impl<T: Canonical> Canonical for Vec<T> {
    fn encode_to(&self, e: &mut Encoder) {
        e.len_prefix(self.len());
        for item in self {
            item.encode_to(e);
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        let len = d.len_prefix("sequence", MAX_SEQUENCE_LEN)?;
        // The prefix is untrusted, so reserve at most `MAX_PREALLOC_BYTES` up
        // front. Growth beyond it is paid for by items that actually decoded.
        let cap = MAX_PREALLOC_BYTES / size_of::<T>().max(1);
        let mut out = Vec::with_capacity(len.min(d.remaining()).min(cap));
        for _ in 0..len {
            out.push(T::decode_from(d)?);
        }
        Ok(out)
    }
}

impl<T: Canonical> Canonical for Option<T> {
    fn encode_to(&self, e: &mut Encoder) {
        match self {
            None => e.u8(0),
            Some(v) => {
                e.u8(1);
                v.encode_to(e);
            }
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            0 => Ok(None),
            1 => Ok(Some(T::decode_from(d)?)),
            tag => Err(Error::InvalidTag {
                field: "option",
                tag,
            }),
        }
    }
}

/// Defines a struct whose canonical encoding is its fields in declaration order.
macro_rules! canonical_struct {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            $( $(#[$fmeta:meta])* pub $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: $ty ),*
        }
        impl $crate::codec::Canonical for $name {
            fn encode_to(&self, e: &mut $crate::codec::Encoder) {
                $( $crate::codec::Canonical::encode_to(&self.$field, e); )*
            }
            fn decode_from(d: &mut $crate::codec::Decoder<'_>) -> $crate::Result<Self> {
                Ok(Self {
                    $( $field: <$ty as $crate::codec::Canonical>::decode_from(d)? ),*
                })
            }
        }
    };
}
pub(crate) use canonical_struct;
