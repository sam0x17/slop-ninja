//! The build's memory model, compiled from `memory-model.json`
//! (`slop_ninja_worker_memory_v2`) by `build.rs`.
//!
//! The model is a build parameter, not a plan parameter: its ceiling applies to
//! every route of the isolate, including routes that have no plan. Nothing in
//! it names or gates on a chain runtime version. Its derived constants (glue
//! tickets, residents and partition) are computed by `build.rs` from measured
//! inputs, each of which records its source in the JSON.

include!(concat!(env!("OUT_DIR"), "/model.rs"));

/// Heap ceiling for a module whose static data and stack end at
/// `static_bytes`: `isolate_bytes - static_bytes - js_reserve_bytes`.
pub const fn heap_ceiling(static_bytes: usize) -> usize {
    ISOLATE_BYTES.saturating_sub(static_bytes).saturating_sub(JS_RESERVE_BYTES)
}

/// Pool pages for that ceiling (glue partition included):
/// `floor(heap_ceiling / page_bytes)`.
pub const fn pool_pages(static_bytes: usize) -> usize {
    heap_ceiling(static_bytes) / PAGE_BYTES
}
