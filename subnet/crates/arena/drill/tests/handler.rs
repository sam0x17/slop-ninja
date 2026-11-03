//! The drill handler, natively under the arena allocator: its output matches
//! a direct reference encoding, it fits its declared bound with no violation,
//! and malformed or oversized bodies are refused.
//! Run with one test thread.

use sha2::{Digest, Sha256};
use slop_ninja_arena::strict::{self, StrictFailure};
use slop_ninja_arena::{ArenaAlloc, Mode, Reason, Scope, set_mode};
use slop_ninja_arena_drill::{
    BUNDLE_ENTRY, CONTROL_BYTES, Ctx, Fail, INDEX_ENTRY, INDEX_HEADER, MIN_ENTRY_JSON, Output, RECEIPT_LEN, ROUTE, SCHEMA,
    handle,
};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

mod common;
use common::Harness;

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Gen {
    x: u32,
}

impl Gen {
    fn next(&mut self) -> u32 {
        self.x ^= self.x << 13;
        self.x ^= self.x >> 17;
        self.x ^= self.x << 5;
        self.x
    }
    fn bytes32(&mut self) -> [u8; 32] {
        std::array::from_fn(|_| self.next() as u8)
    }
}

type Row = ([u8; 32], [u8; 32], u8, [u8; 32]);

/// The same JSON the workerd drill generates (compact, attempts ascending).
fn body(n: usize, seed: u32) -> (String, [u8; 32], [u8; 32], Vec<Row>) {
    let mut g = Gen { x: seed | 1 };
    let manifest = g.bytes32();
    let artifact = g.bytes32();
    let mut rows = Vec::new();
    let mut s = format!(
        "{{\"schema\":\"{SCHEMA}\",\"cohort_manifest\":\"{}\",\"artifact\":\"{}\",\"count\":{n},\"receipts\":[",
        hexs(&manifest),
        hexs(&artifact)
    );
    for i in 0..n {
        let mut attempt = g.bytes32();
        attempt[..4].copy_from_slice(&(i as u32).to_be_bytes());
        let assignment = g.bytes32();
        let outcome = 1 + (g.next() % 5) as u8;
        let commitment = g.bytes32();
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"attempt\":\"{}\",\"assignment\":\"{}\",\"number\":1,\"outcome\":{outcome},\"result_commitment\":\"{}\"}}",
            hexs(&attempt),
            hexs(&assignment),
            hexs(&commitment)
        ));
        rows.push((attempt, assignment, outcome, commitment));
    }
    s.push_str("]}");
    (s, manifest, artifact, rows)
}

/// A direct encoding with ordinary `Vec`s, for comparison.
fn reference(manifest: &[u8; 32], artifact: &[u8; 32], rows: &[Row]) -> Output {
    let mut bundle = Vec::new();
    let mut index = Vec::new();
    index.extend_from_slice(&13u32.to_le_bytes());
    index.extend_from_slice(b"receipt-index");
    index.extend_from_slice(&1u16.to_le_bytes());
    index.extend_from_slice(manifest);
    index.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    for (attempt, assignment, outcome, commitment) in rows {
        let mut r = Vec::new();
        r.extend_from_slice(&15u32.to_le_bytes());
        r.extend_from_slice(b"attempt-receipt");
        r.extend_from_slice(&1u16.to_le_bytes());
        r.extend_from_slice(attempt);
        r.extend_from_slice(assignment);
        r.extend_from_slice(artifact);
        r.push(1);
        r.push(*outcome);
        r.extend_from_slice(commitment);
        assert_eq!(r.len(), RECEIPT_LEN);
        bundle.extend_from_slice(&(r.len() as u32).to_le_bytes());
        bundle.extend_from_slice(&r);
        index.extend_from_slice(attempt);
        index.push(*outcome);
        index.extend_from_slice(&Sha256::digest(&r));
        index.extend_from_slice(&(r.len() as u32).to_le_bytes());
    }
    assert_eq!(index.len(), INDEX_HEADER + INDEX_ENTRY * rows.len());
    Output {
        count: rows.len() as u32,
        bundle_len: bundle.len() as u32,
        bundle_sha256: Sha256::digest(&bundle).into(),
        index_len: index.len() as u32,
        index_root: Sha256::digest(&index).into(),
    }
}

// A test helper: the strict failure carries the scope snapshot by value
// (no allocation on the failure path), which makes the error large.
#[allow(clippy::result_large_err)]
fn run_scoped(h: &Harness, json: &[u8], n: usize, stream: bool) -> Result<(Output, Vec<u8>), StrictFailure> {
    // The emitted bundle is collected outside the route's scope (as JS would
    // hold it), in the harness scope at its exact final size.
    let mut sink: Vec<u8> = h.data(|| Vec::with_capacity(n * BUNDLE_ENTRY));
    let out = strict::run_strict(ROUTE, CONTROL_BYTES, |s| {
        let cx = Ctx::Scoped(s);
        let mut ing = cx.ingest(json.len(), n).unwrap();
        ing.push(json).unwrap();
        let body = ing.finish().unwrap();
        let mut emit = |b: &[u8]| {
            sink.extend_from_slice(b);
            Ok(())
        };
        let o = handle(&cx, body.as_slice(), stream, &mut emit, &mut |_| {});
        drop(body);
        o
    })?;
    Ok((out.unwrap(), sink))
}

#[test]
fn handler_matches_reference_and_fits_its_declared_bound() {
    for (n, stream) in [(0, false), (1, false), (1000, false), (1000, true), (5000, true)] {
        let h = Harness::new(8 << 20);
        let (json, manifest, artifact, rows) = h.data(|| body(n, 7 + n as u32));
        let want = h.data(|| reference(&manifest, &artifact, &rows));
        let (got, bundle) = run_scoped(&h, json.as_bytes(), n, stream).unwrap_or_else(|e| panic!("n={n} stream={stream}: {e:?}"));
        assert_eq!(got, want, "n={n} stream={stream}");
        assert_eq!(Sha256::digest(&bundle).as_slice(), want.bundle_sha256, "emitted bytes");
        h.data(|| drop((json, rows, bundle)));
    }
}

#[test]
fn unscoped_build_produces_the_same_output() {
    // `Ctx::Unscoped` allocates wherever the global allocator puts it: run it
    // inside the harness scope (the default-allocator drill build has no
    // scopes at all).
    let h = Harness::new(16 << 20);
    h.data(|| {
        let (json, manifest, artifact, rows) = body(300, 99);
        let mut emit = |_: &[u8]| Ok(());
        let got = handle(&Ctx::Unscoped, json.as_bytes(), true, &mut emit, &mut |_| {}).unwrap();
        assert_eq!(got, reference(&manifest, &artifact, &rows));
    });
}

#[test]
fn malformed_and_oversized_bodies_are_refused_inside_the_bound() {
    let h = Harness::new(8 << 20);
    let cases: Vec<(String, usize)> = h.data(|| {
        let (ok, ..) = body(10, 3);
        vec![
            (ok.replacen("\"count\":10", "\"count\":11", 1), 11),
            (ok.replacen("\"count\":10", "\"count\":100000", 1), 10),
            (ok.replacen(SCHEMA, "slop_ninja_arena_drill_package_v0", 1), 10),
            (ok.replacen("\"number\":1", "\"number\":1,\"extra\":2", 1), 10),
            (ok[..ok.len() - 3].to_owned(), 10),
        ]
    });
    for (i, (json, n)) in cases.iter().enumerate() {
        let r = strict::run_strict(ROUTE, CONTROL_BYTES, |s| {
            let cx = Ctx::Scoped(s);
            let mut ing = cx.ingest(json.len(), *n).unwrap();
            ing.push(json.as_bytes()).unwrap();
            let body = ing.finish().unwrap();
            let mut emit = |_: &[u8]| Ok(());
            let o = handle(&cx, body.as_slice(), false, &mut emit, &mut |_| {});
            drop(body);
            o
        });
        let o = r.unwrap_or_else(|e| panic!("case {i}: errors stay inside the bound: {e:?}"));
        assert!(o.is_err(), "case {i} refused");
        if i == 1 {
            assert!(matches!(o, Err(Fail::Refused(r)) if r.reason == Reason::Bound), "declared count above input/{MIN_ENTRY_JSON}");
        }
    }
    h.data(|| drop(cases));
}

#[test]
fn a_declared_length_above_the_schema_bound_is_refused_before_allocating() {
    set_mode(Mode::Enforce);
    let s = Scope::open(ROUTE, CONTROL_BYTES).unwrap();
    let before = slop_ninja_arena::stats().allocs;
    let r = Ctx::Scoped(&s).ingest(slop_ninja_drill_bound(10) + 1, 10);
    assert!(matches!(r, Err(e) if e.reason == Reason::Bound));
    assert_eq!(slop_ninja_arena::stats().allocs, before);
    drop(s);
}

fn slop_ninja_drill_bound(n: usize) -> usize {
    slop_ninja_arena_drill::body_bound(n)
}
