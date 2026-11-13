//! G-A1 `return_to` bypass fixtures: each is refused or re-serialized to a
//! same-origin path, and every stored value parses back to the configured
//! origin.

use proptest::prelude::*;
use slop_ninja_auth_core::Origin;
use slop_ninja_auth_core::policy::AuthPolicy;
use slop_ninja_auth_core::return_to::{self, Refused, ReturnTo, sanitize, sanitize_or_default};
use url::Url;

fn site() -> Origin {
    Origin::parse("https://site.example").unwrap()
}

fn check(raw: &str) -> Result<ReturnTo, Refused> {
    let r = sanitize(raw, &site(), &AuthPolicy::v1());
    if let Ok(ok) = &r {
        assert_stays_home(ok);
    }
    r
}

/// The stored value, used as the redirect's `Location` and as a relative
/// reference, stays on the site origin.
fn assert_stays_home(r: &ReturnTo) {
    let site = site();
    let root = Url::parse(site.as_str()).unwrap();
    let loc = Url::parse(&r.location(&site)).unwrap();
    assert_eq!(loc.origin(), root.origin(), "{}", r.as_str());
    let joined = root.join(r.as_str()).unwrap();
    assert_eq!(joined.origin(), root.origin(), "{}", r.as_str());
    assert!(r.as_str().starts_with('/') && !r.as_str().starts_with("//"));
    // A stored value passes again unchanged.
    assert_eq!(
        ReturnTo::from_stored(r.as_str(), &site, &AuthPolicy::v1(), false),
        r.clone()
    );
}

#[test]
fn bypass_fixtures_refused() {
    let refused: &[&str] = &[
        "/\\evil.example/",
        "/\\\\evil.example",
        "\\/evil.example",
        "/%5cevil.example",
        "/%5Cevil.example",
        "/%2f%2fevil.example",
        "/%2F/evil.example",
        "//evil.example",
        "///evil.example",
        "/\t/evil.example",
        "/\n/evil.example",
        "/%09/evil.example",
        "/%0a/evil.example",
        "/%0d%0a/evil.example",
        " /detect/",
        "https://evil.example/",
        "HTTPS://site.example.evil.example/",
        "javascript:alert(1)",
        "/admin/",
        "/detect",
        "/account/#frag",
        "https://user@site.example/account/",
        "http://site.example/account/",
        "/detect/\u{85}x",
        "/detect/\u{2028}",
        "/detect/%c2%85",
        "/detect/%ff",
        "/detect/../admin/",
        "/detect/%2e%2e/admin/",
        "",
    ];
    for raw in refused {
        assert!(check(raw).is_err(), "accepted {raw:?}");
    }
}

#[test]
fn length_bound() {
    let max = AuthPolicy::v1().return_to_max.value;
    let fits = format!("/detect/{}", "a".repeat(max - "/detect/".len()));
    assert_eq!(check(&fits).unwrap().as_str(), fits);
    let over = format!("{fits}a");
    assert_eq!(check(&over), Err(Refused::TooLong));
    // Re-serialization that grows past the bound is refused, not cut.
    let grows = format!("/detect/{}", "\"".repeat((max - 8) / 2));
    assert!(grows.len() <= max);
    assert_eq!(check(&grows), Err(Refused::TooLong));
}

#[test]
fn accepted_and_reserialized() {
    for (raw, stored) in [
        ("/", "/"),
        ("/detect/", "/detect/"),
        ("/d/abc1234", "/d/abc1234"),
        ("/account/?tab=keys", "/account/?tab=keys"),
        ("/detect/@evil.example", "/detect/@evil.example"),
        ("https://site.example/account/", "/account/"),
        ("/detect/./x", "/detect/x"),
        ("/detect/a\"b", "/detect/a%22b"),
    ] {
        assert_eq!(check(raw).unwrap().as_str(), stored, "{raw}");
    }
    // Every configured prefix is a destination, and so is a page under it.
    for prefix in AuthPolicy::v1().return_to_paths.value.prefixes {
        assert_eq!(check(&prefix).unwrap().as_str(), prefix);
        let page = format!("{prefix}x?y=1");
        assert_eq!(check(&page).unwrap().as_str(), page);
    }
}

/// The policy's paths, plus hostile and absolute leads.
fn prefixes() -> Vec<String> {
    let p = AuthPolicy::v1().return_to_paths.value;
    let hostile = ["//", "/\\", "https://site.example"].map(str::to_owned);
    p.exact
        .into_iter()
        .chain(p.prefixes)
        .chain(hostile)
        .collect()
}

#[test]
fn defaults_and_query_value() {
    let p = AuthPolicy::v1();
    assert_eq!(
        sanitize_or_default(Some("//evil.example"), &site(), &p, true).as_str(),
        return_to::SIGNED_IN_DEFAULT
    );
    assert_eq!(
        sanitize_or_default(None, &site(), &p, false).as_str(),
        return_to::SIGNED_OUT_DEFAULT
    );
    let r = check("/account/?a=1&b=2").unwrap();
    assert_eq!(r.as_query_value(), "%2Faccount%2F%3Fa%3D1%26b%3D2");
    assert_eq!(r.location(&site()), "https://site.example/account/?a=1&b=2");
}

proptest! {
    /// Whatever the input, an accepted value stays on the site origin.
    #[test]
    fn never_leaves_the_origin(raw in "[ -~\\t\\n\\u{80}-\\u{2030}]{0,40}") {
        if let Ok(r) = sanitize(&raw, &site(), &AuthPolicy::v1()) {
            assert_stays_home(&r);
        }
    }

    /// Prefix-led inputs with hostile tails also stay home.
    #[test]
    fn prefixed_tails(
        prefix in prop::sample::select(prefixes()),
        tail in "[%/\\\\.@:a-z0-9?#]{0,24}",
    ) {
        let raw = format!("{prefix}{tail}");
        if let Ok(r) = sanitize(&raw, &site(), &AuthPolicy::v1()) {
            assert_stays_home(&r);
        }
    }
}
