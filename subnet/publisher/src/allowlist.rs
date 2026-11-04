//! The versioned allow-list of everything the public host may carry
//! (`slop_ninja.public.allowlist/v1`).
//!
//! Validators and the public never learn held-out data (user rule,
//! 2026-11-04): every object in the public bucket must be one this module
//! names, and every JSON document may carry only the fields its schema lists
//! here. Anything else is refused before it is written: the public bucket is
//! reachable only through [`crate::store::PublicStore`], whose every put
//! runs [`check_object`] first and writes nothing it refuses
//! (`StoreError::NotAllowed`), and whose streamed copies go only to
//! artifact keys or carry bytes checked first and bound by digest, so no
//! publisher write (its JSON documents, the site page and stylesheet, the
//! trust file, the status snapshot, the archived record index, the copied
//! authorization objects and artifacts) can bypass it. What an earlier
//! build left behind is what the key audit (`slop_ninja_publisher_ops
//! audit-keys`, with the operator's private wrapper doing the listing,
//! fetching and deletion) removes. A field added to a public document fails
//! the publisher's tests (every document a run writes, and a status
//! snapshot with every optional field set, are checked here) until it is
//! added here, with a version bump, so a new public field is always a
//! reviewed decision.
//!
//! Binary objects are named by kind:
//!
//! - signed objects under `v1/authorizations/` must decode as a launch or
//!   reward-intent authorization, a renew-by hint refresh, a coordinator row
//!   or a revocation set (whole signed revocations);
//! - `v1/trust/testnet-trust.json` must be the compiled trust file, byte for
//!   byte, and `style.css` the compiled stylesheet;
//! - the rendered page (`index.html`) must be this build's template with
//!   only its summary and row slots filled, neither carrying a layout-v1
//!   score column (the v1 page is rewritten when the v1 index is migrated,
//!   before any cohort is re-listed, and the key audit deletes one left
//!   behind);
//! - artifacts (`v1/artifacts/<sha256>.bin`) and staged reference packages
//!   (`v1/reference/packages/`, staged by the operator; their own contents,
//!   such as `R_0`'s calibration table, are reviewed separately) are allowed
//!   by key.
//!
//! Every cohort object other than `summary.json` and `publication.json`
//! (the signed manifest, package, target and closure, receipt indexes, chunk
//! maps and bundles, published-content and fingerprint indexes, closure
//! fingerprints and components) is outside the allow-list: those are
//! coordinator-private.

use std::collections::BTreeSet;

use slop_ninja_protocol::{
    CoordinatorRow, LaunchAuthorization, RewardIntentAuthorization, RewardIntentRenewal,
    RewardIntentRevocation, Signed,
};

use crate::layout;

pub const ALLOWLIST_VERSION: &str = "slop_ninja.public.allowlist/v1";

/// What a public key holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    SitePage,
    SiteStyle,
    Status,
    CohortIndex,
    /// An earlier record's index, under `v1/records/<label>/cohorts/`.
    ArchivedIndex,
    Summary,
    Publication,
    Leaderboard,
    Reference,
    ReferencePackage,
    Artifact,
    AuthorizationPointer,
    SignedObject,
    Trust,
}

fn hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A path inside a staged reference package: segments of ASCII letters,
/// digits, `.`, `_` and `-` (its notice files keep their conventional
/// capitals), never `.` or `..`.
fn staged_rel(rel: &str) -> bool {
    !rel.is_empty()
        && rel.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// The kind of object `key` may hold, or `None` when the key is outside the
/// allow-list.
pub fn classify(key: &str) -> Option<Kind> {
    match key {
        layout::SITE_INDEX => return Some(Kind::SitePage),
        layout::SITE_STYLE => return Some(Kind::SiteStyle),
        layout::STATUS => return Some(Kind::Status),
        layout::COHORT_INDEX => return Some(Kind::CohortIndex),
        layout::LEADERBOARD => return Some(Kind::Leaderboard),
        layout::REFERENCE_CURRENT | layout::REFERENCE_PREVIOUS => return Some(Kind::Reference),
        layout::AUTHORIZATION_CURRENT => return Some(Kind::AuthorizationPointer),
        layout::TRUST => return Some(Kind::Trust),
        _ => {}
    }
    if let Some(rest) = key.strip_prefix("v1/cohorts/") {
        let (label, rel) = rest.split_once('/')?;
        if !layout::safe_label(label) {
            return None;
        }
        return match rel {
            crate::summary::SUMMARY => Some(Kind::Summary),
            "publication.json" => Some(Kind::Publication),
            _ => None,
        };
    }
    if let Some(rest) = key.strip_prefix("v1/records/") {
        let (label, rel) = rest.split_once('/')?;
        return (layout::safe_label(label) && rel == "cohorts/index.json")
            .then_some(Kind::ArchivedIndex);
    }
    if let Some(name) = key.strip_prefix("v1/artifacts/") {
        return name
            .strip_suffix(".bin")
            .filter(|h| hex64(h))
            .map(|_| Kind::Artifact);
    }
    if let Some(name) = key.strip_prefix("v1/authorizations/") {
        return name
            .strip_suffix(".signed")
            .filter(|h| hex64(h))
            .map(|_| Kind::SignedObject);
    }
    if let Some(rest) = key.strip_prefix("v1/reference/packages/") {
        if rest == "index.json" {
            return Some(Kind::ReferencePackage);
        }
        let (dir, rel) = rest.split_once('/')?;
        return (hex64(dir) && staged_rel(rel)).then_some(Kind::ReferencePackage);
    }
    None
}

const CHAIN: &[&str] = &["chain", "chain.genesis", "chain.netuid", "chain.mechanism"];

const STATUS: &[&str] = &[
    "schema",
    "network",
    "confirmed",
    "confirmed.block",
    "confirmed.epoch",
    "confirmed.age_secs",
    "phase",
    "mode",
    "fault",
    "notice",
    "admitting",
    "admitting.cohort",
    "admitting.label",
    "admitting.opened_epoch",
    "admitting.planned_cutoff_epoch",
    "next",
    "next.cohort",
    "next.earliest_open_epoch",
    "next.admission_tempos",
    "next.output_contract",
    "next.max_object_len",
    "next.artifact_spec",
    "next.artifact_spec.id",
    "next.artifact_spec.sha256",
    "coordinator",
    "active_target",
    "reference",
    "reference_index",
    "generated_at",
    "sources",
    "sources.coordinator",
    "sources.eval",
    "last_published_cohort",
];

const INDEX: &[&str] = &[
    "schema",
    "network",
    "trust",
    "trust.path",
    "trust.sha256",
    "active_target",
    "active_target.digest",
    "active_target.source",
    "active_target.cohort",
    "cohorts",
    "cohorts[].cohort",
    "cohorts[].label",
    "cohorts[].path",
    "cohorts[].opened_epoch",
    "cohorts[].closed_epoch",
    "cohorts[].promoted",
    "cohorts[].target",
    "cohorts[].reference_index",
    "cohorts[].manifest",
    "cohorts[].summary",
    "cohorts[].entrants",
    "cohorts[].artifacts_published",
    "cohorts[].artifacts_withheld",
    "cohorts[].artifacts_pending",
    "initial_references",
    "initial_references[].content",
    "initial_references[].artifact",
    "initial_references[].first_cohort",
    "record_prefix",
    "previous_records",
    "previous_records[].prefix",
    "previous_records[].first_label",
    "previous_records[].initial_references",
    "previous_records[].initial_references[].content",
    "previous_records[].initial_references[].artifact",
    "previous_records[].initial_references[].first_cohort",
    "retired_unpublished",
    "retired_unpublished[].label",
    "retired_unpublished[].cohort",
    "retired_unpublished[].opened_epoch",
    "retired_unpublished[].due_after_cohort",
    "retired_unpublished[].starts_record",
    "refused",
    "refused[].label",
    "refused[].cohort",
    "refused[].opened_epoch",
    "refused[].starts_record",
    "refused[].reason",
    "refused[].build",
];

const REFERENCES: &[&str] = &[
    "references",
    "references.index",
    "references.current",
    "references.current.content",
    "references.current.artifact",
    "references.predecessor",
    "references.predecessor.content",
    "references.predecessor.artifact",
];

// QUANTIZED-SCORE: once the quantization specification is approved, the
// quantized score of each entrant (and of R_k and R_k-1) is the one new
// field of the summary and the leaderboard, with an allow-list version bump.
const ENTRANT: &[&str] = &[
    "uid",
    "hotkey",
    "hotkey_hex",
    "rank",
    "band",
    "gate",
    "copy_gate",
    "cluster",
    "cluster.head",
    "cluster.head_uid",
];

const SUMMARY: &[&str] = &[
    "schema",
    "network",
    "cohort",
    "label",
    "opened_epoch",
    "closed_epoch",
    "promoted",
    "entrants",
];

const ARTIFACT: &[&str] = &[
    "uid",
    "hotkey",
    "sha256",
    "content",
    "state",
    "path",
    "reason",
    "hotkey_hex",
    "registration_block",
    "commitment",
    "nonce",
];

const PUBLICATION: &[&str] = &[
    "schema",
    "cohort",
    "label",
    "publisher",
    "summary",
    "artifacts",
];

const POINTER: &[&str] = &[
    "schema",
    "role",
    "index",
    "content",
    "artifact",
    "as_of_cohort",
    "path",
    "withheld",
    "package",
    "licence",
];

const LEADERBOARD: &[&str] = &[
    "schema",
    "network",
    "cohort",
    "label",
    "closed_epoch",
    "promoted",
    "promotion_rule",
    "promotion_rule.lookup_canary",
    "promotion_rule.scope",
    "promotion_rule.note",
    "active_target",
    "active_target.digest",
    "active_target.source",
    "active_target.cohort",
    "current_reference",
    "previous_reference",
    "entrants",
];

const OBJECT_REF: &[&str] = &["path", "sha256", "bytes"];

const AUTHORIZATION: &[&str] = &[
    "schema",
    "kind",
    "authorization",
    "renewal",
    "revocations",
    "row",
];

fn prefixed(prefix: &str, fields: &[&str]) -> impl Iterator<Item = String> {
    fields.iter().map(move |f| format!("{prefix}{f}"))
}

/// Every field path a JSON document of `kind` may carry (object keys joined
/// with `.`, `[]` for an array's elements), with its required `schema`.
pub fn fields(kind: Kind) -> Option<(&'static str, BTreeSet<String>)> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let schema = match kind {
        Kind::Status => {
            out.extend(prefixed("", STATUS));
            out.extend(prefixed("", CHAIN));
            crate::status::STATUS_SCHEMA
        }
        Kind::CohortIndex | Kind::ArchivedIndex => {
            out.extend(prefixed("", INDEX));
            out.extend(prefixed("", CHAIN));
            crate::index::INDEX_SCHEMA
        }
        Kind::Summary => {
            out.extend(prefixed("", SUMMARY));
            out.extend(prefixed("", CHAIN));
            out.extend(prefixed("", REFERENCES));
            out.extend(prefixed("entrants[].", ENTRANT));
            crate::summary::SUMMARY_SCHEMA
        }
        Kind::Publication => {
            out.extend(prefixed("", PUBLICATION));
            out.extend(prefixed("artifacts[].", ARTIFACT));
            crate::index::PUBLICATION_SCHEMA
        }
        Kind::Leaderboard => {
            out.extend(prefixed("", LEADERBOARD));
            out.extend(prefixed("", CHAIN));
            out.extend(prefixed("", REFERENCES));
            out.extend(prefixed("current_reference.", POINTER));
            out.extend(prefixed("previous_reference.", POINTER));
            out.extend(prefixed("entrants[].", ENTRANT));
            out.insert("entrants[].artifact".into());
            out.extend(prefixed("entrants[].artifact.", ARTIFACT));
            crate::leaderboard::LEADERBOARD_SCHEMA
        }
        Kind::Reference => {
            out.extend(prefixed("", POINTER));
            crate::index::REFERENCE_SCHEMA
        }
        Kind::AuthorizationPointer => {
            out.extend(prefixed("", AUTHORIZATION));
            out.extend(prefixed("", CHAIN));
            for r in ["authorization.", "renewal.", "revocations.", "row."] {
                out.extend(prefixed(r, OBJECT_REF));
            }
            crate::publish::AUTHORIZATION_SCHEMA
        }
        _ => return None,
    };
    Some((schema, out))
}

/// Every object-key path of `v` ([`fields`]'s form).
pub fn paths(v: &serde_json::Value) -> BTreeSet<String> {
    fn walk(v: &serde_json::Value, at: &str, out: &mut BTreeSet<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, child) in m {
                    let p = if at.is_empty() {
                        k.clone()
                    } else {
                        format!("{at}.{k}")
                    };
                    out.insert(p.clone());
                    walk(child, &p, out);
                }
            }
            serde_json::Value::Array(items) => {
                let p = format!("{at}[]");
                for child in items {
                    walk(child, &p, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(v, "", &mut out);
    out
}

/// Whether bytes are a signed object the pointer may name.
fn signed_object(bytes: &[u8]) -> bool {
    if Signed::<LaunchAuthorization>::from_bytes(bytes).is_ok()
        || Signed::<RewardIntentAuthorization>::from_bytes(bytes).is_ok()
        || Signed::<RewardIntentRenewal>::from_bytes(bytes).is_ok()
        || Signed::<CoordinatorRow>::from_bytes(bytes).is_ok()
    {
        return true;
    }
    // A revocation set: whole signed revocations of the schema's fixed
    // length (every field is fixed-width, so any chain's has it).
    let any = slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::Digest([0; 32]),
        netuid: 0,
        mechanism: 0,
    };
    let len = slop_ninja_admission_wire::receipts::revocation_len(&any) as usize;
    len > 0
        && !bytes.is_empty()
        && bytes.len().is_multiple_of(len)
        && bytes
            .chunks_exact(len)
            .all(|c| Signed::<RewardIntentRevocation>::from_bytes(c).is_ok())
}

/// Text only the layout-v1 page carried (its score and credit columns and
/// the reference's score), refused in a page's filled slots too.
const V1_PAGE_MARKERS: &[&str] = &["Brier", " score</dt>", ">Credit<", ">Units<", "Threshold"];

/// A page is the compiled template ([`crate::leaderboard::TEMPLATE`]) with
/// only its two slots (the summary and the table rows) filled, and neither
/// slot carries text only the layout-v1 page did. So a page an earlier
/// build rendered (the v1 template with its score columns) is refused, by
/// the publisher before it writes and by the key audit after.
fn check_page(bytes: &[u8]) -> Result<(), &'static str> {
    let page = std::str::from_utf8(bytes).map_err(|_| "not UTF-8")?;
    let template = crate::leaderboard::TEMPLATE;
    let (head, rest) = template
        .split_once("<!--SUMMARY-->")
        .ok_or("template without a summary slot")?;
    let (mid, tail) = rest
        .split_once("<!--ROWS-->")
        .ok_or("template without a rows slot")?;
    let filled = page
        .strip_prefix(head)
        .and_then(|p| p.strip_suffix(tail))
        .ok_or("not this build's page template")?;
    let (summary, rows) = filled
        .split_once(mid)
        .ok_or("not this build's page template")?;
    if [summary, rows]
        .iter()
        .any(|slot| V1_PAGE_MARKERS.iter().any(|m| slot.contains(m)))
    {
        return Err("carries a layout-v1 score column");
    }
    Ok(())
}

/// Checks one public object: its key is in the allow-list, a JSON document
/// carries its schema and only the fields that schema lists, and a binary
/// object is of its key's kind. Content-free errors.
pub fn check_object(key: &str, bytes: &[u8]) -> Result<(), String> {
    let kind = classify(key).ok_or_else(|| format!("{key}: outside the allow-list"))?;
    match kind {
        Kind::SitePage => check_page(bytes).map_err(|why| format!("{key}: {why}")),
        Kind::Artifact | Kind::ReferencePackage => Ok(()),
        Kind::SiteStyle => (bytes == crate::leaderboard::STYLE.as_bytes())
            .then_some(())
            .ok_or_else(|| format!("{key}: not the compiled stylesheet")),
        Kind::Trust => (bytes == crate::config::TRUST_FILE)
            .then_some(())
            .ok_or_else(|| format!("{key}: not the compiled trust file")),
        Kind::SignedObject => signed_object(bytes)
            .then_some(())
            .ok_or_else(|| format!("{key}: not a signed object the pointer may name")),
        _ => {
            let (schema, allowed) = fields(kind).ok_or_else(|| format!("{key}: no field list"))?;
            let v: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|_| format!("{key}: not JSON"))?;
            if v.get("schema").and_then(|s| s.as_str()) != Some(schema) {
                return Err(format!("{key}: not schema {schema}"));
            }
            let extra: Vec<String> = paths(&v)
                .into_iter()
                .filter(|p| !allowed.contains(p))
                .collect();
            if extra.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{key}: fields outside the allow-list: {}",
                    extra.join(", ")
                ))
            }
        }
    }
}

/// One key the audit deletes, and why (a fixed code, or the content check's
/// content-free reason).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Deletion {
    pub key: String,
    pub reason: String,
}

/// The key audit's plan ([`audit`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AuditPlan {
    pub allowlist: &'static str,
    pub keys: usize,
    pub keep: usize,
    /// Allowed keys of a content-checked kind whose content was not
    /// available to check: a deletion pass refuses to run while any is
    /// listed (`audit-keys --for-delete`).
    pub unchecked: Vec<String>,
    /// Keys allowed by key alone (artifacts, staged reference packages):
    /// kept without a content check.
    pub kept_by_key: Vec<String>,
    pub delete: Vec<Deletion>,
}

/// Plans the deletion of every key outside the allow-list and of every
/// allowed object whose content `read` returns and fails [`check_object`].
/// `read` is asked only for allowed keys of a content-checked kind (every
/// kind but artifacts and staged reference packages, the page included).
pub fn audit(keys: &[String], mut read: impl FnMut(&str) -> Option<Vec<u8>>) -> AuditPlan {
    let mut plan = AuditPlan {
        allowlist: ALLOWLIST_VERSION,
        keys: keys.len(),
        keep: 0,
        unchecked: Vec::new(),
        kept_by_key: Vec::new(),
        delete: Vec::new(),
    };
    for key in keys {
        let Some(kind) = classify(key) else {
            plan.delete.push(Deletion {
                key: key.clone(),
                reason: "outside_allowlist".into(),
            });
            continue;
        };
        if matches!(kind, Kind::Artifact | Kind::ReferencePackage) {
            plan.kept_by_key.push(key.clone());
            plan.keep += 1;
            continue;
        }
        match read(key) {
            None => {
                plan.unchecked.push(key.clone());
                plan.keep += 1;
            }
            Some(bytes) => match check_object(key, &bytes) {
                Ok(()) => plan.keep += 1,
                Err(why) => plan.delete.push(Deletion {
                    key: key.clone(),
                    reason: why,
                }),
            },
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status snapshot is written by the Worker alone (no native run
    /// writes it), so its allow-list is checked here: a snapshot with every
    /// optional field set passes, and the struct literal names every field,
    /// so a field added to `PublicStatus` fails to build this test until it
    /// is set here and, set, fails the check until it is allow-listed.
    #[test]
    fn a_status_snapshot_with_every_optional_field_set_is_allowed() {
        use crate::status::{Admitting, Confirmed, PublicStatus, STATUS_SCHEMA, Sources};
        use slop_ninja_admission_wire::json::{ArtifactSpecRef, Chain, NextCohort};
        let chain = slop_ninja_protocol::ChainBinding {
            genesis: slop_ninja_protocol::TESTNET_GENESIS,
            netuid: 601,
            mechanism: 0,
        };
        let digest = "ab".repeat(32);
        let status = PublicStatus {
            schema: STATUS_SCHEMA.into(),
            network: "testnet".into(),
            chain: Chain::from_binding(&chain),
            confirmed: Some(Confirmed {
                block: 10,
                epoch: Some(2),
                age_secs: Some(12),
            }),
            phase: Some("admission".into()),
            mode: Some("burn".into()),
            fault: true,
            notice: Some("notice".into()),
            admitting: Some(Admitting {
                cohort: 3,
                label: Some("testnet-c3".into()),
                opened_epoch: 2,
                planned_cutoff_epoch: 4,
            }),
            next: Some(NextCohort {
                cohort: 4,
                earliest_open_epoch: Some(6),
                admission_tempos: Some(2),
                output_contract: Some("contract".into()),
                max_object_len: Some(1024),
                artifact_spec: Some(ArtifactSpecRef {
                    id: "spec".into(),
                    sha256: digest.clone(),
                }),
            }),
            coordinator: Some("ok".into()),
            active_target: Some(digest.clone()),
            reference: Some(digest),
            reference_index: Some(0),
            generated_at: 1,
            sources: Sources {
                coordinator: true,
                eval: true,
            },
            last_published_cohort: Some(2),
        };
        let bytes = serde_json::to_vec_pretty(&status).unwrap();
        check_object(layout::STATUS, &bytes).unwrap_or_else(|e| panic!("{e}"));
        // Every optional field really is present in what was checked.
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for path in [
            "confirmed.age_secs",
            "admitting.label",
            "next.artifact_spec.sha256",
            "next.max_object_len",
        ] {
            assert!(paths(&v).iter().any(|p| p == path), "{path}");
        }
    }

    /// The page must be this build's template with only its slots filled:
    /// a page the publisher renders passes; the layout-v1 page (its score,
    /// credit and units columns) and a page with a v1 score in a slot are
    /// refused, so the key audit deletes one an earlier build left.
    #[test]
    fn the_page_must_be_this_builds_template_without_score_columns() {
        check_object(
            layout::SITE_INDEX,
            crate::leaderboard::render(None).as_bytes(),
        )
        .unwrap();
        let v1 = crate::leaderboard::TEMPLATE.replace(
            "<th scope=\"col\">Gate</th>",
            "<th scope=\"col\">Brier utility</th><th scope=\"col\">Gates</th>",
        );
        assert_ne!(v1, crate::leaderboard::TEMPLATE);
        assert!(
            check_object(layout::SITE_INDEX, v1.as_bytes()).is_err(),
            "the v1 column headers"
        );
        let filled = crate::leaderboard::TEMPLATE
            .replace(
                "<!--SUMMARY-->",
                "<dl><div><dt>Reference R<sub>0</sub> score</dt><dd>0.1</dd></div></dl>",
            )
            .replace("<!--ROWS-->", "");
        assert!(
            check_object(layout::SITE_INDEX, filled.as_bytes()).is_err(),
            "a v1 score in a slot"
        );
        assert!(check_object(layout::SITE_INDEX, b"<html>other</html>").is_err());
        // The audit asks for the page's content and deletes a v1 page.
        let keys = vec![
            layout::SITE_INDEX.to_owned(),
            "v1/artifacts/".to_owned() + &"cd".repeat(32) + ".bin",
        ];
        let plan = audit(&keys, |_| Some(v1.clone().into_bytes()));
        assert_eq!(plan.delete.len(), 1);
        assert_eq!(
            plan.kept_by_key.len(),
            1,
            "artifacts are kept by key, never read"
        );
        let plan = audit(&keys, |_| None);
        assert_eq!(
            plan.unchecked,
            vec![layout::SITE_INDEX.to_owned()],
            "an unread page is unchecked, never kept silently"
        );
    }

    #[test]
    fn keys_outside_the_allow_list_are_refused() {
        for key in [
            "v1/cohorts/testnet-c1/package.signed",
            "v1/cohorts/testnet-c1/manifest.signed",
            "v1/cohorts/testnet-c1/target.signed",
            "v1/cohorts/testnet-c1/closure.signed",
            "v1/cohorts/testnet-c1/receipt-index.signed",
            "v1/cohorts/testnet-c1/chunk-map.json",
            "v1/cohorts/testnet-c1/receipts/bundle-job-001.bin",
            "v1/cohorts/testnet-c1/components.json",
            "v1/cohorts/testnet-c1/closure-fingerprints.json",
            "v1/cohorts/testnet-c1/fingerprint-index.bin",
            "v1/cohorts/testnet-c1/fingerprint-index-after.bin",
            "v1/cohorts/testnet-c1/published-contents.bin",
            "v1/cohorts/testnet-c1/published-contents-after.bin",
            "v1/cohorts/../summary.json",
            "v1/artifacts/abc.bin",
            "v1/authorizations/current.json.bak",
            "v1/reference/packages/zz/x",
            "v1/records/testnet-c1/leaderboard.json",
            "cohorts/testnet-c1/summary.json",
            "notes.txt",
        ] {
            assert_eq!(classify(key), None, "{key}");
        }
        let h = "ab".repeat(32);
        assert_eq!(
            classify("v1/cohorts/testnet-c1/summary.json"),
            Some(Kind::Summary)
        );
        assert_eq!(
            classify(&format!("v1/artifacts/{h}.bin")),
            Some(Kind::Artifact)
        );
        assert_eq!(
            classify(&format!("v1/authorizations/{h}.signed")),
            Some(Kind::SignedObject)
        );
        assert_eq!(
            classify(&format!("v1/reference/packages/{h}/parts/{h}")),
            Some(Kind::ReferencePackage)
        );
        assert_eq!(
            classify(&format!("v1/reference/packages/{h}/NOTICE.md")),
            Some(Kind::ReferencePackage)
        );
        assert_eq!(
            classify(&format!(
                "v1/reference/packages/{h}/upstream/ModernBERT-base-LICENSE"
            )),
            Some(Kind::ReferencePackage)
        );
        assert_eq!(classify(&format!("v1/reference/packages/{h}/../x")), None);
        assert_eq!(
            classify("v1/records/testnet-c1/cohorts/index.json"),
            Some(Kind::ArchivedIndex)
        );
    }

    #[test]
    fn a_document_with_a_field_outside_its_list_is_refused() {
        let key = "v1/cohorts/testnet-c1/summary.json";
        let mut v = serde_json::json!({
            "schema": crate::summary::SUMMARY_SCHEMA, "network": "testnet",
            "chain": {"genesis": "00", "netuid": 601, "mechanism": 0},
            "cohort": 1, "label": "testnet-c1", "opened_epoch": 1, "closed_epoch": 21,
            "references": {"index": 0, "current": {"content": "a", "artifact": "b"}, "predecessor": null},
            "promoted": null,
            "entrants": [{"uid": 3, "hotkey": "5F", "hotkey_hex": "00", "rank": 1, "band": 1, "gate": "pass",
                          "copy_gate": "pass", "cluster": {"head": "5F", "head_uid": 3}}]
        });
        check_object(key, &serde_json::to_vec(&v).unwrap()).unwrap();
        v["entrants"][0]["score"] =
            serde_json::json!({"decimal": "0.846878", "raw": "846878585280111913"});
        let err = check_object(key, &serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert!(err.contains("entrants[].score"), "{err}");
        v["entrants"][0].as_object_mut().unwrap().remove("score");
        v["schema"] = "slop_ninja.public.cohort_summary/v0".into();
        assert!(check_object(key, &serde_json::to_vec(&v).unwrap()).is_err());
        assert!(check_object(key, b"not json").is_err());
        assert!(check_object("v1/trust/testnet-trust.json", b"{}").is_err());
        check_object("v1/trust/testnet-trust.json", crate::config::TRUST_FILE).unwrap();
        assert!(
            check_object(
                &format!("v1/authorizations/{}.signed", "ab".repeat(32)),
                b"junk"
            )
            .is_err()
        );
    }

    #[test]
    fn the_audit_plans_every_key_outside_the_allow_list_and_every_failed_document() {
        let keys: Vec<String> = [
            "v1/cohorts/testnet-r0-c/package.signed",
            "v1/cohorts/testnet-r0-c/receipts/bundle-job-000.bin",
            "v1/status.json",
            "v1/trust/testnet-trust.json",
            "v1/leaderboard.json",
        ]
        .iter()
        .map(|k| k.to_string())
        .collect();
        let old_board =
            serde_json::json!({"schema": "slop_ninja.public.leaderboard/v1", "entrants": []});
        let plan = audit(&keys, |key| match key {
            "v1/trust/testnet-trust.json" => Some(crate::config::TRUST_FILE.to_vec()),
            "v1/leaderboard.json" => Some(serde_json::to_vec(&old_board).unwrap()),
            _ => None,
        });
        assert_eq!(plan.keys, 5);
        assert_eq!(plan.keep, 2);
        assert_eq!(plan.unchecked, vec!["v1/status.json".to_owned()]);
        let deleted: Vec<&str> = plan.delete.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            deleted,
            vec![
                "v1/cohorts/testnet-r0-c/package.signed",
                "v1/cohorts/testnet-r0-c/receipts/bundle-job-000.bin",
                "v1/leaderboard.json"
            ]
        );
        assert_eq!(plan.delete[0].reason, "outside_allowlist");
    }
}
