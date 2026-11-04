//! The private receipts-bucket contract the coordinator and the publisher
//! share: the authorization pointer's key, kinds and read bounds, its
//! checks, the marker the coordinator writes before it deletes a cohort's
//! detailed receipts under the N+2 rule, and the name and read bound of its
//! abandonment record ([`ABANDONMENT_RECORD`]). Both crates import these
//! items, so the reader and the writer of each object cannot drift apart.
//!
//! The pointer type itself is [`crate::json::AuthorizationPointer`]
//! (schema [`crate::json::AUTHORIZATION_POINTER_SCHEMA`]). A pointer kind
//! added later must be added here ([`KIND_LAUNCH`], [`KIND_COHORT`]) and to
//! the coordinator's retention hold, which refuses an unknown kind (it then
//! stops deleting and reports the stall rather than guess what the
//! authorization names).

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::ChainBinding;

use crate::json::{AUTHORIZATION_POINTER_SCHEMA, AuthorizationPointer, Chain, ObjectRef};

/// The private pointer in the receipts bucket; its objects are
/// content-addressed under `authorizations/` ([`content_addressed`]).
pub const AUTHORIZATION_POINTER_KEY: &str = "authorizations/current.json";

/// A launch authorization: it names the launch burn row and no receipts.
pub const KIND_LAUNCH: &str = "launch";
/// A reward-intent authorization: it names its carrying cohort and target.
pub const KIND_COHORT: &str = "cohort";

/// Read bound of the pointer: its schema, chain, kind and its three object
/// references (authorization, renewal, revocation set; each a key under
/// `authorizations/`, a 64-hex digest and a length), pretty-printed, within
/// four fixed-record overheads. However many revocations exist, the pointer
/// names one set object, so this bound does not grow with them.
pub const AUTHORIZATION_POINTER_BOUND: u64 = 4 * 1024;
/// Read bound of one signed authorization, renewal or revocation record
/// (fixed fields only).
pub const AUTHORIZATION_OBJECT_BOUND: u64 = 1024;

/// The schema's fixed signed length of one revocation on `chain`: every
/// field is fixed-width, so any instance has it.
pub fn revocation_len(chain: &ChainBinding) -> u64 {
    slop_ninja_protocol::Signed {
        record: slop_ninja_protocol::RewardIntentRevocation {
            chain: chain.clone(),
            target: slop_ninja_protocol::Digest([0; 32]),
            authorization: slop_ninja_protocol::Digest([0; 32]),
            checkpoint_generation: 0,
            issued_epoch: 0,
        },
        public_key: [0; 32],
        signature: [0; 64],
    }
    .to_bytes()
    .map_or(0, |b| b.len() as u64)
}

/// Read bound of the revocation set the pointer names: its declared length,
/// which must be a non-zero whole multiple of [`revocation_len`]. The set's
/// bound follows from its entry count; the reader still reserves it from
/// its own budget.
pub fn revocation_set_bound(declared: u64, chain: &ChainBinding) -> Result<u64, &'static str> {
    let len = revocation_len(chain);
    if len == 0 || declared == 0 || !declared.is_multiple_of(len) {
        return Err("revocation_set");
    }
    Ok(declared)
}

/// The signed records of a revocation set, each [`revocation_len`] bytes.
pub fn revocation_set_entries<'a>(
    set: &'a [u8],
    chain: &ChainBinding,
) -> Result<core::slice::ChunksExact<'a, u8>, &'static str> {
    revocation_set_bound(set.len() as u64, chain)?;
    Ok(set.chunks_exact(revocation_len(chain) as usize))
}

/// Whether `r` names a content-addressed authorization object:
/// `authorizations/<sha256>.signed`.
pub fn content_addressed(r: &ObjectRef) -> bool {
    r.sha256.len() == 64
        && r.sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && r.path
            .strip_prefix("authorizations/")
            .and_then(|p| p.strip_suffix(".signed"))
            == Some(r.sha256.as_str())
}

/// Parses the private pointer and checks what any reader needs before it
/// trusts the object references: the known schema, this deployment's chain
/// binding, and content-addressed object keys. Signatures are the reader's
/// own business (the publisher opens them; the coordinator's retention hold
/// only keeps receipts, so it needs none).
pub fn parse_pointer(
    bytes: &[u8],
    chain: &ChainBinding,
) -> Result<AuthorizationPointer, &'static str> {
    let p: AuthorizationPointer =
        serde_json::from_slice(bytes).map_err(|_| "authorization_pointer")?;
    if p.schema != AUTHORIZATION_POINTER_SCHEMA {
        return Err("authorization_pointer");
    }
    if p.chain != Chain::from_binding(chain) {
        return Err("authorization_chain");
    }
    let refs = core::iter::once(&p.authorization)
        .chain(p.renewal.as_ref())
        .chain(p.revocations.as_ref());
    if !refs.into_iter().all(content_addressed) {
        return Err("authorization_pointer");
    }
    Ok(p)
}

/// The marker's file name in a private cohort directory
/// (`cohorts/<label>/receipts-retired.json`).
pub const RETIRED_MARKER: &str = "receipts-retired.json";
/// The marker's schema.
pub const RETIRED_MARKER_SCHEMA: &str = "slop_ninja.receipts_retired/v1";
/// Read bound of the marker (fixed fields only).
pub const RETIRED_MARKER_BOUND: u64 = 1024;

/// The file name, in a private cohort directory
/// (`cohorts/<label>/abandoned.signed`), of the coordinator's signed record
/// that it ended the cohort without a target, so no closure follows: the
/// protocol's signed `CohortAbandonment` record (package key; its chain,
/// cohort number and label, the manifest digest when sealed, content-free
/// fault and reason codes and the epoch it was abandoned in).
///
/// The coordinator writes it after an operator abandons a cohort, and the
/// publisher reads it within [`ABANDONMENT_RECORD_BOUND`] and verifies its
/// signature, label and chain before it treats the cohort as abandoned.
/// Both import this name. A cohort with a [`RETIRED_MARKER`] and no closure
/// is read the same way: the coordinator writes that marker only for a
/// closed cohort (after its closure) or an abandoned one.
pub const ABANDONMENT_RECORD: &str = "abandoned.signed";

/// Read bound of the abandonment record: its fixed fields (a chain binding,
/// a cohort number, a label of at most the protocol's 256-byte label bound,
/// one digest, two codes of at most 64 bytes and an epoch) and the signed
/// envelope come to well under a third of it.
pub const ABANDONMENT_RECORD_BOUND: u64 = 4 * 1024;

/// Written by the coordinator before the first of a cohort's detailed
/// receipts is deleted: content free, so the publisher can tell receipts
/// deleted before publication from receipts not stored yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredMarker {
    pub schema: String,
    pub label: String,
    pub cohort: u64,
    /// The coordinator's versioned retention rule.
    pub rule: String,
    /// The completion after which the receipts became due.
    pub due_after_cohort: u64,
}

impl RetiredMarker {
    /// Parses a marker and checks its schema and label.
    pub fn parse(bytes: &[u8], label: &str) -> Result<Self, &'static str> {
        let m: Self = serde_json::from_slice(bytes).map_err(|_| "retired_marker")?;
        if m.schema != RETIRED_MARKER_SCHEMA || m.label != label {
            return Err("retired_marker");
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::Digest;

    fn chain() -> ChainBinding {
        ChainBinding {
            genesis: Digest([7; 32]),
            netuid: 601,
            mechanism: 0,
        }
    }

    fn obj(c: char) -> ObjectRef {
        let sha: String = core::iter::repeat_n(c, 64).collect();
        ObjectRef {
            path: format!("authorizations/{sha}.signed"),
            sha256: sha,
            bytes: 10,
        }
    }

    fn pointer() -> AuthorizationPointer {
        AuthorizationPointer {
            schema: AUTHORIZATION_POINTER_SCHEMA.into(),
            chain: Chain::from_binding(&chain()),
            kind: KIND_COHORT.into(),
            authorization: obj('a'),
            renewal: None,
            revocations: Some(obj('b')),
        }
    }

    #[test]
    fn a_pointer_is_checked_for_schema_chain_and_object_keys() {
        let ok = serde_json::to_vec(&pointer()).unwrap();
        assert_eq!(parse_pointer(&ok, &chain()).unwrap(), pointer());
        let other = ChainBinding {
            netuid: 602,
            ..chain()
        };
        assert_eq!(parse_pointer(&ok, &other), Err("authorization_chain"));
        let mut p = pointer();
        p.schema = "slop_ninja.public.authorization/v1".into();
        assert_eq!(
            parse_pointer(&serde_json::to_vec(&p).unwrap(), &chain()),
            Err("authorization_pointer")
        );
        let mut p = pointer();
        p.revocations.as_mut().unwrap().path = "authorizations/elsewhere.signed".into();
        assert_eq!(
            parse_pointer(&serde_json::to_vec(&p).unwrap(), &chain()),
            Err("authorization_pointer")
        );
        let mut v = serde_json::to_value(pointer()).unwrap();
        v["extra"] = 1.into();
        assert_eq!(
            parse_pointer(&serde_json::to_vec(&v).unwrap(), &chain()),
            Err("authorization_pointer")
        );
        assert!(
            serde_json::to_vec_pretty(&pointer()).unwrap().len() as u64
                <= AUTHORIZATION_POINTER_BOUND
        );
    }

    #[test]
    fn a_revocation_set_is_whole_fixed_length_records() {
        let len = revocation_len(&chain());
        assert!(len > 0 && len <= AUTHORIZATION_OBJECT_BOUND);
        assert_eq!(revocation_set_bound(3 * len, &chain()), Ok(3 * len));
        assert_eq!(revocation_set_bound(0, &chain()), Err("revocation_set"));
        assert_eq!(revocation_set_bound(3 * len + 1, &chain()), Err("revocation_set"));
        let set = vec![0u8; 2 * len as usize];
        assert_eq!(revocation_set_entries(&set, &chain()).unwrap().count(), 2);
        assert!(revocation_set_entries(&set[1..], &chain()).is_err());
    }

    #[test]
    fn a_marker_names_its_own_label() {
        let m = RetiredMarker {
            schema: RETIRED_MARKER_SCHEMA.into(),
            label: "r0-c3".into(),
            cohort: 3,
            rule: "rule".into(),
            due_after_cohort: 5,
        };
        let b = serde_json::to_vec(&m).unwrap();
        assert_eq!(RetiredMarker::parse(&b, "r0-c3").unwrap(), m);
        assert_eq!(RetiredMarker::parse(&b, "r0-c4"), Err("retired_marker"));
        assert!(b.len() as u64 <= RETIRED_MARKER_BOUND);
    }
}
