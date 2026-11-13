//! The account store's journal: one content-free row per account change a
//! replay after a D1 restore must find (deletions, merges, unlinks, every
//! signed op).
//!
//! `op` is the canonical JSON (`slop_ninja_signed_op::canonical_json`) of
//! `{"op": <name>, "args": {…}}`, holding ids, modes and counts only: never
//! an address, handle, slug, token or provider subject. An op names an
//! address only as `HMAC(K_journal, email_norm)`.

use serde_json::{Map, Value};

use crate::host::{Stmt, Val};

/// The actor of a reader's own action.
pub const ACTOR_USER: &str = "user";

/// The canonical `op` text.
pub fn op_text(name: &str, args: Map<String, Value>) -> String {
    op_text_result(name, args, None)
}

/// The canonical `op` text with the op's content-free result (counts), kept
/// apart from `args` so a replay re-signs exactly the op's own arguments.
pub fn op_text_result(name: &str, args: Map<String, Value>, result: Option<Map<String, Value>>) -> String {
    let mut obj = Map::new();
    obj.insert("op".into(), Value::String(name.into()));
    obj.insert("args".into(), Value::Object(args));
    if let Some(r) = result {
        obj.insert("result".into(), Value::Object(r));
    }
    slop_ninja_signed_op::canonical_json(&Value::Object(obj)).unwrap_or_else(|| format!("{{\"op\":\"{name}\"}}"))
}

/// One journal row.
pub struct Entry<'a> {
    pub at: u64,
    pub actor: &'a str,
    pub op: String,
    pub user_h: Option<[u8; 32]>,
    pub reason: Option<&'a str>,
    pub signature: Option<[u8; 64]>,
    pub nonce_h: Option<[u8; 32]>,
}

impl Entry<'_> {
    /// The insert, answering the row's `seq`.
    pub fn stmt(&self) -> Stmt {
        Stmt::new(
            "INSERT INTO journal (at, actor, op, user_h, reason, signature, nonce_h) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING seq",
            vec![
                Val::from(self.at),
                Val::from(self.actor),
                Val::from(self.op.clone()),
                Val::from(self.user_h),
                Val::from(self.reason),
                self.signature.map_or(Val::Null, |s| Val::Blob(s.to_vec())),
                Val::from(self.nonce_h),
            ],
        )
    }
}

/// `serde_json::json!`-like object from pairs.
pub fn args(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}
