//! The publisher's Durable Object state and the bodies of its state calls.
//!
//! The state crosses two encodings, and both must round-trip it:
//!
//! - **The state calls.** A run sends `/checkpoint` and `/end` with a
//!   `serde_json` body, and `/begin` answers with one. JSON object keys are
//!   strings, so a map keyed by a number arrives as `{"117": ...}`. The
//!   state object parses these bodies with `serde_json` too
//!   ([`parse_body`]), never with the runtime's `Request::json` (which hands
//!   `JSON.parse`'s object to `serde_wasm_bindgen`: its `u64` refuses the
//!   string key `"117"`, which stopped every checkpoint from 2026-11-04).
//! - **Storage.** `Storage::put` and `get` convert with
//!   `serde_wasm_bindgen`'s defaults: a Rust map becomes a JS `Map` whose
//!   keys stay numbers.
//!
//! Every map with non-string keys in this state reads its keys with
//! [`u64_keys`], which accepts a number or its decimal string, so a map loads
//! from either form whatever path it came by.

use serde::{Deserialize, Serialize};

use crate::alerts::{AlertState, PublisherObs};
use crate::publish::Memo;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Durable {
    pub alerts: AlertState,
    pub memo: Memo,
    /// The last publication pass's own conditions, alerted by the next
    /// run's first phase.
    pub publisher: PublisherObs,
    pub last_published: Option<u64>,
    /// Due alert events the last alert phase left undelivered, and the start
    /// of the first run of the current series that left some (sent in the
    /// relay heartbeat).
    #[serde(default)]
    pub undelivered: u64,
    #[serde(default)]
    pub undelivered_since_ms: Option<u64>,
    /// The dashboard's boundary-latched values and rebuild cursor.
    #[serde(default)]
    pub dashboard: crate::dashboard::Memo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lease {
    pub holder: String,
    pub until_ms: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Begin {
    pub holder: String,
    pub now_ms: u64,
}

/// `/begin`'s answer: the state, and whether the previous holder left its
/// lease behind (it stopped before `/end`).
#[derive(Serialize, Deserialize)]
pub struct Begun {
    pub state: Durable,
    pub interrupted: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Save {
    pub holder: String,
    pub now_ms: u64,
    pub state: Durable,
}

/// A state call's body, in the encoding its sender wrote (`serde_json`).
/// The error names the type and position, never the body's content.
pub fn parse_body<T: for<'d> Deserialize<'d>>(body: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(body)
}

/// A map keyed by `u64` that serializes as an ordinary map and reads each
/// key from a number or from its decimal string (a JSON object key). Use
/// with `#[serde(with = "crate::durable::u64_keys")]`.
pub mod u64_keys {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::marker::PhantomData;

    use serde::de::{self, MapAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<V: Serialize, S: Serializer>(
        map: &BTreeMap<u64, V>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        map.serialize(s)
    }

    pub fn deserialize<'de, V: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<u64, V>, D::Error> {
        d.deserialize_map(MapVisitor(PhantomData))
    }

    struct MapVisitor<V>(PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
        type Value = BTreeMap<u64, V>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a map keyed by u64")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((Key(k), v)) = access.next_entry::<Key, V>()? {
                out.insert(k, v);
            }
            Ok(out)
        }
    }

    /// One key: a non-negative integer, as a number or as decimal digits.
    struct Key(u64);

    impl<'de> Deserialize<'de> for Key {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(KeyVisitor).map(Key)
        }
    }

    struct KeyVisitor;

    impl Visitor<'_> for KeyVisitor {
        type Value = u64;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a u64 or its decimal string")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            // Canonical decimal only: what `u64`'s `Display` writes.
            let canonical = !v.is_empty()
                && v.bytes().all(|b| b.is_ascii_digit())
                && (v == "0" || !v.starts_with('0'));
            canonical
                .then(|| v.parse().ok())
                .flatten()
                .ok_or_else(|| E::invalid_value(de::Unexpected::Str(v), &self))
        }
    }
}

#[cfg(test)]
#[path = "durable_tests.rs"]
mod tests;
