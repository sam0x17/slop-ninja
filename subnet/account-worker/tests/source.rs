//! Source checks for the rules no unit test of a pure function can see:
//! nothing writes to the console, every entry silences panics first, the
//! Worker has no route and calls no other Worker (no service binding but the
//! dev session's stub), observability is off, its config carries
//! placeholders only, dev-only code stays behind the `dev` feature, and the
//! account Worker never reads a forwarded principal.

use std::fs;

fn read(rel: &str) -> String {
    fs::read_to_string(format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))).expect(rel)
}

fn sources() -> Vec<(String, String)> {
    let dir = format!("{}/src", env!("CARGO_MANIFEST_DIR"));
    let mut out: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&p).unwrap()))
        .collect();
    out.sort();
    out
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

fn is_dev(section: &str) -> bool {
    section == "env.dev" || section.starts_with("env.dev.")
}

#[test]
fn nothing_writes_to_the_console() {
    for (name, s) in sources() {
        for banned in [
            "console_log!",
            "console_error!",
            "console_warn!",
            "console_debug!",
            "web_sys::console",
            "println!",
            "eprintln!",
            "dbg!",
            "console.log",
            "console.error",
        ] {
            assert!(!s.contains(banned), "{name} uses {banned}");
        }
    }
}

#[test]
fn every_entry_silences_panics_first() {
    let cf = read("src/cf.rs");
    for entry in
        ["pub fn fetch(req: web_sys::Request", "pub fn scheduled(", "pub fn new(state: worker_sys::DurableObjectState"]
    {
        let at = cf.find(entry).unwrap_or_else(|| panic!("{entry}"));
        let open = at + cf[at..].find('{').unwrap();
        let first = cf[open + 1..].trim_start().lines().next().unwrap().trim();
        assert_eq!(first, "crate::silence_panics();", "{entry}");
    }
    assert!(read("src/lib.rs").contains("std::panic::set_hook(Box::new(|_| {}))"));
}

#[test]
fn no_route_no_service_binding_and_no_logs() {
    let w = read("wrangler.toml");
    let lines = toml_lines(&w);
    for (t, l) in &lines {
        for banned in
            ["route", "routes", "custom_domain", "zone_id", "zone_name", "account_id", "tail_consumers", "logpush"]
        {
            assert!(!l.starts_with(&format!("{banned} ")) && !l.starts_with(&format!("{banned}=")), "[{t}] {l}");
        }
        // The account Worker calls no Worker: only the dev session's stub.
        if t.ends_with("services") {
            assert!(is_dev(t), "a service binding outside the dev session: [{t}] {l}");
        }
    }
    for env in ["", "env.staging.", "env.dev."] {
        let section = |k: &str| format!("{env}{k}");
        let top = if env.is_empty() { String::new() } else { env.trim_end_matches('.').to_owned() };
        assert!(lines.contains(&(top.clone(), "workers_dev = false".into())), "{env}workers_dev");
        assert!(lines.contains(&(top.clone(), "preview_urls = false".into())), "{env}preview_urls");
        assert!(lines.contains(&(section("observability"), "enabled = false".into())), "{env}observability");
    }
    let services: Vec<&(String, String)> = lines.iter().filter(|(t, _)| t.ends_with("services")).collect();
    assert_eq!(services.len(), 2, "one dev binding, AUTH_STUB");
}

#[test]
fn the_release_configs_carry_placeholders_only() {
    let w = read("wrangler.toml");
    let lines = toml_lines(&w);
    for (t, l) in lines.iter().filter(|(t, _)| !is_dev(t)) {
        if let Some(v) = l.strip_prefix("SITE_ORIGIN = ").or_else(|| l.strip_prefix("SECRET_ORIGIN = ")) {
            assert!(v.ends_with(".invalid\""), "[{t}] {l}: origins are restored by the deploy wrappers");
        }
        if l.starts_with("database_id") {
            assert!(l.contains("account-store-id"), "[{t}] {l}");
        }
        for secret in ["CLIENT_SECRET", "ACCOUNT_STORE_SECRET"] {
            assert!(!l.starts_with(secret), "[{t}] {l}: a secret is never a var");
        }
        if l.contains('@') {
            assert!(l.contains("mail-domain.invalid"), "[{t}] {l}");
        }
    }
    // The release build never enables the dev feature.
    assert!(lines.contains(&("build".into(), "command = \"worker-build --release\"".into())));
    assert!(!lines.iter().any(|(t, l)| !is_dev(t) && l.contains("--features")));
}

#[test]
fn dev_only_code_stays_behind_the_dev_feature() {
    for (name, s) in sources() {
        for needle in ["AUTH_STUB", "ServiceCapture", "endpoints_at", "STUB_ORIGIN"] {
            for (i, line) in
                s.lines().enumerate().filter(|(_, l)| l.contains(needle) && !l.trim_start().starts_with("//"))
            {
                let before: Vec<&str> = s.lines().take(i).collect();
                let gated = before
                    .iter()
                    .rev()
                    .take(4)
                    .any(|l| l.contains("cfg(feature = \"dev\")") || l.contains("cfg!(feature = \"dev\")"))
                    || line.contains("cfg!(feature = \"dev\")")
                    || name == "config.rs" && line.contains("pub const STUB_ORIGIN")
                    || name == "cf.rs"
                        && (line.contains("env.AUTH_STUB") || line.contains("bindings") || line.contains("//!"));
                assert!(gated, "{name}:{}: {needle} outside the dev feature: {line}", i + 1);
            }
        }
    }
    // Without the dev feature the stub origin is never read.
    let cfg = read("src/config.rs");
    assert!(cfg.contains("let stub_origin = if cfg!(feature = \"dev\")"));
}

#[test]
fn the_account_worker_takes_no_principal_and_no_owner_hash() {
    for (name, s) in sources() {
        assert!(!s.contains("X_SN_PRINCIPAL"), "{name}: the account Worker never reads a forwarded principal");
        assert!(!s.contains("owner_h"), "{name}: only the detect Worker derives owner hashes");
        assert!(!s.contains("DETECT_STORE_SECRET"), "{name}");
    }
}

#[test]
fn every_account_route_and_internal_route_is_served() {
    let routes = read("src/routes.rs");
    for r in slop_ninja_detect_page::RouteId::ALL
        .iter()
        .filter(|r| r.spec().callee == slop_ninja_detect_page::sn::Callee::Account)
    {
        let name = format!("{r:?}");
        assert!(routes.contains(&format!("R::{name}")), "routes.rs does not name {name}");
    }
}
