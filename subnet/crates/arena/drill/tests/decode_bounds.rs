//! Finding 9: serde_json's error and escape paths are bounded. The drill's
//! decoder pre-scans the body allocation-free and answers every input kind
//! with a static message, so a malformed body inside its schema bound stays
//! inside the route's derived control footprint (`CONTROL_FOOTPRINT`).
//! The first two inputs are the review's reproductions at 58d4de9d (on
//! the retained `workers-arena` evidence branch; a
//! 300 KB unknown key; a 2,000-times escaped schema id).
//! Run with one test thread.

use slop_ninja_arena::strict::{self, StrictFailure};
use slop_ninja_arena::{ArenaAlloc, Mode, set_mode};
use slop_ninja_arena_drill::{
    CONTROL_BYTES, Ctx, ERROR_BOX_BOUND, Fail, MAX_ENTRY_JSON, MAX_ERROR_MESSAGE, ROUTE, SCHEMA, handle,
};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

mod common;
use common::Harness;

thread_local! {
    static H: Harness = Harness::new(16 << 20);
}

/// Builds a test input inside the harness scope.
fn make(f: impl FnOnce() -> String) -> String {
    H.with(|h| h.data(f))
}

// A test helper: the strict failure carries the scope snapshot by value
// (no allocation on the failure path), which makes the error large.
#[allow(clippy::result_large_err)]
fn run(json: &str) -> Result<(Result<(), Fail>, usize), StrictFailure> {
    // Declared receipts so that the body is inside its schema bound.
    let n = json.len() / MAX_ENTRY_JSON + 1;
    strict::run(ROUTE, CONTROL_BYTES, |s| {
        let cx = Ctx::Scoped(s);
        let mut ing = cx.ingest(json.len(), n).unwrap();
        ing.push(json.as_bytes()).unwrap();
        let body = ing.finish().unwrap();
        let before = s.stats().peak_live;
        let mut emit = |_: &[u8]| Ok(());
        let o = handle(&cx, body.as_slice(), false, &mut emit, &mut |_| {}).map(|_| ());
        let control_peak = s.stats().peak_live.saturating_sub(before.max(body.len()));
        drop(body);
        (o, control_peak)
    })
}

fn entry(extra: &str) -> String {
    let z = "0".repeat(64);
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"cohort_manifest\":\"{z}\",\"artifact\":\"{z}\",\"count\":1,\"receipts\":[{{\"attempt\":\"{z}\",\"assignment\":\"{z}\",\"number\":1,\"outcome\":1,\"result_commitment\":\"{z}\"{extra}}}]}}"
    )
}

fn check(name: &str, json: String) {
    set_mode(Mode::Enforce);
    let r = run(&json);
    H.with(|h| h.data(|| drop(json)));
    match r {
        Ok((o, control_peak)) => {
            eprintln!("{name}: refused {:?}, control peak {control_peak} B (bound {} B)", o.err(), ERROR_BOX_BOUND + 2 * MAX_ERROR_MESSAGE.next_power_of_two());
            assert!(o.is_err(), "{name}: malformed input refused");
        }
        Err(e) => panic!("{name}: stays inside CONTROL_BYTES ({CONTROL_BYTES}): {e:?}"),
    }
}

#[test]
fn long_unknown_field_stays_inside_the_control_footprint() {
    check("unknown-field", make(|| entry(&format!(",\"{}\":1", "k".repeat(300_000)))));
}

#[test]
fn escaped_strings_are_refused_before_parsing() {
    check(
        "escaped-schema",
        make(|| {
            let z = "0".repeat(64);
            let schema: String = SCHEMA.bytes().map(|b| format!("\\u{:04x}", b)).collect::<String>().repeat(2_000);
            format!("{{\"schema\":\"{schema}\",\"cohort_manifest\":\"{z}\",\"artifact\":\"{z}\",\"count\":0,\"receipts\":[]}}")
        }),
    );
}

#[test]
fn strings_where_numbers_belong_carry_no_input_in_their_error() {
    check("string-number", make(|| entry("").replacen("\"number\":1", &format!("\"number\":\"{}\"", "9".repeat(200_000)), 1)));
    check("top-level-string", make(|| format!("\"{}\"", "x".repeat(300_000))));
    check("string-entries", make(|| entry("").replacen("\"receipts\":[", &format!("\"receipts\":\"{}\",\"x\":[", "y".repeat(100_000)), 1)));
}

#[test]
fn numbers_outside_the_schema_are_refused_before_parsing() {
    check("long-number", make(|| entry("").replacen("\"number\":1", &format!("\"number\":{}", "9".repeat(5000)), 1)));
    check("float", make(|| entry("").replacen("\"number\":1", "\"number\":1e308", 1)));
    check("negative", make(|| entry("").replacen("\"number\":1", "\"number\":-1", 1)));
    check("out-of-range", make(|| entry("").replacen("\"number\":1", "\"number\":9999999999", 1)));
}

#[test]
fn structural_errors_stay_inside_the_control_footprint() {
    check("truncated", make(|| {
        let ok = entry("");
        ok[..ok.len() - 3].to_owned()
    }));
    check("duplicate", make(|| entry("").replacen("\"number\":1", "\"number\":1,\"number\":2", 1)));
    check("missing", make(|| entry("").replacen(",\"outcome\":1", "", 1)));
    check("hex", make(|| entry("").replacen(&"0".repeat(64), &"g".repeat(64), 1)));
    check("nested-junk", make(|| entry("").replacen("\"number\":1", &format!("\"number\":{}", "[".repeat(10_000)), 1)));
}
