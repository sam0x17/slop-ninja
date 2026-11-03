//! The scorer's load guard, evaluated with its own predicates.
//!
//! [`scorer_guard`] repeats `guard` in `inference/a-container-serve/src/engine.rs`
//! expression for expression, on `serde_json::Value` views of the same files, so
//! the comparisons have the scorer's semantics: `Value == Value` distinguishes
//! the integer `1` from the float `1.0`, `Value == usize` needs an unsigned
//! integer, and `Value == f64` accepts any number with that value. The typed
//! role schemas are an additional layer; they never replace this one.
//!
//! [`crate::spec::SCORER_GUARD_SHA256`] pins the SHA-256 of the scorer's
//! `guard` source. When it changes, a spec test fails until this mirror is
//! re-derived and the pin updated.
//!
//! A view keeps only the top-level keys the guard reads; every other value is
//! skipped without being built.

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Map, Value};

use crate::roles::RoleFiles;
use crate::sha256_hex as sha;

/// The scorer's `MAX_TOKENS` (`engine.rs`), with its type.
const MAX_TOKENS: usize = 2048;

struct View<'k>(&'k [&'k str]);

impl<'de> Visitor<'de> for View<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(k) = a.next_key::<String>()? {
            if self.0.contains(&k.as_str()) {
                let v = a.next_value::<Value>()?;
                out.insert(k, v);
            } else {
                a.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Value::Object(out))
    }
}

impl<'de> DeserializeSeed<'de> for View<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_map(self)
    }
}

fn view(file: &str, bytes: &[u8], keys: &[&str]) -> Result<Value, String> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let v = View(keys)
        .deserialize(&mut de)
        .map_err(|e| format!("{file}: {e}"))?;
    de.end().map_err(|e| format!("{file}: {e}"))?;
    Ok(as_the_scorer_reads(v))
}

/// Every number in `v` as the scorer's own build holds it.
///
/// The scorer's `serde_json` has no `arbitrary_precision`: a number is an
/// unsigned integer, a negative integer or an `f64`, and two floats compare
/// equal by value however each file spells them. A build that also links
/// rtao (the miner client, the coordinator) unifies `arbitrary_precision` in,
/// and a number then keeps its source text, so `Value == Value` would compare
/// spellings: `heads.json` writes `1e-05` where `calibration.json` writes
/// `0.00001`, and an artifact the scorer loads would be refused. Rebuilding
/// each number from its value gives every build the scorer's comparisons,
/// including integer `1` against float `1.0` (unequal) and `-0.0` against
/// `0.0` (equal).
pub(crate) fn as_the_scorer_reads(v: Value) -> Value {
    match v {
        Value::Number(n) => {
            let number = if let Some(u) = n.as_u64() {
                serde_json::Number::from(u)
            } else if let Some(i) = n.as_i64() {
                serde_json::Number::from(i)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                // JSON numbers are finite; `from_f64` refuses nothing else.
                serde_json::Number::from_f64(if f == 0.0 { 0.0 } else { f }).unwrap_or(n)
            };
            Value::Number(number)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(as_the_scorer_reads).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, as_the_scorer_reads(v)))
                .collect(),
        ),
        other => other,
    }
}

fn ensure(ok: bool, what: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(what.into()) }
}

/// `engine.rs` `guard`, minus file reading: refuses with the scorer's message.
pub fn scorer_guard(files: &RoleFiles<'_>) -> Result<(), String> {
    let (hb, cb) = (files.heads, files.contract);
    let w = view(
        "heads.json",
        hb,
        &[
            "base_temperature",
            "contract_sha256",
            "involvement_calibration",
            "profiles",
            "thresholds",
            "span_head",
        ],
    )?;
    let c = view(
        "contract.json",
        cb,
        &[
            "schema",
            "input_mode",
            "raw_dimension",
            "maximum_tokens_including_specials",
            "token_truncation",
            "human_profiles",
            "ai_profiles",
        ],
    )?;
    let m = view(
        "manifest.json",
        files.manifest,
        &[
            "schema",
            "complete",
            "calibrated",
            "arm",
            "seed",
            "selected_checkpoint_sha256",
        ],
    )?;
    let cal = view(
        "calibration.json",
        files.calibration,
        &[
            "schema",
            "complete",
            "fit_split",
            "arm",
            "seed",
            "selected_checkpoint_sha256",
            "contract_sha256",
            "involvement",
            "roles",
        ],
    )?;
    let ch = sha(cb);
    ensure(
        m["schema"] == "slop_ninja_full_a_export_v2"
            && m["complete"] == true
            && m["calibrated"] == true,
        "incomplete or uncalibrated export",
    )?;
    ensure(
        c["schema"] == "slop_ninja_full_a_contract_v2"
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["maximum_tokens_including_specials"] == MAX_TOKENS
            && c["token_truncation"] == false,
        "input meaning differs",
    )?;
    let t = w["base_temperature"]
        .as_f64()
        .ok_or_else(|| "base temperature".to_string())?;
    ensure(
        w["contract_sha256"] == ch.as_str() && t.is_finite() && t > 0.,
        "heads/contract differ",
    )?;
    ensure(
        cal["schema"] == "slop_ninja_full_a_calibration_v2"
            && cal["complete"] == true
            && cal["fit_split"] == "cal"
            && cal["arm"] == m["arm"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == ch.as_str()
            && cal["involvement"]["selected"] == w["involvement_calibration"],
        "calibration/checkpoint/involvement map mismatch",
    )?;
    for role in ["human", "ai"] {
        let r = &cal["roles"][role];
        let empty = r["roster_empty"] == true
            && w["profiles"][role].as_array().is_some_and(|a| a.is_empty())
            && w["thresholds"][role] == 1.0;
        ensure(
            empty || (r["qualified"] == true && r["threshold"] == w["thresholds"][role]),
            "unqualified or changed role threshold",
        )?;
        ensure(
            w["profiles"][role] == c[format!("{role}_profiles")],
            "profile roster differs",
        )?;
    }
    ensure(
        w["span_head"].is_null(),
        "span-head arms need token states; not served here",
    )?;
    Ok(())
}

/// The scorer's shared load guard (`load_guard.rs`, compiled into this crate
/// and pinned by `spec::SCORER_LOAD_GUARD_SHA256`) on views of the role files
/// that keep the keys it reads, each number rebuilt as the scorer reads it.
/// Spec v2 applies it to every form the release guard does not take.
pub fn load_guard(files: &RoleFiles<'_>) -> Result<crate::load_guard::Facts, String> {
    use crate::load_guard as g;
    let m = view("manifest.json", files.manifest, g::MANIFEST_KEYS)?;
    let c = view("contract.json", files.contract, g::CONTRACT_KEYS)?;
    let w = view("heads.json", files.heads, g::HEADS_KEYS)?;
    let cal = view("calibration.json", files.calibration, g::CALIBRATION_KEYS)?;
    g::check(&m, &c, &w, &cal, &sha(files.contract)).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> Value {
        as_the_scorer_reads(serde_json::from_str(json).unwrap())
    }

    /// The comparisons hold in every build, with or without `serde_json`'s
    /// `arbitrary_precision`.
    #[test]
    fn numbers_compare_by_value_with_the_scorers_integer_float_split() {
        // Python `repr` and `serde_json` spellings of the same floats.
        assert_eq!(
            read("[1e-05, 1.5e+16, 2.5]"),
            read("[0.00001, 15000000000000000.0, 2.50]")
        );
        assert_eq!(read("{\"t\": -0.0}"), read("{\"t\": 0.0}"));
        assert_ne!(read("1"), read("1.0"));
        assert_ne!(read("0.1"), read("0.10000000000000002"));
        assert_eq!(read("-3"), Value::from(-3));
        assert_eq!(read("18446744073709551615"), Value::from(u64::MAX));
    }
}
