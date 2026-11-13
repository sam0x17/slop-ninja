//! Every variant against the verifier the account Worker calls
//! (`rtao_core::keys::verify_strict`, at the fleet's pin): the raw and the
//! wrapped signatures verify over exactly the bytes they signed, and the
//! wrong-key and wrong-message signatures over nothing the sign-in accepts.
//! The messages are the account contract's own fixture of a
//! `/wallet/challenge` answer, and each signature travels in the contract's
//! `/wallet/verify` body. The stub's SS58 encoding is checked against the
//! core's own.

use rtao_core::keys::{CRYPTO_SR25519, public_key_from_ss58, ss58_from_public, verify_strict};
use slop_ninja_dev_wallet_stub::{
    Variant, WalletChallengeAnswer, WalletVerifyRequest, decode_ss58, keypair, seed, sign, ss58, unhex, verify_request, wrap,
};

/// The contract's fixture of a `/wallet/challenge` answer (the extension
/// path's message layout).
fn fixture_challenge() -> WalletChallengeAnswer {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/detect-page/tests/fixtures/account/wallet-challenge-answer.json");
    let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    serde_json::from_str(&text).expect("a /wallet/challenge answer")
}

/// The same challenge issued to `addr`: the message's address line (its
/// second) replaced, as the account Worker writes it for that address.
fn issued_to(c: &WalletChallengeAnswer, addr: &str) -> WalletChallengeAnswer {
    let mut lines: Vec<&str> = c.message.split('\n').collect();
    assert!(lines[0].ends_with(" wants you to sign in with your Bittensor account:"), "{}", lines[0]);
    lines[1] = addr;
    WalletChallengeAnswer { challenge_id: c.challenge_id.clone(), message: lines.join("\n") }
}

/// The account Worker's rule: the raw bytes, then the `<Bytes>` wrapping.
fn accepted(addr: &str, msg: &[u8], sig: &[u8]) -> (bool, bool) {
    let check = |m: &[u8]| verify_strict(m, sig, addr, CRYPTO_SR25519).expect("well-formed input");
    (check(msg), check(&wrap(msg)))
}

fn intended(v: Variant) -> (bool, bool) {
    match v {
        Variant::Raw => (true, false),
        Variant::Wrapped => (false, true),
        Variant::WrongKey | Variant::WrongMessage => (false, false),
    }
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
    let fixture = fixture_challenge();
    for name in ["alice", "bob", "wallet-only"] {
        let addr = sign(name, b"", Variant::Raw).ss58;
        let challenge = issued_to(&fixture, &addr);
        for v in Variant::ALL {
            let s = sign(name, challenge.message.as_bytes(), v);
            assert_eq!(accepted(&s.ss58, challenge.message.as_bytes(), &s.signature), intended(v), "{name} {}", v.name());
        }
    }
}

#[test]
fn verify_bodies_carry_each_variant_to_the_verifier() {
    let addr = sign("alice", b"", Variant::Raw).ss58;
    let challenge = issued_to(&fixture_challenge(), &addr);
    for v in Variant::ALL {
        let s = sign("alice", challenge.message.as_bytes(), v);
        // The body as a suite posts it, read back as the account Worker reads it.
        let json = serde_json::to_string(&verify_request(&challenge, &s)).expect("serializes");
        let body: WalletVerifyRequest = serde_json::from_str(&json).expect("a /wallet/verify body");
        assert_eq!(body.challenge_id, challenge.challenge_id);
        assert!(body.signature.starts_with("0x") && body.signature.len() == 2 + 128, "{}", body.signature);
        let sig = unhex(&body.signature).expect("hex");
        assert_eq!(accepted(&addr, challenge.message.as_bytes(), &sig), intended(v), "{}", v.name());
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
