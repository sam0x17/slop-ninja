//! The upload Worker's own decisions that are not the shared front gate
//! ([`crate::gate`]): the cohort read's edge-cache key, the framing route's
//! admission, the check of a part target the eval Worker issued, how an
//! interface answer maps to the client's answer, and the static contract
//! bodies of answers given before any Rust allocation. Storage-neutral and
//! natively tested; the Worker (`cf`) calls them.

use slop_ninja_admission_wire::internal::{self as iface, PartTarget};
use slop_ninja_admission_wire::path;

/// The chain's nominal block time (12 s, the coordinator's
/// `chain::BLOCK_TIME_SECS`): the cohort snapshot changes about once per
/// finalized block, and the finalized head (with its roster) the Worker
/// holds serves one block.
pub const BLOCK_TIME_SECS: u64 = 12;

/// The edge-cache key of the unsigned cohort read: the request's origin and
/// the fixed path, with no query or fragment, so every variant of the URL
/// shares one cached copy of the snapshot and the read reaches the upload
/// bucket about once a block per edge location (concurrent misses are not
/// coalesced). `None` for a URL with no scheme and host.
pub fn cohort_cache_key(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest.get(..end)?;
    if scheme.is_empty() || host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{host}{}", path::COHORT))
}

/// Whether a request to the framing route is the eval Worker's: `POST` to
/// the internal host with the configured interface token. The JS route
/// classifier gives the framing footprint only to requests for that host
/// and method; this check adds the token.
pub fn frame_admitted(host: Option<&str>, method: &str, configured: Option<&str>, presented: Option<&str>) -> bool {
    host == Some(iface::UPLOAD_INTERFACE_HOST) && method == "POST" && iface::token_matches(configured, presented)
}

/// Whether the part target the cohort authority issued is the part the
/// client declared: the same part number and length, and an uploaded
/// object's key. (The authority checked the length against its plan; this
/// guards the stream against an interface defect.)
pub fn target_matches(t: &PartTarget, part: u32, declared: u64) -> bool {
    t.len == declared && t.part == part && iface::is_object_key(&t.key) && !t.upload_id.is_empty()
}

/// How the client is answered when the eval Worker's interface answered a
/// status other than 200.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceFailure {
    /// 400 or 413: the forwarded message itself was refused (malformed or
    /// over the interface bound). Retrying the same request cannot succeed:
    /// `bad_request`.
    Refused,
    /// Anything else (the cohort object unavailable, the interface not
    /// configured or its token not matching, a platform failure): the
    /// operator's or the platform's, so the client retries (`unavailable`).
    Unavailable,
}

impl InterfaceFailure {
    pub fn of(status: u16) -> Self {
        match status {
            400 | 413 => Self::Refused,
            _ => Self::Unavailable,
        }
    }
}

/// The contract body of an answer given before the handler runs or when
/// it fails (an arena refusal, an internal error): `unavailable`, which
/// the client retries after `Retry-After`. Static, so it needs no Rust
/// allocation.
pub const UNAVAILABLE_BODY: &str = concat!(
    r#"{"schema":"slop_ninja.admission.error/v1","error":{"code":"unavailable","#,
    r#""message":"the upload Worker is busy or failed; try again","retryable":true}}"#
);
/// The `Retry-After` of [`UNAVAILABLE_BODY`] (seconds): the eval Worker's
/// own retry wait for a store refusal.
pub const UNAVAILABLE_RETRY_SECS: &str = "2";
/// The contract body of a 404 (no such route on this Worker).
pub const NOT_FOUND_BODY: &str = concat!(
    r#"{"schema":"slop_ninja.admission.error/v1","error":{"code":"not_found","#,
    r#""message":"no such route","retryable":false}}"#
);

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_admission_wire::ErrorCode;
    use slop_ninja_admission_wire::json::{ErrorBody, SCHEMA_ERROR};

    #[test]
    fn cohort_reads_share_one_cache_key_whatever_the_query() {
        let want = "https://testnet-admission.slop.ninja/admission/v1/cohort";
        for url in [
            "https://testnet-admission.slop.ninja/admission/v1/cohort",
            "https://testnet-admission.slop.ninja/admission/v1/cohort?x=1",
            "https://testnet-admission.slop.ninja/admission/v1/cohort?x=2&y=3",
            "https://testnet-admission.slop.ninja/admission/v1/cohort#f",
            "https://testnet-admission.slop.ninja?q",
        ] {
            assert_eq!(cohort_cache_key(url).as_deref(), Some(want), "{url}");
        }
        assert_eq!(
            cohort_cache_key("https://other.example/admission/v1/cohort?x").as_deref(),
            Some("https://other.example/admission/v1/cohort")
        );
        for bad in ["", "no-scheme", "https://", "://host", "https://u@h/x"] {
            assert_eq!(cohort_cache_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_the_eval_workers_post_with_the_token_reaches_framing() {
        let t = "0123456789abcdef0123456789abcdef";
        let host = Some(iface::UPLOAD_INTERFACE_HOST);
        assert!(frame_admitted(host, "POST", Some(t), Some(t)));
        assert!(!frame_admitted(Some("testnet-admission.slop.ninja"), "POST", Some(t), Some(t)));
        assert!(!frame_admitted(None, "POST", Some(t), Some(t)));
        assert!(!frame_admitted(host, "GET", Some(t), Some(t)));
        assert!(!frame_admitted(host, "POST", Some(t), None));
        assert!(!frame_admitted(host, "POST", None, Some(t)));
        assert!(!frame_admitted(host, "POST", Some(t), Some("0123456789abcdef0123456789abcdeX")));
    }

    #[test]
    fn the_js_classifier_gives_framing_only_to_the_internal_host() {
        // The route class is read in JS before any Rust runs (`cf.rs`): it
        // must name the same host and path as the interface constants.
        let src = include_str!("cf.rs");
        let rule = format!(
            "if (p === '{}' && u.host === '{}' && req.method === 'POST') return 2;",
            iface::UPLOAD_FRAME_PATH,
            iface::UPLOAD_INTERFACE_HOST
        );
        assert!(src.contains(&rule), "{rule}");
        assert_eq!(src.matches("return 2;").count(), 1);
    }

    #[test]
    fn every_spec_id_fits_the_frame_request_bound() {
        for v in slop_ninja_artifact_check::spec::Version::ALL {
            assert!(v.id().len() <= iface::SPEC_ID_MAX_BYTES, "{}", v.id());
        }
    }

    #[test]
    fn a_target_must_be_the_declared_part() {
        let t = PartTarget {
            key: "admission/testnet-r0-19/ab/a1.object".into(),
            upload_id: "u".into(),
            attempt: 1,
            part: 3,
            len: 64,
            sha256: "00".repeat(32),
        };
        assert!(target_matches(&t, 3, 64));
        assert!(!target_matches(&t, 4, 64));
        assert!(!target_matches(&t, 3, 63));
        let other = PartTarget { key: "artifacts/blobs/00".into(), ..t.clone() };
        assert!(!target_matches(&other, 3, 64));
        let no_upload = PartTarget { upload_id: String::new(), ..t };
        assert!(!target_matches(&no_upload, 3, 64));
    }

    #[test]
    fn interface_refusals_of_the_message_are_final_others_retry() {
        assert_eq!(InterfaceFailure::of(400), InterfaceFailure::Refused);
        assert_eq!(InterfaceFailure::of(413), InterfaceFailure::Refused);
        for s in [404, 500, 502, 503] {
            assert_eq!(InterfaceFailure::of(s), InterfaceFailure::Unavailable, "{s}");
        }
    }

    #[test]
    fn static_answers_are_contract_error_bodies() {
        let b: ErrorBody = serde_json::from_str(UNAVAILABLE_BODY).unwrap();
        assert_eq!(b.schema, SCHEMA_ERROR);
        assert_eq!(ErrorCode::parse(&b.error.code), Some(ErrorCode::Unavailable));
        assert!(b.error.retryable && ErrorCode::Unavailable.retryable());
        assert!(UNAVAILABLE_RETRY_SECS.parse::<u64>().is_ok());
        let n: ErrorBody = serde_json::from_str(NOT_FOUND_BODY).unwrap();
        assert_eq!(ErrorCode::parse(&n.error.code), Some(ErrorCode::NotFound));
        assert!(!n.error.retryable);
    }
}
