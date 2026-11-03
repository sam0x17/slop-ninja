//! Raw sr25519 application-message signatures compatible with Bittensor hotkeys.
//!
//! `wallet.hotkey.sign(message_bytes)` signs those bytes through `sp_core::Pair`
//! with the `substrate` signing context. This module adds no hash, SCALE encoding,
//! `<Bytes>` wrapper, or extrinsic `MultiSignature` discriminator. The caller must
//! define an unambiguous, domain-separated application message and authenticate
//! the public key against the intended hotkey registration.
//!
//! This is not an extrinsic-payload encoder or a generic wallet verifier. Only
//! sr25519 is accepted; Ed25519/ECDSA require separately specified schemes.
//! Bittensor's wallet verifier also tries a `<Bytes>` fallback, which this strict
//! application protocol deliberately does not accept.
//!
//! Sources: Bittensor `sdk/bittensor-core/src/keys/mod.rs` at revision
//! `67dcf7f791dc495064c293f080a0702cb433e51e`, and its pinned `sp_core` implementation:
//! <https://github.com/RaoFoundation/polkadot-sdk/blob/cacb4310f20c7cac83eb3ccd8ed5a5ad4212608a/substrate/primitives/core/src/sr25519.rs>.

use anyhow::{Result, anyhow};
use schnorrkel::{Keypair, PublicKey, Signature};

const SIGNING_CONTEXT: &[u8] = b"substrate";

/// Verify a raw 64-byte signature over the exact supplied application bytes.
pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> Result<()> {
    let public =
        PublicKey::from_bytes(public_key).map_err(|_| anyhow!("invalid sr25519 public key"))?;
    let signature = Signature::from_bytes(signature)
        .map_err(|_| anyhow!("invalid sr25519 signature encoding"))?;
    public
        .verify_simple(SIGNING_CONTEXT, message, &signature)
        .map_err(|_| anyhow!("sr25519 application signature verification failed"))
}

/// Sign exact application bytes; signatures need not repeat byte-for-byte.
pub fn sign(keypair: &Keypair, message: &[u8]) -> Vec<u8> {
    keypair
        .sign_simple(SIGNING_CONTEXT, message)
        .to_bytes()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use schnorrkel::{ExpansionMode, MiniSecretKey};

    fn substrate_test_keypair() -> Keypair {
        // Public test seed and expected public key from sp_core's
        // sr_test_vector_should_work at the pinned revision cited above.
        let seed = hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap();
        MiniSecretKey::from_bytes(&seed)
            .unwrap()
            .expand_to_keypair(ExpansionMode::Ed25519)
    }

    #[test]
    fn substrate_public_key_vector_and_raw_message_roundtrip() {
        let keypair = substrate_test_keypair();
        assert_eq!(
            hex::encode(keypair.public.to_bytes()),
            "44a996beb1eef7bdcab976ab6d2ca26104834164ecf28fb375600576fcc6eb0f"
        );
        for message in [b"".as_slice(), b"slopninja/recipient-registration/v1\0test"] {
            let signature = sign(&keypair, message);
            assert_eq!(signature.len(), 64);
            verify(&keypair.public.to_bytes(), message, &signature).unwrap();
        }
    }

    #[test]
    fn rejects_wrong_context_wrapping_message_and_signature_encoding() {
        let keypair = substrate_test_keypair();
        let public = keypair.public.to_bytes();
        let message = b"slopninja/recipient-registration/v1\0test";
        let wrong_context = keypair.sign_simple(b"other-application", message);
        assert!(verify(&public, message, &wrong_context.to_bytes()).is_err());

        let wrapped = [b"<Bytes>".as_slice(), message, b"</Bytes>"].concat();
        assert!(verify(&public, message, &sign(&keypair, &wrapped)).is_err());
        let signature = sign(&keypair, message);
        assert!(verify(&public, b"different payload", &signature).is_err());
        assert!(verify(&public, message, &signature[..63]).is_err());
        let prefixed = [vec![1], signature].concat();
        assert!(verify(&public, message, &prefixed).is_err());
        assert!(verify(&[255; 32], message, &prefixed).is_err());
    }

    #[test]
    fn rejects_official_deprecated_signature_vector() {
        // Fixed negative vector from sp_core's verify_known_old_message_should_work.
        // Upstream accepts it only through verify_deprecated, not modern Pair::verify.
        let public: [u8; 32] =
            hex::decode("b4bfa1f7a5166695eb75299fd1c4c03ea212871c342f2c5dfea0902b2c246918")
                .unwrap()
                .try_into()
                .unwrap();
        let signature = hex::decode(concat!(
            "5a9755f069939f45d96aaf125cf5ce7ba1db998686f87f2fb3cbdea922078741a",
            "73891ba265f70c31436e18a9acd14d189d73c12317ab6c313285cd938453202"
        ))
        .unwrap();
        let message = b"Verifying that I am the owner of 5G9hQLdsKQswNPgB499DeA5PkFBbgkLPJWkkS6FAM6xGQ8xD. Hash: 221455a3\n";
        assert!(verify(&public, message, &signature).is_err());
    }
}
