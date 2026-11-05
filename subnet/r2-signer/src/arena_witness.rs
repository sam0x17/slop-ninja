//! Native witness tests for the signer's routes (`--features
//! arena-witness`, one test thread: `cargo test --features arena-witness
//! arena_witness -- --test-threads=1`).
//!
//! The arena is this test binary's global allocator (lib.rs). Each witness
//! runs a route's heaviest native path under `Mode::Strict` inside a scope
//! with exactly the route's declared control footprint ([`crate::routes`]):
//! the request as read (one admitted block, as the bounded readers admit
//! it), the decision with every entry at its longest, and the reply buffer.
//! It fails on any allocation outside the scope, any overdraft past the
//! footprint and anything left live at the end. Native sizes are 64-bit,
//! so they bound the wasm32 sizes of the same structures from above.

use crate::mint::{self, COPY_REPLY_BYTES, Config, CopyConfig, MAX_ENTRIES, REPLY_BYTES};
use crate::routes::{COPY_CONTROL_BYTES, MINT_CONTROL_BYTES, R_COPY, R_MINT};
use slop_ninja_arena::{Closed, Mode, Route, Scope, admit_current_block, set_mode, strict};
use slop_ninja_r2_presign::copy::{CopyKind, CopySpec};
use slop_ninja_r2_presign::presign::Credentials;
use slop_ninja_r2_presign::wire::{self, Entry, Op, Status};

const NOW: u64 = 1_764_612_312;

/// The longest host an R2 S3 URL can name (a 253-byte DNS name).
fn long_host() -> String {
    let suffix = ".r2.cloudflarestorage.com";
    "a".repeat(253 - suffix.len()) + suffix
}

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

fn read_in(input: &[u8]) -> Vec<u8> {
    assert!(admit_current_block(input.len().max(1), 1).expect("the body block is admitted"));
    let mut out = Vec::with_capacity(input.len().max(1));
    out.extend_from_slice(input);
    out
}

/// The mint route at its bounds: a request of [`MAX_ENTRIES`] entries (a
/// header and the spec's whole file table, each file at the largest length
/// the signer binds), the longest host and credential strings, the reply
/// buffer at its bound.
#[test]
fn arena_witness_a_full_mint_fits_its_footprint() {
    set_mode(Mode::Enforce);
    let file = |n: u8| Entry {
        op: Op::File,
        cohort: u64::MAX,
        commitment: [7; 32],
        sha256: [n; 32],
        len: mint::max_file_len(),
        valid_secs: 604_800,
    };
    let mut entries = vec![Entry { op: Op::Header, len: u64::from(slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN), ..file(0) }];
    entries.extend((1..MAX_ENTRIES as u8).map(file));
    assert_eq!(entries.len(), MAX_ENTRIES);
    let mut req = vec![0u8; wire::request_len(entries.len())];
    wire::encode_request(&entries, &mut req).unwrap();
    let host = long_host();
    let key_id = "K".repeat(64);
    let secret = vec![b's'; 128];
    let n = witness(R_MINT.route(), MINT_CONTROL_BYTES, || {
        let body = read_in(&req);
        let cfg = Config {
            host: &host,
            bucket: "slop-ninja-admission-testnet",
            cred: Credentials { access_key_id: &key_id, secret_access_key: &secret },
        };
        let mut out = vec![0u8; REPLY_BYTES];
        let n = mint::mint(&body, &cfg, NOW, &mut out).unwrap();
        let ok = wire::decode_reply(&out[..n], MAX_ENTRIES).unwrap().all(|e| e.is_some_and(|e| e.status == Status::Ok));
        assert!(ok, "every entry minted");
        n
    });
    assert!(n <= REPLY_BYTES);
}

/// The copy route at its bounds: one copy request, the longest host, both
/// bucket names and the copy credential, the reply buffer at its bound.
#[test]
fn arena_witness_a_copy_fits_its_footprint() {
    set_mode(Mode::Enforce);
    let spec = CopySpec { cohort: u64::MAX, commitment: [7; 32], kind: CopyKind::File, sha256: [8; 32], etag: [5; 16] };
    let req = wire::encode_copy_request(&spec, 604_800);
    let host = long_host();
    let key_id = "K".repeat(64);
    let secret = vec![b's'; 128];
    witness(R_COPY.route(), COPY_CONTROL_BYTES, || {
        let body = read_in(&req);
        let cfg = CopyConfig {
            host: &host,
            source_bucket: "slop-ninja-admission-testnet",
            destination_bucket: "public-bucket-testnet",
            cred: Credentials { access_key_id: &key_id, secret_access_key: &secret },
        };
        let mut out = vec![0u8; COPY_REPLY_BYTES];
        let n = mint::copy(&body, &cfg, NOW, &mut out).unwrap();
        assert_eq!(wire::decode_copy_reply(&out[..n]).unwrap().status, Status::Ok);
    });
}
