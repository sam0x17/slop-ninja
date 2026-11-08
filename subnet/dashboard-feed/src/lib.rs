//! The SlopNinja dashboard feed: a private, read-only Worker that serves
//! the publisher's `dashboard/v1` documents from a dedicated R2 bucket to the
//! website Worker, which reaches it only through a service binding. It has
//! no route, no workers.dev name and no preview URLs, so no other caller can
//! reach it.
//!
//! The Worker answers `GET` and `HEAD` on the exact `dashboard/v1` object
//! paths ([`contract::Doc::parse_key`]) and nothing else: any other method
//! is a 405, any other path (or a path with a query string) an empty 404. The
//! R2 key is rebuilt from the parsed parts, never taken from the raw path.
//! An object larger than its kind's bound ([`contract::Doc::bound`]) is a
//! 502 with no body, so a document the publisher should have refused never
//! reaches the website (whose own bound check stays in place). The body
//! streams from R2 to the response inside the runtime and never passes
//! through Wasm memory.
//!
//! The crate never writes, deletes or lists objects, makes no outbound
//! fetch, reads no secret or variable and has no other binding
//! (`tests/source.rs` checks the sources for those calls). All logic except
//! `cf.rs` is plain Rust and tested natively; `cf.rs` holds the Workers
//! entry point and compiles only for `wasm32`.

pub mod contract;

#[cfg(target_arch = "wasm32")]
mod cf;

use contract::Doc;

/// The bucket binding the feed reads (testnet; a mainnet deployment would
/// bind its own bucket under its own name).
pub const BUCKET: &str = "DASHBOARD_TESTNET";

/// What a request asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// A `dashboard/v1` object; `head` for a `HEAD` request.
    Object { doc: Doc, head: bool },
    /// Outside the grammar: an empty 404.
    NotFound,
    /// Not `GET` or `HEAD`: a 405 with `Allow: GET, HEAD`.
    MethodNotAllowed,
}

/// Routes a request by its method, its URL path (with the leading slash)
/// and whether its URL carries a query string (`?`, even an empty one).
pub fn route(method: &str, path: &str, has_query: bool) -> Route {
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Route::MethodNotAllowed,
    };
    if has_query {
        return Route::NotFound;
    }
    match path.strip_prefix('/').and_then(Doc::parse_key) {
        Some(doc) => Route::Object { doc, head },
        None => Route::NotFound,
    }
}

/// What the bucket said about the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    Present { size: u64 },
    Missing,
    Error,
}

/// The status of a served object: 200 within its bound, 502 over it (no
/// body), 404 when missing, 503 on a bucket error.
pub fn status(doc: &Doc, stored: Stored) -> u16 {
    match stored {
        Stored::Present { size } if size <= doc.bound() as u64 => 200,
        Stored::Present { .. } => 502,
        Stored::Missing => 404,
        Stored::Error => 503,
    }
}

/// The headers of a 200 answer. No CORS headers, no cookies, no R2 custom
/// metadata.
pub fn headers(doc: &Doc) -> [(&'static str, String); 3] {
    [
        ("content-type", "application/json".to_owned()),
        ("cache-control", format!("public, max-age={}", doc.max_age())),
        ("x-content-type-options", "nosniff".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_the_dashboard_grammar() {
        assert_eq!(
            route("GET", "/dashboard/v1/m/a/now.json", false),
            Route::Object { doc: Doc::Now("a"), head: false }
        );
        assert_eq!(
            route("HEAD", "/dashboard/v1/m/a/cohorts/testnet-r0-c3.json", false),
            Route::Object { doc: Doc::Cohort("a", "testnet-r0-c3".into()), head: true }
        );
        assert_eq!(
            route("GET", "/dashboard/v1/m/a/months/2026-11.json", false),
            Route::Object { doc: Doc::Month("a", 2026, 11), head: false }
        );
        // Rule v3: the registry and the per-cohort rosters.
        assert_eq!(
            route("GET", "/dashboard/v1/registry.json", false),
            Route::Object { doc: Doc::Registry, head: false }
        );
        assert_eq!(
            route("HEAD", "/dashboard/v1/m/a/rosters/testnet-r0-20.json", false),
            Route::Object { doc: Doc::Roster("a", "testnet-r0-20".into()), head: true }
        );
        for m in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS", "get"] {
            assert_eq!(route(m, "/dashboard/v1/index.json", false), Route::MethodNotAllowed, "{m}");
        }
        for bad in [
            "/",
            "/dashboard/v1/",
            "dashboard/v1/index.json",
            "//dashboard/v1/index.json",
            "/dashboard/v1/m/a/cohorts/index.json/",
            "/dashboard/v1/m/a/cohorts/..%2Fnow.json",
            "/dashboard/v1/m/a/cohorts/%2e%2e.json",
            "/dashboard/v1/m/a/cohorts/a%2Fb.json",
            "/dashboard/v1/m/a/cohorts/../../v1/status.json",
            "/dashboard/v1/m/a/cohorts/index.json",
            "/dashboard/v1/m/z/now.json",
            "/dashboard/v1/m/a/months/2026-13.json",
            "/dashboard/v1/m/a/rosters/index.json",
            "/dashboard/v1/m/a/rosters/../now.json",
            "/dashboard/v1/m/a/registry.json",
            "/v1/status.json",
            "/v1/cohorts/index.json",
            "/index.html",
        ] {
            let r = route("GET", bad, false);
            // The cohorts index is a document of its own, not a label.
            if bad == "/dashboard/v1/m/a/cohorts/index.json" {
                assert_eq!(r, Route::Object { doc: Doc::Cohorts("a"), head: false });
            } else {
                assert_eq!(r, Route::NotFound, "{bad}");
            }
        }
        assert_eq!(route("GET", "/dashboard/v1/index.json", true), Route::NotFound);
        // `index` is never a cohort label.
        assert_eq!(Doc::parse_key("dashboard/v1/m/a/cohorts/index.json"), Some(Doc::Cohorts("a")));
    }

    #[test]
    fn serves_within_the_bound_only() {
        let cohort = Doc::Cohort("a", "c1".into());
        let bound = cohort.bound() as u64;
        assert_eq!(status(&cohort, Stored::Present { size: bound }), 200);
        assert_eq!(status(&cohort, Stored::Present { size: bound + 1 }), 502);
        let now = Doc::Now("a");
        // Rule v2: the small bound plus a full UID map.
        assert_eq!(status(&now, Stored::Present { size: 64 * 1024 + 1 }), 200);
        assert_eq!(status(&now, Stored::Present { size: crate::contract::bounds::NOW as u64 }), 200);
        assert_eq!(status(&now, Stored::Present { size: crate::contract::bounds::NOW as u64 + 1 }), 502);
        let index = Doc::Index;
        assert_eq!(status(&index, Stored::Present { size: 64 * 1024 + 1 }), 502);
        assert_eq!(status(&now, Stored::Missing), 404);
        assert_eq!(status(&now, Stored::Error), 503);
        assert_eq!(headers(&now)[1].1, "public, max-age=60");
        assert_eq!(headers(&cohort)[1].1, "public, max-age=300");
        assert_eq!(headers(&cohort)[0].1, "application/json");
        let roster = Doc::Roster("a", "c1".into());
        assert_eq!(status(&roster, Stored::Present { size: crate::contract::bounds::ROSTER_STATUS as u64 }), 200);
        assert_eq!(status(&roster, Stored::Present { size: crate::contract::bounds::ROSTER_STATUS as u64 + 1 }), 502);
        assert_eq!(headers(&roster)[1].1, "public, max-age=60");
        assert_eq!(status(&Doc::Registry, Stored::Present { size: crate::contract::bounds::REGISTRY as u64 + 1 }), 502);
    }
}
