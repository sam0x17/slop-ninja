//! Source checks for the dev tools as a whole (this shared crate hosts them):
//!
//! - each stub Worker's config can only run locally: no account, route,
//!   custom domain, `workers_dev` subdomain, preview URL or environment, logging
//!   off, a `slop-ninja-dev-` name;
//! - no deployable config names a stub: in every other Worker config under
//!   `subnet/`, a `slop-ninja-dev-` service appears only inside `[env.dev]`;
//! - the dev session script runs Wrangler locally only and never logs in,
//!   deploys or opens a tunnel.

use std::fs;
use std::path::{Path, PathBuf};

fn devtools() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("devtools").to_path_buf()
}

const STUBS: &[&str] = &["auth-stub", "stub-scorer", "fixture-site"];

/// `(table header, line)` for every non-comment line.
fn lines_by_table(src: &str) -> Vec<(String, String)> {
    let mut table = String::new();
    let mut out = Vec::new();
    for line in src.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') {
            table = l.trim_matches(|c| c == '[' || c == ']').to_owned();
        }
        out.push((table.clone(), l.to_owned()));
    }
    out
}

#[test]
fn stub_configs_run_only_locally() {
    for s in STUBS {
        let src = fs::read_to_string(devtools().join(s).join("wrangler.toml")).expect(s);
        let lines = lines_by_table(&src);
        let top = |k: &str| lines.iter().find(|(t, l)| t.is_empty() && l.starts_with(&format!("{k} "))).map(|(_, l)| l.clone());
        assert_eq!(top("workers_dev").as_deref(), Some("workers_dev = false"), "{s}");
        assert_eq!(top("preview_urls").as_deref(), Some("preview_urls = false"), "{s}");
        assert!(top("name").is_some_and(|n| n.starts_with("name = \"slop-ninja-dev-")), "{s}");
        for (t, l) in &lines {
            for banned in ["account_id", "route", "custom_domain", "zone_id", "zone_name", "tail_consumers", "logpush"] {
                assert!(!l.starts_with(banned) && !l.contains(&format!("{banned} =")), "{s}: {l}");
            }
            assert!(!t.starts_with("env"), "{s}: no environments: [{t}]");
        }
        assert!(lines.iter().any(|(t, l)| t == "observability" && l == "enabled = false"), "{s}: observability off");
    }
}

#[test]
fn only_dev_environments_name_a_stub() {
    let subnet = devtools().parent().expect("subnet").to_path_buf();
    let mut seen = 0;
    for entry in fs::read_dir(&subnet).expect("subnet dir").flatten() {
        let dir = entry.path();
        if dir.file_name().is_some_and(|n| n == "devtools") {
            continue;
        }
        for name in ["wrangler.toml", "wrangler.jsonc", "wrangler.json"] {
            let cfg = dir.join(name);
            let Ok(src) = fs::read_to_string(&cfg) else { continue };
            seen += 1;
            if name == "wrangler.toml" {
                for (t, l) in lines_by_table(&src) {
                    if l.contains("slop-ninja-dev-") {
                        assert!(t == "env.dev" || t.starts_with("env.dev."), "{}: [{t}] {l}", cfg.display());
                    }
                }
            } else {
                assert!(!src.contains("slop-ninja-dev-"), "{}: JSON configs are checked by hand", cfg.display());
            }
        }
    }
    assert!(seen >= 5, "found the subnet Worker configs");
}

#[test]
fn the_session_script_stays_local() {
    let s = fs::read_to_string(devtools().join("session/dev-session.sh")).expect("script");
    for banned in ["--remote", "--tunnel", "wrangler login", "wrangler logout", "wrangler deploy", " deploy ", "versions upload", "secret put"] {
        assert!(!s.contains(banned), "dev-session.sh: {banned}");
    }
    assert!(s.contains("--local --ip 127.0.0.1"));
    for off in ["WRANGLER_SEND_METRICS=false", "WRANGLER_HIDE_BANNER=true", "CLOUDFLARE_CF_FETCH_ENABLED=false", "WRANGLER_WRITE_LOGS=false"] {
        assert!(s.contains(off), "dev-session.sh: {off}");
    }
    let smoke = fs::read_to_string(devtools().join("session/smoke.mjs")).expect("smoke");
    // Its one fetch call goes to the session's origin.
    assert_eq!(smoke.matches("fetch(").count(), 1, "smoke.mjs: one fetch call");
    assert!(smoke.contains("fetch(BASE + path"), "smoke.mjs fetches the session only");
    assert!(smoke.contains("process.argv[2] || \"https://auth.slop.localhost:8789\""));
}
