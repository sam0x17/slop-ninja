//! The value cost (`canonical::check_cost`) bounds what a `serde_json::Value`
//! parse of the file holds. A counting allocator records the peak live heap of
//! each parse, every block rounded up to its glibc chunk (at least 32 bytes,
//! the request plus 8 rounded up to 16), with both blocks of a reallocation
//! live at once; the peak must not exceed the cost. The shapes are the costly
//! ones (small objects, deep nesting, short strings, short numbers, empty
//! containers, long escaped strings) and the artifact-like ones (float arrays,
//! knot objects). With `A_R0_MODEL_DIR` set, R0's four JSON files are checked
//! too.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

use serde_json::{Value, json};
use slop_ninja_artifact_check::canonical::{self, Layout as JsonLayout};

struct Counting;

static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
thread_local! {
    /// Counting is on for the measuring thread only; other tests run alongside.
    static ON: Cell<bool> = const { Cell::new(false) };
}
/// One measurement at a time: the counters are process-wide.
static ONE: Mutex<()> = Mutex::new(());

fn chunk(size: usize) -> u64 {
    (((size as u64) + 8 + 15) & !15).max(32)
}

fn on() -> bool {
    ON.try_with(Cell::get).unwrap_or(false)
}

fn grow(by: u64) {
    if on() {
        let now = LIVE.fetch_add(by, SeqCst) + by;
        PEAK.fetch_max(now, SeqCst);
    }
}

fn shrink(by: u64) {
    if on() {
        LIVE.fetch_sub(by.min(LIVE.load(SeqCst)), SeqCst);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        grow(chunk(l.size()));
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        shrink(chunk(l.size()));
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // Both blocks are live while the contents move.
        grow(chunk(new));
        shrink(chunk(l.size()));
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Peak live heap, in chunk bytes, of parsing `bytes` into a `Value` and
/// dropping it.
fn peak_of_parse(bytes: &[u8]) -> u64 {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    LIVE.store(0, SeqCst);
    PEAK.store(0, SeqCst);
    ON.with(|on| on.set(true));
    let v: Value = serde_json::from_slice(bytes).unwrap();
    drop(v);
    ON.with(|on| on.set(false));
    PEAK.load(SeqCst)
}

fn bounded(name: &str, bytes: &[u8], layout: JsonLayout) {
    let cost = canonical::check_cost(bytes, layout).unwrap_or_else(|e| panic!("{name}: {e}"));
    let peak = peak_of_parse(bytes);
    eprintln!(
        "{name}: {} bytes, peak {peak}, cost {cost} ({:.1} per byte, peak/cost {:.2})",
        bytes.len(),
        cost as f64 / bytes.len() as f64,
        peak as f64 / cost as f64
    );
    assert!(peak <= cost, "{name}: a parse held {peak} bytes; the cost is {cost}");
}

fn both(name: &str, v: &Value) {
    for layout in [JsonLayout::PythonCompact, JsonLayout::SerdePretty] {
        bounded(&format!("{name} {layout:?}"), &canonical::encode(v, layout), layout);
    }
}

fn nested(depth: usize) -> Value {
    (0..depth).fold(json!(0), |v, _| json!({ "a": v }))
}

#[test]
fn the_cost_bounds_a_value_parse_of_the_costly_shapes() {
    both("small objects", &Value::Array(vec![json!({"a": 0}); 20_000]));
    both("empty-key objects", &Value::Array(vec![json!({"": 0}); 20_000]));
    both("nested objects", &Value::Array(vec![nested(120); 300]));
    both("short strings", &Value::Array(vec![json!("a"); 50_000]));
    both("empty strings", &Value::Array(vec![json!(""); 50_000]));
    both("short numbers", &Value::Array(vec![json!(0); 100_000]));
    both("empty arrays", &Value::Array(vec![json!([]); 100_000]));
    both("empty objects", &Value::Array(vec![json!({}); 100_000]));
    let deep = (0..120).fold(json!([]), |v, _| json!([v]));
    both("deep arrays", &Value::Array(vec![deep; 300]));
    let many: serde_json::Map<String, Value> = (0..20_000).map(|i| (format!("k{i:05}"), json!(i))).collect();
    both("one wide object", &Value::Object(many));
    both("long escaped string", &json!(["\n\"\\".repeat(300_000)]));
    let lengths: Vec<Value> = (0..2_000).map(|i| json!("x".repeat(i % 97))).collect();
    both("strings of many lengths", &Value::Array(lengths));
}

#[test]
fn the_cost_bounds_a_value_parse_of_artifact_like_shapes() {
    let values: Vec<f64> = (0..200_000).map(|i| ((i as f64) * 0.618_033_988_749_895).sin() / 7.0).collect();
    let knots: Vec<Value> = (0..5_000)
        .map(|i| {
            json!({"involvement": i as f64 / 5e3, "rows": i + 1, "score": i as f64 * 1e-3,
                "weight": 1.5, "weighted_positives": 0.25})
        })
        .collect();
    both(
        "heads-like",
        &json!({"weights": {"first": {"shape": [400, 500], "values": values}},
            "involvement_calibration": {"knots": knots, "lo": 0.001, "hi": 0.999}}),
    );
}

#[test]
fn the_cost_bounds_r0s_files() {
    let Some(dir) = std::env::var_os("A_R0_MODEL_DIR").map(std::path::PathBuf::from) else {
        eprintln!("notice: A_R0_MODEL_DIR is unset; test passes without checking it");
        return;
    };
    let mut total = 0u64;
    for name in ["heads.json", "contract.json", "calibration.json", "manifest.json"] {
        let layout = slop_ninja_artifact_check::spec::role_by_name(name).unwrap().layout.unwrap();
        let bytes = std::fs::read(dir.join(name)).unwrap();
        bounded(name, &bytes, layout);
        total += canonical::check_cost(&bytes, layout).unwrap();
    }
    let budget = slop_ninja_artifact_check::spec::MAX_JSON_VALUE_BYTES_V2;
    eprintln!("R0: value cost {total}, {:.1}% of the spec v2 budget {budget}", total as f64 * 100.0 / budget as f64);
    assert!(total <= budget);
}

#[test]
fn the_prices_cover_this_builds_value_layout() {
    use canonical::value_cost as c;
    use std::mem::size_of;
    let value = size_of::<Value>() as u64;
    eprintln!("this build: Value {value} bytes, Map {} bytes", size_of::<serde_json::Map<String, Value>>());
    assert!(c::ROOT >= value);
    assert!(c::ARRAY_SLOT >= 2 * value);
    // An index map bucket: the hash, the key and the value; three of them
    // while the entries vector grows, plus the table's share.
    let bucket = 8 + size_of::<String>() as u64 + value;
    assert!(c::OBJECT_ENTRY >= 3 * bucket + 32, "{bucket}");
}

#[test]
fn prices() {
    use canonical::value_cost as c;
    let cost = |s: &str| canonical::check_cost(format!("{s}\n").as_bytes(), JsonLayout::PythonCompact).unwrap();
    assert_eq!(cost("[]"), c::ROOT);
    assert_eq!(cost("{}"), c::ROOT);
    assert_eq!(cost("null"), c::ROOT);
    // One number in an array: the array at its smallest capacity and the literal.
    assert_eq!(cost("[0]"), c::ROOT + c::ARRAY + 4 * c::ARRAY_SLOT + c::TEXT + 1);
    // Five elements: capacity 8.
    assert_eq!(cost("[0,0,0,0,0]"), c::ROOT + c::ARRAY + 8 * c::ARRAY_SLOT + 5 * (c::TEXT + 1));
    // An object: its first entry, the key and the number; scratch for the key.
    assert_eq!(cost("{\"a\":0}"), c::ROOT + c::OBJECT + 2 * (c::TEXT + 1) + c::SCRATCH_PER_LONGEST_TEXT);
    assert_eq!(
        cost("{\"a\":0,\"b\":\"xy\"}"),
        c::ROOT + c::OBJECT + c::OBJECT_ENTRY + 3 * (c::TEXT + 1) + c::TEXT + 2 + 2 * c::SCRATCH_PER_LONGEST_TEXT
    );
    // Empty strings and keys allocate nothing.
    assert_eq!(cost("{\"\":\"\"}"), c::ROOT + c::OBJECT);
    // The cost is the same in either layout.
    let v = json!({"a": [1, 2.5, "x"], "b": {"c": null}});
    assert_eq!(
        canonical::check_cost(&canonical::encode(&v, JsonLayout::PythonCompact), JsonLayout::PythonCompact),
        canonical::check_cost(&canonical::encode(&v, JsonLayout::SerdePretty), JsonLayout::SerdePretty),
    );
}
