//! rtao-rpc block webhook authentication and body validation.
//!
//! Implements receiver contract v1: HMAC-SHA256 over a fixed framing of the
//! `X-Rtao-Signature` fields and the exact raw body, keyed by the subscription's
//! 32-byte signing secret. The raw body is authenticated before it is parsed.
//! A notification is a wakeup hint only: it establishes neither finality nor
//! authority, and its `header_only` state does not mean state reads are ready.
//!
//! The provider names its delivery contract in `X-Rtao-Webhook-Contract`
//! ([`contract`]). `rtao-block-webhooks/1` and `rtao-block-webhooks/2` share
//! wire version 1 (this framing and these bodies); they differ only in
//! timing. A `/2` delivery is pushed within about a second of finality, so it
//! can arrive before the RPC endpoints this Worker reads serve that block.
//! Nothing here or downstream reads the delivered block through RPC: the
//! cursor advances on the parent link alone, and every chain read happens at
//! the RPC-confirmed watermark, which only RPC's own finalized head moves
//! (`journal`, `heartbeat`).

use crate::wire::{DecimalU64, FinalizedHeader, Id, MAX_SAFE_INTEGER};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub const SIGNATURE_HEADER: &str = "X-Rtao-Signature";
/// The provider's contract name for the delivery (absent before the
/// provider started naming it, which is contract 1).
pub const CONTRACT_HEADER: &str = "X-Rtao-Webhook-Contract";
pub const CONTRACT_V1: &str = "rtao-block-webhooks/1";
pub const CONTRACT_V2: &str = "rtao-block-webhooks/2";
pub const CHALLENGE_RESPONSE_HEADER: &str = "X-Rtao-Challenge-Response";
pub const HMAC_DOMAIN: &[u8] = b"rtao.webhooks.hmac-sha256.v1\0";
pub const SIGNATURE_VERSION: u8 = 1;
pub const EVENT_VERSION: u8 = 1;
pub const MAX_SKEW_SECONDS: u64 = 300;
pub const MAX_BODY_BYTES: usize = 256 * 1024;
pub const MAX_SIGNATURE_HEADER_BYTES: usize = 4096;
/// The provider's acknowledgement window (hosted webhook service contract,
/// "Retries and automatic pause": 5 s to acknowledge; a later answer is a
/// failed attempt). The provider sends a delivery's body whole at once, so a
/// body still incomplete after this window can no longer make its delivery
/// succeed: the receiver stops reading it (`408`).
pub const DELIVERY_ACK_MS: u64 = 5_000;
const MAX_KEY_ID_BYTES: usize = 64;
const MAX_CHALLENGE_BYTES: usize = 256;

/// The subscription's signing secret. It has no Debug, Clone or Serialize
/// implementation and is zeroed on drop.
pub struct SigningKey {
    key_id: String,
    secret: Zeroizing<[u8; 32]>,
}

impl SigningKey {
    /// `secret_hex` is the 64-hex-character `signing_secret` from registration.
    pub fn from_hex(key_id: &str, secret_hex: &str) -> Result<Self, ConfigError> {
        if !valid_key_id(key_id) {
            return Err(ConfigError::KeyId);
        }
        let secret_hex = secret_hex.trim();
        if secret_hex.len() != 64 {
            return Err(ConfigError::SigningSecret);
        }
        let mut secret = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(secret_hex, secret.as_mut())
            .map_err(|_| ConfigError::SigningSecret)?;
        if *secret == [0; 32] {
            return Err(ConfigError::SigningSecret);
        }
        Ok(Self {
            key_id: key_id.to_owned(),
            secret,
        })
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// HMAC-SHA256 over the v1 framing. Exposed for fixtures; receivers call
    /// [`authenticate`].
    pub fn mac(&self, signature: &SignedFields, raw: &[u8]) -> [u8; 32] {
        // HMAC accepts keys of any length, so this constructor cannot fail.
        #[allow(clippy::expect_used)]
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_ref())
            .expect("HMAC-SHA256 accepts any key length");
        mac.update(HMAC_DOMAIN);
        mac.update(&[signature.version]);
        mac.update(&(signature.key_id.len() as u64).to_be_bytes());
        mac.update(signature.key_id.as_bytes());
        mac.update(&signature.timestamp.to_be_bytes());
        mac.update(&signature.context.subscription.0);
        mac.update(&signature.context.event_id.0);
        mac.update(&signature.context.delivery_id.0);
        mac.update(&signature.attempt.to_be_bytes());
        mac.update(&(raw.len() as u64).to_be_bytes());
        mac.update(raw);
        mac.finalize().into_bytes().into()
    }
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_ID_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    KeyId,
    SigningSecret,
    Subscription,
    Chain,
    Netuid,
    /// See [`crate::config::TESTNET_ONLY_GENESIS`].
    TestnetOnlyGenesis,
    /// `NETWORK` must be `testnet` (see [`crate::config::NETWORK`]).
    Network,
    /// A watchdog or RPC setting is not a valid positive integer.
    Setting,
    RpcUrl,
    /// A bearer token secret shorter than [`crate::config::MIN_TOKEN_BYTES`].
    Token,
    /// `COORDINATOR_HOTKEY_SS58` is missing or not an SS58 account.
    Hotkey,
    /// The T12 application public keys are missing, malformed or not distinct.
    TrustedKeys,
    /// `EVAL_MAIN_AUTHORITY` or `EVAL_MAIN_FIRST_COHORT` is malformed
    /// (see [`crate::config::Authority::parse`]).
    Authority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryContext {
    pub subscription: Id,
    pub event_id: Id,
    pub delivery_id: Id,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureWire {
    version: u8,
    key_id: String,
    timestamp: DecimalU64,
    context: DeliveryContext,
    attempt: u16,
    mac: Id,
}

/// The authenticated (non-MAC) fields of `X-Rtao-Signature`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedFields {
    pub version: u8,
    pub key_id: String,
    /// Unix seconds.
    pub timestamp: u64,
    pub context: DeliveryContext,
    pub attempt: u16,
}

/// What a receiver expects: its own key, subscription and chain.
pub struct ReceiverConfig {
    pub key: SigningKey,
    pub subscription: Id,
    pub chain: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    MissingSignature,
    BodyTooLarge,
    MalformedSignature,
    Version,
    KeyId,
    Subscription,
    Attempt,
    Skew,
    Mac,
}

impl AuthError {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingSignature => "missing_signature",
            Self::BodyTooLarge => "body_too_large",
            Self::MalformedSignature => "malformed_signature",
            Self::Version => "signature_version",
            Self::KeyId => "key_id",
            Self::Subscription => "subscription",
            Self::Attempt => "attempt",
            Self::Skew => "timestamp_skew",
            Self::Mac => "mac",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            Self::BodyTooLarge => 413,
            _ => 401,
        }
    }
}

/// Checks the signature header's fixed fields (shape, version, key id,
/// subscription, attempt and timestamp window) before any body byte is read:
/// a request that fails here can never authenticate, so the receiver refuses
/// it without admitting its body. Authentication itself is [`authenticate`].
pub fn precheck(config: &ReceiverConfig, header: Option<&str>, now_secs: u64) -> Result<(), AuthError> {
    header_fields(config, header.ok_or(AuthError::MissingSignature)?, now_secs).map(|_| ())
}

/// Authenticates the exact raw body against the signature header. Nothing in
/// the body is read before this succeeds.
pub fn authenticate(
    config: &ReceiverConfig,
    header: Option<&str>,
    raw: &[u8],
    now_secs: u64,
) -> Result<SignedFields, AuthError> {
    let header = header.ok_or(AuthError::MissingSignature)?;
    if raw.len() > MAX_BODY_BYTES {
        return Err(AuthError::BodyTooLarge);
    }
    let (fields, mac) = header_fields(config, header, now_secs)?;
    let expected = config.key.mac(&fields, raw);
    if !bool::from(expected.ct_eq(&mac.0)) {
        return Err(AuthError::Mac);
    }
    Ok(fields)
}

/// The signature header's fields and MAC, every field but the MAC checked.
fn header_fields(config: &ReceiverConfig, header: &str, now_secs: u64) -> Result<(SignedFields, Id), AuthError> {
    if header.len() > MAX_SIGNATURE_HEADER_BYTES {
        return Err(AuthError::MalformedSignature);
    }
    let wire: SignatureWire =
        serde_json::from_str(header).map_err(|_| AuthError::MalformedSignature)?;
    if wire.version != SIGNATURE_VERSION {
        return Err(AuthError::Version);
    }
    if wire.key_id != config.key.key_id {
        return Err(AuthError::KeyId);
    }
    if wire.context.subscription != config.subscription {
        return Err(AuthError::Subscription);
    }
    if wire.attempt == 0 {
        return Err(AuthError::Attempt);
    }
    if now_secs.abs_diff(wire.timestamp.0) > MAX_SKEW_SECONDS {
        return Err(AuthError::Skew);
    }
    let fields = SignedFields {
        version: wire.version,
        key_id: wire.key_id,
        timestamp: wire.timestamp.0,
        context: wire.context,
        attempt: wire.attempt,
    };
    Ok((fields, wire.mac))
}

/// The delivery contract a delivery names. Both accepted contracts use wire
/// version 1; `/2` only delivers sooner (push-based, possibly before RPC
/// serves the block).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Contract {
    /// No contract header: a provider from before contract names, which
    /// delivered under contract 1.
    #[default]
    Unnamed,
    V1,
    V2,
}

impl Contract {
    /// The journal counter of new deliveries under this contract.
    pub fn counter(self) -> &'static str {
        match self {
            Self::Unnamed => "deliveries_contract_unnamed",
            Self::V1 => "deliveries_contract_v1",
            Self::V2 => "deliveries_contract_v2",
        }
    }
}

/// Reads `X-Rtao-Webhook-Contract`. Contracts 1 and 2 (and no header) are
/// accepted; any other name is refused, since its wire format is unqualified
/// here (the provider then retries, and the watchdog carries the cursor
/// meanwhile).
pub fn contract(header: Option<&str>) -> Result<Contract, BodyError> {
    match header.map(|h| h.trim_matches([' ', '\t'])) {
        None => Ok(Contract::Unnamed),
        Some(CONTRACT_V1) => Ok(Contract::V1),
        Some(CONTRACT_V2) => Ok(Contract::V2),
        Some(_) => Err(BodyError::Contract),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateReadiness {
    HeaderOnly,
    Executed,
}

/// An authenticated finalized-block notification, ready for durable ingestion.
/// It is also the Worker-to-journal message, so it carries no body text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockNotification {
    pub delivery_id: Id,
    pub subscription: Id,
    pub event_id: Id,
    pub attempt: u16,
    /// SHA-256 of the exact raw body, for duplicate-delivery comparison.
    pub body_sha256: Id,
    pub journal_position: u64,
    pub observed_at_ms: u64,
    pub header: FinalizedHeader,
    pub state_readiness: StateReadiness,
    /// The contract the delivery named (set by the receiver from
    /// [`CONTRACT_HEADER`]; the body does not carry it).
    #[serde(default)]
    pub contract: Contract,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Echo `challenge` in [`CHALLENGE_RESPONSE_HEADER`]; never enqueue it.
    Challenge {
        challenge: String,
    },
    Finalized(Box<BlockNotification>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyError {
    Malformed,
    ChallengeSubscription,
    ChallengeValue,
    EventId,
    EventVersion,
    Chain,
    JournalPosition,
    /// Only the finalized stream is accepted; best/reorg handling is unqualified.
    UnsupportedKind,
    Block,
    StorageRange,
    /// `X-Rtao-Webhook-Contract` names a contract other than 1 or 2.
    Contract,
}

impl BodyError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed_body",
            Self::ChallengeSubscription => "challenge_subscription",
            Self::ChallengeValue => "challenge_value",
            Self::EventId => "event_id",
            Self::EventVersion => "event_version",
            Self::Chain => "chain",
            Self::JournalPosition => "journal_position",
            Self::UnsupportedKind => "unsupported_kind",
            Self::Block => "block",
            Self::StorageRange => "storage_range",
            Self::Contract => "webhook_contract",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            Self::Malformed => 400,
            _ => 422,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeWire {
    #[serde(rename = "type")]
    ty: String,
    subscription: Id,
    challenge: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeWire {
    event_id: Id,
    event: EventWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    version: u8,
    chain: Id,
    journal_position: DecimalU64,
    observed_at_ms: DecimalU64,
    kind: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizedKindWire {
    #[serde(rename = "type")]
    ty: String,
    block: BlockWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockWire {
    number: DecimalU64,
    hash: Id,
    parent_hash: Id,
    // Parsed for schema validation only; the cursor verifies blocks through RPC.
    #[allow(dead_code)]
    state_root: Id,
    #[allow(dead_code)]
    extrinsics_root: Id,
    #[allow(dead_code)]
    #[serde(default)]
    chain_timestamp_ms: Option<DecimalU64>,
    state_readiness: StateReadiness,
}

/// Interprets a body that [`authenticate`] already accepted.
pub fn parse_authenticated(
    config: &ReceiverConfig,
    signed: &SignedFields,
    raw: &[u8],
) -> Result<Delivery, BodyError> {
    let value: serde_json::Value = serde_json::from_slice(raw).map_err(|_| BodyError::Malformed)?;
    let object = value.as_object().ok_or(BodyError::Malformed)?;
    if object.contains_key("type") {
        let challenge: ChallengeWire =
            serde_json::from_value(value).map_err(|_| BodyError::Malformed)?;
        if challenge.ty != "challenge" {
            return Err(BodyError::Malformed);
        }
        if challenge.subscription != config.subscription {
            return Err(BodyError::ChallengeSubscription);
        }
        if !valid_challenge(&challenge.challenge) {
            return Err(BodyError::ChallengeValue);
        }
        return Ok(Delivery::Challenge {
            challenge: challenge.challenge,
        });
    }

    let envelope: EnvelopeWire = serde_json::from_value(value).map_err(|_| BodyError::Malformed)?;
    if envelope.event_id != signed.context.event_id {
        return Err(BodyError::EventId);
    }
    let event = envelope.event;
    if event.version != EVENT_VERSION {
        return Err(BodyError::EventVersion);
    }
    if event.chain != config.chain {
        return Err(BodyError::Chain);
    }
    if event.journal_position.0 == 0 {
        return Err(BodyError::JournalPosition);
    }
    if event.kind.get("type").and_then(|t| t.as_str()) != Some("finalized") {
        return Err(BodyError::UnsupportedKind);
    }
    let kind: FinalizedKindWire =
        serde_json::from_value(event.kind).map_err(|_| BodyError::Malformed)?;
    debug_assert_eq!(kind.ty, "finalized");
    let block = kind.block;
    validate_block(&block)?;
    for value in [
        block.number.0,
        event.journal_position.0,
        event.observed_at_ms.0,
    ] {
        if value > MAX_SAFE_INTEGER {
            return Err(BodyError::StorageRange);
        }
    }
    Ok(Delivery::Finalized(Box::new(BlockNotification {
        delivery_id: signed.context.delivery_id,
        subscription: signed.context.subscription,
        event_id: signed.context.event_id,
        attempt: signed.attempt,
        body_sha256: Id(Sha256::digest(raw).into()),
        journal_position: event.journal_position.0,
        observed_at_ms: event.observed_at_ms.0,
        header: FinalizedHeader {
            number: block.number.0,
            hash: block.hash,
            parent_hash: block.parent_hash,
        },
        state_readiness: block.state_readiness,
        contract: Contract::Unnamed,
    })))
}

fn validate_block(block: &BlockWire) -> Result<(), BodyError> {
    if block.hash.is_zero() || block.hash == block.parent_hash {
        return Err(BodyError::Block);
    }
    if block.number.0 == 0 {
        if !block.parent_hash.is_zero() {
            return Err(BodyError::Block);
        }
    } else if block.parent_hash.is_zero() {
        return Err(BodyError::Block);
    }
    Ok(())
}

/// The nonce is echoed in a response header, so only header-safe tokens pass.
fn valid_challenge(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CHALLENGE_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests;
