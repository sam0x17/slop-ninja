//! Chain encoding and cryptography for the coordinator's own weight commits.
//!
//! Everything here is rtao's portable core seam (`rtao-core` without default
//! features, which is upstream `bittensor-core` without `host`), used exactly
//! as rtao's native client uses it:
//!
//! - calls are composed **by name** from live V15 metadata
//!   (`Runtime::compose_call`), never from a baked index or runtime version;
//! - storage keys and values are resolved by pallet and item name, with the
//!   metadata's hashers, key types and defaults;
//! - the signing payload is `call ++ extra ++ additional` over the runtime's
//!   declared signed extensions (`Runtime::signature_payload`); the spec and
//!   transaction versions inside `additional` are the live ones the chain
//!   reports, read per runtime and compared nowhere;
//! - the signed extrinsic is assembled by `Runtime::encode_signed_extrinsic`
//!   with an sr25519 `MultiSignature`;
//! - the timelocked payload and its drand quicknet round come from rtao's
//!   `timelock_commit` (`commit_payload`, `reveal_round_at`) and upstream
//!   `encrypt_and_compress`.
//!
//! rtao's client, transaction executor and journal (`rtao-client`, `rtao-tx`)
//! need tokio's network stack and do not build for `wasm32-unknown-unknown`;
//! the coordinator replaces only their I/O and journaling, in `signing` and
//! `cf`. The parity crate (`parity/`) checks these encodings byte for byte
//! against rtao's native builders.

pub use rtao_core::codec::Value;
use rtao_core::codec::decode::Cursor;
use rtao_core::extrinsic::TxParams;
use rtao_core::keys::Keypair;
use rtao_core::runtime::Runtime;
use rtao_core::timelock::epoch_schedule::{
    self, EpochScheduleState, current_epoch_pre_run_coinbase, simulate_run_coinbase,
};
use sha2::{Digest as _, Sha256};
use std::fmt;
use zeroize::Zeroizing;

pub const SUBTENSOR: &str = "SubtensorModule";
pub const COMMIT_CALL: &str = "commit_timelocked_mechanism_weights";
/// MultiSignature variant index of sr25519.
pub const SR25519_SIGNATURE: u8 = 1;
/// Bittensor's SS58 prefix (presentation only).
pub const SS58_FORMAT: u16 = 42;
/// Nominal block time used for the drand round, as rtao's scheduler uses it.
pub const BLOCK_TIME_SECS: f64 = 12.0;
/// Yuma's ciphertext limit (`YUMA_COMMIT_SIZE_BYTES`).
pub const YUMA_COMMIT_LIMIT: usize = 5_000;

/// A content-free failure. Messages never carry secret material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainError {
    pub code: &'static str,
    pub detail: String,
}

impl ChainError {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

fn codec(code: &'static str) -> impl Fn(rtao_core::codec::Error) -> ChainError {
    move |e| ChainError::new(code, e.to_string())
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha256(bytes))
}

/// Decodes the `Option<OpaqueMetadata>` SCALE value that
/// `state_call("Metadata_metadata_at_version", u32 15)` returns into the
/// prefixed metadata blob (`meta` magic, version byte, payload).
pub fn opaque_metadata(scale: &[u8]) -> Result<Vec<u8>, ChainError> {
    let mut cursor = Cursor::new(scale);
    match cursor.byte().map_err(codec("metadata_malformed"))? {
        1 => {}
        0 => return Err(ChainError::new("metadata_unavailable", "V15 not served")),
        _ => return Err(ChainError::new("metadata_malformed", "bad option tag")),
    }
    let len =
        rtao_core::codec::decode::compact_len(&mut cursor).map_err(codec("metadata_malformed"))?;
    let body = cursor.take(len).map_err(codec("metadata_malformed"))?;
    if cursor.remaining() != 0 {
        return Err(ChainError::new("metadata_malformed", "trailing bytes"));
    }
    Ok(body.to_vec())
}

/// One runtime's parsed V15 metadata plus the live versions it was served at.
pub struct ChainRuntime {
    core: Runtime,
    metadata_sha256: [u8; 32],
}

impl ChainRuntime {
    /// Parses prefixed V15 metadata. rtao composes calls only from V15, so a
    /// degraded (V14) blob is refused the same way.
    pub fn parse(prefixed: &[u8], spec_version: u32, tx_version: u32) -> Result<Self, ChainError> {
        let core = Runtime::parse(prefixed, spec_version, tx_version, SS58_FORMAT)
            .map_err(codec("metadata_unparsable"))?;
        if !core.is_v15 {
            return Err(ChainError::new(
                "metadata_degraded",
                "V15 metadata required",
            ));
        }
        Ok(Self {
            core,
            metadata_sha256: sha256(prefixed),
        })
    }

    pub fn spec_version(&self) -> u32 {
        self.core.spec_version
    }

    pub fn tx_version(&self) -> u32 {
        self.core.transaction_version
    }

    pub fn metadata_sha256(&self) -> [u8; 32] {
        self.metadata_sha256
    }

    pub fn core(&self) -> &Runtime {
        &self.core
    }

    fn entry(
        &self,
        pallet: &str,
        item: &str,
    ) -> Result<&rtao_core::runtime::StorageInfo, ChainError> {
        self.core
            .storage_entry(pallet, item)
            .ok_or_else(|| ChainError::new("storage_absent", format!("{pallet}.{item}")))
    }

    /// Whether live metadata declares `pallet.item`.
    pub fn has_storage(&self, pallet: &str, item: &str) -> bool {
        self.core.storage_entry(pallet, item).is_some()
    }

    /// The full storage key for `pallet.item(params...)`, by name.
    pub fn storage_key(
        &self,
        pallet: &str,
        item: &str,
        params: &[Value],
    ) -> Result<Vec<u8>, ChainError> {
        let entry = self.entry(pallet, item)?;
        if params.len() != entry.key_types.len() {
            return Err(ChainError::new(
                "storage_key_arity",
                format!("{pallet}.{item} takes {} keys", entry.key_types.len()),
            ));
        }
        self.core
            .storage_key(entry, params)
            .map_err(codec("storage_key_unencodable"))
    }

    /// Decodes a storage value by name. An absent value is `Null` for an
    /// optional item and the metadata default otherwise.
    pub fn decode_storage(
        &self,
        pallet: &str,
        item: &str,
        raw: Option<&[u8]>,
    ) -> Result<Value, ChainError> {
        let entry = self.entry(pallet, item)?;
        let bytes = match raw {
            Some(bytes) => bytes,
            None if entry.modifier == "Optional" => return Ok(Value::Null),
            None => entry.default_bytes.as_slice(),
        };
        let mut cursor = Cursor::new(bytes);
        cursor.strict = true;
        let value = self
            .core
            .decode_id(entry.value_type, &mut cursor)
            .map_err(codec("storage_value_undecodable"))?;
        if cursor.remaining() != 0 {
            return Err(ChainError::new(
                "storage_value_undecodable",
                format!("{pallet}.{item}: trailing bytes"),
            ));
        }
        Ok(value)
    }

    /// `SubtensorModule.commit_timelocked_mechanism_weights`, composed by name.
    /// The same argument names and values as rtao's generated builder.
    pub fn commit_call(
        &self,
        netuid: u16,
        mecid: u8,
        commit: &[u8],
        reveal_round: u64,
        commit_reveal_version: u16,
    ) -> Result<Vec<u8>, ChainError> {
        let args = Value::record(vec![
            ("netuid".into(), Value::Int(i128::from(netuid))),
            ("mecid".into(), Value::Int(i128::from(mecid))),
            ("commit".into(), Value::Bytes(commit.to_vec())),
            ("reveal_round".into(), Value::Int(i128::from(reveal_round))),
            (
                "commit_reveal_version".into(),
                Value::Int(i128::from(commit_reveal_version)),
            ),
        ]);
        self.core
            .compose_call(SUBTENSOR, COMMIT_CALL, &args)
            .map_err(codec("call_unencodable"))
    }

    /// The exact bytes the hotkey signs (hashed when longer than 256 bytes).
    pub fn signing_payload(&self, call: &[u8], params: &TxParams) -> Result<Vec<u8>, ChainError> {
        self.core
            .signature_payload(call, params)
            .map_err(codec("payload_unencodable"))
    }

    /// The signed extrinsic and its blake2-256 hash.
    pub fn signed_extrinsic(
        &self,
        call: &[u8],
        public: [u8; 32],
        signature: &[u8; 64],
        params: &TxParams,
    ) -> Result<(Vec<u8>, [u8; 32]), ChainError> {
        self.core
            .encode_signed_extrinsic(call, public, signature, SR25519_SIGNATURE, params)
            .map_err(codec("extrinsic_unencodable"))
    }

    /// Decodes `System.Events` into flat records.
    pub fn events(&self, raw: Option<&[u8]>) -> Result<Vec<EventRecord>, ChainError> {
        let value = self.decode_storage("System", "Events", raw)?;
        let Value::List(items) = value else {
            return Err(ChainError::new("events_undecodable", "not a list"));
        };
        items.iter().map(EventRecord::from_value).collect()
    }

    /// The pallet and call names at the head of one encoded extrinsic.
    pub fn extrinsic_call(&self, extrinsic: &[u8]) -> Result<(String, String), ChainError> {
        let decoded = self
            .core
            .decode_extrinsic(extrinsic, false)
            .map_err(codec("extrinsic_undecodable"))?;
        let call =
            field(&decoded, "call").ok_or(ChainError::new("extrinsic_undecodable", "no call"))?;
        let module = field(call, "call_module").and_then(as_str);
        let function = field(call, "call_function").and_then(as_str);
        match (module, function) {
            (Some(m), Some(f)) => Ok((m.to_owned(), f.to_owned())),
            _ => Err(ChainError::new("extrinsic_undecodable", "no call name")),
        }
    }
}

/// One `frame_system::EventRecord`.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRecord {
    pub phase: String,
    pub extrinsic_index: Option<u32>,
    pub pallet: String,
    pub name: String,
    pub attributes: Value,
}

impl EventRecord {
    fn from_value(value: &Value) -> Result<Self, ChainError> {
        let bad = || ChainError::new("events_undecodable", "record shape");
        let phase = field(value, "phase")
            .and_then(as_str)
            .ok_or_else(bad)?
            .to_owned();
        let extrinsic_index = match field(value, "extrinsic_idx") {
            None | Some(Value::Null) => None,
            Some(v) => Some(u32::try_from(as_u64(v).ok_or_else(bad)?).map_err(|_| bad())?),
        };
        Ok(Self {
            phase,
            extrinsic_index,
            pallet: field(value, "module_id")
                .and_then(as_str)
                .ok_or_else(bad)?
                .to_owned(),
            name: field(value, "event_id")
                .and_then(as_str)
                .ok_or_else(bad)?
                .to_owned(),
            attributes: field(value, "attributes").cloned().unwrap_or(Value::Null),
        })
    }

    /// The positional attributes (unnamed fields) or the named ones in order.
    pub fn args(&self) -> Vec<&Value> {
        match &self.attributes {
            Value::Tuple(items) | Value::List(items) => items.iter().collect(),
            Value::Dict(entries) => entries.iter().map(|(_, v)| v).collect(),
            Value::Null => Vec::new(),
            other => vec![other],
        }
    }
}

// ------------------------------------------------------------ value access

pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, Value::Str(s) if s == name))
            .map(|(_, v)| v),
        _ => None,
    }
}

pub fn as_str(value: &Value) -> Option<&str> {
    match value {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

pub fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Int(i) => u64::try_from(*i).ok(),
        Value::Uint(u) => u64::try_from(*u).ok(),
        // A newtype around one integer (NetUid, MechId) decodes unwrapped, but
        // tolerate a one-element tuple too.
        Value::Tuple(items) if items.len() == 1 => items.first().and_then(as_u64),
        _ => None,
    }
}

pub fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// An AccountId32 as cyscale renders it (SS58), or raw bytes / 0x-hex.
pub fn as_account(value: &Value) -> Option<[u8; 32]> {
    match value {
        Value::Str(s) if s.starts_with("0x") => hex::decode(&s[2..]).ok()?.try_into().ok(),
        Value::Str(s) => rtao_core::keys::public_key_from_ss58(s).ok(),
        Value::Bytes(b) => b.clone().try_into().ok(),
        _ => None,
    }
}

pub fn as_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Bytes(b) => Some(b.clone()),
        Value::Str(s) if s.starts_with("0x") => hex::decode(&s[2..]).ok(),
        Value::List(items) => items
            .iter()
            .map(|v| as_u64(v).and_then(|b| u8::try_from(b).ok()))
            .collect(),
        _ => None,
    }
}

pub fn as_list(value: &Value) -> Option<&[Value]> {
    match value {
        Value::List(items) | Value::Tuple(items) => Some(items),
        _ => None,
    }
}

// ----------------------------------------------------------------- keys

/// The coordinator hotkey: sr25519 from the 32-byte mini-secret seed, the
/// same derivation rtao's `KeypairSigner::from_seed` uses. Errors are fixed
/// codes; upstream diagnostics can echo secret input and are dropped.
pub struct Hotkey {
    pair: Keypair,
}

impl Hotkey {
    pub fn from_seed_hex(seed_hex: &str) -> Result<Self, ChainError> {
        let trimmed = seed_hex.trim();
        let raw = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        if raw.len() != 64 {
            return Err(ChainError::new("hotkey_seed_invalid", "expected 32 bytes"));
        }
        let seed = Zeroizing::new(
            hex::decode(raw).map_err(|_| ChainError::new("hotkey_seed_invalid", "not hex"))?,
        );
        let pair = Keypair::from_seed(&seed, rtao_core::keys::CRYPTO_SR25519)
            .map_err(|_| ChainError::new("hotkey_seed_invalid", "derivation failed"))?;
        Ok(Self { pair })
    }

    pub fn public(&self) -> [u8; 32] {
        self.pair.public_key_bytes()
    }

    pub fn sign(&self, payload: &[u8]) -> Result<[u8; 64], ChainError> {
        let signature = self
            .pair
            .sign(payload)
            .map_err(|_| ChainError::new("signing_failed", "sr25519"))?;
        signature
            .try_into()
            .map_err(|_| ChainError::new("signing_failed", "signature length"))
    }
}

/// Strict sr25519 verification of exactly `payload` (no `<Bytes>` retry).
pub fn verify_sr25519(public: &[u8; 32], payload: &[u8], signature: &[u8; 64]) -> bool {
    rtao_core::keys::verify_strict(
        payload,
        signature,
        &ss58(*public),
        rtao_core::keys::CRYPTO_SR25519,
    )
    .unwrap_or(false)
}

pub fn ss58(public: [u8; 32]) -> String {
    rtao_core::keys::ss58_from_public(public, SS58_FORMAT)
}

pub fn public_from_ss58(address: &str) -> Result<[u8; 32], ChainError> {
    rtao_core::keys::public_key_from_ss58(address)
        .map_err(|_| ChainError::new("bad_address", "not an SS58 account"))
}

// ------------------------------------------------------------ transaction

/// A mortal era `{period, current}`, its birth block and its expiry: the
/// first height at which the transaction can no longer be included. rtao's
/// client uses this shape, anchored at the birth block's hash.
pub fn mortal_era(period: u64, current: u64) -> (Value, u64, u64) {
    let birth = rtao_core::extrinsic::era_birth(period, current);
    let era = Value::record(vec![
        ("period".into(), Value::Uint(u128::from(period))),
        ("current".into(), Value::Uint(u128::from(current))),
    ]);
    (era, birth, birth.saturating_add(period))
}

pub fn tx_params(era: Value, nonce: u64, genesis: [u8; 32], era_block_hash: [u8; 32]) -> TxParams {
    TxParams {
        era,
        nonce,
        tip: 0,
        tip_asset_id: None,
        genesis_hash: genesis,
        era_block_hash,
        // A keyfile signer has no METADATA_HASH capability in rtao, so
        // CheckMetadataHash is Disabled, exactly as rtao signs.
        metadata_hash: None,
    }
}

// ------------------------------------------------------------- timelock

/// The SCALE `WeightsTlockPayload` the chain decrypts and checks against the
/// committing hotkey.
pub fn commit_payload(hotkey: [u8; 32], uids: &[u16], values: &[u16], version_key: u64) -> Vec<u8> {
    rtao_core::timelock::timelock_commit::commit_payload(
        hotkey.to_vec(),
        uids.to_vec(),
        values.to_vec(),
        version_key,
    )
}

/// The drand round for a commit included right after `state.current_block`,
/// with `now_secs` the timestamp of that block: rtao's pallet-faithful
/// prediction plus its three-block security offset.
pub fn reveal_round(
    now_secs: f64,
    state: &EpochScheduleState,
    reveal_period_epochs: u64,
) -> Result<u64, ChainError> {
    rtao_core::timelock::timelock_commit::reveal_round_at(
        now_secs,
        state,
        reveal_period_epochs,
        BLOCK_TIME_SECS,
    )
    .map_err(|e| ChainError::new("reveal_round", e.to_string()))
}

/// Compressed TLE ciphertext of `payload` to drand quicknet round `round`.
/// Randomized: every call yields different bytes.
pub fn encrypt(payload: &[u8], round: u64) -> Result<Vec<u8>, ChainError> {
    rtao_core::timelock::encrypt_and_compress(payload, round)
        .map_err(|e| ChainError::new("timelock_failed", e.to_string()))
}

/// Decrypts a compressed TLE ciphertext with a quicknet round signature.
pub fn decrypt(ciphertext: &[u8], signature: &[u8]) -> Result<Vec<u8>, ChainError> {
    rtao_core::timelock::decrypt_and_decompress(ciphertext, signature)
        .map_err(|e| ChainError::new("timelock_failed", e.to_string()))
}

// --------------------------------------------------------------- epochs

/// The schedule fields one preflight reads, at block `current_block`.
pub fn schedule(
    tempo: u16,
    last_epoch_block: u64,
    pending_epoch_at: u64,
    subnet_epoch_index: u64,
    blocks_since_last_step: u64,
    current_block: u64,
) -> EpochScheduleState {
    EpochScheduleState {
        last_epoch_block,
        pending_epoch_at,
        subnet_epoch_index,
        tempo,
        blocks_since_last_step,
        current_block,
    }
}

/// The epoch a commit included at `current_block + 1` is keyed under, as
/// `predict_first_reveal_block` computes it.
pub fn commit_epoch(state: &EpochScheduleState) -> u64 {
    let block = state.current_block.saturating_add(1);
    let after = simulate_run_coinbase(state, block);
    current_epoch_pre_run_coinbase(&after, block)
}

/// The first block after `state.current_block` whose epoch runs, if one comes
/// within `limit` blocks.
pub fn next_epoch_block(state: &EpochScheduleState, limit: u64) -> Option<u64> {
    let mut current = state.clone();
    for block in state.current_block.saturating_add(1)..=state.current_block.saturating_add(limit) {
        let next = simulate_run_coinbase(&current, block);
        if next.subnet_epoch_index != current.subnet_epoch_index {
            return Some(block);
        }
        current = next;
    }
    None
}

/// The first block of the commit's reveal epoch (rtao's prediction).
pub fn first_reveal_block(
    state: &EpochScheduleState,
    reveal_period_epochs: u64,
) -> Result<u64, ChainError> {
    epoch_schedule::predict_first_reveal_block(state, reveal_period_epochs)
        .map_err(|e| ChainError::new("reveal_block", e.to_string()))
}

#[cfg(test)]
mod tests;
