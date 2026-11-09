//! Source checks for the privacy and deployment rules that no unit test of
//! a pure function can see: nothing writes to the console, every entry
//! point silences panics first, the Worker has no way in but the service
//! binding and no logging sink, and every resource is a testnet resource.

use std::fs;

fn src(name: &str) -> String {
    fs::read_to_string(format!("{}/src/{name}", env!("CARGO_MANIFEST_DIR"))).expect(name)
}

const SOURCES: &[&str] = &[
    "lib.rs",
    "bound.rs",
    "english.rs",
    "envelope.rs",
    "policy.rs",
    "answer.rs",
    "receipt.rs",
    "gate.rs",
    "routes.rs",
    "cf.rs",
    "cf_gate.rs",
    "cf_lane.rs",
];

#[test]
fn nothing_writes_to_the_console() {
    for name in SOURCES {
        let s = src(name);
        for banned in [
            "console_error_panic_hook",
            "console_log!",
            "console_error!",
            "console_warn!",
            "console_debug!",
            "web_sys::console",
            "console.",
            "println!",
            "eprintln!",
            "dbg!",
        ] {
            assert!(!s.contains(banned), "{name} uses {banned}");
        }
    }
}

/// The body of the function starting at `sig`, up to its first statement.
fn first_statement(s: &str, sig: &str) -> String {
    let at = s.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
    let open = at + s[at..].find('{').expect("body");
    s[open + 1..].trim_start().lines().next().unwrap_or("").trim().to_owned()
}

#[test]
fn every_entry_point_silences_panics_first() {
    let cf = src("cf.rs");
    assert_eq!(first_statement(&cf, "pub fn fetch("), "silence_panics();");
    for name in ["cf_gate.rs", "cf_lane.rs"] {
        let s = src(name);
        assert_eq!(first_statement(&s, "fn new(state: State, env: Env)"), "silence_panics();", "{name} new");
        assert_eq!(first_statement(&s, "async fn fetch("), "silence_panics();", "{name} fetch");
    }
    assert_eq!(first_statement(&src("cf_lane.rs"), "async fn alarm("), "silence_panics();");
    let lib = src("lib.rs");
    assert!(lib.contains("std::panic::set_hook(Box::new(|_| {}))"), "the hook writes nothing");
}

#[test]
fn the_allow_list_reads_no_hash_or_attribution() {
    let s = src("answer.rs");
    let structs = &s[..s.find("#[cfg(test)]").expect("tests")];
    for field in ["text_sha256:", "output_sha256:", "attribution:", "origin:", "sentences:", "ms:"] {
        assert!(!structs.contains(field), "answer.rs reads {field}");
    }
}

#[test]
fn the_worker_has_no_way_in_but_the_binding_and_no_log_sink() {
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).expect("wrangler.toml");
    let lines: Vec<&str> = w.lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.is_empty()).collect();
    for banned in ["route", "routes", "custom_domain", "tail_consumers", "logpush", "account_id"] {
        assert!(!lines.iter().any(|l| l.starts_with(banned)), "wrangler.toml sets {banned}");
    }
    assert!(lines.iter().filter(|l| l.starts_with("workers_dev")).all(|l| *l == "workers_dev = false"));
    assert!(lines.iter().filter(|l| l.starts_with("preview_urls")).all(|l| *l == "preview_urls = false"));
    assert!(lines.iter().filter(|l| l.starts_with("enabled")).all(|l| *l == "enabled = false"), "observability off");
}

#[test]
fn every_resource_is_a_testnet_resource() {
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).expect("wrangler.toml");
    let testnet = &w[w.find("[env.testnet]").expect("testnet env")..];
    for l in testnet.lines().map(str::trim) {
        if let Some(v) = l.strip_prefix("name = \"").or_else(|| l.strip_prefix("bucket_name = \"")) {
            let v = v.trim_end_matches('"');
            if v.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                continue; // a binding name
            }
            assert!(v.ends_with("-testnet"), "{v}");
        }
        if let Some(v) = l.strip_prefix("dataset = \"") {
            assert!(v.trim_end_matches('"').ends_with("_testnet"), "{v}");
        }
    }
    assert!(testnet.contains("NETWORK = \"testnet\""));
    assert!(testnet.contains("PRODUCT = \"detect\""));
    assert!(testnet.contains("max_instances = 1"));
    assert!(testnet.contains("instance_type = \"standard-3\""));
}

#[test]
fn the_site_key_and_secrets_are_not_tracked() {
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).expect("wrangler.toml");
    for l in w.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        assert!(!l.starts_with("TURNSTILE_SITE_KEY"), "the site key is passed at deploy time");
        assert!(!l.starts_with("TURNSTILE_SECRET"), "the secret is a Worker secret");
    }
}
