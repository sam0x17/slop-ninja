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
    "waits.rs",
    "cf.rs",
    "cf_gate.rs",
    "cf_feedback.rs",
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
    for name in ["cf_gate.rs", "cf_feedback.rs", "cf_lane.rs"] {
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

/// The body of the function starting at `sig`, to its closing brace at the
/// start of a line.
fn function_body<'a>(s: &'a str, sig: &str) -> &'a str {
    let at = s.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
    let end = s[at..].find("\n}\n").map_or(s.len(), |e| at + e);
    &s[at..end]
}

#[test]
fn feedback_never_reaches_the_admission_gate() {
    // The feedback route needs no challenge: it calls the feedback object,
    // drops a receipt of the wrong shape first, and never calls the gate.
    let cf = src("cf.rs");
    let route = function_body(&cf, "async fn feedback(");
    assert!(!route.contains("gate_call") && !route.contains("gate_tell") && !route.contains("GATE"), "the feedback route reaches the gate");
    assert!(route.contains("feedback_call::<FeedbackReply, _>(env, \"/vote\""));
    let shape = route.find("receipt::parse(").expect("shape check");
    let call = route.find("feedback_call").expect("feedback call");
    assert!(shape < call, "the shape check runs before any object call");
    let gate = src("cf_gate.rs");
    let code = &gate[gate.find("\nuse ").expect("code")..];
    for banned in ["\"/feedback\"", "receipt", "Ledger", "Claim"] {
        assert!(!code.contains(banned), "cf_gate.rs handles {banned}");
    }
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).expect("wrangler.toml");
    assert!(w.contains("class_name = \"DetectFeedback\""));
}

#[test]
fn the_scan_lease_is_derived_and_every_lane_call_is_bounded() {
    let w = fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).expect("wrangler.toml");
    let vars: Vec<&str> = w.lines().map(str::trim).filter(|l| !l.starts_with('#')).collect();
    assert!(!vars.iter().any(|l| l.starts_with("SCAN_LEASE_MS")), "the lease is derived, not a var");
    for v in ["SCORER_START_WAIT_MS", "LOAD_WAIT_MS", "SCORE_WAIT_MS", "QUEUE_HOLD_MS"] {
        assert!(vars.iter().any(|l| l.starts_with(&format!("{v} = "))), "{v}");
    }
    // Every instance call in the lane goes through a deadline: each function
    // that calls `port_fetch` (other than its definition) races it with
    // `within(...)`.
    let lane = src("cf_lane.rs");
    let starts: Vec<usize> = lane.match_indices("fn ").map(|(i, _)| i).filter(|i| lane[..*i].ends_with("async ") || lane[..*i].ends_with("    ") || lane[..*i].ends_with('\n')).collect();
    let mut calling = 0;
    for (k, start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(lane.len());
        let f = &lane[*start..end];
        if f.starts_with("fn port_fetch(") || !f.contains("port_fetch(") {
            continue;
        }
        calling += 1;
        assert!(f.contains("within("), "an unbounded instance call in {}", &f[..f.find('(').unwrap_or(0)]);
    }
    assert!(calling >= 3, "healthz, load and score call the instance");
    assert!(lane.contains("Outlasted its bound: a hung instance is destroyed."));
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
