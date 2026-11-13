//! G-A1: the CSRF function over the Fetch Metadata and `Origin` matrix,
//! including a secret-host page posting to the site host.

use slop_ninja_auth_core::csrf::{
    BodyType, CsrfRefusal, RequestClass, applies, check, check_for, content_type_ok,
};
use slop_ninja_auth_core::origin::AuthOrigins;

const SITE: &str = "https://site.example";
const SECRET: &str = "https://s.site.example";

fn origins() -> AuthOrigins {
    AuthOrigins::parse(SITE, SECRET).unwrap()
}

/// The rule, stated independently of the implementation.
fn expected(method: &str, sfs: Option<&str>, origin: Option<&str>, host: &str) -> bool {
    match method {
        "GET" | "HEAD" => true,
        "POST" => match sfs {
            Some("same-origin") => origin.is_none_or(|o| o == host),
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
    assert_eq!(
        check("POST", Some("same-origin"), Some("null"), &o.site),
        Err(CsrfRefusal::Origin)
    );
    // The good forms.
    assert_eq!(
        check("POST", Some("same-origin"), Some(SITE), &o.site),
        Ok(())
    );
    assert_eq!(check("POST", Some("same-origin"), None, &o.site), Ok(()));
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

#[test]
fn scope() {
    let o = origins();
    for class in [RequestClass::ForwardedBrowser, RequestClass::AccountBrowser] {
        assert!(applies(class));
        assert_eq!(
            check_for(class, "POST", Some("cross-site"), None, &o.site),
            Err(CsrfRefusal::FetchSite)
        );
    }
    for class in [
        RequestClass::Internal,
        RequestClass::Op,
        RequestClass::Api,
        RequestClass::Webhook,
    ] {
        assert!(!applies(class));
        assert_eq!(check_for(class, "POST", None, None, &o.site), Ok(()));
    }
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
