//! The card renderer behind SlopNinja's share cards (`detect-og`).
//!
//! A stateless Worker with no route and no binding of its own: the website
//! and the secret-host Worker reach it through their `DETECT_OG` service
//! bindings with `POST /render {card}` ([`wire`]) and get the 1200 x 630 PNG
//! of that card. The card JSON is `slop_ninja_detect_page::Card`, which the
//! detect Worker builds from the page; a secret page's card carries no slug,
//! so its PNG prints the secret host and nothing after it.
//!
//! - [`card`]: the template, one SVG document per card, laid out as the
//!   detection pages' header block;
//! - [`layout`]: line breaking and fitting with the embedded fonts' metrics;
//! - [`fonts`]: the Inter subset (SIL Open Font License 1.1) and its metrics;
//! - [`art`]: the ninja states, gauge busts and wordmark from
//!   `assets/brand/`, embedded at build time;
//! - [`render`]: resvg rasterization and PNG encoding, with no file, font or
//!   network access;
//! - [`routes`]: the arena routes and footprints (the bounded-arena rule);
//! - `cf` (wasm32 Worker builds): the `fetch` export.
//!
//! **Privacy.** A card holds no text from the page. Nothing is logged,
//! cached or stored here; panics write nothing ([`silence_panics`]).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod art;
pub mod card;
pub mod fonts;
pub mod layout;
pub mod render;
pub mod routes;
pub mod svg;
pub mod wire;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod cf;
// The coordinator's bounded body reader, shared by path as the fetch Worker
// shares it (its file keeps the coordinator's own formatting).
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../coordinator/src/cf_io.rs"]
mod cf_io;

pub use render::{RenderError, card_png};

// Native witness tests on the bounded arena: the arena is the test binary's
// global allocator (see `arena_witness`).
#[cfg(all(test, feature = "arena-witness"))]
#[global_allocator]
static WITNESS_ALLOC: slop_ninja_arena::ArenaAlloc = slop_ninja_arena::ArenaAlloc;
#[cfg(all(test, feature = "arena-witness"))]
mod arena_witness;

thread_local! {
    static SILENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Replaces the panic hook with one that writes nothing, once per instance.
/// workers-rs installs a hook at module start that prints the panic
/// message; every entry calls this first. A panic still aborts the instance.
pub fn silence_panics() {
    SILENT.with(|s| {
        if !s.get() {
            std::panic::set_hook(Box::new(|_| {}));
            s.set(true);
        }
    });
}
