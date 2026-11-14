//! G-A1: the CSRF function over the Fetch Metadata and `Origin` matrix,
//! including a secret-host page posting to the site host.

use slop_ninja_auth_core::csrf::{
    BodyType, CONTENT_TYPE_REFUSAL, CsrfRefusal, applies, check, check_route, content_type_ok,
};
use slop_ninja_auth_core::detect_page::sn::{Callee, Hosts};
use slop_ninja_auth_core::{HostRole, Origins, RefusalCode, RouteId};

const SITE: &str = "https://site.example";
const SECRET: &str = "https://s.site.example";

fn origins() -> Origins {
    Origins::from_vars(SITE, SECRET).unwrap()
}

/// The rule, stated independently of the implementation: a `POST` passes
/// with the host's own `Origin`, or with `Origin: null` or none when the
/// browser says `same-origin`; any other `Sec-Fetch-Site`, another origin,
/// and neither header are refused.
fn expected(method: &str, sfs: Option<&str>, origin: Option<&str>, host: &str) -> bool {
    match method {
        "GET" | "HEAD" => true,
        "POST" => match sfs {
            Some("same-origin") => matches!(origin, None | Some("null")) || origin == Some(host),
            Some(_) => false,
            None => origin == Some(host),
        },
        _ => false,
    }
}

#[test]
fn full_matrix() {
    let o = origins();
    let methods = [
        "GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "post",
    ];
    let fetch_sites = [
        None,
        Some("same-origin"),
        Some("same-site"),
        Some("cross-site"),
        Some("none"),
        Some("SAME-ORIGIN"),
        Some(""),
    ];
    let origin_values = [
        None,
        Some("null"),
        Some(SITE),
        Some(SECRET),
        Some("https://evil.example"),
        Some("https://site.example/"),
        Some("https://SITE.example"),
        Some("http://site.example"),
        Some("https://site.example:443"),
        Some("https://site.example.evil.example"),
        Some(""),
    ];
    let mut cases = 0;
    for (host_name, host) in [(SITE, &o.site), (SECRET, &o.secret)] {
        for m in methods {
            for sfs in fetch_sites {
                for org in origin_values {
                    let got = check(m, sfs, org, host);
                    let want = expected(m, sfs, org, host_name);
                    assert_eq!(
                        got.is_ok(),
                        want,
                        "{m} sfs={sfs:?} origin={org:?} host={host_name}"
                    );
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 2 * 8 * 7 * 11);
}

#[test]
fn named_cases() {
    let o = origins();
    // A page on the secret host posting to the site host: same-site, not
    // same-origin.
    assert_eq!(
        check("POST", Some("same-site"), Some(SECRET), &o.site),
        Err(CsrfRefusal::FetchSite)
    );
    // The same without Fetch Metadata (an older browser).
    assert_eq!(
        check("POST", None, Some(SECRET), &o.site),
        Err(CsrfRefusal::Origin)
    );
    // And the reverse.
    assert_eq!(
        check("POST", Some("same-site"), Some(SITE), &o.secret),
        Err(CsrfRefusal::FetchSite)
    );
    // Cross-site, `Origin: null`, a missing Origin.
    assert_eq!(
        check(
            "POST",
            Some("cross-site"),
            Some("https://evil.example"),
            &o.site
        ),
        Err(CsrfRefusal::FetchSite)
    );
    assert_eq!(
        check("POST", None, Some("null"), &o.site),
        Err(CsrfRefusal::Origin)
    );
    assert_eq!(check("POST", None, None, &o.site), Err(CsrfRefusal::Origin));
    // Fetch Metadata says same-origin but Origin disagrees.
    assert_eq!(
        check("POST", Some("same-origin"), Some(SECRET), &o.site),
        Err(CsrfRefusal::Origin)
    );
    // The good forms, `Origin: null` under `same-origin` among them (a
    // form post from a page served with `Referrer-Policy: no-referrer`).
    assert_eq!(
        check("POST", Some("same-origin"), Some(SITE), &o.site),
        Ok(())
    );
    assert_eq!(check("POST", Some("same-origin"), None, &o.site), Ok(()));
    assert_eq!(
        check("POST", Some("same-origin"), Some("null"), &o.site),
        Ok(())
    );
    assert_eq!(
        check("POST", Some("same-origin"), Some("null"), &o.secret),
        Ok(())
    );
    assert_eq!(check("POST", None, Some(SITE), &o.site), Ok(()));
    assert_eq!(
        check("POST", Some("same-origin"), Some(SECRET), &o.secret),
        Ok(())
    );
    assert_eq!(
        check("PUT", Some("same-origin"), Some(SITE), &o.site),
        Err(CsrfRefusal::Method)
    );
}

/// Each case of the rule for state-changing requests, one by one: what a
/// browser sends for a form post from a page with each referrer policy, and
/// what other pages and tools send.
#[test]
fn state_changing_requests_case_by_case() {
    let o = origins();
    let post = |sfs: Option<&str>, origin: Option<&str>| check("POST", sfs, origin, &o.site);
    // Accepted: the configured origin itself (any referrer policy but
    // no-referrer, and fetch() posts under no-referrer)...
    assert_eq!(
        post(Some("same-origin"), Some(SITE)),
        Ok(()),
        "same-origin, own origin"
    );
    assert_eq!(
        post(None, Some(SITE)),
        Ok(()),
        "no Fetch Metadata, own origin"
    );
    // ...and `null` or no Origin when the browser says same-origin (a form
    // post from a no-referrer page).
    assert_eq!(
        post(Some("same-origin"), Some("null")),
        Ok(()),
        "same-origin, Origin: null"
    );
    assert_eq!(
        post(Some("same-origin"), None),
        Ok(()),
        "same-origin, no Origin"
    );
    // Refused: cross-site, same-site and none, whatever the Origin says.
    for sfs in ["cross-site", "same-site", "none"] {
        for origin in [
            None,
            Some("null"),
            Some(SITE),
            Some(SECRET),
            Some("https://evil.example"),
        ] {
            assert_eq!(
                post(Some(sfs), origin),
                Err(CsrfRefusal::FetchSite),
                "{sfs} {origin:?}"
            );
        }
    }
    // Refused: same-origin metadata with another origin (the secret host,
    // another site), and `null`, another origin or nothing without metadata.
    assert_eq!(
        post(Some("same-origin"), Some(SECRET)),
        Err(CsrfRefusal::Origin)
    );
    assert_eq!(
        post(Some("same-origin"), Some("https://evil.example")),
        Err(CsrfRefusal::Origin)
    );
    assert_eq!(
        post(None, Some("null")),
        Err(CsrfRefusal::Origin),
        "null without metadata"
    );
    assert_eq!(post(None, Some(SECRET)), Err(CsrfRefusal::Origin));
    assert_eq!(post(None, None), Err(CsrfRefusal::Origin), "missing both");
    // The value is exact: no case folding of the metadata or the null.
    assert_eq!(
        post(Some("SAME-ORIGIN"), Some("null")),
        Err(CsrfRefusal::FetchSite)
    );
    assert_eq!(
        post(Some("same-origin"), Some("NULL")),
        Err(CsrfRefusal::Origin)
    );
    assert_eq!(
        post(Some("same-origin"), Some("")),
        Err(CsrfRefusal::Origin)
    );
    // Every refusal is the route's `csrf` code (403).
    assert_eq!(CsrfRefusal::FetchSite.code(), RefusalCode::Csrf);
    assert_eq!(CsrfRefusal::Origin.code(), RefusalCode::Csrf);
}

#[test]
fn scope() {
    let o = origins();
    // The scope is the forwarded route table's `csrf` column: every browser
    // POST is marked, and the op routes, the paid API and webhooks are not.
    let unmarked = [
        RouteId::DetectOp,
        RouteId::PaidApi,
        RouteId::AccountOp,
        RouteId::BillingWebhook,
    ];
    for &route in RouteId::ALL {
        let spec = route.spec();
        if unmarked.contains(&route) {
            assert!(!applies(route), "{route:?}");
            // Their own authentication decides; this check passes them.
            for host in [HostRole::Site, HostRole::Secret] {
                assert_eq!(
                    check_route(route, host, &o, "POST", Some("cross-site"), None),
                    Ok(()),
                    "{route:?}"
                );
            }
        } else if spec.method == "POST" {
            assert!(applies(route), "{route:?}");
            for host in [HostRole::Site, HostRole::Secret] {
                if !spec.hosts.admits(host) {
                    continue;
                }
                let own = o.of(host).as_str();
                assert_eq!(
                    check_route(route, host, &o, "POST", Some("cross-site"), None),
                    Err(CsrfRefusal::FetchSite),
                    "{route:?}"
                );
                assert_eq!(
                    check_route(route, host, &o, "POST", Some("same-origin"), Some(own)),
                    Ok(()),
                    "{route:?}"
                );
                // A form post from a page that sends no referrer.
                assert_eq!(
                    check_route(route, host, &o, "POST", Some("same-origin"), Some("null")),
                    Ok(()),
                    "{route:?}"
                );
                assert_eq!(
                    check_route(route, host, &o, "POST", None, Some("null")),
                    Err(CsrfRefusal::Origin),
                    "{route:?}"
                );
            }
        }
    }
    // Every account route that changes state is a marked POST.
    for &route in RouteId::ALL {
        let spec = route.spec();
        if spec.callee == Callee::Account && spec.method == "POST" && !unmarked.contains(&route) {
            assert!(applies(route), "{route:?}");
        }
    }
    // A secret-host page posting to an account route on the site host: the
    // route checks against the site's origin and refuses it.
    assert_eq!(
        check_route(
            RouteId::EmailStart,
            HostRole::Site,
            &o,
            "POST",
            Some("same-site"),
            Some(SECRET)
        ),
        Err(CsrfRefusal::FetchSite)
    );
    assert_eq!(
        check_route(
            RouteId::LinkConfirm,
            HostRole::Site,
            &o,
            "POST",
            None,
            Some(SECRET)
        ),
        Err(CsrfRefusal::Origin)
    );
    // A route on both hosts checks against the host it arrived on.
    assert_eq!(RouteId::Feedback.spec().hosts, Hosts::Both);
    assert_eq!(
        check_route(
            RouteId::Feedback,
            HostRole::Secret,
            &o,
            "POST",
            None,
            Some(SECRET)
        ),
        Ok(())
    );
    assert_eq!(
        check_route(
            RouteId::Feedback,
            HostRole::Secret,
            &o,
            "POST",
            None,
            Some(SITE)
        ),
        Err(CsrfRefusal::Origin)
    );
}

#[test]
fn refusal_codes() {
    assert_eq!(CsrfRefusal::FetchSite.code(), RefusalCode::Csrf);
    assert_eq!(CsrfRefusal::Origin.code(), RefusalCode::Csrf);
    assert_eq!(CsrfRefusal::Method.code(), RefusalCode::Method);
    assert_eq!(RefusalCode::Csrf.status(), 403);
    assert_eq!(RefusalCode::Method.status(), 405);
    assert_eq!(CONTENT_TYPE_REFUSAL.status(), 415);
}

#[test]
fn content_types() {
    assert!(content_type_ok(Some("application/json"), BodyType::Json));
    assert!(content_type_ok(
        Some("application/json; charset=utf-8"),
        BodyType::Json
    ));
    assert!(content_type_ok(Some("Application/JSON"), BodyType::Json));
    assert!(!content_type_ok(Some("text/plain"), BodyType::Json));
    assert!(!content_type_ok(
        Some("application/json-patch+json"),
        BodyType::Json
    ));
    assert!(!content_type_ok(None, BodyType::Json));
    assert!(content_type_ok(
        Some("application/x-www-form-urlencoded"),
        BodyType::Form
    ));
    assert!(!content_type_ok(
        Some("multipart/form-data; boundary=x"),
        BodyType::Form
    ));
    assert!(!content_type_ok(Some("application/json"), BodyType::Form));
}

/// The README's `csrf` row states the whole rule: every case `check`
/// refuses is named there, `Origin: null` without Fetch Metadata among them.
#[test]
fn the_readme_names_every_refused_case() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md");
    let row = readme
        .lines()
        .find(|l| l.starts_with("| `csrf` |"))
        .expect("the csrf row");
    let refused = &row[row.find("; `cross-site`").expect("the refused list")..];
    let o = origins();
    for (named, sfs, origin) in [
        ("`cross-site`", Some("cross-site"), Some(SITE)),
        ("`same-site`", Some("same-site"), Some(SITE)),
        ("`none`", Some("none"), Some(SITE)),
        (
            "another origin",
            Some("same-origin"),
            Some("https://evil.example"),
        ),
        ("`Origin: null` without Fetch Metadata", None, Some("null")),
        ("neither header", None, None),
    ] {
        assert!(
            refused.contains(named),
            "the README's refused list names {named}"
        );
        assert!(
            check("POST", sfs, origin, &o.site).is_err(),
            "{named} is refused"
        );
    }
}
