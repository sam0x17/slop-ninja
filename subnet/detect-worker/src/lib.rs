//! The detect Worker: the service behind the free `slop.ninja/detect/` page.
//!
//! The website Worker forwards `/api/detect/v1/*` here through its service
//! binding; this Worker has no route, custom domain, default `workers_dev` subdomain or
//! preview URL of its own. It scores one pasted text with A's reference
//! package on one scorer Container and returns an evidence band, the two
//! percentages and, in the strong bands only, highlighted passages.
//!
//! - [`bound`]: the request bounds, derived from the scorer's token limit,
//!   the pinned tokenizer and the scorer's NFC tables;
//! - [`envelope`]: the strict request parser (`{"text"}`);
//! - [`xpost`]: rule `x_post_v1`, a pasted link to a post on X;
//! - [`switches`]: the kill switches, the var AND the store's row;
//! - [`english`]: the human pools' English screen, ported;
//! - [`cut`]: the free check's scored part: at most the first 1,000 words,
//!   never cut inside a sentence, within the scorer's tokens;
//! - [`policy`]: length policy, bands, display rules and notices;
//! - [`answer`]: the response allow-list and UTF-16 offsets;
//! - [`receipt`]: content-free feedback receipts and their ledger;
//! - [`seal`]: the result seal on the answer's final line;
//! - [`gate`]: the tickets-only fair queue, its classes and the instance's
//!   warmth;
//! - [`counters`]: the gate's durable hourly counts and their flush;
//! - [`waits`]: the liveness waits and the scan lease derived from them;
//! - [`routes`]: arena routes, footprints, the isolate's scan memory and the
//!   admission of forwarded requests (the `X-SN-*` allow-list and CSRF);
//! - the store (secret links, pages and background work): [`canon`]
//!   (`canon_v1`), [`keys`] (hashes, keys and document encryption),
//!   [`slug`], [`store`] (the state machine), [`doc`] (the stored result and
//!   documents), [`display`] (the display policy), [`page`] (page JSON),
//!   [`db`] (the SQL and I/O seam the flows run on), [`handoff`] (a sealed
//!   free check's result as the store keeps it), [`lookup`], the flows
//!   [`share`], [`manage`] (manage, owner and states) and [`rerun`], the
//!   paid whole-document check [`paid`] (quote, start, the fenced job
//!   driver and how its hold ends) with its [`sections`], the
//!   read routes [`reads`], and the background work [`queue`], [`cron`],
//!   [`sitemap`] and [`store_ops`];
//! - `cf`, `cf_routes`, `cf_store`, `cf_gate`, `cf_feedback`, `cf_lane`
//!   (wasm32): the exports (`fetch`, `queue`, `scheduled`), the store's
//!   routes and its Cloudflare glue, the `DetectGate` object, the
//!   `DetectFeedback` object and the `DetectLane` object with its Container.
//!
//! **Privacy.** The text lives in the request's memory only. Nothing derived
//! from it is stored, logged, cached or returned beyond the allow-list: no
//! hash in the clear (the seal is encrypted), no echo in an error. Panics write nothing: [`silence_panics`]
//! replaces the runtime's hook (which would print the panic message, and a
//! string-slicing panic's message quotes the string) at every entry, and the
//! crate denies the lints that panic on input.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod answer;
pub mod bound;
pub mod canon;
pub mod counters;
pub mod cron;
pub mod cut;
pub mod db;
pub mod display;
pub mod doc;
pub mod english;
pub mod envelope;
pub mod gate;
pub mod handoff;
pub mod keys;
pub mod lookup;
pub mod manage;
pub mod page;
pub mod paid;
pub mod policy;
pub mod queue;
pub mod reads;
pub mod receipt;
pub mod rerun;
pub mod routes;
pub mod seal;
pub mod sections;
pub mod share;
pub mod sitemap;
pub mod slug;
pub mod store;
pub mod store_ops;
pub mod switches;
pub mod waits;
pub mod xpost;

#[cfg(target_arch = "wasm32")]
mod cf;
#[cfg(target_arch = "wasm32")]
mod cf_feedback;
#[cfg(target_arch = "wasm32")]
mod cf_gate;
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../coordinator/src/cf_io.rs"]
mod cf_io;
#[cfg(target_arch = "wasm32")]
mod cf_lane;
// The paid check's routes: quote, start and the job's stream.
#[cfg(target_arch = "wasm32")]
mod cf_paid;
#[cfg(target_arch = "wasm32")]
mod cf_routes;
// The store's Cloudflare glue (D1, R2, the queue and the bindings).
#[cfg(target_arch = "wasm32")]
mod cf_store;

// Native witness tests on the bounded arena: the arena is the test binary's
// global allocator.
#[cfg(all(test, feature = "arena-witness"))]
#[global_allocator]
static WITNESS_ALLOC: slop_ninja_arena::ArenaAlloc = slop_ninja_arena::ArenaAlloc;

thread_local! {
    static SILENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Replaces the panic hook with one that writes nothing, once per instance.
/// workers-rs installs a hook at module start that prints the panic
/// message to the console; every entry point calls this first, so that hook
/// never runs on a request. A panic still aborts the instance, which the
/// runtime answers with an error and a fresh instance.
pub fn silence_panics() {
    SILENT.with(|s| {
        if !s.get() {
            std::panic::set_hook(Box::new(|_| {}));
            s.set(true);
        }
    });
}

/// The public API's path prefix (the website forwards it unchanged).
pub const API_PREFIX: &str = "/api/detect/v1/";
/// The warm route.
pub const WARM_PATH: &str = "/api/detect/v1/warm";
/// The score route.
pub const SCORE_PATH: &str = "/api/detect/v1/score";
/// The feedback route.
pub const FEEDBACK_PATH: &str = "/api/detect/v1/feedback";
/// The binding-only routes' prefix.
pub const INTERNAL_PREFIX: &str = "/internal/";

/// The client key's input: the address's network prefix (IPv4 /24, IPv6
/// /64, the usual unit a single site or subscriber holds), so one network
/// is one turn in the fair queue. Never stored or returned.
pub fn address_prefix(ip: &str) -> Option<String> {
    let ip: std::net::IpAddr = ip.trim().parse().ok()?;
    Some(match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("4:{}.{}.{}", o[0], o[1], o[2])
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("6:{:x}:{:x}:{:x}:{:x}", s[0], s[1], s[2], s[3])
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(address_prefix("203.0.113.9").as_deref(), Some("4:203.0.113"));
        assert_eq!(address_prefix("2001:db8:1:2:3::9").as_deref(), Some("6:2001:db8:1:2"));
        assert_eq!(address_prefix("not an ip"), None);
    }

    /// A forced panic after [`silence_panics`] writes nothing: the test
    /// binary runs itself with a canary panic and reads its stderr.
    #[test]
    fn a_forced_panic_writes_nothing() {
        const CANARY: &str = "canary-text-7f3a9";
        if std::env::var("SLOP_NINJA_DETECT_PANIC_CHILD").is_ok() {
            silence_panics();
            let s = String::from(CANARY);
            // A string-slicing panic, whose default message quotes the string.
            let n = s.len() + 3;
            let _ = std::hint::black_box(&s[..n]);
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args(["--exact", "tests::a_forced_panic_writes_nothing", "--nocapture", "--test-threads=1"])
            .env("SLOP_NINJA_DETECT_PANIC_CHILD", "1")
            .output()
            .unwrap();
        let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success(), "the child must panic");
        assert!(!all.contains(CANARY), "the panic wrote the text: {all}");
        assert!(!all.contains("byte index"), "the panic wrote its message: {all}");
    }
}
