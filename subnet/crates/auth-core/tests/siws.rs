//! G-A1 wallet rows: challenge expiry and reuse, a signature over different
//! bytes, and raw and `<Bytes>`-wrapped signatures accepted only for the
//! issued bytes. Test keys come from sr25519 development seeds.

use rtao_core::keys::{CRYPTO_SR25519, Keypair};
use slop_ninja_auth_core::Origin;
use slop_ninja_auth_core::account_wire::WalletMode;
use slop_ninja_auth_core::siws::{
    self, Challenge, ChallengeFacts, Nonce, SignedForm, SiwsError, check_challenge,
};
use slop_ninja_auth_core::state::Refusal;
use slop_ninja_auth_core::token::Token;

const NOW_MS: u64 = 1_794_547_800_000;

fn pair(uri: &str) -> Keypair {
    Keypair::from_uri(uri, CRYPTO_SR25519).unwrap()
}

fn site() -> Origin {
    Origin::parse("https://site.example").unwrap()
}

fn issued(mode: WalletMode, pk: &[u8; 32]) -> Vec<u8> {
    let site = site();
    let nonce = Nonce::from_random([0xA5; 16]);
    siws::message(
        mode,
        &Challenge {
            site: &site,
            public_key: pk,
            nonce: &nonce,
            issued_at_ms: NOW_MS,
            expires_at_ms: NOW_MS + 300_000,
        },
    )
    .into_bytes()
}

fn sig(kp: &Keypair, bytes: &[u8]) -> [u8; 64] {
    kp.sign(bytes).unwrap().try_into().unwrap()
}

fn wrap(bytes: &[u8]) -> Vec<u8> {
    [b"<Bytes>".as_slice(), bytes, b"</Bytes>"].concat()
}

#[test]
fn message_layout() {
    let alice = pair("//Alice");
    let pk = alice.public_key_bytes();
    let text = String::from_utf8(issued(WalletMode::Extension, &pk)).unwrap();
    let address = siws::address_42(&pk);
    // A generic-Substrate (prefix 42) address: 48 characters, leading `5`,
    // the same one the keypair reports.
    assert_eq!(address.len(), 48);
    assert!(address.starts_with('5'));
    assert_eq!(address, alice.ss58_address());
    assert_eq!(
        text,
        format!(
            "site.example wants you to sign in with your Bittensor account:\n{address}\n\nSign in to SlopNinja. This is not a transaction and costs nothing.\n\nURI: https://site.example/signin/\nVersion: 1\nNonce: 55MPJTB9D5MPJTB9D5MPJTB9D5\nIssued At: 2026-11-13T05:30:00.000Z\nExpiration Time: 2026-11-13T05:35:00.000Z"
        )
    );
    let cli = String::from_utf8(issued(WalletMode::Cli, &pk)).unwrap();
    assert_eq!(
        cli,
        format!(
            "site.example sign-in for {address}; nonce 55MPJTB9D5MPJTB9D5MPJTB9D5; issued 2026-11-13T05:30:00.000Z; expires 2026-11-13T05:35:00.000Z"
        )
    );
    assert!(!cli.contains('\n'));
    assert_eq!(Nonce::from_random([0; 16]).encode(), "0".repeat(26));
    assert_eq!(
        Nonce::from_random([0xFF; 16]).encode(),
        format!("7{}", "Z".repeat(25))
    );
}

#[test]
fn address_decoding() {
    let alice = pair("//Alice");
    let pk = alice.public_key_bytes();
    // Prefix 42 and the Polkadot prefix 0 both decode to the same key.
    assert_eq!(siws::decode_address(&siws::address_42(&pk)), Ok(pk));
    let polkadot = rtao_core::keys::ss58_from_public(pk, 0);
    assert_eq!(siws::decode_address(&polkadot), Ok(pk));
    // The same address with its last character changed fails its checksum.
    let good = siws::address_42(&pk);
    let last = if good.ends_with('Y') { "Z" } else { "Y" };
    let bad_checksum = format!("{}{last}", &good[..good.len() - 1]);
    for bad in ["", bad_checksum.as_str(), "0x1234", &"5".repeat(65)] {
        assert_eq!(siws::decode_address(bad), Err(SiwsError::Address), "{bad}");
    }
}

#[test]
fn raw_and_wrapped_only_for_issued_bytes() {
    let alice = pair("//Alice");
    let pk = alice.public_key_bytes();
    for mode in [WalletMode::Extension, WalletMode::Cli] {
        let msg = issued(mode, &pk);
        assert_eq!(
            siws::verify(&msg, &sig(&alice, &msg), &pk),
            Ok(SignedForm::Raw)
        );
        assert_eq!(
            siws::verify(&msg, &sig(&alice, &wrap(&msg)), &pk),
            Ok(SignedForm::Wrapped)
        );
        // Nothing else: double wrapping, the hex text of the bytes, a
        // trailing newline, other bytes, another key.
        let refused: Vec<Vec<u8>> = vec![
            wrap(&wrap(&msg)),
            hex::encode(&msg).into_bytes(),
            format!("0x{}", hex::encode(&msg)).into_bytes(),
            [msg.as_slice(), b"\n"].concat(),
            [b"<Bytes>".as_slice(), &msg].concat(),
            b"site.example wants you to sign in".to_vec(),
        ];
        for other in refused {
            assert_eq!(
                siws::verify(&msg, &sig(&alice, &other), &pk),
                Err(SiwsError::BadSignature)
            );
        }
        let bob = pair("//Bob");
        assert_eq!(
            siws::verify(&msg, &sig(&bob, &msg), &pk),
            Err(SiwsError::BadSignature)
        );
        // A signature over another challenge's bytes.
        let mut later = msg.clone();
        let n = later.len();
        later[n - 2] ^= 1;
        assert_eq!(
            siws::verify(&msg, &sig(&alice, &later), &pk),
            Err(SiwsError::BadSignature)
        );
    }
}

#[test]
fn signature_text() {
    let s = [0xABu8; 64];
    let h = hex::encode(s);
    assert_eq!(siws::parse_signature(&h), Ok(s));
    assert_eq!(siws::parse_signature(&format!("0x{h}")), Ok(s));
    assert_eq!(
        siws::parse_signature(&format!("0x{}", h.to_uppercase())),
        Ok(s)
    );
    assert_eq!(siws::parse_signature(&format!(" {h}\n")), Ok(s));
    for bad in [
        h[..126].to_owned(),
        format!("{h}00"),
        format!("01{h}"),
        format!("{}zz", &h[..126]),
        String::new(),
    ] {
        assert_eq!(
            siws::parse_signature(&bad),
            Err(SiwsError::SignatureFormat),
            "{bad}"
        );
    }
}

#[test]
fn challenge_expiry_and_reuse() {
    let binding = Token::from_random([5; 32]);
    let row = ChallengeFacts {
        binding_h: binding.hash(),
        expires_at_ms: NOW_MS + 300_000,
    };
    let cookie = binding.encode();
    let mut store = Some(row);
    // The verify takes the row once.
    assert_eq!(
        check_challenge(store.take().as_ref(), Some(&cookie), NOW_MS),
        Ok(())
    );
    // Reuse of the challenge id finds nothing.
    assert_eq!(
        check_challenge(store.take().as_ref(), Some(&cookie), NOW_MS),
        Err(Refusal::Unknown)
    );
    // Expired.
    assert_eq!(
        check_challenge(Some(&row), Some(&cookie), NOW_MS + 300_000),
        Err(Refusal::Expired)
    );
    // Another browser's flow cookie, or none.
    let other = Token::from_random([6; 32]).encode();
    assert_eq!(
        check_challenge(Some(&row), Some(&other), NOW_MS),
        Err(Refusal::BindingMismatch)
    );
    assert_eq!(
        check_challenge(Some(&row), None, NOW_MS),
        Err(Refusal::MissingFlowCookie)
    );
}

#[test]
fn local_domain_line_carries_the_port() {
    // Local development serves both hosts over HTTPS on their own ports.
    let site = Origin::parse("https://site.localhost:8787").unwrap();
    let alice = pair("//Alice");
    let pk = alice.public_key_bytes();
    let nonce = Nonce::from_random([1; 16]);
    let text = siws::message(
        WalletMode::Extension,
        &Challenge {
            site: &site,
            public_key: &pk,
            nonce: &nonce,
            issued_at_ms: NOW_MS,
            expires_at_ms: NOW_MS + 1,
        },
    );
    assert!(text.starts_with("site.localhost:8787 wants you to sign in"));
    assert!(text.contains("\nURI: https://site.localhost:8787/signin/\n"));
}
