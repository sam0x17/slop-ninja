//! The upload Worker's own decisions that are not the shared front gate
//! ([`crate::gate`]): the cohort read's edge-cache key and the snapshot it
//! serves, the header route's admission and answer, how an interface answer
//! maps to the client's answer, and the static contract bodies of answers
//! given before any Rust allocation. Storage-neutral and natively tested;
//! the Worker (`cf`) calls them, and the arena witnesses
//! (`arena_witness`) run them at their bounds.

use slop_ninja_admission_wire::internal::{self as iface, CohortSnapshot};
use slop_ninja_admission_wire::path;
use slop_ninja_artifact_check::admission::{SpecBounds, check_header_bytes};
use slop_ninja_artifact_check::spec::Version;

/// The chain's nominal block time (12 s, the coordinator's
/// `chain::BLOCK_TIME_SECS`): the cohort snapshot changes about once per
/// finalized block, and the finalized head (with its roster) the Worker
/// holds serves one block.
pub const BLOCK_TIME_SECS: u64 = 12;

/// The edge-cache key of the unsigned cohort read: the request's origin and
/// the fixed path, with no query or fragment, so every variant of the URL
/// shares one cached copy of the snapshot and the read reaches the eval
/// Worker's snapshot operation about once a block per edge location
/// (concurrent misses are not coalesced). `None` for a URL with no scheme
/// and host.
pub fn cohort_cache_key(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest.get(..end)?;
    if scheme.is_empty() || host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{host}{}", path::COHORT))
}

/// The cohort response to serve at `now_ms` from a snapshot's exact bytes
/// (the eval Worker's [`iface::op::SNAPSHOT`] answer, or the edge copy of
/// it), as JSON: `None` when the bytes are over
/// [`iface::cohort_snapshot_limit`], do not parse, or name another schema
/// (a snapshot that fails here is never cached). `paused` once the snapshot
/// is older than the coordinator's heartbeat freshness
/// ([`CohortSnapshot::serve`]).
pub fn serve_snapshot(raw: &[u8], now_ms: u64) -> Option<Vec<u8>> {
    if raw.len() > iface::cohort_snapshot_limit() {
        return None;
    }
    let snap: CohortSnapshot = serde_json::from_slice(raw).ok()?;
    if snap.schema != iface::COHORT_SNAPSHOT_SCHEMA {
        return None;
    }
    serde_json::to_vec(&snap.serve(now_ms)).ok()
}

/// Whether the snapshot's repair list lets `who` read its session or
/// repair without being registered ([`CohortSnapshot::may_repair`]), from
/// the snapshot's exact bytes (the edge copy, or the eval Worker's
/// answer): `None` when the bytes do not serve as a snapshot
/// ([`serve_snapshot`]'s checks). A stale snapshot's list still answers: it
/// only ever lists more than the authority would.
pub fn repair_listed(raw: &[u8], who: &slop_ninja_protocol::AccountId) -> Option<bool> {
    if raw.len() > iface::cohort_snapshot_limit() {
        return None;
    }
    let snap: CohortSnapshot = serde_json::from_slice(raw).ok()?;
    (snap.schema == iface::COHORT_SNAPSHOT_SCHEMA).then(|| snap.may_repair(who))
}

/// Whether a request to the header route is the eval Worker's: `POST` to
/// the internal host with the configured interface token. The JS route
/// classifier gives the header footprint only to requests for that path,
/// host and method; this check adds the token.
pub fn header_admitted(host: Option<&str>, method: &str, configured: Option<&str>, presented: Option<&str>) -> bool {
    host == Some(iface::UPLOAD_INTERFACE_HOST) && method == "POST" && iface::token_matches(configured, presented)
}

/// The value of [`iface::HEADER_FAILURE_HEADER`] on an answer that failed
/// for this Worker's platform's reason (its arena refused the scope, the
/// body could not be read): the eval Worker retries it without counting it
/// against the miner.
pub const HEADER_FAILURE_PLATFORM: &str = "platform";

/// The header route's answer to a request whose body was read: status and
/// JSON body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderAnswer {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The body of the probe answer.
pub const PROBE_OK: &[u8] = br#"{"probe":"ok"}"#;

/// `POST /internal/upload/v2/header` once admitted and its body read (at
/// most [`iface::HEADER_REQUEST_BYTES`]): the link probe (marked by
/// [`iface::HEADER_PROBE_HEADER`], never by its body) is answered before
/// any body is read (`cf.rs`, [`PROBE_OK`]); a spec id that is not an
/// admission spec answers 400; anything else is decoded and checked by
/// [`check_header_bytes`] (which refuses a length outside the protocol's
/// bound and a declared file count above the spec's table before it decodes)
/// and answered 200 with the `HeaderOutcome`, whatever its verdict. No byte
/// is hashed here: the outcome's content digest is a domain hash over the
/// decoded record, recomputed by the eval Worker from the returned table.
pub fn header_check(spec: Option<&str>, body: &[u8]) -> HeaderAnswer {
    let Some(version) = spec.map(str::trim).and_then(Version::from_id).filter(|v| SpecBounds::of(*v).is_some()) else {
        return HeaderAnswer { status: 400, body: br#"{"error":"unknown_spec"}"#.to_vec() };
    };
    match serde_json::to_vec(&check_header_bytes(body, version)) {
        Ok(b) => HeaderAnswer { status: 200, body: b },
        Err(_) => HeaderAnswer { status: 500, body: br#"{"error":"encode"}"#.to_vec() },
    }
}

/// The room of a [`iface::Reply`]'s JSON beside its body: `{"status":<u16>,
/// "body":` and `,"retry_after":<u64>}` at their widest (tested exact).
pub const REPLY_ENVELOPE_BYTES: usize =
    r#"{"status":"#.len() + 5 + r#","body":"#.len() + r#","retry_after":"#.len() + 20 + "}".len();

/// The bound an interface reply is read within: a client response at the
/// contract's [`slop_ninja_admission_wire::response_limit`] and the
/// envelope.
pub fn reply_limit() -> usize {
    slop_ninja_admission_wire::response_limit() + REPLY_ENVELOPE_BYTES
}

/// A cohort authority reply as the client receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relayed {
    pub status: u16,
    pub body: Vec<u8>,
    pub retry_after: Option<u64>,
}

/// A [`iface::Reply`] as read for relaying: its body borrowed as raw JSON from the
/// reply's bytes, never parsed into a tree, so relaying holds one copy of a
/// body of at most the contract's response limit.
#[derive(serde::Deserialize)]
struct RawReply<'a> {
    status: u16,
    #[serde(borrow)]
    body: &'a serde_json::value::RawValue,
    #[serde(default)]
    retry_after: Option<u64>,
}

/// The client's answer from an interface reply's exact bytes: `None` when
/// they are over [`reply_limit`] or not a [`iface::Reply`] (the client then gets
/// `unavailable`). The body is passed on byte for byte.
pub fn relay(raw: &[u8]) -> Option<Relayed> {
    if raw.len() > reply_limit() {
        return None;
    }
    let r: RawReply<'_> = serde_json::from_slice(raw).ok()?;
    Some(Relayed { status: r.status, body: r.body.get().as_bytes().to_vec(), retry_after: r.retry_after })
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
    r#"{"schema":"slop_ninja.admission.error/v2","error":{"code":"unavailable","#,
    r#""message":"the upload Worker is busy or failed; try again","retryable":true}}"#
);
/// The `Retry-After` of [`UNAVAILABLE_BODY`] (seconds): the eval Worker's
/// own retry wait for a store refusal.
pub const UNAVAILABLE_RETRY_SECS: &str = "2";
/// The contract body of a 404 (no such route on this Worker).
pub const NOT_FOUND_BODY: &str = concat!(
    r#"{"schema":"slop_ninja.admission.error/v2","error":{"code":"not_found","#,
    r#""message":"no such route","retryable":false}}"#
);

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use slop_ninja_admission_wire::ErrorCode;
    use slop_ninja_admission_wire::json::{ErrorBody, SCHEMA_ERROR};
    use slop_ninja_admission_wire::internal::Reply;
    use slop_ninja_artifact_check::admission::{HeaderOutcome, HeaderResult};

    #[test]
    fn cohort_reads_share_one_cache_key_whatever_the_query() {
        let want = "https://testnet-admission.slop.ninja/admission/v2/cohort";
        for url in [
            "https://testnet-admission.slop.ninja/admission/v2/cohort",
            "https://testnet-admission.slop.ninja/admission/v2/cohort?x=1",
            "https://testnet-admission.slop.ninja/admission/v2/cohort?x=2&y=3",
            "https://testnet-admission.slop.ninja/admission/v2/cohort#f",
            "https://testnet-admission.slop.ninja?q",
        ] {
            assert_eq!(cohort_cache_key(url).as_deref(), Some(want), "{url}");
        }
        assert_eq!(
            cohort_cache_key("https://other.example/admission/v2/cohort?x").as_deref(),
            Some("https://other.example/admission/v2/cohort")
        );
        for bad in ["", "no-scheme", "https://", "://host", "https://u@h/x"] {
            assert_eq!(cohort_cache_key(bad), None, "{bad}");
        }
    }

    pub(crate) fn snapshot(written_ms: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": iface::COHORT_SNAPSHOT_SCHEMA, "written_ms": written_ms,
            "cohort": {
                "schema": slop_ninja_admission_wire::json::SCHEMA_COHORT, "contract": slop_ninja_admission_wire::CONTRACT,
                "chain": {"genesis": "00", "netuid": 601, "mechanism": 0},
                "finalized_block": 9, "request_validity_blocks": 360, "paused": false,
                "admitting": null, "next": null
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_snapshot_serves_its_cohort_response_and_pauses_when_stale() {
        let raw = snapshot(1_000);
        let fresh: serde_json::Value = serde_json::from_slice(&serve_snapshot(&raw, 1_000).unwrap()).unwrap();
        assert_eq!(fresh["paused"], false);
        assert_eq!(fresh["finalized_block"], 9);
        let stale = serve_snapshot(&raw, 1_000 + iface::COHORT_SNAPSHOT_FRESH_MS).unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&stale).unwrap()["paused"], true);
        // Another schema, a non-snapshot and an over-bound body are refused.
        let other = String::from_utf8(raw.clone()).unwrap().replace(iface::COHORT_SNAPSHOT_SCHEMA, "x/v9");
        assert_eq!(serve_snapshot(other.as_bytes(), 1_000), None);
        assert_eq!(serve_snapshot(b"{}", 1_000), None);
        let mut long = raw.clone();
        long.resize(iface::cohort_snapshot_limit() + 1, b' ');
        assert_eq!(serve_snapshot(&long, 1_000), None);
        // The repair list is the gate's, never the client's.
        let who = slop_ninja_protocol::AccountId([0xab; 32]);
        assert_eq!(repair_listed(&raw, &who), Some(false));
        let mut v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        v["repair"] = serde_json::json!([hex::encode(who.0)]);
        let listed = serde_json::to_vec(&v).unwrap();
        assert_eq!(repair_listed(&listed, &who), Some(true));
        assert_eq!(repair_listed(&listed, &slop_ninja_protocol::AccountId([1; 32])), Some(false));
        assert!(!String::from_utf8(serve_snapshot(&listed, 1_000).unwrap()).unwrap().contains("repair"));
        assert_eq!(repair_listed(other.as_bytes(), &who), None);
        assert_eq!(repair_listed(&long, &who), None);
    }

    #[test]
    fn only_the_eval_workers_post_with_the_token_reaches_the_header_route() {
        let t = "0123456789abcdef0123456789abcdef";
        let host = Some(iface::UPLOAD_INTERFACE_HOST);
        assert!(header_admitted(host, "POST", Some(t), Some(t)));
        assert!(!header_admitted(Some("testnet-admission.slop.ninja"), "POST", Some(t), Some(t)));
        assert!(!header_admitted(None, "POST", Some(t), Some(t)));
        assert!(!header_admitted(host, "GET", Some(t), Some(t)));
        assert!(!header_admitted(host, "POST", Some(t), None));
        assert!(!header_admitted(host, "POST", None, Some(t)));
        assert!(!header_admitted(host, "POST", Some(t), Some("0123456789abcdef0123456789abcdeX")));
    }

    #[test]
    fn the_js_classifier_gives_the_header_footprint_only_to_the_internal_host() {
        // The route class is read in JS before any Rust runs (`cf.rs`): it
        // must name the same paths and host as the contract and interface
        // constants.
        let src = include_str!("cf.rs");
        let rule = format!(
            "if (p === '{}' && u.host === '{}' && req.method === 'POST') return 2;",
            iface::UPLOAD_HEADER_PATH,
            iface::UPLOAD_INTERFACE_HOST
        );
        assert!(src.contains(&rule), "{rule}");
        assert_eq!(src.matches("return 2;").count(), 1);
        let public = path::COHORT.strip_suffix("cohort").unwrap();
        assert!(src.contains(&format!("if (p.startsWith('{public}')) return 1;")), "{public}");
    }

    #[test]
    fn the_worker_holds_no_object_store() {
        // No upload byte and no object passes through this Worker: no R2
        // binding is configured or used (the contract's rule).
        let toml = include_str!("../wrangler.toml");
        assert!(!toml.contains("r2_buckets"), "wrangler.toml binds a bucket");
        let src = include_str!("cf.rs");
        for banned in ["env.bucket(", ".bucket(", "uploadPart", "resumeMultipartUpload", "FixedLengthStream", "Framer"] {
            assert!(!src.contains(banned), "cf.rs uses {banned}");
        }
    }

    #[test]
    fn every_admission_spec_id_fits_the_spec_header() {
        for v in Version::ALL.into_iter().filter(|v| SpecBounds::of(*v).is_some()) {
            assert!(v.id().len() <= iface::SPEC_ID_MAX_BYTES, "{}", v.id());
        }
    }

    /// A spec v3 header that passes every header-only check: the spec's six
    /// names and roles, a served graph class's exact length, the pinned
    /// tokenizer, small role files (their digests are placeholders: no file
    /// byte is read here).
    pub(crate) fn checked_header() -> Vec<u8> {
        use slop_ninja_artifact_check::spec;
        use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, Record};
        let graph = spec::GRAPHS_V3.iter().find(|c| c.served).unwrap();
        let mut files: Vec<ArtifactFile> = spec::ROLES
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (sha256, len) = match r.file_role {
                    slop_ninja_protocol::FileRole::Graph => (Digest([1; 32]), graph.bytes),
                    slop_ninja_protocol::FileRole::Tokenizer => (
                        slop_ninja_artifact_check::admission::parse_digest(spec::TOKENIZER_SHA256).unwrap(),
                        spec::TOKENIZER_BYTES,
                    ),
                    _ => (Digest([2 + i as u8; 32]), 64),
                };
                ArtifactFile { name: r.name.into(), role: r.file_role, sha256, len }
            })
            .collect();
        files.sort_by(|a, b| a.name.cmp(&b.name));
        ArtifactHeader { contract: spec::CONTRACT_ID.into(), files }.to_bytes().unwrap()
    }

    #[test]
    fn the_header_route_answers_probe_spec_and_outcome() {
        // A body equal to any constant is a header like any other: the
        // probe is marked out of band (review finding: a 5-byte header
        // "probe" was once answered as the probe).
        let spec3 = Some(slop_ninja_artifact_check::spec::SPEC_ID_V3);
        let o: HeaderOutcome = serde_json::from_slice(&header_check(spec3, b"probe").body).unwrap();
        assert!(matches!(o.result, HeaderResult::Invalid { .. }), "{o:?}");
        assert_eq!(header_check(Some("slop_ninja_a_artifact_spec_v1"), b"x").status, 400, "v1 is not an admission spec");
        assert_eq!(header_check(None, b"x").status, 400);
        assert_eq!(header_check(Some("nope"), b"x").status, 400);
        let spec = Some(slop_ninja_artifact_check::spec::SPEC_ID_V3);
        let bound = slop_ninja_artifact_check::admission::header_outcome_bound();
        for body in [&b""[..], b"x", &vec![0u8; iface::HEADER_REQUEST_BYTES + 1]] {
            let a = header_check(spec, body);
            assert_eq!(a.status, 200);
            let o: HeaderOutcome = serde_json::from_slice(&a.body).unwrap();
            assert!(matches!(o.result, HeaderResult::Invalid { .. }), "{o:?}");
            assert!(a.body.len() <= bound);
        }
        let h = checked_header();
        let a = header_check(spec, &h);
        let o: HeaderOutcome = serde_json::from_slice(&a.body).unwrap();
        let HeaderResult::Checked { contract, content, files } = o.result else { panic!("{o:?}") };
        let (_, again) = slop_ninja_artifact_check::admission::rebuild_header(&contract, &files).unwrap();
        assert_eq!(again.hex(), content, "the eval side recomputes the same content");
        assert!(a.body.len() <= bound);
        // The spec header is read trimmed, and the same header under spec v2.
        assert_eq!(header_check(Some(" slop_ninja_a_artifact_spec_v3 "), &h), a);
        assert_eq!(header_check(Some(slop_ninja_artifact_check::spec::SPEC_ID_V2), &h).status, 200);
    }

    #[test]
    fn replies_relay_within_their_bound() {
        let max = Reply::new(u16::MAX, serde_json::Value::Null, Some(u64::MAX));
        let empty = serde_json::to_string(&max).unwrap().len() - "null".len();
        assert_eq!(empty, REPLY_ENVELOPE_BYTES);
        let r = Reply::new(409, serde_json::json!({"a": 1}), Some(2));
        let got = relay(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(got, Relayed { status: 409, body: br#"{"a":1}"#.to_vec(), retry_after: Some(2) });
        // A reply whose body is at the client's limit fits; one byte more does not.
        let pad = "x".repeat(slop_ninja_admission_wire::response_limit() - 2);
        let at = Reply::new(200, serde_json::Value::String(pad.clone()), Some(u64::MAX));
        let raw = serde_json::to_vec(&Reply { status: u16::MAX, ..at }).unwrap();
        assert_eq!(raw.len(), reply_limit());
        assert!(relay(&raw).is_some());
        let over = Reply::new(u16::MAX, serde_json::Value::String(pad + "x"), Some(u64::MAX));
        assert_eq!(relay(&serde_json::to_vec(&over).unwrap()), None);
        assert_eq!(relay(b"{}"), None);
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
