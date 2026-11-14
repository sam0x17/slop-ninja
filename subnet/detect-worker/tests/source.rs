//! Source checks for the privacy and deployment rules that no unit test of
//! a pure function can see: nothing writes to the console, every entry
//! point silences panics first, the Worker reads no header outside the
//! forwarders' allow-list (never a cookie), it has no way in but the
//! service bindings and no logging sink, every production resource is a
//! testnet resource and every staging resource a staging one, and no
//! challenge token or secret is tracked.

use std::fs;

fn src(name: &str) -> String {
    fs::read_to_string(format!("{}/src/{name}", env!("CARGO_MANIFEST_DIR"))).expect(name)
}

const SOURCES: &[&str] = &[
    "lib.rs",
    "bound.rs",
    "counters.rs",
    "cut.rs",
    "english.rs",
    "envelope.rs",
    "policy.rs",
    "answer.rs",
    "receipt.rs",
    "gate.rs",
    "routes.rs",
    "seal.rs",
    "switches.rs",
    "waits.rs",
    "xpost.rs",
    "canon.rs",
    "handoff.rs",
    "sections.rs",
    "paid.rs",
    "cf.rs",
    "cf_paid.rs",
    "cf_routes.rs",
    "cf_store.rs",
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
    for export in ["pub fn fetch(", "pub fn queue(", "pub fn scheduled("] {
        assert_eq!(first_statement(&cf, export), "silence_panics();", "{export}");
    }
    for name in ["cf_gate.rs", "cf_feedback.rs", "cf_lane.rs"] {
        let s = src(name);
        assert_eq!(first_statement(&s, "fn new(state: State, env: Env)"), "silence_panics();", "{name} new");
        assert_eq!(first_statement(&s, "async fn fetch("), "silence_panics();", "{name} fetch");
    }
    for name in ["cf_lane.rs", "cf_gate.rs"] {
        assert_eq!(first_statement(&src(name), "async fn alarm("), "silence_panics();", "{name} alarm");
    }
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
    assert!(
        !route.contains("gate_call") && !route.contains("gate_tell") && !route.contains("GATE"),
        "the feedback route reaches the gate"
    );
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
    let starts: Vec<usize> = lane
        .match_indices("fn ")
        .map(|(i, _)| i)
        .filter(|i| lane[..*i].ends_with("async ") || lane[..*i].ends_with("    ") || lane[..*i].ends_with('\n'))
        .collect();
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

/// The non-test part of a source (up to its first `#[cfg(test)]`).
fn code(s: &str) -> &str {
    &s[..s.find("#[cfg(test)]").unwrap_or(s.len())]
}

fn wrangler() -> String {
    fs::read_to_string(format!("{}/wrangler.toml", env!("CARGO_MANIFEST_DIR"))).expect("wrangler.toml")
}

/// One env's section of the Wrangler file: from its header to the next env's.
fn env_section<'a>(w: &'a str, env: &str) -> &'a str {
    // The header at a line's start (comments may name it too).
    let head = format!("\n[env.{env}]\n");
    let at = w.find(&head).unwrap_or_else(|| panic!("no {head}"));
    let rest = &w[at + head.len()..];
    // The next `[env.<other>...]` header; `[[env.<env>...]]` tables and
    // `[env.<env>.*]` subtables belong to this env.
    let end = rest
        .match_indices("\n[env.")
        .map(|(i, _)| i)
        .find(|i| !rest[i + "\n[env.".len()..].starts_with(env))
        .unwrap_or(rest.len());
    &w[at..at + head.len() + end]
}

#[test]
fn every_resource_is_a_testnet_resource() {
    let w = wrangler();
    let testnet = env_section(&w, "testnet");
    assert!(!testnet.contains("[env.staging"), "the testnet section ends before staging");
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

/// Staging names are staging names, its origins placeholders; the one
/// production resource it binds is the package bucket, read only.
#[test]
fn every_staging_resource_is_a_staging_resource() {
    let w = wrangler();
    let staging = env_section(&w, "staging");
    let mut bucket_binding = None;
    for l in staging.lines().map(str::trim) {
        if let Some(v) = l.strip_prefix("binding = \"") {
            bucket_binding = Some(v.trim_end_matches('"').to_owned());
        }
        if let Some(v) = l.strip_prefix("name = \"").or_else(|| l.strip_prefix("database_name = \"")) {
            let v = v.trim_end_matches('"');
            if v.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                continue;
            }
            assert!(v.ends_with("-staging"), "{v}");
        }
        if let Some(v) = l.strip_prefix("bucket_name = \"") {
            let v = v.trim_end_matches('"');
            if bucket_binding.as_deref() == Some("DETECT_PACKAGES_TESTNET") {
                assert_eq!(v, "detect-packages-bucket-testnet");
            } else {
                assert!(v.ends_with("-staging"), "only the package bucket is shared: {v}");
            }
        }
        for key in ["queue = \"", "dead_letter_queue = \""] {
            if let Some(v) = l.strip_prefix(key) {
                assert!(v.trim_end_matches('"').ends_with("-staging"), "{v}");
            }
        }
        if let Some(v) = l.strip_prefix("service = \"") {
            // The one other production resource staging binds: the alerts
            // relay, whose legal stream serves both.
            let v = v.trim_end_matches('"');
            let relay = bucket_binding.as_deref() == Some("ALERTS_RELAY") && v == "slop-ninja-alerts-testnet";
            assert!(relay || v.ends_with("-staging"), "{v}");
        }
        if let Some(v) = l.strip_prefix("dataset = \"") {
            assert!(v.trim_end_matches('"').ends_with("_staging"), "{v}");
        }
        if let Some(v) = l.strip_prefix("database_id = \"") {
            assert!(v.trim_end_matches('"').ends_with("-staging"), "{v}");
        }
        for o in ["SITE_ORIGIN = \"", "SECRET_ORIGIN = \""] {
            if let Some(v) = l.strip_prefix(o) {
                assert!(v.trim_end_matches('"').ends_with(".invalid"), "staging origins are placeholders: {v}");
            }
        }
    }
    let testnet = env_section(&w, "testnet");
    for v in ["SITE_ORIGIN", "SECRET_ORIGIN", "DETECT_TOOL", "DETECT_SPANS", "DETECT_URLS", "DETECT_X", "BODY_WAIT_MS"]
    {
        assert!(testnet.contains(&format!("\n{v} = ")), "testnet sets {v}");
        assert!(staging.contains(&format!("\n{v} = ")), "staging sets {v}");
    }
    for b in [
        "DETECT_GATE",
        "DETECT_LANE",
        "DETECT_FEEDBACK",
        "DETECT_STORE",
        "DETECT_METRICS",
        "DETECT_WARM_LIMIT",
        "DETECT_DOCS",
        "DETECT_JOBS",
        "ACCOUNT",
        "ALERTS_RELAY",
    ] {
        assert!(testnet.contains(&format!("\"{b}\"")) && staging.contains(&format!("\"{b}\"")), "{b} in both envs");
    }
    for v in ["PACKAGE_LABEL", "EVIDENCE_ID"] {
        assert!(testnet.contains(&format!("\n{v} = ")) && staging.contains(&format!("\n{v} = ")), "{v} in both envs");
    }
    for env in [testnet, staging] {
        assert!(env.contains("crons = [\"0 * * * *\"]"), "the hourly cron in every env");
    }
    assert!(staging.contains("max_instances = 1"));
    assert_ne!(
        testnet.lines().find(|l| l.starts_with("namespace_id")),
        staging.lines().find(|l| l.starts_with("namespace_id")),
        "each env has its own rate-limit namespace"
    );
}

/// The Worker reads only the forwarders' allow-listed headers, never a
/// cookie, and reads `X-SN-*` through the one admission path.
#[test]
fn only_allow_listed_headers_are_read() {
    use slop_ninja_detect_worker::routes::forwarded::READ_HEADERS;
    let mut seen = Vec::new();
    for name in SOURCES.iter().filter(|n| n.starts_with("cf")) {
        let s = src(name);
        let c = code(&s);
        assert!(!c.to_ascii_lowercase().contains("\"cookie\""), "{name} names the cookie header");
        for pat in ["header(req, \"", "header(&req, \"", "headers().get(\""] {
            for (i, _) in c.match_indices(pat) {
                let lit = &c[i + pat.len()..];
                let lit = &lit[..lit.find('"').expect("literal")];
                assert!(READ_HEADERS.contains(&lit), "{name} reads header {lit}");
                seen.push(lit.to_owned());
            }
        }
        assert!(
            !c.contains("headers().get(&") && !c.contains(".headers().entries()"),
            "{name} reads a computed header name"
        );
    }
    for h in ["x-sn-route", "x-sn-host", "x-sn-principal", "origin", "sec-fetch-site", "content-type", "content-length"]
    {
        assert!(seen.iter().any(|s| s == h), "{h} is read");
    }
    // Only the admission reads the X-SN headers.
    let cf = src("cf.rs");
    let heads = function_body(code(&cf), "impl Heads {");
    for h in ["x-sn-route", "x-sn-host", "x-sn-principal"] {
        assert_eq!(code(&cf).matches(&format!("\"{h}\"")).count(), 1, "{h} read once");
        assert!(heads.contains(&format!("\"{h}\"")), "{h} read by Heads");
    }
}

/// A lone X post link is answered before the reservation grows, the cut,
/// the floor and any ticket; `canon_v1` runs before the cut, whose part
/// before any ticket (`prepare`) holds the cut, the floor and the screen.
#[test]
fn an_x_post_link_takes_no_ticket() {
    let cf = src("cf.rs");
    let score = function_body(code(&cf), "async fn score(");
    let x = score.find("xpost::x_post_v1(text)").expect("the rule runs in score");
    for later in ["held.grow(", "canon::canon_v1(text)", "prepare(&env", "run(&env"] {
        let at = score.find(later).unwrap_or_else(|| panic!("{later}"));
        assert!(x < at, "x_post_v1 runs before {later}");
    }
    let canon = score.find("canon::canon_v1(text)").unwrap();
    assert!(canon < score.find("prepare(&env").unwrap(), "canon_v1 before the cut");
    let env = score.find("envelope::parse(").expect("envelope");
    assert!(env < x, "on the envelope's text");
    let prepare = function_body(code(&cf), "pub(crate) fn prepare<");
    for step in ["cut::free_cut(canonical", "policy::length_bin(", "english::english_screen("] {
        assert!(prepare.contains(step), "prepare runs {step}");
    }
    // The re-run scores through the same path.
    let routes = src("cf_routes.rs");
    let rerun = function_body(code(&routes), "impl rerun::Scorer for CfScorer");
    for step in ["canon::canon_v1(text)", "prepare(env, RouteId::Rerun", "run(env,", "answer::to_original("] {
        assert!(rerun.contains(step), "the re-run runs {step}");
    }
}

/// The in-page challenge is gone (edge rules protect the routes), and the
/// store secret is a Worker secret, never a var.
#[test]
fn no_challenge_token_and_no_secret_is_tracked() {
    let w = wrangler();
    for l in w.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        assert!(!l.starts_with("DETECT_STORE_SECRET"), "the store secret is a Worker secret");
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let mut files: Vec<(String, String)> = SOURCES.iter().map(|n| (n.to_string(), code(&src(n)).to_owned())).collect();
    for f in ["wrangler.toml", "Cargo.toml", "README.md", "memory-model.json"] {
        files.push((f.into(), fs::read_to_string(format!("{root}/{f}")).expect(f)));
    }
    for (name, text) in files {
        assert!(!text.to_ascii_lowercase().contains("turnstile"), "{name} names the removed challenge");
        assert!(!text.contains("siteverify") && !text.contains("challenges.cloudflare.com"), "{name}");
    }
}
