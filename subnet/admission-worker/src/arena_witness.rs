//! Native witness tests for the upload Worker's routes (`--features
//! arena-witness`, one test thread: `cargo test --features arena-witness
//! arena_witness -- --test-threads=1`).
//!
//! The arena is this test binary's global allocator (lib.rs). Each witness
//! runs a route's heaviest native path under `Mode::Strict` inside a scope
//! with exactly the route's declared control footprint ([`crate::routes`]),
//! with each read buffer admitted as its own block as the bounded readers
//! (`cf_io`) admit it, and fails on any allocation outside the scope, any
//! overdraft past the footprint and anything left live at the end. Native
//! sizes are 64-bit, so they bound the wasm32 sizes of the same structures
//! from above.

use crate::front;
use crate::routes::{self, HEADER_BODY_BYTES, HEADER_CONTROL_BYTES, PUBLIC_CONTROL_BYTES};
use slop_ninja_admission_wire::internal as iface;
use slop_ninja_arena::{Closed, Mode, Route, Scope, admit_current_block, set_mode, strict};
use slop_ninja_artifact_check::spec;

const HEADER: Route = Route::new(routes::id::HEADER, "upload/header");
const PUBLIC: Route = Route::new(routes::id::PUBLIC, "upload/public");

/// Runs `f` under `Strict` in a scope of exactly `control` bytes and fails
/// the test on any violation; prints the scope's peak pages.
fn witness<R>(route: Route, control: usize, f: impl FnOnce() -> R) -> R {
    let prev = set_mode(Mode::Strict);
    let _ = strict::take();
    let armed = strict::arm();
    let scope = Scope::open(route, control).unwrap_or_else(|r| panic!("route {} admitted: {r:?}", route.name));
    let out = scope.enter(f);
    let st = scope.stats();
    let closed = scope.close();
    drop(armed);
    set_mode(prev);
    let violations = strict::take();
    eprintln!(
        "arena witness: route {} peak {} pages of {} reserved ({} B footprint), peak live {} B",
        route.name, st.peak_pages, st.reserved_pages, control, st.peak_live
    );
    assert!(violations.is_clean(), "route {} broke its footprint of {control} B: {violations:?}", route.name);
    assert!(matches!(closed, Closed::Released { .. }), "route {} left values live: {closed:?}", route.name);
    out
}

/// A copy of `input` in the current scope, as the bounded readers make it:
/// one admitted block of exactly its length, then the bytes.
fn read_in(input: &[u8]) -> Vec<u8> {
    assert!(admit_current_block(input.len().max(1), 1).expect("the body block is admitted"));
    let mut out = Vec::with_capacity(input.len().max(1));
    out.extend_from_slice(input);
    out
}

/// The byte offset of the contract string's length prefix and the file
/// count prefix of an encoded header record.
fn contract_at(h: &[u8]) -> (usize, usize) {
    let kind = u32::from_le_bytes(h[..4].try_into().unwrap()) as usize;
    let at = 4 + kind + 2;
    let contract = u32::from_le_bytes(h[at..at + 4].try_into().unwrap()) as usize;
    (at, at + 4 + contract)
}

/// `h` with its contract string replaced by `len` bytes of `a`.
fn with_contract(h: &[u8], len: usize) -> Vec<u8> {
    let (at, rest) = contract_at(h);
    let mut out = h[..at].to_vec();
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend(std::iter::repeat_n(b'a', len));
    out.extend_from_slice(&h[rest..]);
    out
}

/// `h` with its file count prefix set to `n`.
fn with_count(h: &[u8], n: u32) -> Vec<u8> {
    let (_, count) = contract_at(h);
    let mut out = h.to_vec();
    out[count..count + 4].copy_from_slice(&n.to_le_bytes());
    out
}

/// The header shapes that cost the most memory, each at most the body
/// bound: the largest contract string a body can carry, a valid table, the
/// longest names a record accepts, garbage of the full length and a count
/// above the table.
fn header_shapes() -> Vec<(&'static str, Vec<u8>)> {
    let valid = front::tests::checked_header();
    let longest_contract = HEADER_BODY_BYTES - (valid.len() - (contract_at(&valid).1 - contract_at(&valid).0 - 4));
    let big = with_contract(&valid, longest_contract);
    assert_eq!(big.len(), HEADER_BODY_BYTES, "the contract fills the body bound");
    let long_names = {
        use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, Record};
        // The spec's roles under the longest names a record accepts (in
        // increasing order): the record decodes, the spec check refuses it.
        let files = spec::ROLES
            .iter()
            .enumerate()
            .map(|(i, r)| ArtifactFile {
                name: format!("{i}{}", "n".repeat(slop_ninja_protocol::artifact::MAX_FILE_NAME_LEN - 1)),
                role: r.file_role,
                sha256: Digest([i as u8 + 1; 32]),
                len: 1,
            })
            .collect();
        ArtifactHeader { contract: spec::CONTRACT_ID.into(), files }.to_bytes().expect("the long-name header encodes")
    };
    assert!(long_names.len() > spec::ROLES.len() * slop_ninja_protocol::artifact::MAX_FILE_NAME_LEN);
    vec![
        ("valid", valid.clone()),
        ("longest contract", big),
        ("longest names", long_names),
        ("garbage", vec![0xff; HEADER_BODY_BYTES]),
        ("zeros", vec![0; HEADER_BODY_BYTES]),
        ("count above the table", with_count(&valid, u32::MAX)),
    ]
}

#[test]
fn arena_witness_header_route_fits() {
    set_mode(Mode::Enforce);
    for (name, body) in header_shapes() {
        assert!(body.len() <= iface::HEADER_REQUEST_BYTES, "{name}");
        let spec_id = spec::SPEC_ID_V3;
        let status = witness(HEADER, HEADER_CONTROL_BYTES, || {
            let b = read_in(&body);
            let a = front::header_check(Some(spec_id), &b);
            drop(b);
            a.status
        });
        eprintln!("arena witness: header shape {name:?} ({} B) answered {status}", body.len());
        assert_eq!(status, 200, "{name}");
    }
    // An unknown spec answers without decoding (the probe is answered
    // before any body is read, `cf.rs`).
    witness(HEADER, HEADER_CONTROL_BYTES, || {
        assert_eq!(front::header_check(Some("x"), &read_in(b"x")).status, 400);
    });
}

/// A roster `state_queryStorageAt` reply of exactly `len` bytes whose
/// result is the densest tree a reply can carry (`[0,0,...]`).
fn dense_rpc_reply(len: usize) -> Vec<u8> {
    let head = br#"{"jsonrpc":"2.0","id":6,"result":["#;
    let tail = b"]}";
    let room = len - head.len() - tail.len();
    let mut out = head.to_vec();
    out.push(b'0');
    out.extend(std::iter::repeat_n(*b",0", (room - 1) / 2).flatten());
    if room.is_multiple_of(2) {
        out.push(b' ');
    }
    out.extend_from_slice(tail);
    assert_eq!(out.len(), len);
    out
}

/// A well-formed reply of at most `len` bytes for `keys` at `at`.
fn storage_reply(keys: &[String], at: &str) -> Vec<u8> {
    let changes: Vec<_> = keys.iter().map(|k| serde_json::json!([k, format!("0x{}", "ab".repeat(32))])).collect();
    serde_json::to_vec(&serde_json::json!({"jsonrpc": "2.0", "id": 6, "result": [{"block": at, "changes": changes}]})).unwrap()
}

#[test]
fn arena_witness_public_route_fits() {
    set_mode(Mode::Enforce);
    let bound = crate::registry::MAX_RESPONSE_BYTES;
    let at = format!("0x{}", "cd".repeat(32));
    // The densest roster reply: its tree is the footprint's largest term.
    let dense = dense_rpc_reply(bound);
    witness(PUBLIC, PUBLIC_CONTROL_BYTES, || {
        let b = read_in(&dense);
        let keys: Vec<&str> = Vec::new();
        assert!(crate::registry::parse_query_storage(6, &b, &keys, &at).is_err());
    });
    // A full roster call: the most keys one reply can answer, their request,
    // the reply and its decoded values.
    let key_len = 32 + 2 + 16 + 2 + 16;
    let per = crate::registry::keys_per_call(key_len).max(1);
    let keys: Vec<String> = (0..per).map(|i| format!("0x{:0w$x}", i, w = 2 * key_len)).collect();
    let reply = storage_reply(&keys, &at);
    assert!(reply.len() <= bound, "{} keys answer within one reply ({} B)", per, reply.len());
    witness(PUBLIC, PUBLIC_CONTROL_BYTES, || {
        let hexes: Vec<String> = keys.to_vec();
        let refs: Vec<&str> = hexes.iter().map(String::as_str).collect();
        let req = crate::registry::query_storage_request(6, &refs, &at);
        let b = read_in(&reply);
        let values = crate::registry::parse_query_storage(6, &b, &refs, &at).expect("the reply parses");
        assert_eq!(values.len(), per);
        drop((req, values));
    });
    // A relayed reply at the interface bound, its body passed on raw.
    let pad = "x".repeat(slop_ninja_admission_wire::response_limit() - 2);
    let raw = serde_json::to_vec(&iface::Reply::new(u16::MAX, serde_json::Value::String(pad), Some(u64::MAX))).unwrap();
    assert_eq!(raw.len(), front::reply_limit());
    let mut forward = iface::Forward::default();
    forward.signed.body = Some("ff".repeat(slop_ninja_admission_wire::UPLOAD_RECORD_BYTES));
    witness(PUBLIC, PUBLIC_CONTROL_BYTES, || {
        let msg = serde_json::to_string(&forward).unwrap();
        assert!(msg.len() <= iface::FORWARD_BYTES);
        let b = read_in(&raw);
        let r = front::relay(&b).expect("the reply relays");
        drop((msg, b, r));
    });
    // The snapshot at its bound: the longest string a cohort response holds.
    let base = front::tests::snapshot(1_000);
    let room = iface::cohort_snapshot_limit() - base.len();
    let text = String::from_utf8(base).unwrap();
    let wire = format!("\"contract\":\"{}\"", slop_ninja_admission_wire::CONTRACT);
    let long = format!("\"contract\":\"{}{}\"", slop_ninja_admission_wire::CONTRACT, "c".repeat(room));
    let snap = text.replacen(&wire, &long, 1).into_bytes();
    assert_eq!(snap.len(), iface::cohort_snapshot_limit());
    witness(PUBLIC, PUBLIC_CONTROL_BYTES, || {
        let b = read_in(&snap);
        let served = front::serve_snapshot(&b, 1_000).expect("the snapshot serves");
        drop((b, served));
    });
    // The repair list at its bound (one roster of hotkeys), the cohort
    // response padded to the limit beside it: the gate's membership read for
    // a session read or repair the roster did not pass.
    let who = slop_ninja_protocol::AccountId([0xab; 32]);
    let mut v: serde_json::Value = serde_json::from_slice(&front::tests::snapshot(1_000)).unwrap();
    v["repair"] = serde_json::Value::Array(
        (0..iface::REPAIR_LIST_MAX).map(|n| serde_json::Value::String(format!("{n:064x}"))).collect(),
    );
    v["repair_all"] = serde_json::Value::Bool(false);
    let base = serde_json::to_string(&v).unwrap();
    let room = iface::cohort_snapshot_limit() - base.len();
    let listed = base.replacen(&wire, &format!("\"contract\":\"{}{}\"", slop_ninja_admission_wire::CONTRACT, "c".repeat(room)), 1);
    assert_eq!(listed.len(), iface::cohort_snapshot_limit());
    witness(PUBLIC, PUBLIC_CONTROL_BYTES, || {
        let b = read_in(listed.as_bytes());
        assert_eq!(front::repair_listed(&b, &who), Some(false));
        drop(b);
    });
}
