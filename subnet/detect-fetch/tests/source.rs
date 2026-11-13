//! Source checks for the rules no unit test of a pure function can see:
//! nothing writes to the console, every entry silences panics first, the
//! Worker has no way in but the detect Worker's service binding, no binding
//! of its own (never a private-network one), no logging sink and no real
//! resource identifier, the staging fixture allowance cannot reach the
//! production configuration, and dev-only code stays behind the `dev`
//! feature.

use std::fs;

fn read(rel: &str) -> String {
    fs::read_to_string(format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))).expect(rel)
}

const SOURCES: &[&str] = &[
    "src/lib.rs",
    "src/bluesky.rs",
    "src/bound.rs",
    "src/cf.rs",
    "src/extract.rs",
    "src/fetch.rs",
    "src/fresh.rs",
    "src/net.rs",
    "src/redirects.rs",
    "src/robots.rs",
    "src/routes.rs",
    "src/screen.rs",
    "src/sign.rs",
    "src/strip.rs",
    "wire/src/lib.rs",
    "wire/src/robots.rs",
];

/// Lines that are not comments.
fn code(s: &str) -> Vec<&str> {
    s.lines().map(str::trim).filter(|l| !l.starts_with("//") && !l.starts_with('#') && !l.is_empty()).collect()
}

/// `(section, line)` of every non-comment line of a TOML file.
fn toml_lines(s: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for l in s.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') {
            section = l.trim_matches(|c| c == '[' || c == ']').to_owned();
            continue;
        }
        out.push((section.clone(), l.to_owned()));
    }
    out
}

#[test]
fn nothing_writes_to_the_console() {
    for name in SOURCES {
        let s = read(name);
        let s = s.replace("eprintln!", "eprint_ln");
        for banned in [
            "console_error_panic_hook",
            "console_log!",
            "console_error!",
            "console_warn!",
            "web_sys::console",
            "console.",
            "println!",
            "dbg!",
        ] {
            assert!(!s.contains(banned), "{name} uses {banned}");
        }
        if !name.contains("arena_witness") {
            let body = s.split("#[cfg(test)]").next().unwrap_or("");
            assert!(!body.contains("eprint_ln"), "{name} uses eprintln! outside tests");
        }
    }
}

#[test]
fn the_entry_silences_panics_first() {
    let cf = read("src/cf.rs");
    let at = cf.find("pub fn fetch(").expect("fetch export");
    let open = at + cf[at..].find('{').expect("body");
    let first = cf[open + 1..].trim_start().lines().next().unwrap_or("").trim();
    assert_eq!(first, "silence_panics();");
    assert!(read("src/lib.rs").contains("std::panic::set_hook(Box::new(|_| {}))"));
}

#[test]
fn no_route_no_binding_no_logs() {
    let w = read("wrangler.toml");
    let lines = toml_lines(&w);
    // No array-of-tables (every binding kind is one) and no binding key.
    for l in w.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        assert!(!l.starts_with("[["), "wrangler.toml declares a binding: {l}");
        for banned in [
            "services",
            "vpc",
            "tunnel",
            "hyperdrive",
            "durable_objects",
            "r2_buckets",
            "kv_namespaces",
            "d1_databases",
            "queues",
            "analytics_engine",
            "ratelimits",
            "containers",
            "browser",
            "secrets_store",
            "mtls_certificates",
            "dispatch_namespaces",
            "send_email",
            "tail_consumers",
            "logpush",
            "route",
            "custom_domain",
            "account_id",
            "triggers",
        ] {
            assert!(!l.to_ascii_lowercase().contains(banned), "wrangler.toml names {banned}: {l}");
        }
    }
    for (section, l) in &lines {
        if l.starts_with("workers_dev") || l.starts_with("preview_urls") {
            assert!(l.ends_with("false"), "[{section}] {l}");
        }
        if section.ends_with("observability") {
            assert_eq!(l, "enabled = false", "[{section}]");
        }
    }
    for env in ["", "env.testnet", "env.staging"] {
        assert!(lines.iter().any(|(s, l)| s == env && l == "workers_dev = false"), "[{env}] workers_dev");
        assert!(lines.iter().any(|(s, l)| s == env && l == "preview_urls = false"), "[{env}] preview_urls");
        let obs = if env.is_empty() { "observability".to_owned() } else { format!("{env}.observability") };
        assert!(lines.iter().any(|(s, l)| *s == obs && l == "enabled = false"), "[{obs}]");
    }
    // Placeholders only: no 32-hex account or resource id.
    let hex32 = w.split(|c: char| !c.is_ascii_hexdigit()).any(|t| t.len() >= 32);
    assert!(!hex32, "wrangler.toml holds a long hex id");
    assert!(lines.iter().any(|(s, l)| s == "env.testnet" && l == "name = \"slop-ninja-detect-fetch-testnet\""));
    // The release build enables no feature.
    assert!(lines.iter().any(|(s, l)| s == "build" && l == "command = \"worker-build --release\""));
}

#[test]
fn the_production_config_cannot_carry_the_fixture_allowance() {
    let w = read("wrangler.toml");
    let lines = toml_lines(&w);
    for (section, l) in &lines {
        if l.starts_with("URL_FIXTURE_PREFIX") {
            assert_eq!(section, "env.staging.vars", "URL_FIXTURE_PREFIX outside the staging vars");
            assert_eq!(l, "URL_FIXTURE_PREFIX = \"<URL_FIXTURE_PREFIX>\"", "a placeholder only");
        }
        assert!(!l.starts_with("DEV_"), "a dev var in a deploy config: [{section}] {l}");
    }
    let var = |env: &str, k: &str| {
        lines.iter().find(|(s, l)| s == &format!("env.{env}.vars") && l.starts_with(&format!("{k} = "))).map(|(_, l)| l.clone())
    };
    assert_eq!(var("testnet", "DEPLOY_ENV").as_deref(), Some("DEPLOY_ENV = \"production\""));
    assert_eq!(var("staging", "DEPLOY_ENV").as_deref(), Some("DEPLOY_ENV = \"staging\""));
    assert_eq!(var("testnet", "SITE_ORIGIN").as_deref(), Some("SITE_ORIGIN = \"https://slop.ninja\""));
    // Staging origins are placeholders the deploy wrappers restore.
    assert_eq!(var("staging", "SITE_ORIGIN").as_deref(), Some("SITE_ORIGIN = \"<SITE_ORIGIN>\""));
    assert_eq!(var("staging", "SECRET_ORIGIN").as_deref(), Some("SECRET_ORIGIN = \"<SECRET_ORIGIN>\""));
    // The Worker reads the prefix only inside the staging condition.
    let cf = read("src/cf.rs");
    let reads: Vec<usize> = cf.match_indices("\"URL_FIXTURE_PREFIX\"").map(|(i, _)| i).collect();
    assert_eq!(reads.len(), 1, "the prefix is read in one place");
    let before = &cf[..reads[0]];
    let cond = before.rfind("if var(env, \"DEPLOY_ENV\").as_deref() == Some(ENV_STAGING)").expect("the staging condition");
    assert!(reads[0] - cond < 200, "the prefix read is guarded by the staging condition");
}

#[test]
fn dev_only_code_is_behind_the_dev_feature() {
    for name in ["src/cf.rs", "src/fetch.rs", "src/net.rs"] {
        let s = read(name);
        let lines: Vec<&str> = s.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.contains("DEV_FIXTURE_HOST") && !l.trim_start().starts_with("//")
                || l.contains("with_dev_host")
                || l.contains("\"FIXTURE_SITE\"")
            {
                let window = &lines[i.saturating_sub(4)..i];
                assert!(
                    window.iter().any(|w| w.contains("#[cfg(feature = \"dev\")]")),
                    "{name}:{}: dev-only code outside the dev feature: {l}",
                    i + 1
                );
            }
        }
    }
    let url = fs::read_to_string(format!("{}/../crates/detect-url/src/check.rs", env!("CARGO_MANIFEST_DIR"))).expect("check.rs");
    for (i, l) in url.lines().enumerate() {
        if l.contains("fn with_dev_host") || l.contains("dev_host:") {
            let prev: Vec<&str> = url.lines().collect();
            assert!(prev[i.saturating_sub(4)..i].iter().any(|w| w.contains("#[cfg(feature = \"dev\")]")), "check.rs:{}", i + 1);
        }
    }
}

#[test]
fn requests_carry_no_identity_of_the_submitter() {
    // Risk F17: the fetch Worker reads no principal, cookie or forwarded
    // header, and sends no cookie, authorization or referrer.
    for name in SOURCES {
        let s = read(name);
        let body = s.split("#[cfg(test)]").next().unwrap_or("");
        for banned in
            ["X-SN-Principal", "x-sn-principal", "\"cookie\"", "\"authorization\"", "\"referer\"", "CF-Connecting-IP", "cf-connecting-ip"]
        {
            assert!(!body.contains(banned), "{name} uses {banned}");
        }
    }
    let fetch = read("src/fetch.rs");
    let lines = code(&fetch);
    assert!(lines.iter().any(|l| l.contains("vec![(\"user-agent\", USER_AGENT.to_owned()), (\"accept\", accept.to_owned())]")));
}
