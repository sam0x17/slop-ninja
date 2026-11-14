//! Dev only: signed operator ops for the local dev session.
//!
//! The session makes a throwaway Ed25519 key pair in its state directory
//! (`dev-session.sh secrets`: the 32-byte seed as 64 hex digits, and its
//! verifying half as the detect and account Workers' `DETECT_OP_KEY`), so the
//! suites can drive the op routes of the local Workers the way the
//! operator's CLI drives a deployed one. Every op is built and signed by the
//! `slop-ninja-signed-op` crate itself ([`slop_ninja_signed_op::sign`]); this
//! crate only reads the seed, fills the envelope and offers the variants a
//! test needs (another environment or target, an issue time in the past, a
//! chosen nonce). Never used outside a local session.

use serde_json::{Map, Value};
use slop_ninja_signed_op::{Env, Nonce, Op, OperatorKey, SigningKey, Target, Wire, sign};

/// How long an op stays valid when the caller names no window: well inside
/// the Workers' `op-policy/v1` limit.
pub const DEFAULT_VALID_MS: u64 = 5 * 60 * 1000;

/// The signing key from a seed file's text (64 hex digits, surrounding
/// whitespace ignored).
pub fn key_from_seed(text: &str) -> Result<SigningKey, String> {
    let t = text.trim();
    if t.len() != 64 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the seed is not 64 hex digits".into());
    }
    let mut seed = [0u8; 32];
    hex::decode_to_slice(t, &mut seed).map_err(|e| e.to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The verifying key's var form (64 lowercase hex) and its fingerprint.
pub fn public(key: &SigningKey) -> (String, String) {
    let k = OperatorKey::from_verifying_key(key.verifying_key());
    (k.hex(), k.fingerprint())
}

/// What one op names.
#[derive(Clone, Debug)]
pub struct Spec {
    pub target: Target,
    pub env: Env,
    pub op: String,
    pub args: Map<String, Value>,
    pub issued_ms: u64,
    pub valid_ms: u64,
    pub nonce: Nonce,
}

/// The parsed `--args` value: a JSON object.
pub fn args(text: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(text).map_err(|e| format!("--args: {e}"))? {
        Value::Object(m) => Ok(m),
        _ => Err("--args is not a JSON object".into()),
    }
}

pub fn target(s: &str) -> Result<Target, String> {
    Target::parse(s).ok_or_else(|| format!("unknown target {s:?}"))
}

pub fn env(s: &str) -> Result<Env, String> {
    Env::parse(s).ok_or_else(|| format!("unknown env {s:?}"))
}

pub fn nonce(s: &str) -> Result<Nonce, String> {
    Nonce::parse(s).ok_or_else(|| "the nonce is not 32 lowercase hex digits".into())
}

/// The signed wire body of `spec`.
pub fn signed(key: &SigningKey, spec: &Spec) -> Result<Wire, String> {
    let op = Op {
        target: spec.target,
        env: spec.env,
        issued_ms: spec.issued_ms,
        expires_ms: spec.issued_ms.saturating_add(spec.valid_ms),
        nonce: spec.nonce,
        op: spec.op.clone(),
        args: spec.args.clone(),
    };
    sign(&op, key).map_err(|r| format!("refused: {}", r.code()))
}
