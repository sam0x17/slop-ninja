//! Redaction: what may leave the private side.
//!
//! The publisher never passes a private document through. It publishes:
//!
//! - signed and unsigned protocol records it decoded completely and
//!   canonically ([`crate::verify`]): digests, identities, scores, gates and
//!   outcome classes, nothing else;
//! - documents it builds itself from typed fields (the status snapshot, the
//!   cohort index, the leaderboard, the publication records), never from a
//!   copy of a status response;
//! - one copied diagnostic, `components.json`, only when it parses as its
//!   typed schema (unknown fields refused at every level), agrees with the
//!   signed manifest and package, and [`scan_value`] passes;
//! - admitted outside entrants' artifact objects after their cohort closed
//!   (miner submissions published by design), default-deny, never a private
//!   reference.
//!
//! [`scan_value`] refuses any JSON object key that names document text,
//! labels, per-document predictions, generation, spend or credentials, and
//! any string long enough to carry a document.

use serde_json::Value;

/// Key fragments that never appear in a public JSON document (matched against
/// the lowercased key).
pub const FORBIDDEN_KEY_FRAGMENTS: &[&str] = &[
    // Document content and its private metadata.
    "text",
    "prompt",
    "sentence",
    "span",
    "source_group",
    "generator",
    "words",
    "passage",
    "excerpt",
    // Labels and per-document predictions.
    "prediction",
    "involvement",
    "probabilit",
    "vector",
    "label_",
    "truth",
    // Spend.
    "usd",
    "cost",
    "spend",
    "spent",
    "budget",
    "ledger",
    "price",
    "billing",
    // Credentials.
    "token",
    "secret",
    "seed",
    "password",
    "credential",
];

/// Whole keys that never appear in a public JSON document.
pub const FORBIDDEN_KEYS: &[&str] = &[
    "labels",
    "y",
    "kind",
    "kinds",
    "document",
    "documents",
    "doc",
    "docs",
    "preds",
    "share_ppb",
    "cap",
    "results",
];

/// Keys whose value is a version identifier, not content: allowed despite a
/// forbidden fragment when the value is a short `[a-z0-9_.-]` identifier.
/// `rewrite_prompts` names the coordinator's rewrite prompt set
/// (`rewrite_voice_operation_v1`), never a prompt.
pub const IDENTIFIER_KEYS: &[&str] = &["rewrite_prompts"];

fn identifier(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
            })
    })
}

/// No public string is document-sized. The longest public strings are rule
/// descriptions written by the coordinator (under 1 KiB).
pub const MAX_PUBLIC_STRING: usize = 2048;

fn key_refused(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    FORBIDDEN_KEYS.contains(&k.as_str()) || FORBIDDEN_KEY_FRAGMENTS.iter().any(|f| k.contains(f))
}

/// Refuses a JSON value that could carry private data; the error names the
/// offending key path (never the value).
pub fn scan_value(value: &Value) -> Result<(), String> {
    fn walk(v: &Value, path: &mut Vec<String>) -> Result<(), String> {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    if IDENTIFIER_KEYS.contains(&k.as_str()) && identifier(child) {
                        continue;
                    }
                    if key_refused(k) {
                        path.push(k.clone());
                        return Err(format!("forbidden key {}", path.join(".")));
                    }
                    path.push(k.clone());
                    walk(child, path)?;
                    path.pop();
                }
                Ok(())
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    path.push(i.to_string());
                    walk(child, path)?;
                    path.pop();
                }
                Ok(())
            }
            Value::String(s) if s.len() > MAX_PUBLIC_STRING => {
                Err(format!("string of {} bytes at {}", s.len(), path.join(".")))
            }
            _ => Ok(()),
        }
    }
    walk(value, &mut Vec::new())
}

/// [`scan_value`] over serialized JSON.
pub fn scan_json(bytes: &[u8]) -> Result<(), String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "not JSON".to_owned())?;
    scan_value(&value)
}

/// Phrases public text never carries (matched against the lowercased text):
/// retired wording. The publisher's sources are checked
/// against them in its tests, and an object written once for good (a record
/// archive) is checked before it is written ([`scan_wording`]).
pub const REFUSED_WORDING: &[&str] = &[
    "rights question",
    "counsel",
    "stays private",
    "stay private until",
    "weights not published",
    "private reference",
];

/// Refuses bytes that carry a [`REFUSED_WORDING`] phrase; the error names the
/// phrase (never the text around it).
pub fn scan_wording(bytes: &[u8]) -> Result<(), String> {
    let lower = String::from_utf8_lossy(bytes).to_lowercase();
    match REFUSED_WORDING.iter().find(|p| lower.contains(*p)) {
        Some(p) => Err(format!("refused wording {p:?}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refuses_private_fields() {
        for bad in [
            json!({"documents": []}),
            json!({"x": {"text": "a"}}),
            json!({"entrants": [{"predictions": [0.1]}]}),
            json!({"generation_cost_microusd": 1}),
            json!({"ledger": {}}),
            json!({"labels": [1, 0]}),
            json!({"kind": "mixed"}),
            json!({"ai_byte_span": [1, 2]}),
            json!({"status_token": "x"}),
            json!({"note": "x".repeat(MAX_PUBLIC_STRING + 1)}),
            json!({"rewrite_prompts": "You are a careful copy editor."}),
            json!({"rewrite_prompts": ["rewrite_voice_operation_v1"]}),
        ] {
            assert!(scan_value(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refuses_retired_wording() {
        let held = format!("{{\"withheld\": \"x {} y\"}}", REFUSED_WORDING[0]);
        assert!(scan_wording(held.as_bytes()).is_err());
        assert!(scan_wording(br#"{"withheld": "reference package not yet staged"}"#).is_ok());
    }

    #[test]
    fn accepts_public_components() {
        let ok = json!({
            "schema": "slop_ninja_eval_score_components_v2", "label": "c1", "cohort": 3,
            "mode": "Margin", "reference_score": "1", "gate_policy": "x", "policy": {"share": {"max": 1}},
            "entrants": [{"hotkey": "00", "uid": 1, "score": "1", "gates": [], "cluster_head": null, "units": 7}],
            "crash_rule": "crash isolation: a rule text",
            "reuse": {"max_uses": 4, "rewrite_prompts": "rewrite_voice_operation_v1"},
        });
        scan_value(&ok).unwrap();
    }
}
