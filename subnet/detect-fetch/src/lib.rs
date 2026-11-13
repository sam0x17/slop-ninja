//! The fetch Worker behind the public URL check (release R2).
//!
//! The detect Worker reaches it only through its service binding
//! `DETECT_FETCH` (no route, custom domain, default `workers_dev` subdomain or preview
//! URL, and no binding of its own: outbound Fetch only, never a Tunnel or
//! VPC binding, so it reaches no private network; risk F4). It keeps
//! nothing between requests. Its contract is `slop-ninja-detect-fetch-wire`:
//!
//! - `POST /robots`: one robots.txt read for an origin under RFC 9309
//!   ([`robots`]), parsed into the group that applies to SlopNinjaBot;
//! - `POST /fetch`: one page under the politeness slot the detect Worker's
//!   `DetectHost` object handed out ([`fetch`]): `GET` only, no cookies, no
//!   referrer, the fixed `Accept`, the SlopNinjaBot user agent and Web Bot
//!   Auth signatures ([`sign`]); manual redirects, each hop re-checked by the
//!   URL rules, re-screened for malware and phishing ([`screen`]) and, on a
//!   new origin, checked against that origin's robots.txt, with an `https`
//!   to `http` downgrade ending the chain ([`redirects`]); the body read as
//!   a counted stream through the strip pass ([`strip`]: lol_html, the depth
//!   and element bounds, the page's metadata and robots directives) into at
//!   most `HTML_BOUND` bytes ([`bound`]), then main-text extraction
//!   ([`extract`]: dom_smoothie); Bluesky posts through the public AppView
//!   ([`bluesky`]).
//!
//! **Memory** (Δ21). Every invocation runs in its own bounded arena scope
//! ([`routes`]); the body-dependent work is reserved from the isolate's
//! fetch memory by its measured footprint ([`bound`], checked by the native
//! arena witnesses), the extraction stack's lazy statics live in one home
//! scope, and an isolate whose linear memory grew past half its ceiling is
//! recycled at its next idle entry (risk F14).
//!
//! **Privacy.** Nothing is logged, cached or stored. Panics write nothing
//! ([`silence_panics`]). Outcome codes are content-free; no answer echoes a
//! failed URL.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]
// The pipeline's traits are used only by this crate (generic, never `dyn`).
#![allow(async_fn_in_trait)]

pub mod bluesky;
pub mod bound;
pub mod extract;
pub mod fetch;
pub mod fresh;
pub mod net;
pub mod redirects;
pub mod robots;
pub mod routes;
pub mod screen;
pub mod sign;
pub mod strip;

#[cfg(target_arch = "wasm32")]
mod cf;
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../coordinator/src/cf_io.rs"]
mod cf_io;

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
/// message; every entry calls this first, so a panic never writes a URL or
/// page text anywhere. A panic still aborts the instance.
pub fn silence_panics() {
    SILENT.with(|s| {
        if !s.get() {
            std::panic::set_hook(Box::new(|_| {}));
            s.set(true);
        }
    });
}

/// The deployment environment names (`DEPLOY_ENV`).
pub const ENV_STAGING: &str = "staging";
pub const ENV_PRODUCTION: &str = "production";

/// Minimal executor for the native tests: the in-process fakes never pend.
#[cfg(test)]
pub(crate) fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A forced panic after [`silence_panics`] writes nothing.
    #[test]
    fn a_forced_panic_writes_nothing() {
        const CANARY: &str = "canary-url-4e1b";
        if std::env::var("SLOP_NINJA_FETCH_PANIC_CHILD").is_ok() {
            silence_panics();
            let s = String::from(CANARY);
            let n = s.len() + 3;
            let _ = std::hint::black_box(&s[..n]);
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args(["--exact", "tests::a_forced_panic_writes_nothing", "--nocapture", "--test-threads=1"])
            .env("SLOP_NINJA_FETCH_PANIC_CHILD", "1")
            .output()
            .unwrap();
        let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success(), "the child must panic");
        assert!(!all.contains(CANARY), "the panic wrote the text: {all}");
    }
}
