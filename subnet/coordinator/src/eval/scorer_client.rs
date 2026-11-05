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
//! - **Loads (files manifest v2).** The client posts the manifest
//!   (`/v2/prepare`), writes each file's missing chunks at their offsets
//!   (`/v2/files/<index>/<offset>`, read from R2 by ranged reads pinned to
//!   the entity tag the manifest names), then asks for the load
//!   (`/v2/load`). While chunks arrive the instance reports `receiving`.
//!   Nothing is hashed on the way: R2 verified every file at ingest.
//! - **Platform replies.** A chunk or load meeting another binding (409
//!   `instance_bound`), chunks missing at load (409 `missing_chunks`), a
//!   chunk the instance refused (its length: a transfer fault) and too little
//!   memory (507 `insufficient_memory`) are the platform's. An object the
//!   load could not read at its recorded entity tag ([`OBJECT_CODES`]) is
//!   neither the scorer's nor yet the miner's: admission audits it.
//! - **Artifact spec.** A cohort that names an artifact spec
//!   (`fanout.score.artifact_spec`) names its digest on every load
//!   (`spec=<digest>`, [`load_path`]), so the scorer admits the cohort's
//!   artifacts by that spec; one that names none sends none and is admitted
//!   by the scorer's unnamed-load spec (spec v2). The client sends a digest
//!   only when the scorer lists it (`/healthz` `build.limits.specs`); a scorer
//!   that does not is the platform's failure ([`SPEC_NOT_SERVED`]), never the
//!   artifact's. The instance's binding includes the spec it loaded by
//!   (`/healthz` `spec`), so an instance bound under another spec is never
//!   used ([`slot`]).

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

/// A chunk or load met an instance bound to another package or cohort.
pub const INSTANCE_BOUND: &str = "instance bound to another artifact";
/// A file object the load reads is absent from its bucket.
pub const OBJECT_MISSING: &str = "artifact object missing";
/// A file object the load reads no longer has the entity tag admission
/// recorded (the pinned read refused it).
pub const OBJECT_CHANGED: &str = "artifact object changed";
/// The load failures that send an admission run's session to an audit of
/// its recorded objects (`admission::record_run`).
pub const OBJECT_CODES: &[&str] = &[OBJECT_MISSING, OBJECT_CHANGED];
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

/// The scorer does not list the cohort's artifact spec among the specs it
/// admits loads by: a scorer build that cannot serve the cohort.
pub const SPEC_NOT_SERVED: &str = "scorer does not admit by the cohort's artifact spec";

/// The id of the spec a scorer whose `/healthz` is `h` lists with digest
/// `sha256` (`build.limits.specs`).
fn listed_id<'h>(h: &'h Value, sha256: &str) -> Option<&'h str> {
    h["build"]["limits"]["specs"].as_array()?.iter().find(|s| s["sha256"].as_str() == Some(sha256))?["id"].as_str()
}

/// The spec, by id, a load for a cohort naming `spec` (a digest; `None`: no
/// spec) binds an instance to: the listed spec with that digest, or the
/// scorer's unnamed-load spec (`build.limits.unnamed_load_spec`). `None` when
/// the scorer lists neither.
pub fn bound_spec<'h>(h: &'h Value, spec: Option<&str>) -> Option<&'h str> {
    match spec {
        Some(d) => listed_id(h, d),
        None => h["build"]["limits"]["unnamed_load_spec"].as_str(),
    }
}

/// The spec digest a load names (`spec=`): the cohort's, when the scorer
/// lists it among the specs it admits loads by. A cohort that names none
/// sends none.
pub fn spec_sent<'a>(h: &Value, spec: Option<&'a str>) -> Result<Option<&'a str>, Prep> {
    match spec {
        None => Ok(None),
        Some(d) if listed_id(h, d).is_some() => Ok(Some(d)),
        Some(_) => Err(Prep::Infra(SPEC_NOT_SERVED)),
    }
}

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
    /// Prepared: its chunks are arriving.
    Receiving,
    Loading,
    Ready,
    Failed,
}

/// The slot `h` (a `/healthz` reply) shows for `package`, the lane's
/// `cohort` and the cohort's artifact `spec` (a digest). The cohort is
/// compared only on a single-use scorer, where an absent cohort is a binding
/// too (`null` in `/healthz`); the spec only on a scorer that reports the
/// spec a binding loaded by, against the spec this load binds to
/// ([`bound_spec`]).
pub fn slot(h: &Value, package: &str, cohort: Option<&str>, spec: Option<&str>) -> Slot {
    let state = h["state"].as_str();
    if state == Some("empty") {
        return Slot::Empty;
    }
    let spec_ours = match h.get("spec") {
        None => true,
        Some(bound) => bound.as_str().is_some() && bound.as_str() == bound_spec(h, spec),
    };
    let ours = h["package"].as_str() == Some(package) && h["cohort"].as_str() == sent(h, cohort) && spec_ours;
    match (state, ours) {
        (Some("receiving"), true) => Slot::Ours(Ours::Receiving),
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

pub fn entry(h: Option<&Value>, package: &str, cohort: Option<&str>, spec: Option<&str>) -> Entry {
    let Some(h) = h else { return Entry::Keep };
    match slot(h, package, cohort, spec) {
        Slot::Empty | Slot::Ours(Ours::Receiving | Ours::Loading | Ours::Ready) => Entry::Keep,
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

/// A `PUT /v2/files/<index>/<offset>` reply other than 201 (or 200 for a
/// chunk already written): another binding, or a chunk the instance refused
/// (its exact length or offset), which is the transfer's fault: the bytes
/// were read from R2 at the recorded length and entity tag.
pub fn chunk_refused(status: u16) -> Prep {
    match status {
        409 => Prep::Infra(INSTANCE_BOUND),
        _ => Prep::Infra("chunk rejected"),
    }
}

/// A `POST /v2/prepare` reply and its JSON body: `Ok(missing chunks)` as
/// `(file index, offset)` when the instance took (or already holds) this
/// binding, else the failure. The manifest check reads no file, so a
/// refusal there is the manifest's and definitive.
pub fn prepare_reply(status: u16, body: &Value) -> Result<Vec<(usize, u64)>, Prep> {
    let error = body["error"].as_str().unwrap_or("");
    match status {
        200 | 201 => Ok(missing_chunks(body)),
        422 => Err(match refusal(error) {
            Some(code) => Prep::Artifact(code, true),
            None => Prep::Artifact("prepare refused", false),
        }),
        409 if error == "instance_bound" => Err(Prep::Infra(INSTANCE_BOUND)),
        507 => Err(Prep::Infra("insufficient_memory")),
        _ => Err(Prep::Infra("prepare refused")),
    }
}

/// The chunks a prepare reply lists as missing (`missing`: `[[index,
/// offset], ...]`); an entry that does not parse is skipped (the load then
/// answers `missing_chunks`, the platform's).
pub fn missing_chunks(body: &Value) -> Vec<(usize, u64)> {
    body["missing"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| Some((usize::try_from(c.get(0)?.as_u64()?).ok()?, c.get(1)?.as_u64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// A `POST /v2/load` reply and its JSON body (`Null` when unreadable): `None`
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
        409 if error == "missing_chunks" => Some(Prep::Infra("missing chunks")),
        409 if error == "not_prepared" => Some(Prep::Infra("not prepared")),
        507 => Some(Prep::Infra("insufficient_memory")),
        _ => Some(Prep::Infra("load refused")),
    }
}

/// The prepare route (`/v2/prepare`), with the load's query.
pub fn prepare_path(package: &str, cohort: Option<&str>, spec: Option<&str>) -> String {
    format!("/v2/prepare{}", load_query(package, cohort, spec))
}

/// The load route (`/v2/load`), naming the cohort and the artifact spec's
/// digest when the client sends them ([`sent`], [`spec_sent`]).
pub fn load_path(package: &str, cohort: Option<&str>, spec: Option<&str>) -> String {
    format!("/v2/load{}", load_query(package, cohort, spec))
}

/// The chunk route of file `index` at `offset`.
pub fn chunk_path(index: usize, offset: u64) -> String {
    format!("/v2/files/{index}/{offset}")
}

fn load_query(package: &str, cohort: Option<&str>, spec: Option<&str>) -> String {
    let mut path = format!("?package={package}");
    if let Some(c) = cohort {
        path.push_str("&cohort=");
        path.push_str(c);
    }
    if let Some(d) = spec {
        path.push_str("&spec=");
        path.push_str(d);
    }
    path
}

/// A score request body; `cohort` is omitted, not `null`, when none is sent,
/// and `spans` is sent only when asked (the operator probe of a package with a
/// token head: each prediction then carries its sentences and spans; cohort
/// lanes never ask, and the scorer's document outputs are the same either way).
pub fn score_body(package: &str, cohort: Option<&str>, docs: &[(u32, String, String)], spans: bool) -> String {
    let docs: Vec<Value> = docs.iter().map(|(t, text, _)| json!({"id": t.to_string(), "text": text})).collect();
    let mut v = json!({"package": package, "docs": docs});
    if let Some(c) = cohort {
        v["cohort"] = json!(c);
    }
    if spans {
        v["spans"] = json!(true);
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
        assert_eq!(slot(&json!({"state": "empty", "single_use": true}), P, c, None), Slot::Empty);
        assert_eq!(slot(&new_scorer("loading", P, c), P, c, None), Slot::Ours(Ours::Loading));
        assert_eq!(slot(&new_scorer("ready", P, c), P, c, None), Slot::Ours(Ours::Ready));
        assert_eq!(slot(&new_scorer("failed", P, c), P, c, None), Slot::Ours(Ours::Failed));
        // Another package, another cohort, or the absent cohort (a probe's load).
        assert_eq!(slot(&new_scorer("ready", "bb", c), P, c, None), Slot::Elsewhere);
        assert_eq!(slot(&new_scorer("ready", P, Some("testnet-rv1-b")), P, c, None), Slot::Elsewhere);
        assert_eq!(slot(&new_scorer("ready", P, None), P, c, None), Slot::Elsewhere);
        assert_eq!(slot(&new_scorer("ready", P, c), P, None, None), Slot::Elsewhere);
        // The probe names no cohort and matches the absent one.
        assert_eq!(slot(&new_scorer("ready", P, None), P, None, None), Slot::Ours(Ours::Ready));
        // A state this client does not know is never used.
        assert_eq!(slot(&new_scorer("draining", P, c), P, c, None), Slot::Elsewhere);
        assert_eq!(slot(&json!({}), P, c, None), Slot::Elsewhere);
    }

    #[test]
    fn an_older_scorer_is_compared_by_package_only_and_named_no_cohort() {
        let c = Some("testnet-rv1-a");
        assert!(!names_cohort(&old_scorer("ready", P)));
        assert!(!names_cohort(&json!({"single_use": "true"})));
        assert_eq!(sent(&old_scorer("ready", P), c), None);
        assert_eq!(sent(&new_scorer("ready", P, c), c), c);
        assert_eq!(slot(&json!({"state": "empty"}), P, c, None), Slot::Empty);
        assert_eq!(slot(&old_scorer("ready", P), P, c, None), Slot::Ours(Ours::Ready));
        assert_eq!(slot(&old_scorer("loading", P), P, c, None), Slot::Ours(Ours::Loading));
        assert_eq!(slot(&old_scorer("failed", P), P, None, None), Slot::Ours(Ours::Failed));
        assert_eq!(slot(&old_scorer("ready", "bb"), P, c, None), Slot::Elsewhere);
    }

    #[test]
    fn a_running_instance_is_kept_only_for_this_binding() {
        let c = Some("testnet-rv1-a");
        assert_eq!(entry(None, P, c, None), Entry::Keep);
        assert_eq!(entry(Some(&json!({"state": "empty", "single_use": true})), P, c, None), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("loading", P, c)), P, c, None), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("ready", P, c)), P, c, None), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("ready", "bb", c)), P, c, None), Entry::Fresh);
        assert_eq!(entry(Some(&new_scorer("ready", P, Some("testnet-rv1-b"))), P, c, None), Entry::Fresh);
        assert_eq!(entry(Some(&old_scorer("ready", "bb")), P, c, None), Entry::Fresh);
        // A failed binding: a refusal is final, anything else is retried fresh.
        let mut failed = new_scorer("failed", P, c);
        failed["error"] = json!("refused json_layout: heads.json: not the canonical JSON layout: key order");
        assert_eq!(entry(Some(&failed), P, c, None), Entry::Refused("refused json_layout"));
        failed["error"] = json!("invalid h");
        assert_eq!(entry(Some(&failed), P, c, None), Entry::Fresh);
        // A host fault reading the assembled files is retried fresh too.
        failed["error"] = json!(
            "refused object: artifact object: /tmp/a-scorer/files/aa/heads.json: Input/output error (os error 5)"
        );
        assert_eq!(entry(Some(&failed), P, c, None), Entry::Fresh);
        let mut old = old_scorer("failed", P);
        old["error"] = json!("profile check: heads.json digest differs");
        assert_eq!(entry(Some(&old), P, None, None), Entry::Fresh);
        // Another binding's refusal is not this artifact's.
        let mut other = new_scorer("failed", "bb", c);
        other["error"] = json!("refused json_layout: heads.json");
        assert_eq!(entry(Some(&other), P, c, None), Entry::Fresh);
    }

    const V2: &str = "0f25e636ba52b45269c0a4d0520c2f01550b51325bb6399d4ddae418c8384b50";
    const V3: &str = "0bb326a93b1f4669018dadac3186f08bcc0ac527983eed0c75ce1e889e970d25";

    /// The deployed scorer's `/healthz`: its binding with the spec it loaded
    /// by, and the specs it admits loads by.
    fn spec_scorer(state: &str, package: &str, cohort: Option<&str>, spec: &str) -> Value {
        json!({"state": state, "package": package, "cohort": cohort, "spec": spec, "single_use": true,
            "build": {"limits": {"specs": [{"id": "slop_ninja_a_artifact_spec_v2", "sha256": V2},
                {"id": "slop_ninja_a_artifact_spec_v3", "sha256": V3}],
                "unnamed_load_spec": "slop_ninja_a_artifact_spec_v2"}}})
    }

    #[test]
    fn a_load_names_the_cohort_spec_and_an_instance_bound_under_another_spec_is_never_used() {
        let c = Some("testnet-r0b-14");
        let empty = json!({"state": "empty", "single_use": true, "build": spec_scorer("empty", P, None, "")["build"]});
        // The digest is sent only when the scorer lists it; a cohort that names
        // no spec sends none, whatever the scorer.
        assert_eq!(spec_sent(&empty, None), Ok(None));
        assert_eq!(spec_sent(&empty, Some(V3)), Ok(Some(V3)));
        assert_eq!(spec_sent(&empty, Some(V2)), Ok(Some(V2)));
        assert_eq!(spec_sent(&empty, Some(&"ab".repeat(32))), Err(Prep::Infra(SPEC_NOT_SERVED)));
        assert_eq!(spec_sent(&old_scorer("empty", P), Some(V3)), Err(Prep::Infra(SPEC_NOT_SERVED)));
        assert_eq!(spec_sent(&old_scorer("empty", P), None), Ok(None));
        assert_eq!(SPEC_NOT_SERVED, Prep::Infra(SPEC_NOT_SERVED).code(), "the platform's, never the artifact's");
        assert_eq!(load_path(P, c, Some(V3)), format!("/v2/load?package=aa&cohort=testnet-r0b-14&spec={V3}"));
        assert_eq!(load_path(P, None, Some(V3)), format!("/v2/load?package=aa&spec={V3}"));
        assert_eq!(prepare_path(P, c, Some(V3)), format!("/v2/prepare?package=aa&cohort=testnet-r0b-14&spec={V3}"));
        // What the load binds to: the named spec, or the unnamed-load spec.
        assert_eq!(bound_spec(&empty, Some(V3)), Some("slop_ninja_a_artifact_spec_v3"));
        assert_eq!(bound_spec(&empty, None), Some("slop_ninja_a_artifact_spec_v2"));
        assert_eq!(bound_spec(&old_scorer("empty", P), None), None);
        // An instance is this lane's only under the spec this load binds to.
        let v3 = spec_scorer("ready", P, c, "slop_ninja_a_artifact_spec_v3");
        let v2 = spec_scorer("ready", P, c, "slop_ninja_a_artifact_spec_v2");
        assert_eq!(slot(&v3, P, c, Some(V3)), Slot::Ours(Ours::Ready));
        assert_eq!(slot(&v2, P, c, Some(V3)), Slot::Elsewhere);
        assert_eq!(slot(&v3, P, c, None), Slot::Elsewhere);
        assert_eq!(slot(&v2, P, c, None), Slot::Ours(Ours::Ready));
        // Naming spec v2 binds what an unnamed load binds.
        assert_eq!(slot(&v2, P, c, Some(V2)), Slot::Ours(Ours::Ready));
        assert_eq!(slot(&v3, P, c, Some(&"ab".repeat(32))), Slot::Elsewhere);
        let mut bad = v3.clone();
        bad["spec"] = Value::Null;
        assert_eq!(slot(&bad, P, c, Some(V3)), Slot::Elsewhere);
        // A probe of the same package under another spec starts fresh; a
        // failed load under another spec is not this load's refusal.
        let probe_v2 = spec_scorer("ready", P, None, "slop_ninja_a_artifact_spec_v2");
        assert_eq!(entry(Some(&probe_v2), P, None, Some(V3)), Entry::Fresh);
        assert_eq!(entry(Some(&probe_v2), P, None, None), Entry::Keep);
        let mut refused_v2 = spec_scorer("failed", P, None, "slop_ninja_a_artifact_spec_v2");
        refused_v2["error"] = json!("refused graph_size: the graph is not a class spec v2 serves");
        assert_eq!(entry(Some(&refused_v2), P, None, None), Entry::Refused("refused graph_size"));
        assert_eq!(entry(Some(&refused_v2), P, None, Some(V3)), Entry::Fresh);
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
            load_reply(409, &json!({"error": "missing_chunks", "missing": [[0, 0]]})),
            Some(Prep::Infra("missing chunks"))
        );
        assert_eq!(load_reply(409, &Value::Null), Some(Prep::Infra("load refused")));
        assert_eq!(load_reply(507, &e("insufficient_memory")), Some(Prep::Infra("insufficient_memory")));
        assert_eq!(load_reply(400, &e("query")), Some(Prep::Infra("load refused")));
        assert_eq!(load_reply(500, &Value::Null), Some(Prep::Infra("load refused")));
        assert_eq!(chunk_refused(409), Prep::Infra(INSTANCE_BOUND));
        assert_eq!(chunk_refused(422), Prep::Infra("chunk rejected"));
        assert_eq!(chunk_refused(400), Prep::Infra("chunk rejected"));
        // The prepare: the missing chunks, or the manifest's own refusal.
        let ok = json!({"state": "receiving", "missing": [[0, 0], [0, 67108864], [5, 0], ["x", 1]]});
        assert_eq!(prepare_reply(201, &ok), Ok(vec![(0, 0), (0, 64 << 20), (5, 0)]));
        assert_eq!(prepare_reply(200, &json!({"state": "receiving"})), Ok(Vec::new()));
        assert_eq!(
            prepare_reply(422, &e("refused graph_class: not a served class")),
            Err(Prep::Artifact("refused graph_class", true))
        );
        assert_eq!(prepare_reply(422, &e("files manifest: schema")), Err(Prep::Artifact("prepare refused", false)));
        assert_eq!(prepare_reply(409, &e("instance_bound")), Err(Prep::Infra(INSTANCE_BOUND)));
        assert_eq!(prepare_reply(507, &e("insufficient_memory")), Err(Prep::Infra("insufficient_memory")));
        assert_eq!(prepare_reply(400, &e("query")), Err(Prep::Infra("prepare refused")));
        assert_eq!(chunk_path(3, 64 << 20), "/v2/files/3/67108864");
    }

    #[test]
    fn a_receiving_instance_is_kept_for_its_binding() {
        let c = Some("testnet-rv1-a");
        assert_eq!(slot(&new_scorer("receiving", P, c), P, c, None), Slot::Ours(Ours::Receiving));
        assert_eq!(entry(Some(&new_scorer("receiving", P, c)), P, c, None), Entry::Keep);
        assert_eq!(entry(Some(&new_scorer("receiving", "bb", c)), P, c, None), Entry::Fresh);
    }

    #[test]
    fn the_cohort_is_named_only_when_sent() {
        assert_eq!(load_path(P, None, None), "/v2/load?package=aa");
        assert_eq!(load_path(P, Some("load-s2a"), None), "/v2/load?package=aa&cohort=load-s2a");
        let docs = vec![(7u32, "x".to_string(), String::new())];
        let without: Value = serde_json::from_str(&score_body(P, None, &docs, false)).unwrap();
        // The older scorer refuses unknown fields, `"cohort": null` included.
        assert_eq!(without, json!({"package": P, "docs": [{"id": "7", "text": "x"}]}));
        let with: Value = serde_json::from_str(&score_body(P, Some("load-s2a"), &docs, false)).unwrap();
        assert_eq!(with, json!({"package": P, "cohort": "load-s2a", "docs": [{"id": "7", "text": "x"}]}));
        // Spans only when the probe asks.
        let spans: Value = serde_json::from_str(&score_body(P, None, &docs, true)).unwrap();
        assert_eq!(spans, json!({"package": P, "spans": true, "docs": [{"id": "7", "text": "x"}]}));
    }

    /// The recorded codes survive the cohort's code sanitizer (`textgen_safe`:
    /// 64 characters of `[A-Za-z0-9 :._-()/,']`).
    #[test]
    fn recorded_codes_are_stored_intact() {
        let safe = |s: &str| s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || " :._-()/,'".contains(c));
        for (_, label) in REFUSALS {
            assert!(safe(label), "{label}");
        }
        for code in [REFUSED, INSTANCE_BOUND, REUSED, OBJECT_MISSING, OBJECT_CHANGED, "missing chunks", "chunk rejected", "insufficient_memory", "artifact load failed"] {
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
