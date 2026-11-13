//! Dev only: sr25519 signatures over named test seeds, in the forms a wallet
//! sign-in must accept or refuse.
//!
//! A test seed is `SHA-256("slop_ninja_dev_wallet_stub/v1/" ‖ name)` taken as
//! a mini secret key and expanded as Substrate expands one (Ed25519 mode).
//! The keys are public by construction and only for tests: they are not
//! Substrate's `//Alice` keys and must never hold funds.
//!
//! Signatures use Substrate's signing context `b"substrate"`, as
//! `rtao_core::keys::verify_strict` checks them. Each is derandomized from
//! the seed and the signed bytes, so the same input always gives the same
//! signature (fixtures stay stable). The four variants:
//!
//! - `raw`: the message bytes as given (what `signRaw` with `type: "bytes"`
//!   signs in a wallet that does not wrap);
//! - `wrapped`: `<Bytes>` ‖ message ‖ `</Bytes>` (polkadot{.js} and its kin);
//! - `wrong-key`: the raw message signed by another test key, reported under
//!   the requested seed's address;
//! - `wrong-message`: the message with ` (altered)` appended, signed by the
//!   right key.

use blake2::{Blake2b512, Digest as _};
use rand_core::{CryptoRng, RngCore};
use schnorrkel::{ExpansionMode, Keypair, MiniSecretKey, PublicKey, Signature, signing_context};
use sha2::{Sha256, Sha512};

/// Substrate's sr25519 signing context.
pub const CONTEXT: &[u8] = b"substrate";
/// The generic Substrate SS58 prefix the sign-in message uses.
pub const PREFIX_GENERIC: u16 = 42;
/// What `wrong-message` appends.
pub const ALTERED: &[u8] = b" (altered)";

/// The signature variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Variant {
    Raw,
    Wrapped,
    WrongKey,
    WrongMessage,
}

impl Variant {
    pub const ALL: [Variant; 4] = [Variant::Raw, Variant::Wrapped, Variant::WrongKey, Variant::WrongMessage];

    pub fn name(self) -> &'static str {
        match self {
            Variant::Raw => "raw",
            Variant::Wrapped => "wrapped",
            Variant::WrongKey => "wrong-key",
            Variant::WrongMessage => "wrong-message",
        }
    }
}

/// The 32-byte mini secret of a named test seed.
pub fn seed(name: &str) -> [u8; 32] {
    Sha256::digest(format!("slop_ninja_dev_wallet_stub/v1/{name}").as_bytes()).into()
}

/// The keypair of a mini secret.
pub fn keypair(mini: &[u8; 32]) -> Keypair {
    // A 32-byte array always parses as a mini secret key.
    match MiniSecretKey::from_bytes(mini) {
        Ok(m) => m.expand_to_keypair(ExpansionMode::Ed25519),
        Err(_) => unreachable!("32 bytes"),
    }
}

/// `<Bytes>` ‖ `m` ‖ `</Bytes>`.
pub fn wrap(m: &[u8]) -> Vec<u8> {
    [b"<Bytes>".as_slice(), m, b"</Bytes>".as_slice()].concat()
}

/// A deterministic witness source: SHA-512 in counter mode over the seed
/// and the signed bytes. For test signatures only.
struct Witness {
    key: [u8; 64],
    counter: u64,
    buf: Vec<u8>,
}

impl Witness {
    fn new(mini: &[u8; 32], msg: &[u8]) -> Witness {
        let mut h = Sha512::new();
        h.update(b"slop_ninja_dev_wallet_stub/witness/v1");
        h.update(mini);
        h.update(msg);
        Witness { key: h.finalize().into(), counter: 0, buf: Vec::new() }
    }
}

impl RngCore for Witness {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }

    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for d in dest.iter_mut() {
            if self.buf.is_empty() {
                let mut h = Sha512::new();
                h.update(self.key);
                h.update(self.counter.to_le_bytes());
                self.counter += 1;
                self.buf = h.finalize().to_vec();
            }
            *d = self.buf.remove(0);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for Witness {}

/// The signature of `msg` under `mini`.
pub fn sign_bytes(mini: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let kp = keypair(mini);
    let t = schnorrkel::context::attach_rng(signing_context(CONTEXT).bytes(msg), Witness::new(mini, msg));
    kp.sign(t).to_bytes()
}

/// One signature of a variant, by the seed named `name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    pub variant: Variant,
    /// The requested seed's address and public key.
    pub ss58: String,
    pub public_key: [u8; 32],
    /// The bytes actually signed.
    pub signed: Vec<u8>,
    pub signature: [u8; 64],
}

pub fn sign(name: &str, message: &[u8], variant: Variant) -> Signed {
    let mini = seed(name);
    let public_key = keypair(&mini).public.to_bytes();
    let (key, signed) = match variant {
        Variant::Raw => (mini, message.to_vec()),
        Variant::Wrapped => (mini, wrap(message)),
        Variant::WrongKey => (seed(&format!("{name}/other")), message.to_vec()),
        Variant::WrongMessage => (mini, [message, ALTERED].concat()),
    };
    Signed { variant, ss58: ss58(&public_key, PREFIX_GENERIC), public_key, signature: sign_bytes(&key, &signed), signed }
}

fn ss58_hash(data: &[u8]) -> [u8; 64] {
    let mut h = Blake2b512::new();
    h.update(b"SS58PRE");
    h.update(data);
    h.finalize().into()
}

fn prefix_bytes(prefix: u16) -> Vec<u8> {
    if prefix < 64 {
        vec![prefix as u8]
    } else {
        let p = prefix & 0x3FFF;
        vec![(((p & 0b0000_0000_1111_1100) >> 2) as u8) | 0b0100_0000, ((p >> 8) as u8) | (((p & 0b11) as u8) << 6)]
    }
}

/// The SS58 address of a 32-byte public key.
pub fn ss58(public_key: &[u8; 32], prefix: u16) -> String {
    let mut data = prefix_bytes(prefix);
    data.extend_from_slice(public_key);
    let sum = ss58_hash(&data);
    data.extend_from_slice(&sum[..2]);
    bs58::encode(data).into_string()
}

/// The public key of an SS58 address with any prefix.
pub fn decode_ss58(addr: &str) -> Option<[u8; 32]> {
    let raw = bs58::decode(addr).into_vec().ok()?;
    let plen = match raw.first()? {
        0..=63 => 1,
        64..=127 => 2,
        _ => return None,
    };
    if raw.len() != plen + 32 + 2 {
        return None;
    }
    let (body, sum) = raw.split_at(plen + 32);
    if ss58_hash(body)[..2] != *sum {
        return None;
    }
    body[plen..].try_into().ok()
}

/// What a sign-in verifier accepts: the raw bytes, then the `<Bytes>`
/// wrapping, nothing else. `(raw, wrapped)`.
pub fn verify(addr: &str, message: &[u8], signature: &[u8]) -> Option<(bool, bool)> {
    let pk = PublicKey::from_bytes(&decode_ss58(addr)?).ok()?;
    let sig = Signature::from_bytes(signature).ok()?;
    let ok = |m: &[u8]| pk.verify_simple(CONTEXT, m, &sig).is_ok();
    Some((ok(message), ok(&wrap(message))))
}

/// Hex with a `0x` prefix, as wallets print signatures.
pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Bytes from hex, with or without `0x`.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    hex::decode(s.trim().trim_start_matches("0x")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ss58_round_trips_and_checks_its_checksum() {
        // Encoding against Substrate's own is checked in tests/verify_strict.rs.
        let pk = keypair(&seed("alice")).public.to_bytes();
        assert!(ss58(&pk, 42).starts_with('5'), "prefix 42 addresses start with 5");
        // One-byte and two-byte prefixes round-trip.
        for p in [0u16, 2, 42, 64, 1000] {
            assert_eq!(decode_ss58(&ss58(&pk, p)), Some(pk), "prefix {p}");
        }
        let mut bad = ss58(&pk, 42).into_bytes();
        bad[10] = if bad[10] == b'a' { b'b' } else { b'a' };
        assert_eq!(decode_ss58(std::str::from_utf8(&bad).unwrap()), None);
    }

    #[test]
    fn variants_verify_only_as_intended() {
        let msg = b"slop.localhost:8787 wants you to sign in with your Bittensor account:\nexample";
        for v in Variant::ALL {
            let s = sign("alice", msg, v);
            let got = verify(&s.ss58, msg, &s.signature).unwrap();
            let want = match v {
                Variant::Raw => (true, false),
                Variant::Wrapped => (false, true),
                Variant::WrongKey | Variant::WrongMessage => (false, false),
            };
            assert_eq!(got, want, "{}", v.name());
        }
    }

    #[test]
    fn signatures_are_deterministic_and_seeds_distinct() {
        let a = sign("alice", b"m", Variant::Raw);
        assert_eq!(a, sign("alice", b"m", Variant::Raw));
        assert_ne!(a.signature, sign("alice", b"n", Variant::Raw).signature);
        assert_ne!(a.public_key, sign("bob", b"m", Variant::Raw).public_key);
        assert_eq!(sign("alice", b"m", Variant::WrongKey).ss58, a.ss58);
    }
}
