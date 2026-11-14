//! Source tests of the URL pipeline: its bindings in both envs, the X
//! sweep's call shape, and what never appears in its code.

use std::fs;

fn src(f: &str) -> String {
    fs::read_to_string(format!("{}/src/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn both_envs_bind_the_host_object_and_the_fetch_worker() {
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    for env in ["testnet", "staging"] {
        let host =
            format!("[[env.{env}.durable_objects.bindings]]\nname = \"DETECT_HOST\"\nclass_name = \"DetectHost\"");
        assert!(w.contains(&host), "{env}: DETECT_HOST");
        let mig = format!("[[env.{env}.migrations]]\ntag = \"v2\"\nnew_sqlite_classes = [\"DetectHost\"]");
        assert!(w.contains(&mig), "{env}: migration v2");
        let svc =
            format!("[[env.{env}.services]]\nbinding = \"DETECT_FETCH\"\nservice = \"slop-ninja-detect-fetch-{env}\"");
        assert!(w.contains(&svc), "{env}: DETECT_FETCH");
    }
    // The URL route's own glue names the same bindings.
    let glue = src("cf_url.rs");
    assert!(glue.contains("\"DETECT_FETCH\"") && glue.contains("\"DETECT_HOST\""));
}

#[test]
fn the_url_flow_keeps_no_text_and_no_x_markup() {
    // The flows write no X field beyond the post's canonical URL, and keep
    // no page text: the run document is built by `excerpt`.
    let flow = src("url_flow.rs");
    for banned in ["author_name", "blockquote", "profile_image", "handle", "\"html\""] {
        assert!(!flow.contains(banned), "url_flow names {banned}");
    }
    let x = src("x_flow.rs");
    for banned in ["X_CONTROL_ID", "X_GONE_CONTROL_ID", "publish.x.com"] {
        assert!(!x.contains(banned), "the detect Worker never holds {banned}: detect-fetch does");
    }
    // No X request happens outside the fetch Worker's two routes.
    for f in ["url_flow.rs", "x_flow.rs", "cf_url.rs", "cf_host.rs", "host.rs"] {
        let s = src(f);
        assert!(!s.contains("x.com/oembed") && !s.contains("api.x.com"), "{f}");
    }
}

#[test]
fn every_x_point_carries_no_slug_or_id() {
    // Each xread, xcheck and xsweep point is written with a fixed code
    // literal as its one blob.
    for f in ["url_flow.rs", "x_flow.rs"] {
        let s = src(f);
        for (i, _) in s.match_indices("metric(\"x").chain(s.match_indices("metric_n(\"x")) {
            let line = s[i..].lines().next().unwrap();
            assert!(
                line.contains("&[\"")
                    || line.contains("&[o]")
                    || line.contains("&[outcome]")
                    || line.contains("&[step]"),
                "{f}: {line}"
            );
        }
    }
}
