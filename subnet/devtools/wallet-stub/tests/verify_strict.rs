//! Every variant against the verifier the account Worker calls
//! (`rtao_core::keys::verify_strict`, at the fleet's pin): the raw and the
//! wrapped signatures verify over exactly the bytes they signed, and the
//! wrong-key and wrong-message signatures over nothing the sign-in accepts.
//! The stub's SS58 encoding is checked against the core's own.

use rtao_core::keys::{CRYPTO_SR25519, public_key_from_ss58, ss58_from_public, verify_strict};
use slop_ninja_dev_wallet_stub::{Variant, decode_ss58, keypair, seed, sign, ss58, wrap};

/// A sign-in message in the extension path's layout, on the dev session's
/// site origin.
fn siws(addr: &str) -> String {
    format!(
        "slop.localhost:8787 wants you to sign in with your Bittensor account:\n{addr}\n\n\
         Sign in to SlopNinja. This is not a transaction and costs nothing.\n\n\
         URI: https://slop.localhost:8787/signin/\nVersion: 1\nNonce: 0123456789ABCDEFGHJKMNPQRS\n\
         Issued At: 2026-11-13T00:00:00.000Z\nExpiration Time: 2026-11-13T00:05:00.000Z"
    )
}

/// The account Worker's rule: the raw bytes, then the `<Bytes>` wrapping.
fn accepted(addr: &str, msg: &[u8], sig: &[u8]) -> (bool, bool) {
    let check = |m: &[u8]| verify_strict(m, sig, addr, CRYPTO_SR25519).expect("well-formed input");
    (check(msg), check(&wrap(msg)))
}

#[test]
fn ss58_matches_the_core() {
    for name in ["alice", "bob", "wallet-only"] {
        let pk = keypair(&seed(name)).public.to_bytes();
        for prefix in [0u16, 2, 42, 64, 1000, 16_383] {
            let ours = ss58(&pk, prefix);
            assert_eq!(ours, ss58_from_public(pk, prefix), "{name} prefix {prefix}");
            assert_eq!(decode_ss58(&ours), Some(pk));
            // The core refuses some registry prefixes; where it decodes, it agrees.
            if let Ok(core) = public_key_from_ss58(&ours) {
                assert_eq!(core, pk, "{name} prefix {prefix}");
            }
        }
        assert_eq!(public_key_from_ss58(&ss58(&pk, 42)).ok(), Some(pk), "the sign-in message's prefix");
    }
}

#[test]
fn every_variant_meets_verify_strict_as_intended() {
    for name in ["alice", "bob", "wallet-only"] {
        let addr = sign(name, b"", Variant::Raw).ss58;
        let message = siws(&addr);
        for v in Variant::ALL {
            let s = sign(name, message.as_bytes(), v);
            let got = accepted(&s.ss58, message.as_bytes(), &s.signature);
            let want = match v {
                Variant::Raw => (true, false),
                Variant::Wrapped => (false, true),
                Variant::WrongKey | Variant::WrongMessage => (false, false),
            };
            assert_eq!(got, want, "{name} {}", v.name());
        }
    }
}

#[test]
fn the_cli_format_one_line_message_also_verifies() {
    let addr = sign("cli-user", b"", Variant::Raw).ss58;
    let m = format!("slop.localhost:8787 sign-in for {addr}; nonce 01HZZ; issued 2026-11-13T00:00:00.000Z; expires 2026-11-13T00:05:00.000Z");
    let s = sign("cli-user", m.as_bytes(), Variant::Raw);
    assert_eq!(accepted(&s.ss58, m.as_bytes(), &s.signature), (true, false));
    // A signature is 64 bytes: anything else is an error, not a refusal.
    assert!(verify_strict(m.as_bytes(), &s.signature[..63], &s.ss58, CRYPTO_SR25519).is_err());
}
