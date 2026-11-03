//! Recipient-specific delivery of exact committed bytes. HPKE encrypts the
//! opening; sr25519 signatures authenticate the recipient key and sender.
//!
//! Callers must obtain the expected context/registration from their trusted
//! assignment, verify chain finality and beacon selection, and persist replay
//! decisions. This module does not consult the chain or select validators.

use anyhow::{Result, anyhow, ensure};
use hpke::{
    Deserializable, Kem as _, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256,
};
use schnorrkel::Keypair;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::hotkey_signature;

type Kem = X25519HkdfSha256;
const COMMIT_DOMAIN: &[u8] = b"slopninja/receipt-commitment/v1\0";
const KEY_DOMAIN: &[u8] = b"slopninja/validator-encryption-key/v1\0";
const SIGN_DOMAIN: &[u8] = b"slopninja/receipt-envelope-signature/v1\0";
const HPKE_INFO: &[u8] = b"slopninja/receipt-hpke/x25519-hkdfsha256-chacha20poly1305/v1";
const FRAME_MAGIC: &[u8; 8] = b"SNJOPEN1";
const FRAME_HEADER: usize = 44;
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
pub const PADDING_BUCKET_BYTES: usize = 4096;
const MAX_CIPHERTEXT_BYTES: usize =
    (MAX_PAYLOAD_BYTES + FRAME_HEADER).div_ceil(PADDING_BUCKET_BYTES) * PADDING_BUCKET_BYTES + 16;

fn random_32() -> Result<Zeroizing<[u8; 32]>> {
    let mut bytes = Zeroizing::new([0; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| anyhow!("OS randomness unavailable"))?;
    Ok(bytes)
}

/// Public, fixed-width fields known before the audit beacon is published.
/// IDs must be opaque, assigned protocol identifiers, never plaintext hashes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitmentContext {
    pub network_genesis: [u8; 32],
    pub netuid: u16,
    pub epoch: u64,
    pub task_id: [u8; 32],
    pub miner_hotkey: [u8; 32],
    pub beacon_chain: [u8; 32],
    pub beacon_round: u64,
}

impl CommitmentContext {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.network_genesis);
        out.extend(self.netuid.to_be_bytes());
        out.extend(self.epoch.to_be_bytes());
        out.extend(self.task_id);
        out.extend(self.miner_hotkey);
        out.extend(self.beacon_chain);
        out.extend(self.beacon_round.to_be_bytes());
    }
}

/// Complete expected delivery context. The caller supplies this independently
/// of the received envelope, after authenticating the assignment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryContext {
    pub committed: CommitmentContext,
    pub commitment: [u8; 32],
    pub recipient_hotkey: [u8; 32],
    pub key_id: [u8; 32],
    pub recipient_public_key: [u8; 32],
    pub not_before: u64,
    pub expires_at: u64,
}

impl DeliveryContext {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.committed.encode(&mut out);
        out.extend(self.commitment);
        out.extend(self.recipient_hotkey);
        out.extend(self.key_id);
        out.extend(self.recipient_public_key);
        out.extend(self.not_before.to_be_bytes());
        out.extend(self.expires_at.to_be_bytes());
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientKeyRecord {
    pub network_genesis: [u8; 32],
    pub netuid: u16,
    pub epoch: u64,
    pub validator_hotkey: [u8; 32],
    pub key_id: [u8; 32],
    pub public_key: [u8; 32],
    pub not_before: u64,
    pub expires_at: u64,
}

impl RecipientKeyRecord {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = KEY_DOMAIN.to_vec();
        out.extend(self.network_genesis);
        out.extend(self.netuid.to_be_bytes());
        out.extend(self.epoch.to_be_bytes());
        out.extend(self.validator_hotkey);
        out.extend(self.key_id);
        out.extend(self.public_key);
        out.extend(self.not_before.to_be_bytes());
        out.extend(self.expires_at.to_be_bytes());
        out
    }

    pub fn sign(self, key: &Keypair) -> Result<RecipientRegistration> {
        ensure!(
            key.public.to_bytes() == self.validator_hotkey,
            "Validator signing key differs"
        );
        let signature = hotkey_signature::sign(key, &self.signing_bytes());
        Ok(RecipientRegistration {
            record: self,
            signature,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientRegistration {
    pub record: RecipientKeyRecord,
    pub signature: Vec<u8>,
}

impl RecipientRegistration {
    fn check(&self, expected: &DeliveryContext, now: u64) -> Result<()> {
        let r = &self.record;
        hotkey_signature::verify(&r.validator_hotkey, &r.signing_bytes(), &self.signature)?;
        ensure!(
            r.network_genesis == expected.committed.network_genesis
                && r.netuid == expected.committed.netuid
                && r.epoch == expected.committed.epoch,
            "Recipient registration network or epoch differs"
        );
        ensure!(
            r.validator_hotkey == expected.recipient_hotkey
                && r.key_id == expected.key_id
                && r.public_key == expected.recipient_public_key,
            "Recipient registration binding differs"
        );
        ensure!(
            r.not_before <= expected.not_before
                && expected.not_before <= now
                && now < expected.expires_at
                && expected.expires_at <= r.expires_at,
            "Delivery or recipient key outside valid window"
        );
        Ok(())
    }
}

/// Private key wrapper intentionally has no Debug or Serialize implementation.
pub struct RecipientSecret(<Kem as hpke::Kem>::PrivateKey);

impl RecipientSecret {
    pub fn generate() -> Result<Self> {
        let seed = random_32()?;
        let (secret, _) = Kem::derive_keypair(seed.as_ref());
        Ok(Self(secret))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self(
            <Kem as hpke::Kem>::PrivateKey::from_bytes(bytes)
                .map_err(|_| anyhow!("Invalid recipient private key"))?,
        ))
    }

    pub fn public_key(&self) -> [u8; 32] {
        Kem::sk_to_pk(&self.0).to_bytes().into()
    }

    /// The caller must store this only in protected validator key storage.
    pub fn export_private(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes().into())
    }
}

/// Exact opaque payload plus secret salt. Deliberately neither Debug nor
/// Serialize: sending an opening publicly defeats the commitment's privacy.
pub struct PreparedPayload {
    context: CommitmentContext,
    salt: Zeroizing<[u8; 32]>,
    payload: Zeroizing<Vec<u8>>,
}

fn commitment(context: &CommitmentContext, salt: &[u8; 32], payload: &[u8]) -> [u8; 32] {
    let mut header = COMMIT_DOMAIN.to_vec();
    context.encode(&mut header);
    let mut hash = Sha256::new();
    hash.update(header);
    hash.update(salt);
    hash.update((payload.len() as u64).to_be_bytes());
    hash.update(payload);
    hash.finalize().into()
}

impl PreparedPayload {
    pub fn new(context: CommitmentContext, payload: &[u8]) -> Result<Self> {
        ensure!(
            !payload.is_empty() && payload.len() <= MAX_PAYLOAD_BYTES,
            "Payload size outside limits"
        );
        Ok(Self {
            context,
            salt: random_32()?,
            payload: Zeroizing::new(payload.to_vec()),
        })
    }

    pub fn commitment(&self) -> [u8; 32] {
        commitment(&self.context, &self.salt, &self.payload)
    }

    fn frame(&self) -> Zeroizing<Vec<u8>> {
        let len = FRAME_HEADER + self.payload.len();
        let mut frame = Zeroizing::new(Vec::with_capacity(
            len.div_ceil(PADDING_BUCKET_BYTES) * PADDING_BUCKET_BYTES,
        ));
        frame.extend(FRAME_MAGIC);
        frame.extend(self.salt.as_ref());
        frame.extend((self.payload.len() as u32).to_be_bytes());
        frame.extend(self.payload.iter());
        frame.resize(len.div_ceil(PADDING_BUCKET_BYTES) * PADDING_BUCKET_BYTES, 0);
        frame
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u16,
    pub context: DeliveryContext,
    pub encapsulated_key: [u8; 32],
    pub ciphertext: Vec<u8>,
    pub sender_signature: Vec<u8>,
}

impl Envelope {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_CIPHERTEXT_BYTES * 5 + 8192,
            "Envelope encoding exceeds limit"
        );
        let envelope: Self =
            serde_json::from_slice(bytes).map_err(|_| anyhow!("Invalid envelope encoding"))?;
        envelope.check_shape()?;
        Ok(envelope)
    }

    fn check_shape(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.sender_signature.len() == 64,
            "Unsupported or malformed envelope"
        );
        ensure!(
            (PADDING_BUCKET_BYTES + 16..=MAX_CIPHERTEXT_BYTES).contains(&self.ciphertext.len())
                && (self.ciphertext.len() - 16).is_multiple_of(PADDING_BUCKET_BYTES),
            "Ciphertext size outside limits"
        );
        Ok(())
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = SIGN_DOMAIN.to_vec();
        bytes.extend(self.version.to_be_bytes());
        bytes.extend(self.context.encode());
        bytes.extend(self.encapsulated_key);
        bytes.extend((self.ciphertext.len() as u32).to_be_bytes());
        bytes.extend(&self.ciphertext);
        bytes
    }
}

/// Uses fresh encapsulation randomness for every recipient and every call.
pub fn seal(
    opening: &PreparedPayload,
    registration: &RecipientRegistration,
    context: DeliveryContext,
    sender: &Keypair,
    now: u64,
) -> Result<Envelope> {
    registration.check(&context, now)?;
    ensure!(
        opening.context == context.committed && opening.commitment() == context.commitment,
        "Opening does not match committed assignment"
    );
    ensure!(
        sender.public.to_bytes() == context.committed.miner_hotkey,
        "Sender signing key differs"
    );
    let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(&context.recipient_public_key)
        .map_err(|_| anyhow!("Invalid recipient public key"))?;
    let aad = context.encode();
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeS::Base,
        &recipient,
        HPKE_INFO,
        &opening.frame(),
        &aad,
    )
    .map_err(|_| anyhow!("Receipt encryption failed"))?;
    let mut envelope = Envelope {
        version: 1,
        context,
        encapsulated_key: encapped.to_bytes().into(),
        ciphertext,
        sender_signature: Vec::new(),
    };
    envelope.sender_signature = hotkey_signature::sign(sender, &envelope.signing_bytes());
    Ok(envelope)
}

/// Returned plaintext stays private to the authorized caller and is zeroized
/// on drop. Neither payload nor salt is included in diagnostic formatting.
pub struct OpenedPayload {
    frame: Zeroizing<Vec<u8>>,
    length: usize,
}

impl OpenedPayload {
    pub fn as_bytes(&self) -> &[u8] {
        &self.frame[FRAME_HEADER..FRAME_HEADER + self.length]
    }
}

pub fn open(
    envelope: &Envelope,
    expected: &DeliveryContext,
    registration: &RecipientRegistration,
    recipient: &RecipientSecret,
    now: u64,
) -> Result<OpenedPayload> {
    envelope.check_shape()?;
    ensure!(
        &envelope.context == expected,
        "Envelope differs from expected assignment"
    );
    registration.check(expected, now)?;
    hotkey_signature::verify(
        &expected.committed.miner_hotkey,
        &envelope.signing_bytes(),
        &envelope.sender_signature,
    )?;
    ensure!(
        recipient.public_key() == expected.recipient_public_key,
        "Recipient private key differs"
    );
    let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(&envelope.encapsulated_key)
        .map_err(|_| anyhow!("Invalid encapsulated key"))?;
    let frame = Zeroizing::new(
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &OpModeR::Base,
            &recipient.0,
            &encapped,
            HPKE_INFO,
            &envelope.ciphertext,
            &expected.encode(),
        )
        .map_err(|_| anyhow!("Receipt decryption failed"))?,
    );
    ensure!(
        frame.len() >= FRAME_HEADER && &frame[..8] == FRAME_MAGIC,
        "Invalid encrypted opening"
    );
    let length = u32::from_be_bytes(frame[40..44].try_into()?) as usize;
    ensure!(
        length > 0 && length <= MAX_PAYLOAD_BYTES && FRAME_HEADER + length <= frame.len(),
        "Invalid encrypted payload length"
    );
    ensure!(
        frame.len()
            == (FRAME_HEADER + length).div_ceil(PADDING_BUCKET_BYTES) * PADDING_BUCKET_BYTES
            && frame[FRAME_HEADER + length..].iter().all(|b| *b == 0),
        "Invalid opening padding"
    );
    let salt: &[u8; 32] = frame[8..40].try_into()?;
    ensure!(
        commitment(
            &expected.committed,
            salt,
            &frame[FRAME_HEADER..FRAME_HEADER + length]
        ) == expected.commitment,
        "Decrypted opening does not match commitment"
    );
    Ok(OpenedPayload { frame, length })
}

#[cfg(test)]
#[path = "receipt_envelope_tests.rs"]
mod tests;
