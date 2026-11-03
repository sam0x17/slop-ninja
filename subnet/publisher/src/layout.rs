//! Every key the publisher reads or writes, in one place. The public layout is
//! documented in `docs/testnet/public-layout.md`; this module is its source.
//!
//! Private keys are the coordinator's (`cohorts/<label>/...` in the receipts
//! bucket). Public keys live under `v1/` in the public-candidate bucket, plus
//! the static site files at its root.

use slop_ninja_protocol::Digest;

/// Prefix of every cohort in the private receipts bucket.
pub const PRIVATE_COHORTS: &str = "cohorts/";

pub const STATUS: &str = "v1/status.json";
pub const COHORT_INDEX: &str = "v1/cohorts/index.json";
pub const LEADERBOARD: &str = "v1/leaderboard.json";
pub const TRUST: &str = "v1/trust/testnet-trust.json";
pub const REFERENCE_CURRENT: &str = "v1/reference/current.json";
pub const REFERENCE_PREVIOUS: &str = "v1/reference/previous.json";
pub const AUTHORIZATION_CURRENT: &str = "v1/authorizations/current.json";
/// The coordinator's private pointer to the current authorization and its
/// renewal, in the receipts bucket.
pub const PRIVATE_AUTHORIZATION_POINTER: &str =
    slop_ninja_admission_wire::receipts::AUTHORIZATION_POINTER_KEY;
pub const SITE_INDEX: &str = "index.html";
/// Keys per listing page: R2's `list` returns at most 1,000.
pub const LIST_PAGE: usize = 1_000;
pub const SITE_STYLE: &str = "style.css";

/// Where a previous public record's index, leaderboard and reference
/// pointers are moved when a new record starts (a chain-record restart):
/// under its first listed cohort's label, unique across authorities.
pub fn record_archive(first_label: &str, rel: &str) -> String {
    format!("v1/records/{first_label}/{rel}")
}

/// The coordinator's private directory of one cohort (no trailing slash).
pub fn private_cohort_dir(label: &str) -> String {
    format!("cohorts/{label}")
}

/// The public directory of one cohort (no trailing slash).
pub fn cohort_dir(label: &str) -> String {
    format!("v1/cohorts/{label}")
}

/// `dir/rel`.
pub fn join(dir: &str, rel: &str) -> String {
    format!("{dir}/{rel}")
}

/// The publisher's own record of one published cohort.
pub fn publication(label: &str) -> String {
    format!("v1/cohorts/{label}/publication.json")
}

/// A published artifact object, addressed by the SHA-256 the score package
/// binds (`EntrantScore::artifact`).
pub fn artifact(sha256: &Digest) -> String {
    format!("v1/artifacts/{}.bin", sha256.hex())
}

/// A staged reference package's public directory (trailing slash), by the
/// SHA-256 of its parts manifest: the parts manifest, its parts and its
/// licence notices, staged by the operator. The publisher never writes under
/// it; it only checks the parts manifest ([`reference_package_manifest`]).
pub fn reference_package(sha256: &Digest) -> String {
    format!("v1/reference/packages/{}/", sha256.hex())
}

/// The parts manifest of a staged reference package, whose SHA-256 is the
/// directory's digest.
pub fn reference_package_manifest(sha256: &Digest) -> String {
    format!("{}artifact-files.json", reference_package(sha256))
}

/// Largest parts manifest a staged reference package can have: the artifact
/// spec's `MAX_MANIFEST_BYTES`, the scorer's manifest limit
/// (`subnet/crates/artifact-check/src/spec.rs`; a test checks they agree).
pub const REFERENCE_PACKAGE_MANIFEST_BOUND: u64 = 1 << 20;

/// The licence of Slop Ninja's reference packages, as their pointers name it.
pub const REFERENCE_PACKAGE_LICENCE: &str = "CC-BY-SA-4.0";

/// A signed authorization or renewal in the receipts bucket, by SHA-256.
pub fn private_authorization(sha256: &Digest) -> String {
    format!("authorizations/{}.signed", sha256.hex())
}

/// A published authorization or renewal, by SHA-256.
pub fn authorization(sha256: &Digest) -> String {
    format!("v1/authorizations/{}.signed", sha256.hex())
}

/// A cohort label the publisher accepts as a key segment: the protocol's label
/// alphabet, never `.` or `..`.
pub fn safe_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= slop_ninja_protocol::MAX_STRING_LEN as usize
        && label != "."
        && label != ".."
        && label.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// A relative object path: safe segments separated by `/`.
pub fn safe_rel(rel: &str) -> bool {
    !rel.is_empty() && rel.split('/').all(safe_label)
}

/// The path of a private cohort object relative to its cohort directory, when
/// `key` is a safe key inside it.
pub fn rel_of_private_key<'a>(label: &str, key: &'a str) -> Option<&'a str> {
    let rel = key
        .strip_prefix(PRIVATE_COHORTS)?
        .strip_prefix(label)?
        .strip_prefix('/')?;
    safe_rel(rel).then_some(rel)
}

/// Content type of a published object, by extension.
pub fn content_type(key: &str) -> &'static str {
    match key.rsplit('.').next() {
        Some("json") => "application/json",
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// How long a public cache may keep an object. Signed cohort objects,
/// artifacts and authorizations never change once published; detailed
/// receipts are deleted after N+2, so caches keep them a bounded time; the
/// snapshot and the index documents are rewritten in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    Immutable,
    /// At most this many seconds, never `immutable` (one tempo for detailed
    /// receipts, so a deleted bundle leaves caches within a tempo).
    Bounded(u64),
    Snapshot,
    Index,
}

impl Cache {
    pub fn header(self) -> String {
        match self {
            Self::Immutable => "public, max-age=31536000, immutable".into(),
            Self::Bounded(secs) => format!("public, max-age={secs}"),
            // One cron period: a reader is at most one run behind.
            Self::Snapshot | Self::Index => "public, max-age=60".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_paths() {
        assert!(safe_label("testnet-c7"));
        assert!(safe_label("synthetic-reward-v1-3"));
        for bad in ["", ".", "..", "A", "a/b", "a b", "a%2f"] {
            assert!(!safe_label(bad), "{bad:?}");
        }
        assert!(safe_rel("receipts/bundle-job-001.bin"));
        assert!(!safe_rel("receipts/../x"));
        assert!(!safe_rel("/x"));
        assert!(!safe_rel("x/"));
        assert_eq!(
            rel_of_private_key("c1", "cohorts/c1/receipts/bundle-job-001.bin"),
            Some("receipts/bundle-job-001.bin")
        );
        assert_eq!(rel_of_private_key("c1", "cohorts/c10/x.bin"), None);
        assert_eq!(rel_of_private_key("c1", "cohorts/c1/../c2/x.bin"), None);
        assert_eq!(rel_of_private_key("c1", "pools/c1/x.bin"), None);
    }
}
