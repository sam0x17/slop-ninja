//! Fixed-size identifiers, lowercase hex on the wire and nothing else.

use serde::{Deserialize, Serialize};

macro_rules! hex_id {
    ($(#[$m:meta])* $name:ident, $n:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub [u8; $n]);

        impl $name {
            /// Lowercase hex.
            pub fn hex(&self) -> String {
                let mut s = String::with_capacity(2 * $n);
                for b in self.0 {
                    s.push(HEX[usize::from(b >> 4)] as char);
                    s.push(HEX[usize::from(b & 15)] as char);
                }
                s
            }

            /// Exactly the lowercase hex form.
            pub fn parse(s: &str) -> Option<$name> {
                parse_hex::<$n>(s).map($name)
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.hex())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                $name::parse(&s).ok_or_else(|| serde::de::Error::custom(concat!("expected ", stringify!($n), " bytes of lowercase hex")))
            }
        }
    };
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != 2 * N {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
    }
    Some(out)
}

hex_id! {
    /// An account's user id: 16 random bytes (32 hex digits). It crosses
    /// Workers only in `X-SN-Principal`, internal bodies and queue messages,
    /// never in a public answer or a log.
    UserId, 16
}

hex_id! {
    /// A detection's `id_h`: `H("slug_v1" ‖ slug)` for a public page,
    /// `HMAC(pepper_v, "slug_v1" ‖ slug)` for a secret one (64 hex digits).
    /// Only the detect Worker computes it.
    IdH, 32
}
