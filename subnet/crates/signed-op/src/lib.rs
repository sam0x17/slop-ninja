//! Signed operator ops for the detect and account Workers.
//!
//! The operator's private CLI signs each op with an Ed25519 **operator key**
//! (one key per environment: staging and production never share one) and
//! posts it to `/api/detect/v1/op` or `/api/account/v1/op`. The Worker checks
//! it here, then applies the op and its journal row in one D1 batch. This key
//! is not the offline operator-decision key of the protocol crate.
//!
//! # What is signed
//!
//! An [`Op`]: the `target` Worker ([`Target`]), the environment ([`Env`]),
//! `issued_ms` and `expires_ms` (Unix milliseconds), a 128-bit random
//! `nonce`, the op name and its arguments (a JSON object). The signature
//! covers [`SIGNED_PREFIX`] followed by the canonical JSON ([`canonical_json`])
//! of the object
//!
//! ```text
//! {"args":{…},"env":"staging","expires_ms":…,"issued_ms":…,"nonce":"<32 hex>","op":"<name>","target":"detect","v":1}
//! ```
//!
//! with keys in byte order at every level, no whitespace, integers only (no
//! fractions or exponents) and serde_json's string escaping. The prefix keeps
//! these bytes from ever reading as any other SlopNinja signed message.
//!
//! # The wire form
//!
//! The request body is one JSON object ([`Wire`]): the signed fields, plus
//! `key` (the signer's [`OperatorKey::fingerprint`]) and `sig` (the 64-byte
//! signature in lowercase hex). Unknown fields are refused. The account
//! Worker's op route takes this object as its body; the detect Worker's takes
//! it as the `signed` field of a wrapper that may also carry `x_url`, the
//! one value sent beside a signature (detect-page's `OpRequest`), and checks
//! it with [`Verifier::verify_wire`].
//!
//! # What a Worker checks ([`Verifier::verify`])
//!
//! In order, each failure with its own [`Refusal`]:
//! 1. the body parses as [`Wire`] and every field has its exact form
//!    ([`Refusal::Malformed`]);
//! 2. `key` names this Worker's configured key and `sig` verifies under strict
//!    Ed25519 rules over the canonical bytes ([`Refusal::BadSignature`]);
//! 3. the target is one this Worker serves and the op belongs to that target
//!    in [`CATALOG`] ([`Refusal::WrongTarget`], [`Refusal::UnknownOp`]);
//! 4. the environment is the Worker's own ([`Refusal::WrongEnv`]);
//! 5. the validity window: `issued_ms < expires_ms`, at most
//!    [`OpPolicy::max_validity_ms`] long, issued no later than
//!    [`OpPolicy::clock_skew_ms`] past now ([`Refusal::NotYetValid`]), and not
//!    yet expired ([`Refusal::Expired`]);
//! 6. the nonce has never been used ([`Refusal::ReusedNonce`]). A Worker
//!    enforces this with the `nonce_h UNIQUE` column of its journal, written
//!    in the same D1 batch as the op, so a reused nonce fails the whole batch.
//!    [`Verified::nonce_h`] is the value it writes; [`NonceLedger`] is the
//!    same rule for callers that keep nonces elsewhere (tests, the CLI's own
//!    replay log).
//!
//! The expiry bounds how long a captured op stays usable if journal rows are
//! ever lost, for example after a D1 point-in-time restore.

use std::collections::{BTreeMap, HashSet};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

/// The envelope version (`v`).
pub const VERSION: u8 = 1;
/// The bytes every signed message starts with.
pub const SIGNED_PREFIX: &[u8] = b"slop_ninja_signed_op_v1\n";
/// Domain of [`Verified::nonce_h`].
pub const NONCE_DOMAIN: &[u8] = b"slop_ninja_signed_op_nonce_v1\0";
/// Domain of [`OperatorKey::fingerprint`].
pub const KEY_DOMAIN: &[u8] = b"slop_ninja_signed_op_key_v1\0";
/// Deepest JSON nesting accepted in `args`. Ops carry flat objects and short
/// lists; refusing anything deeper bounds the canonical encoder's recursion
/// on a Worker's stack.
pub const MAX_ARGS_DEPTH: usize = 8;
/// Longest op name, the longest name in [`CATALOG`] rounded up.
pub const MAX_OP_NAME: usize = 32;

/// The Worker an op is addressed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// The detect Worker (`/api/detect/v1/op`).
    Detect,
    /// The account Worker (`/api/account/v1/op`).
    Account,
    /// The edge-rules tooling's coverage reports, posted to the detect
    /// Worker, which accepts this target for `edge_coverage` only.
    Edge,
}

impl Target {
    pub const ALL: [Target; 3] = [Target::Detect, Target::Account, Target::Edge];

    pub fn as_str(self) -> &'static str {
        match self {
            Target::Detect => "detect",
            Target::Account => "account",
            Target::Edge => "edge",
        }
    }

    pub fn parse(s: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// The deployment an op is addressed to. Each has its own operator key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Env {
    Production,
    Staging,
}

impl Env {
    pub const ALL: [Env; 2] = [Env::Production, Env::Staging];

    pub fn as_str(self) -> &'static str {
        match self {
            Env::Production => "production",
            Env::Staging => "staging",
        }
    }

    pub fn parse(s: &str) -> Option<Env> {
        Env::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

/// Every op and the one target that serves it. `set_switch` exists on both Workers, each for its own
/// switches. A name outside this table is refused before anything runs.
pub const CATALOG: &[(Target, &str)] = &[
    // detect: cases and takedowns
    (Target::Detect, "withhold_excerpt"),
    (Target::Detect, "remove"),
    // `restore {case_id}`. On an X page it is a fresh check of the post, and
    // the op still names no post: the CLI sends the post's link beside the
    // envelope as `x_url` (detect-page's `OpRequest`), outside the signed
    // bytes, and the Worker never journals it.
    (Target::Detect, "restore"),
    (Target::Detect, "reject"),
    (Target::Detect, "close_case"),
    (Target::Detect, "case_note"),
    (Target::Detect, "case_show"),
    (Target::Detect, "review_strike"),
    (Target::Detect, "owner_match"),
    // detect: sources, pages and the store
    (Target::Detect, "exclude_host"),
    (Target::Detect, "rerun"),
    (Target::Detect, "release_claim"),
    (Target::Detect, "purge"),
    (Target::Detect, "delete_private"),
    (Target::Detect, "index_force"),
    // `x_purge {reason, dry_run}`: deletes every X run, run document and
    // source row as a class, in `x_purge_page` messages, and leaves each X
    // detection a content-free tombstone (`x_purged`); run at once when X ends
    // or gives notice to end the agreement. `dry_run: true` answers counts
    // only and writes nothing but its journal row.
    (Target::Detect, "x_purge"),
    // `x_oembed_trust {reason}`: ends oEmbed distrust (held after a lookup
    // contradicted an oEmbed "gone" answer), so the second-candidate fallback
    // may delete again.
    (Target::Detect, "x_oembed_trust"),
    (Target::Detect, "set_switch"),
    (Target::Detect, "set_examples"),
    (Target::Detect, "status"),
    // edge: the hourly coverage probe
    (Target::Edge, "edge_coverage"),
    // account
    (Target::Account, "disable_account"),
    (Target::Account, "restore_account"),
    (Target::Account, "revoke_sessions"),
    (Target::Account, "delete_account"),
    (Target::Account, "attach_email"),
    (Target::Account, "merge_accounts"),
    (Target::Account, "undo_merge"),
    (Target::Account, "set_flag"),
    (Target::Account, "grant_credit"),
    (Target::Account, "grant_plan"),
    (Target::Account, "void_grant"),
    (Target::Account, "set_switch"),
    (Target::Account, "reset_quota"),
    (Target::Account, "unsuppress"),
];

/// Whether `op` is an op of `target`.
pub fn in_catalog(target: Target, op: &str) -> bool {
    CATALOG.iter().any(|(t, o)| *t == target && *o == op)
}

/// `op-policy/v1`: the validity window. Each value carries its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpPolicy {
    /// The longest `expires_ms - issued_ms`. The CLI signs immediately before
    /// it posts; a quarter hour covers a slow manual retry and keeps a
    /// captured op short-lived if journal rows are lost (a D1 restore).
    pub max_validity_ms: u64,
    /// How far `issued_ms` may lie in the Worker's future. The operator host
    /// and Cloudflare keep NTP time; two minutes is far above their drift.
    pub clock_skew_ms: u64,
}

/// The policy's version name.
pub const OP_POLICY_NAME: &str = "op-policy/v1";
/// `op-policy/v1`.
pub const OP_POLICY_V1: OpPolicy = OpPolicy { max_validity_ms: 15 * 60 * 1000, clock_skew_ms: 2 * 60 * 1000 };

/// Why an op was refused. Each code is also a `detect-page` refusal code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The body or a field does not have its exact form.
    Malformed,
    /// The key is not this Worker's, or the signature does not verify.
    BadSignature,
    /// Signed for another Worker.
    WrongTarget,
    /// Signed for another environment.
    WrongEnv,
    /// Past `expires_ms`.
    Expired,
    /// Issued in the future beyond the allowed skew.
    NotYetValid,
    /// The nonce was used before.
    ReusedNonce,
    /// Not an op of its target.
    UnknownOp,
}

impl Refusal {
    pub const ALL: [Refusal; 8] = [
        Refusal::Malformed,
        Refusal::BadSignature,
        Refusal::WrongTarget,
        Refusal::WrongEnv,
        Refusal::Expired,
        Refusal::NotYetValid,
        Refusal::ReusedNonce,
        Refusal::UnknownOp,
    ];

    /// The stable code in the refusal body `{"state":"refused","code":…}`.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Malformed => "op_malformed",
            Refusal::BadSignature => "op_bad_signature",
            Refusal::WrongTarget => "op_wrong_target",
            Refusal::WrongEnv => "op_wrong_env",
            Refusal::Expired => "op_expired",
            Refusal::NotYetValid => "op_not_yet_valid",
            Refusal::ReusedNonce => "op_reused_nonce",
            Refusal::UnknownOp => "op_unknown",
        }
    }

    /// The HTTP status of the refusal.
    pub fn status(self) -> u16 {
        match self {
            Refusal::Malformed | Refusal::UnknownOp => 400,
            Refusal::ReusedNonce => 409,
            Refusal::BadSignature
            | Refusal::WrongTarget
            | Refusal::WrongEnv
            | Refusal::Expired
            | Refusal::NotYetValid => 403,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Refusal {}

/// A 128-bit op nonce, lowercase hex on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nonce(pub [u8; 16]);

impl Nonce {
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn parse(s: &str) -> Option<Nonce> {
        parse_lower_hex::<16>(s).map(Nonce)
    }
}

/// An unsigned op.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub target: Target,
    pub env: Env,
    pub issued_ms: u64,
    pub expires_ms: u64,
    pub nonce: Nonce,
    /// The op name, an entry of [`CATALOG`] for `target`.
    pub op: String,
    /// The op's arguments.
    pub args: Map<String, Value>,
}

impl Op {
    /// The bytes the signature covers: [`SIGNED_PREFIX`] and the canonical
    /// JSON of the signed fields. Refused when the arguments hold a
    /// non-integer number or nest past [`MAX_ARGS_DEPTH`].
    pub fn signed_bytes(&self) -> Result<Vec<u8>, Refusal> {
        let mut obj = Map::new();
        obj.insert("args".into(), Value::Object(self.args.clone()));
        obj.insert("env".into(), Value::String(self.env.as_str().into()));
        obj.insert("expires_ms".into(), Value::from(self.expires_ms));
        obj.insert("issued_ms".into(), Value::from(self.issued_ms));
        obj.insert("nonce".into(), Value::String(self.nonce.hex()));
        obj.insert("op".into(), Value::String(self.op.clone()));
        obj.insert("target".into(), Value::String(self.target.as_str().into()));
        obj.insert("v".into(), Value::from(VERSION));
        let mut out = SIGNED_PREFIX.to_vec();
        out.extend_from_slice(canonical_json(&Value::Object(obj)).ok_or(Refusal::Malformed)?.as_bytes());
        Ok(out)
    }

    /// `SHA-256(NONCE_DOMAIN ‖ target ‖ 0 ‖ env ‖ 0 ‖ nonce)`: the journal's
    /// `nonce_h`.
    pub fn nonce_h(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(NONCE_DOMAIN);
        h.update(self.target.as_str().as_bytes());
        h.update([0]);
        h.update(self.env.as_str().as_bytes());
        h.update([0]);
        h.update(self.nonce.0);
        h.finalize().into()
    }
}

/// The request body of an op route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wire {
    pub v: u8,
    pub target: String,
    pub env: String,
    pub issued_ms: u64,
    pub expires_ms: u64,
    pub nonce: String,
    pub op: String,
    pub args: Value,
    /// [`OperatorKey::fingerprint`] of the signing key.
    pub key: String,
    /// The signature, 128 lowercase hex digits.
    pub sig: String,
}

/// An operator public key (a Worker's `DETECT_OP_KEY` var).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorKey(VerifyingKey);

impl OperatorKey {
    /// The key as 64 lowercase hex digits (the var's form).
    pub fn parse(s: &str) -> Option<OperatorKey> {
        let bytes = parse_lower_hex::<32>(s.trim())?;
        VerifyingKey::from_bytes(&bytes).ok().map(OperatorKey)
    }

    pub fn from_verifying_key(k: VerifyingKey) -> OperatorKey {
        OperatorKey(k)
    }

    pub fn bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// The first 8 bytes of `SHA-256(KEY_DOMAIN ‖ key)`, as 16 lowercase hex
    /// digits: the wire's `key` and the journal actor `operator:<fpr>`.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(KEY_DOMAIN);
        h.update(self.0.as_bytes());
        hex::encode(&h.finalize()[..8])
    }
}

/// A verified op.
#[derive(Clone, Debug, PartialEq)]
pub struct Verified {
    pub op: Op,
    /// The signer's fingerprint.
    pub key: String,
    pub sig: [u8; 64],
}

impl Verified {
    /// The journal's `nonce_h` ([`Op::nonce_h`]).
    pub fn nonce_h(&self) -> [u8; 32] {
        self.op.nonce_h()
    }

    /// The journal actor, `operator:<fingerprint>`.
    pub fn actor(&self) -> String {
        format!("operator:{}", self.key)
    }
}

/// Where used nonces are recorded. A Worker's journal (`nonce_h UNIQUE`) is
/// one; [`MemoryLedger`] is another.
pub trait NonceLedger {
    /// Records `nonce_h`; false when it was recorded before.
    fn claim(&mut self, nonce_h: &[u8; 32]) -> bool;
}

/// An in-memory [`NonceLedger`].
#[derive(Debug, Default)]
pub struct MemoryLedger(HashSet<[u8; 32]>);

impl NonceLedger for MemoryLedger {
    fn claim(&mut self, nonce_h: &[u8; 32]) -> bool {
        self.0.insert(*nonce_h)
    }
}

/// One Worker's verifier: its key, its environment and the targets it serves.
#[derive(Clone, Debug)]
pub struct Verifier {
    key: OperatorKey,
    env: Env,
    accepts: Vec<Target>,
    policy: OpPolicy,
}

impl Verifier {
    /// The detect Worker serves [`Target::Detect`] and [`Target::Edge`]; the
    /// account Worker serves [`Target::Account`].
    pub fn new(key: OperatorKey, env: Env, accepts: &[Target]) -> Verifier {
        Verifier { key, env, accepts: accepts.to_vec(), policy: OP_POLICY_V1 }
    }

    pub fn with_policy(mut self, policy: OpPolicy) -> Verifier {
        self.policy = policy;
        self
    }

    /// Checks steps 1 to 5 of the crate documentation. The caller bounds the
    /// body's length before reading it, and then enforces the nonce rule.
    pub fn verify(&self, body: &[u8], now_ms: u64) -> Result<Verified, Refusal> {
        let w: Wire = serde_json::from_slice(body).map_err(|_| Refusal::Malformed)?;
        self.verify_wire(&w, now_ms)
    }

    /// Checks an envelope already parsed as [`Wire`] (by a route whose body
    /// wraps it): the field forms of step 1, then steps 2 to 5.
    pub fn verify_wire(&self, w: &Wire, now_ms: u64) -> Result<Verified, Refusal> {
        let (op, sig) = decode(w)?;
        if w.key != self.key.fingerprint() {
            return Err(Refusal::BadSignature);
        }
        let bytes = op.signed_bytes()?;
        self.key.0.verify_strict(&bytes, &Signature::from_bytes(&sig)).map_err(|_| Refusal::BadSignature)?;
        if !self.accepts.contains(&op.target) {
            return Err(Refusal::WrongTarget);
        }
        if !in_catalog(op.target, &op.op) {
            return Err(Refusal::UnknownOp);
        }
        if op.env != self.env {
            return Err(Refusal::WrongEnv);
        }
        if op.expires_ms <= op.issued_ms || op.expires_ms - op.issued_ms > self.policy.max_validity_ms {
            return Err(Refusal::Malformed);
        }
        if op.issued_ms > now_ms.saturating_add(self.policy.clock_skew_ms) {
            return Err(Refusal::NotYetValid);
        }
        if now_ms >= op.expires_ms {
            return Err(Refusal::Expired);
        }
        Ok(Verified { op, key: w.key.clone(), sig })
    }

    /// [`Verifier::verify`], then the nonce rule against `ledger`.
    pub fn verify_and_claim(
        &self,
        body: &[u8],
        now_ms: u64,
        ledger: &mut impl NonceLedger,
    ) -> Result<Verified, Refusal> {
        let v = self.verify(body, now_ms)?;
        if !ledger.claim(&v.nonce_h()) {
            return Err(Refusal::ReusedNonce);
        }
        Ok(v)
    }
}

/// The exact forms of the wire's fields.
fn decode(w: &Wire) -> Result<(Op, [u8; 64]), Refusal> {
    if w.v != VERSION {
        return Err(Refusal::Malformed);
    }
    let target = Target::parse(&w.target).ok_or(Refusal::Malformed)?;
    let env = Env::parse(&w.env).ok_or(Refusal::Malformed)?;
    let nonce = Nonce::parse(&w.nonce).ok_or(Refusal::Malformed)?;
    if w.op.is_empty() || w.op.len() > MAX_OP_NAME || !w.op.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return Err(Refusal::Malformed);
    }
    let Value::Object(args) = &w.args else { return Err(Refusal::Malformed) };
    if w.key.len() != 16 || !w.key.bytes().all(is_lower_hex) {
        return Err(Refusal::Malformed);
    }
    let sig = parse_lower_hex::<64>(&w.sig).ok_or(Refusal::Malformed)?;
    let op = Op {
        target,
        env,
        issued_ms: w.issued_ms,
        expires_ms: w.expires_ms,
        nonce,
        op: w.op.clone(),
        args: args.clone(),
    };
    Ok((op, sig))
}

/// The canonical JSON of `v`: object keys in byte order at every level, no
/// whitespace, serde_json's string escaping, and integers only. `None` for a
/// non-integer number or nesting past [`MAX_ARGS_DEPTH`] below the top
/// object. The encoder sorts keys itself, so the result does not depend on
/// serde_json's map feature.
pub fn canonical_json(v: &Value) -> Option<String> {
    let mut out = String::new();
    write_canonical(v, 0, &mut out)?;
    Some(out)
}

fn write_canonical(v: &Value, depth: usize, out: &mut String) -> Option<()> {
    if depth > MAX_ARGS_DEPTH + 1 {
        return None;
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_u64() {
                out.push_str(&i.to_string());
            } else {
                out.push_str(&n.as_i64()?.to_string());
            }
        }
        Value::String(s) => out.push_str(&serde_json::to_string(s).ok()?),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, depth + 1, out)?;
            }
            out.push(']');
        }
        Value::Object(m) => {
            let sorted: BTreeMap<&[u8], (&String, &Value)> = m.iter().map(|(k, x)| (k.as_bytes(), (k, x))).collect();
            out.push('{');
            for (i, (_, (k, x))) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).ok()?);
                out.push(':');
                write_canonical(x, depth + 1, out)?;
            }
            out.push('}');
        }
    }
    Some(())
}

fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

fn parse_lower_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || !s.bytes().all(is_lower_hex) {
        return None;
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).ok()?;
    Some(out)
}

/// The signing key type [`sign`] takes (the operator CLI and tests only).
#[cfg(feature = "sign")]
pub use ed25519_dalek::SigningKey;

/// Signs `op` and returns its wire body. The operator CLI and tests only.
#[cfg(feature = "sign")]
pub fn sign(op: &Op, key: &ed25519_dalek::SigningKey) -> Result<Wire, Refusal> {
    use ed25519_dalek::Signer as _;
    let bytes = op.signed_bytes()?;
    let sig = key.sign(&bytes);
    Ok(Wire {
        v: VERSION,
        target: op.target.as_str().into(),
        env: op.env.as_str().into(),
        issued_ms: op.issued_ms,
        expires_ms: op.expires_ms,
        nonce: op.nonce.hex(),
        op: op.op.clone(),
        args: Value::Object(op.args.clone()),
        key: OperatorKey(key.verifying_key()).fingerprint(),
        sig: hex::encode(sig.to_bytes()),
    })
}
