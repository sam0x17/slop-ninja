//! Compiles the memory model (`slop_ninja_worker_memory_v2`) into constants.
//!
//! The model file is `memory-model.json` beside this crate, or the file named
//! by `SLOP_NINJA_MEMORY_MODEL` (each Worker build carries its own model; the
//! glue probe and the drill have theirs). Every input carries its measured or
//! documented source; the derived bounds are computed here, never typed in:
//!
//! - `glue_tickets = floor(pool_pages_upper / min_scope_pages)`: the most
//!   scopes the pool could ever admit at once, so a ticket is refused exactly
//!   when no scope could be admitted for it either;
//! - `glue_residents = glue_tickets` (a Durable Object instance is created by
//!   an admitted invocation; one past the budget is refused at construction);
//! - `glue_budget_bytes = glue_static_bytes + glue_tickets × glue_ticket_bytes +
//!   glue_residents × glue_resident_bytes`, rounded up to whole pages with the
//!   glue heap's segment overhead.
//!
//! - `stack_frame_bound_bytes = stack_bytes / 2 - stack_high_water_bytes`:
//!   the clippy `stack-size-threshold`, `future-size-threshold` and
//!   `array-size-threshold`. The stated margin is half the linked stack: the
//!   deepest measured stack plus any one new frame, future or stack array
//!   must stay within `stack_bytes / 2`.
//!
//! The build refuses a model whose JS reserve, glue costs or stack
//! high-water are zero or unmeasured, unless the `unmeasured-model` feature
//! is set (measurement and unit-test builds, and `Observe`-mode builds:
//! the feature is refused without `observe` or the test knobs, so an
//! unmeasured model never enforces). When the model lists the
//! measured glue per entry kind (`glue_ticket_kinds`), `glue_ticket_bytes`
//! must cover the largest.

use std::{env, fs, path::Path};

const PAGE: u64 = 65_536;
/// dlmalloc 0.2.12 `top_foot_size + malloc_alignment` on a 64-bit host (12
/// words); an upper bound for wasm32 (12 × 4 bytes).
const SEGMENT_OVERHEAD_UPPER: u64 = 12 * 8;

fn main() {
    println!("cargo:rerun-if-env-changed=SLOP_NINJA_MEMORY_MODEL");
    let path = env::var("SLOP_NINJA_MEMORY_MODEL").unwrap_or_else(|_| "memory-model.json".to_owned());
    println!("cargo:rerun-if-changed={path}");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    // Build time, on the host: the model file is a few hundred bytes.
    #[allow(clippy::disallowed_types)]
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let s = |k: &str| v[k].as_str().unwrap_or_else(|| panic!("{path}: string {k}")).to_owned();
    let n = |k: &str| v[k].as_u64().unwrap_or_else(|| panic!("{path}: integer {k}"));
    let b = |k: &str| v[k].as_bool().unwrap_or_else(|| panic!("{path}: bool {k}"));
    let src = |k: &str| {
        let key = format!("{k}_source");
        assert!(v[&key].as_str().is_some_and(|t| !t.is_empty()), "{path}: {key} must record the derivation of {k}");
    };
    assert_eq!(s("schema"), "slop_ninja_worker_memory_v2", "{path}: schema");
    let unmeasured_ok = env::var_os("CARGO_FEATURE_UNMEASURED_MODEL").is_some();
    // An unmeasured model never enforces: it builds only in `Observe` mode
    // (migration step 1, where nothing the pool can physically serve is
    // refused) or with the test knobs (unit tests, drills, measurement).
    let observe = env::var_os("CARGO_FEATURE_OBSERVE").is_some();
    let knobs = env::var_os("CARGO_FEATURE_KNOBS").is_some();
    assert!(
        !unmeasured_ok || observe || knobs,
        "{path}: the unmeasured-model feature needs the observe feature (or the test knobs): an unmeasured model never enforces"
    );

    let page = n("page_bytes");
    assert_eq!(page, PAGE, "page_bytes must equal the wasm page size");
    src("page_bytes");
    let isolate = n("isolate_bytes");
    src("isolate_bytes");

    let js = n("js_reserve_bytes");
    src("js_reserve_bytes");
    let js_measured = b("js_reserve_measured");
    assert!(js > 0, "{path}: js_reserve_bytes must be a measured or derived bound, never 0");
    assert!(js < isolate, "{path}: js_reserve_bytes must leave room for linear memory");
    assert!(js_measured || unmeasured_ok, "{path}: js_reserve_measured is false (only measurement builds may use an unmeasured model)");
    let js_envelope = n("js_envelope_bytes");
    src("js_envelope_bytes");
    assert!(js_envelope > 0 && js_envelope < js, "{path}: js_envelope_bytes (the per-invocation JS need measured inside js_reserve_bytes) must be below it");

    let glue_static = n("glue_static_bytes");
    let glue_ticket = n("glue_ticket_bytes");
    let glue_resident = n("glue_resident_bytes");
    let glue_measured = b("glue_measured");
    src("glue_static_bytes");
    src("glue_ticket_bytes");
    src("glue_resident_bytes");
    assert!(glue_ticket > 0, "{path}: glue_ticket_bytes must be measured, never 0");
    assert!(glue_measured || unmeasured_ok, "{path}: glue_measured is false (only measurement builds may use an unmeasured model)");

    if let Some(kinds) = v["glue_ticket_kinds"].as_object() {
        for (kind, bytes) in kinds {
            let b = bytes.as_u64().unwrap_or_else(|| panic!("{path}: glue_ticket_kinds.{kind} must be an integer"));
            assert!(glue_ticket >= b, "{path}: glue_ticket_bytes ({glue_ticket}) is below the measured glue of entry kind {kind} ({b})");
        }
    }

    let stack = n("stack_bytes");
    src("stack_bytes");
    let stack_hw = n("stack_high_water_bytes");
    src("stack_high_water_bytes");
    let stack_measured = b("stack_measured");
    assert!(stack > 0 && stack % PAGE == 0, "{path}: stack_bytes is the link-time stack size, whole pages");
    assert!(stack_measured || unmeasured_ok, "{path}: stack_measured is false (only measurement builds may use an unmeasured model)");
    if stack_measured {
        assert!(stack_hw > 0, "{path}: a measured stack high-water is never 0");
        assert!(stack_hw.saturating_mul(2) <= stack, "{path}: the stated stack margin needs 2 x stack_high_water_bytes ({stack_hw}) <= stack_bytes ({stack})");
    }

    let min_route = n("min_route_bytes");
    let largest_route = n("largest_route_bytes");
    src("min_route_bytes");
    src("largest_route_bytes");
    assert!(largest_route >= min_route, "{path}: largest_route_bytes below min_route_bytes");

    // Upper bound on pool pages (static bytes are only known at run time, so
    // ignoring them over-counts tickets: the glue budget stays conservative).
    let pool_upper = (isolate - js) / PAGE;
    // A scope is its state page plus at least one page of heap.
    let min_scope_pages = 1 + (min_route + SEGMENT_OVERHEAD_UPPER).div_ceil(PAGE).max(1);
    let tickets = pool_upper / min_scope_pages;
    let residents = tickets;
    let glue_bytes = glue_static + tickets * glue_ticket + residents * glue_resident;
    let glue_pages = (glue_bytes + SEGMENT_OVERHEAD_UPPER).div_ceil(PAGE).max(1);
    assert!(glue_pages < pool_upper / 2, "{path}: the derived glue partition ({glue_pages} pages) leaves too little for scopes");
    let largest_pages = 1 + (largest_route + SEGMENT_OVERHEAD_UPPER).div_ceil(PAGE);
    assert!(glue_pages + largest_pages <= pool_upper, "{path}: the largest route ({largest_pages} pages) does not fit beside the glue partition");

    // The fixed linear memory a Worker of this model links with (`initial =
    // maximum`): everything the isolate leaves beside the JS reserve, in
    // whole wasm pages. Static data, the stack and the pool all live in it
    // (`static + pool_pages(static) <= linear_pages` for every static size),
    // so it never grows at run time. Derived, never typed in.
    let linear_pages = (isolate - js) / PAGE;
    let linear_bytes = linear_pages * PAGE;
    assert!(linear_pages <= u64::from(u32::MAX), "{path}: linear memory pages exceed u32");

    let out = format!(
        "pub const SCHEMA: &str = {schema:?};\n\
         pub const VERSION: &str = {version:?};\n\
         pub const PAGE_BYTES: usize = {page};\n\
         pub const ISOLATE_BYTES: usize = {isolate};\n\
         pub const JS_RESERVE_BYTES: usize = {js};\n\
         pub const JS_RESERVE_MEASURED: bool = {js_measured};\n\
         /// The largest per-invocation JS need the JS reserve was measured\n\
         /// with; an invocation (or plan) needing more is outside the envelope.\n\
         pub const JS_ENVELOPE_BYTES: usize = {js_envelope};\n\
         /// Derived: the fixed wasm32 linear memory (`initial = maximum`),\n\
         /// `floor((isolate_bytes - js_reserve_bytes) / page_bytes)` pages.\n\
         pub const LINEAR_MEMORY_BYTES: usize = {linear_bytes};\n\
         pub const LINEAR_MEMORY_PAGES: usize = {linear_pages};\n\
         pub const GLUE_MEASURED: bool = {glue_measured};\n\
         pub const GLUE_STATIC_BYTES: usize = {glue_static};\n\
         pub const GLUE_TICKET_BYTES: usize = {glue_ticket};\n\
         pub const GLUE_RESIDENT_BYTES: usize = {glue_resident};\n\
         /// Derived: `floor(pool_pages_upper / min_scope_pages)`.\n\
         pub const GLUE_TICKETS: u32 = {tickets};\n\
         /// Derived: equal to `GLUE_TICKETS`.\n\
         pub const GLUE_RESIDENTS: u32 = {residents};\n\
         /// Derived: `glue_static + tickets × ticket + residents × resident`.\n\
         pub const GLUE_BUDGET_BYTES: usize = {glue_bytes};\n\
         pub const GLUE_BUDGET_PAGES: u32 = {glue_pages};\n\
         pub const MIN_ROUTE_BYTES: usize = {min_route};\n\
         pub const LARGEST_ROUTE_BYTES: usize = {largest_route};\n\
         /// The link-time wasm32 stack (outside the pool, below it).\n\
         pub const STACK_BYTES: usize = {stack};\n\
         pub const STACK_MEASURED: bool = {stack_measured};\n\
         /// The deepest measured stack over the model's routes.\n\
         pub const STACK_HIGH_WATER_BYTES: usize = {stack_hw};\n\
         /// Derived: the clippy frame, future and stack-array threshold.\n\
         pub const STACK_FRAME_BOUND_BYTES: usize = {frame_bound};\n",
        frame_bound = (stack / 2).saturating_sub(stack_hw),
        schema = s("schema"),
        version = s("version"),
    );
    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("model.rs");
    fs::write(dest, out).expect("write model.rs");
}
