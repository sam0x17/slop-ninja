//! What the scoring lanes' client (`cf_lanes.rs`, wasm32 only) reads from a
//! scorer Container's replies, and what it sends. Pure, so native tests cover
//! both scorer builds a deployment can run.
//!
//! - **Bindings.** A single-use scorer reports `"single_use": true` and the
//!   binding (package and cohort) in `/healthz`. [`slot`] compares that
//!   binding with the lane's artifact and cohort; a scorer that does not
//!   report single use is compared by package only, and the client names no
//!   cohort to it ([`sent`]): such a scorer refuses an unknown `cohort` field
//!   on score and reads `&cohort=` on load as part of the package digest.
//! - **Refusals.** The scorer's admission refusals read `refused <code>:
//!   <detail>`, on a 422 load reply (the manifest check) or in a failed
//!   slot's `error` (the check of the assembled files). They are
//!   deterministic, so they are definitive, except the codes the check of the
//!   assembled files also reports for a host fault ([`HOST_FAULT_CODES`]);
//!   those, and every other load failure, stay attributable only on a fresh
//!   instance.
//! - **Platform replies.** A part or load meeting another binding (409
//!   `instance_bound`), parts missing at load (409 `missing_parts`) and too
//!   little memory (507 `insufficient_memory`) are the platform's.

use crate::eval::lane_logic::RestartFailure;
use serde_json::{Value, json};

/// Why a prepare (start, stream, load) failed: the platform's, or the
/// artifact's on a freshly started instance (`definitive` for a deterministic
/// check: the size bound, the manifest digest or form, or the scorer's
/// admission refusal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prep {
    Infra(&'static str),
    Artifact(&'static str, bool),
}

impl Prep {
    pub fn code(self) -> &'static str {
        match self {
            Prep::Infra(c) | Prep::Artifact(c, _) => c,
        }
    }

    pub fn restart(self) -> RestartFailure {
        match self {
            Prep::Infra(c) => RestartFailure::Infra(c),
            Prep::Artifact(code, definitive) => RestartFailure::Artifact { code, definitive },
        }
    }
}

/// A part or load met an instance bound to another package or cohort.
pub const INSTANCE_BOUND: &str = "instance bound to another artifact";
/// A non-definitive artifact failure on an instance this prepare did not start.
pub const REUSED: &str = "artifact load failed on a reused instance";
/// The code recorded for a refusal whose code is not in [`REFUSALS`].
pub const REFUSED: &str = "refused";

macro_rules! refusal_codes {
    ($($code:literal),* $(,)?) => {
        /// Every admission refusal code a scorer reports, with the code the
        /// lane records: the artifact spec checker's (`Refusal::code` in
        /// `slop_ninja_artifact_check`) and the scorer's own `profile_id`.
        pub const REFUSALS: &[(&str, &str)] = &[$(($code, concat!("refused ", $code))),*];
    };
}

refusal_codes!(
    "object",
    "contract_id",
    "undeclared_file",
    "missing_file",
    "file_role",
    "tokenizer_mismatch",
    "file_size",
    "json_budget",
    "json_value_budget",
    "json_layout",
    "schema",
    "binding",
    "graph_malformed",
    "graph_unknown_field",
    "graph_custom_domain",
    "graph_external_data",
    "graph_unsupported",
    "graph_op_not_allowed",
    "graph_dtype_not_allowed",
    "graph_size",
    "graph_class_not_served",
    "too_many_parameters",
    "graph_not_allowed",
    "graph_class",
    "profile_id",
);

/// Refusals the scorer's check of its assembled files also reports for a host
/// fault: the checker files a failed read of those files, or a failed
/// allocation while buffering one, under `object`. In a failed slot they count
/// only on a fresh instance, like any other load failure; in a 422 load reply
/// (the manifest check, which reads no file) they are the manifest's and
/// definitive.
pub const HOST_FAULT_CODES: &[&str] = &["refused object"];

/// Whether the scorer binds each instance to one package and cohort.
pub fn names_cohort(h: &Value) -> bool {
    h["single_use"] == Value::Bool(true)
}

/// The cohort the client names to an instance whose `/healthz` is `h`: the
/// lane's, when the scorer is single use.
pub fn sent<'a>(h: &Value, cohort: Option<&'a str>) -> Option<&'a str> {
    cohort.filter(|_| names_cohort(h))
}

/// Where an instance stands for this lane's artifact and cohort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Nothing loaded: the instance may bind to this artifact.
    Empty,
    /// Bound to this package and cohort.
    Ours(Ours),
    /// Bound to another package or cohort, or in a state this client does not
    /// know.
    Elsewhere,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ours {
    Loading,
    Ready,
    Failed,
}

/// The slot `h` (a `/healthz` reply) shows for `package` and the lane's
/// `cohort`. The cohort is compared only on a single-use scorer, where an
/// absent cohort is a binding too (`null` in `/healthz`).
pub fn slot(h: &Value, package: &str, cohort: Option<&str>) -> Slot {
    let state = h["state"].as_str();
    if state == Some("empty") {
        return Slot::Empty;
    }
    let ours = h["package"].as_str() == Some(package) && h["cohort"].as_str() == sent(h, cohort);
    match (state, ours) {
        (Some("loading"), true) => Slot::Ours(Ours::Loading),
        (Some("ready"), true) => Slot::Ours(Ours::Ready),
        (Some("failed"), true) => Slot::Ours(Ours::Failed),
        _ => Slot::Elsewhere,
    }
}

/// What a prepare does with an instance that is already running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Use it: empty, this binding loading or ready, or not answering yet
    /// (the start wait decides).
    Keep,
    /// Destroy it and start a fresh one, counting nothing: it is bound
    /// elsewhere, or this binding failed without a definitive refusal (so the
    /// failure is attributed on a fresh instance).
    Fresh,
    /// This binding was refused: definitive (the instance is destroyed).
    Refused(&'static str),
}

pub fn entry(h: Option<&Value>, package: &str, cohort: Option<&str>) -> Entry {
    let Some(h) = h else { return Entry::Keep };
    match slot(h, package, cohort) {
        Slot::Empty | Slot::Ours(Ours::Loading | Ours::Ready) => Entry::Keep,
        Slot::Ours(Ours::Failed) => match load_failed(h["error"].as_str().unwrap_or("")) {
            Prep::Artifact(code, true) => Entry::Refused(code),
            _ => Entry::Fresh,
        },
        Slot::Elsewhere => Entry::Fresh,
    }
}

/// The code of an admission refusal (`refused <code>: <detail>`), as the lane
/// records it; [`REFUSED`] for a code not in [`REFUSALS`]. `None` for any
/// other error.
pub fn refusal(error: &str) -> Option<&'static str> {
    let rest = error.strip_prefix("refused ")?;
    let code = rest.split(|c: char| c == ':' || c.is_whitespace()).next().unwrap_or("");
    Some(REFUSALS.iter().find(|(c, _)| *c == code).map_or(REFUSED, |(_, label)| *label))
}

/// A failed slot's `error`: a refusal is definitive unless its code also
/// reports a host fault ([`HOST_FAULT_CODES`]); that and anything else (a
/// guard, ONNX Runtime, the warm-up document) is attributable on a fresh
/// instance.
pub fn load_failed(error: &str) -> Prep {
    match refusal(error) {
        Some(code) => Prep::Artifact(code, !HOST_FAULT_CODES.contains(&code)),
        None => Prep::Artifact("artifact load failed", false),
    }
}

/// A `PUT /v1/blobs/<sha256>` reply other than 201.
pub fn part_refused(status: u16) -> Prep {
    match status {
        409 => Prep::Infra(INSTANCE_BOUND),
        // The container refused the bytes stored under this digest.
        _ => Prep::Artifact("part rejected", false),
    }
}

/// A `POST /v1/load` reply and its JSON body (`Null` when unreadable): `None`
/// when the load started or this binding is already loading or loaded.
pub fn load_reply(status: u16, body: &Value) -> Option<Prep> {
    let error = body["error"].as_str().unwrap_or("");
    match status {
        200 | 202 => None,
        // The scorer refused the manifest itself.
        422 => Some(match refusal(error) {
            Some(code) => Prep::Artifact(code, true),
            None => Prep::Artifact("load refused", false),
        }),
        409 if error == "instance_bound" => Some(Prep::Infra(INSTANCE_BOUND)),
        409 if error == "missing_parts" => Some(Prep::Infra("missing parts")),
        507 => Some(Prep::Infra("insufficient_memory")),
        _ => Some(Prep::Infra("load refused")),
    }
}

/// The load route, naming the cohort when the client sends one.
pub fn load_path(package: &str, cohort: Option<&str>) -> String {
    match cohort {
        Some(c) => format!("/v1/load?package={package}&cohort={c}"),
        None => format!("/v1/load?package={package}"),
    }
}

/// A score request body; `cohort` is omitted, not `null`, when none is sent.
pub fn score_body(package: &str, cohort: Option<&str>, docs: &[(u32, String, String)]) -> String {
    let docs: Vec<Value> = docs.iter().map(|(t, text, _)| json!({"id": t.to_string(), "text": text})).collect();
    let mut v = json!({"package": package, "docs": docs});
    if let Some(c) = cohort {
        v["cohort"] = json!(c);
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const P: &str = "aa";

    fn new_scorer(state: &str, package: &str, cohort: Option<&str>) -> Value {
        json!({"state": state, "package": package, "cohort": cohort, "single_use": true, "build": {}})
    }

    /// The scorer `9efb681a` reports neither `cohort` nor `single_use`.
    fn old_scorer(state: &str, package: &str) -> Value {
        json!({"state": state, "package": package, "build": {}})
    }

    #[test]
    fn a_single_use_scorer_is_compared_by_package_and_cohort() {
        let c = Some("testnet-rv1-a");
        assert_eq!(slot(&json!({"state": "empty", "single_use": true}), P, c), Slot::Empty);
        assert_eq!(slot(&new_scorer("loading", P, c), P, c), Slot::Ours(Ours::Loading));
        assert_eq!(slot(&new_scorer("ready", P, c), P, c), Slot::Ours(Ours::Ready));
        assert_eq!(slot(&new_scorer("failed", P, c), P, c), Slot::Ours(Ours::Failed));
        // Another package, another cohort, or the absent cohort (a probe's load).
        assert_eq!(slot(&new_scorer("ready", "bb", c), P, c), Slot::Elsewhere);
        assert_eq!(slot(&new_scorer("ready", P, Some("testnet-rv1-b")), P, c), Slot::Elsewhere);
        assert_eq!(slot(&new_scorer("ready", P, None), P, c), Slot::Elsewhere);
        assert_eq!(slot(&new_scorer("ready", P, c), P, None), Slot::Elsewhere);
        // The probe names no cohort and matches the absent one.
        assert_eq!(slot(&new_scorer("ready", P, None), P, None), Slot::Ours(Ours::Ready));
        // A state this client does not know is never used.
        assert_eq!(slot(&new_scorer("draining", P, c), P, c), Slot::Elsewhere);
        assert_eq!(slot(&json!({}), P, c), Slot::Elsewhere);
    }

    #[test]
    fn an_older_scorer_is_compared_by_package_only_and_named_no_cohort() {
        let c = Some("testnet-rv1-a");
        assert!(!names_cohort(&old_scorer("ready", P)));
        assert!(!names_cohort(&json!({"single_use": "true"})));
        assert_eq!(sent(&old_scorer("ready", P), c), None);
        assert_eq!(sent(&new_scorer("ready", P, c), c), c);
        assert_eq!(slot(&json!({"state": "empty"}), P, c), Slot::Empty);
        assert_eq!(slot(&old_scorer("ready", P), P, c), Slot::Ours(Ours::Ready));
        assert_eq!(slot(&old_scorer("loading", P), P, c), Slot::Ours(Ours::Loading));
        assert_eq!(slot(&old_scorer("failed", P), P, None), Slot::Ours(Ours::Failed));
        assert_eq!(slot(&old_scorer("ready", "bb"), P, c), Slot::Elsewhere);
    }

    #[test]
    fn a_running_instance_is_kept_only_for_this_binding() {
        let c = Some("testnet-rv1-a");
        assert_eq!(entry(None, P, c), Entry::Keep);
        assert_eq!(entry(Some(&json!({"state": "empty", "single_use": true})), P, c), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("loading", P, c)), P, c), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("ready", P, c)), P, c), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("ready", "bb", c)), P, c), Entry::Fresh);
        assert_eq!(entry(Some(&new_scorer("ready", P, Some("testnet-rv1-b"))), P, c), Entry::Fresh);
        assert_eq!(entry(Some(&old_scorer("ready", "bb")), P, c), Entry::Fresh);
        // A failed binding: a refusal is final, anything else is retried fresh.
        let mut failed = new_scorer("failed", P, c);
        failed["error"] = json!("refused json_layout: heads.json: not the canonical JSON layout: key order");
        assert_eq!(entry(Some(&failed), P, c), Entry::Refused("refused json_layout"));
        failed["error"] = json!("invalid h");
        assert_eq!(entry(Some(&failed), P, c), Entry::Fresh);
        // A host fault reading the assembled files is retried fresh too.
        failed["error"] = json!(
            "refused object: artifact object: /tmp/a-scorer/files/aa/heads.json: Input/output error (os error 5)"
        );
        assert_eq!(entry(Some(&failed), P, c), Entry::Fresh);
        let mut old = old_scorer("failed", P);
        old["error"] = json!("profile check: heads.json digest differs");
        assert_eq!(entry(Some(&old), P, None), Entry::Fresh);
        // Another binding's refusal is not this artifact's.
        let mut other = new_scorer("failed", "bb", c);
        other["error"] = json!("refused json_layout: heads.json");
        assert_eq!(entry(Some(&other), P, c), Entry::Fresh);
    }

    #[test]
    fn only_admission_refusals_are_definitive() {
        assert_eq!(
            refusal("refused json_layout: heads.json: not the canonical JSON layout"),
            Some("refused json_layout")
        );
        assert_eq!(refusal("refused profile_id: not a packer's profile id"), Some("refused profile_id"));
        assert_eq!(
            refusal("refused tokenizer_mismatch: tokenizer.json is not byte-identical"),
            Some("refused tokenizer_mismatch")
        );
        assert_eq!(refusal("refused graph_class_not_served"), Some("refused graph_class_not_served"));
        assert_eq!(refusal("refused some_new_code: detail"), Some(REFUSED));
        assert_eq!(refusal("refused : detail"), Some(REFUSED));
        for not in [
            "artifact manifest JSON: expected value",
            "",
            "invalid h",
            "Refused json_layout: x",
            " refused json_layout: x",
            "load refused",
            "refusedjson_layout: x",
        ] {
            assert_eq!(refusal(not), None, "{not:?}");
        }
        assert_eq!(
            load_failed("refused binding: export manifest heads digest"),
            Prep::Artifact("refused binding", true)
        );
        assert_eq!(load_failed("invalid h"), Prep::Artifact("artifact load failed", false));
        // After assembly, `object` also files the instance's own read faults.
        let read =
            "refused object: artifact object: /tmp/a-scorer/files/aa/a-fp32band.onnx: Permission denied (os error 13)";
        assert_eq!(refusal(read), Some("refused object"));
        assert_eq!(load_failed(read), Prep::Artifact("refused object", false));
        assert_eq!(
            load_failed("refused object: heads.json: cannot buffer 8 bytes"),
            Prep::Artifact("refused object", false)
        );
    }

    #[test]
    fn load_and_part_replies_map_to_the_platform_or_the_artifact() {
        let e = |s: &str| json!({"error": s});
        assert_eq!(load_reply(202, &json!({"state": "loading"})), None);
        assert_eq!(load_reply(200, &json!({"state": "ready"})), None);
        assert_eq!(
            load_reply(422, &e("refused profile_id: not a packer's profile id")),
            Some(Prep::Artifact("refused profile_id", true))
        );
        // The manifest check reads no file: its `object` is the manifest's.
        assert_eq!(
            load_reply(422, &e("refused object: artifact object: part list per file")),
            Some(Prep::Artifact("refused object", true))
        );
        assert_eq!(
            load_reply(422, &e("artifact manifest JSON: trailing characters")),
            Some(Prep::Artifact("load refused", false))
        );
        assert_eq!(load_reply(422, &Value::Null), Some(Prep::Artifact("load refused", false)));
        assert_eq!(load_reply(409, &e("instance_bound")), Some(Prep::Infra(INSTANCE_BOUND)));
        assert_eq!(
            load_reply(409, &json!({"error": "missing_parts", "missing": ["bb"]})),
            Some(Prep::Infra("missing parts"))
        );
        assert_eq!(load_reply(409, &Value::Null), Some(Prep::Infra("load refused")));
        assert_eq!(load_reply(507, &e("insufficient_memory")), Some(Prep::Infra("insufficient_memory")));
        assert_eq!(load_reply(400, &e("query")), Some(Prep::Infra("load refused")));
        assert_eq!(load_reply(500, &Value::Null), Some(Prep::Infra("load refused")));
        assert_eq!(part_refused(409), Prep::Infra(INSTANCE_BOUND));
        assert_eq!(part_refused(422), Prep::Artifact("part rejected", false));
        assert_eq!(part_refused(400), Prep::Artifact("part rejected", false));
    }

    #[test]
    fn the_cohort_is_named_only_when_sent() {
        assert_eq!(load_path(P, None), "/v1/load?package=aa");
        assert_eq!(load_path(P, Some("load-s2a")), "/v1/load?package=aa&cohort=load-s2a");
        let docs = vec![(7u32, "x".to_string(), String::new())];
        let without: Value = serde_json::from_str(&score_body(P, None, &docs)).unwrap();
        // The older scorer refuses unknown fields, `"cohort": null` included.
        assert_eq!(without, json!({"package": P, "docs": [{"id": "7", "text": "x"}]}));
        let with: Value = serde_json::from_str(&score_body(P, Some("load-s2a"), &docs)).unwrap();
        assert_eq!(with, json!({"package": P, "cohort": "load-s2a", "docs": [{"id": "7", "text": "x"}]}));
    }

    /// The recorded codes survive the cohort's code sanitizer (`textgen_safe`:
    /// 64 characters of `[A-Za-z0-9 :._-()/,']`).
    #[test]
    fn recorded_codes_are_stored_intact() {
        let safe = |s: &str| s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || " :._-()/,'".contains(c));
        for (_, label) in REFUSALS {
            assert!(safe(label), "{label}");
        }
        for code in [REFUSED, INSTANCE_BOUND, REUSED, "missing parts", "insufficient_memory", "artifact load failed"] {
            assert!(safe(code), "{code}");
        }
    }

    /// Every code the checker's `Refusal::code` and the scorer's manifest check
    /// can report is in [`REFUSALS`], and nothing else is.
    #[test]
    fn the_refusal_table_matches_the_checker_and_the_scorer() {
        let checker = include_str!("../../../crates/artifact-check/src/lib.rs");
        let scorer = include_str!("../../../../inference/a-container-serve/src/limits.rs");
        let arms = checker
            .split_once("pub fn code(&self) -> &'static str {")
            .and_then(|(_, rest)| rest.split_once("impl std::fmt::Display for Refusal"))
            .expect("Refusal::code in the checker")
            .0;
        let mut want: BTreeSet<&str> =
            arms.split("=> \"").skip(1).map(|s| s.split_once('"').expect("code literal").0).collect();
        assert!(want.contains("json_layout") && want.contains("graph_class"), "Refusal::code arms");
        let ident = |s: &str| s.len() - s.trim_start_matches(|c: char| c.is_ascii_lowercase() || c == '_').len();
        let own: BTreeSet<&str> =
            scorer.split("\"refused ").skip(1).map(|s| &s[..ident(s)]).filter(|c| !c.is_empty()).collect();
        assert!(own.contains("profile_id"), "the scorer's own refusals");
        want.extend(own);
        let table: BTreeSet<&str> = REFUSALS.iter().map(|(c, _)| *c).collect();
        assert_eq!(table.len(), REFUSALS.len(), "duplicate codes");
        assert_eq!(table, want);
        // The checker's host read faults (`fs.rs` `io`) are filed under a
        // code the client retries after assembly.
        let fs = include_str!("../../../crates/artifact-check/src/fs.rs");
        let io = fs.split_once("fn io(").and_then(|(_, rest)| rest.split_once("\n}")).expect("fs.rs io()").0;
        let variant = io.split_once("Refusal::").and_then(|(_, rest)| rest.split_once('(')).expect("io()'s refusal").0;
        let code = arms
            .split_once(&format!("Refusal::{variant}("))
            .and_then(|(_, rest)| rest.split_once("=> \""))
            .and_then(|(_, rest)| rest.split_once('"'))
            .expect("io()'s code")
            .0;
        assert!(HOST_FAULT_CODES.contains(&format!("refused {code}").as_str()), "{code}");
        for label in HOST_FAULT_CODES {
            assert!(REFUSALS.iter().any(|(_, l)| l == label), "{label}");
        }
    }
}
