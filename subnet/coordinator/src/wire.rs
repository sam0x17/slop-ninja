//! JSON wire forms of chain identifiers used by webhook ingestion and the
//! finalized cursor.
//!
//! Protocol records, digests and the testnet genesis come from the shared
//! `slop-ninja-protocol` crate. These types add only the canonical JSON
//! encodings (lowercase `0x` hex, decimal strings) that the webhook contract and
//! Durable Object SQL storage need; [`Id`] converts losslessly to and from the
//! shared [`Digest`].

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use slop_ninja_protocol::Digest;
use std::fmt;

/// Largest integer that Durable Object SQL storage round-trips exactly through
/// JavaScript numbers. Stored heights, positions and timestamps must not exceed it.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Canonical lowercase `0x`-prefixed 32-byte identifier or hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(pub [u8; 32]);

impl Id {
    pub const ZERO: Self = Self([0; 32]);

    /// Parses only the canonical form: `0x` followed by 64 lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, WireError> {
        let raw = value.strip_prefix("0x").ok_or(WireError::Id)?;
        if raw.len() != 64 || !raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(WireError::Id);
        }
        let mut out = [0; 32];
        hex::decode_to_slice(raw, &mut out).map_err(|_| WireError::Id)?;
        Ok(Self(out))
    }

    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 32]
    }
}

impl From<Digest> for Id {
    fn from(digest: Digest) -> Self {
        Self(digest.0)
    }
}

impl From<Id> for Digest {
    fn from(id: Id) -> Self {
        Digest(id.0)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .map_err(|_| de::Error::custom("canonical 32-byte lowercase hex required"))
    }
}

/// A u64 carried as a canonical decimal string (no sign, no leading zeros), so
/// no JSON reader passes it through floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DecimalU64(pub u64);

impl DecimalU64 {
    pub fn parse(value: &str) -> Result<Self, WireError> {
        if value.is_empty()
            || value.len() > 20
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(WireError::Decimal);
        }
        value
            .parse::<u64>()
            .map(Self)
            .map_err(|_| WireError::Decimal)
    }
}

impl Serialize for DecimalU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(|_| de::Error::custom("canonical decimal u64 string required"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Id,
    Decimal,
}

/// A finalized block identity: height and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRef {
    pub number: u64,
    pub hash: Id,
}

/// The header fields the cursor needs to check chain continuity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizedHeader {
    pub number: u64,
    pub hash: Id,
    pub parent_hash: Id,
}

impl FinalizedHeader {
    pub fn block_ref(&self) -> BlockRef {
        BlockRef {
            number: self.number,
            hash: self.hash,
        }
    }
}
