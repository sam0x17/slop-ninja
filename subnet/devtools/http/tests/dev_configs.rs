//! Source checks for the dev tools as a whole (this shared crate hosts them):
//!
//! - each stub Worker's config, and the session's stores config, can only run
//!   locally: no account, route, custom domain, `workers_dev` subdomain,
//!   preview URL or environment, logging off, a `slop-ninja-dev-` name;
//! - no deployable config names a stub: in every other Worker config under
//!   `subnet/`, a `slop-ninja-dev-` service appears only inside `[env.dev]`;
//! - the dev session script runs Wrangler locally only and never logs in,
//!   deploys or opens a tunnel;
//! - the dev origins parse as every Worker parses its origin vars, and every
//!   config, the script and the smoke check use the same ones;
//! - the session's D1 stores take exactly the detect and account Workers'
//!   own migrations, and a Worker's `[env.dev]` D1 entry names the store
//!   that `dev-session.sh migrate` fills.

use slop_ninja_detect_page::{Origin, Origins};
use slop_ninja_dev_http::session::{MAIL_SUBJECT_PREFIX, SECRET_ORIGIN, SITE_ORIGIN, STORES, STUB_ORIGIN};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn devtools() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("devtools").to_path_buf()
}

fn subnet() -> PathBuf {
    devtools().parent().expect("subnet").to_path_buf()
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
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

/// The key and string value of a `key = "value"` line.
fn kv(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    let v = v.trim().strip_prefix('"')?.split_once('"')?.0;
    Some((k.trim().trim_matches('"'), v))
}

/// The string values of each `[[<table>]]` entry, in order.
fn entries(src: &str, table: &str) -> Vec<BTreeMap<String, String>> {
    let header = format!("[[{table}]]");
    let mut out = Vec::new();
    let mut cur: Option<BTreeMap<String, String>> = None;
    for line in src.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') {
            out.extend(cur.take());
            if l == header {
                cur = Some(BTreeMap::new());
            }
            continue;
        }
        if let (Some(c), Some((k, v))) = (cur.as_mut(), kv(l)) {
            c.insert(k.to_owned(), v.to_owned());
        }
    }
    out.extend(cur);
    out
}

/// Every Worker config under `subnet/` outside the dev tools: `(the
/// Worker's directory name, path, source)`.
fn subnet_configs() -> Vec<(String, PathBuf, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(subnet()).expect("subnet dir").flatten() {
        let dir = entry.path();
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if name == "devtools" {
            continue;
        }
        for file in ["wrangler.toml", "wrangler.jsonc", "wrangler.json"] {
            let cfg = dir.join(file);
            if let Ok(src) = fs::read_to_string(&cfg) {
                out.push((name.clone(), cfg, src));
            }
        }
    }
    out.sort();
    out
}

/// A config that can only run locally.
fn assert_local_only(what: &str, src: &str) {
    let lines = lines_by_table(src);
    let top = |k: &str| lines.iter().find(|(t, l)| t.is_empty() && l.starts_with(&format!("{k} "))).map(|(_, l)| l.clone());
    assert_eq!(top("workers_dev").as_deref(), Some("workers_dev = false"), "{what}");
    assert_eq!(top("preview_urls").as_deref(), Some("preview_urls = false"), "{what}");
    assert!(top("name").is_some_and(|n| n.starts_with("name = \"slop-ninja-dev-")), "{what}");
    for (t, l) in &lines {
        for banned in ["account_id", "route", "custom_domain", "zone_id", "zone_name", "tail_consumers", "logpush"] {
            assert!(!l.starts_with(banned) && !l.contains(&format!("{banned} =")), "{what}: {l}");
        }
        assert!(!t.starts_with("env"), "{what}: no environments: [{t}]");
    }
    assert!(lines.iter().any(|(t, l)| t == "observability" && l == "enabled = false"), "{what}: observability off");
}

#[test]
fn stub_configs_run_only_locally() {
    for s in STUBS {
        assert_local_only(s, &read(&devtools().join(s).join("wrangler.toml")));
    }
    assert_local_only("session/stores.toml", &read(&devtools().join("session/stores.toml")));
}

#[test]
fn only_dev_environments_name_a_stub() {
    let configs = subnet_configs();
    for (_, cfg, src) in &configs {
        if cfg.extension().is_some_and(|e| e == "toml") {
            for (t, l) in lines_by_table(src) {
                if l.contains("slop-ninja-dev-") {
                    assert!(t == "env.dev" || t.starts_with("env.dev."), "{}: [{t}] {l}", cfg.display());
                }
            }
        } else {
            assert!(!src.contains("slop-ninja-dev-"), "{}: JSON configs are checked by hand", cfg.display());
        }
    }
    assert!(configs.len() >= 5, "found the subnet Worker configs");
}

#[test]
fn the_session_script_stays_local() {
    let s = read(&devtools().join("session/dev-session.sh"));
    for banned in ["--remote", "--tunnel", "wrangler login", "wrangler logout", "wrangler deploy", " deploy ", "versions upload", "secret put"] {
        assert!(!s.contains(banned), "dev-session.sh: {banned}");
    }
    assert!(s.contains("--local --ip 127.0.0.1"));
    // Both D1 commands run on the local stores only.
    assert_eq!(s.matches("wrangler d1 ").count(), 2, "dev-session.sh: migrate and stores");
    for l in s.lines().filter(|l| l.contains("wrangler d1 ")) {
        assert!(l.contains(" --local ") && l.contains("-c \"$HERE/stores.toml\""), "dev-session.sh: {l}");
    }
    for off in ["WRANGLER_SEND_METRICS=false", "WRANGLER_HIDE_BANNER=true", "CLOUDFLARE_CF_FETCH_ENABLED=false", "WRANGLER_WRITE_LOGS=false"] {
        assert!(s.contains(off), "dev-session.sh: {off}");
    }
    let smoke = read(&devtools().join("session/smoke.mjs"));
    // Its one fetch call goes to the session's origin.
    assert_eq!(smoke.matches("fetch(").count(), 1, "smoke.mjs: one fetch call");
    assert!(smoke.contains("fetch(BASE + path"), "smoke.mjs fetches the session only");
}

#[test]
fn the_dev_origins_parse_and_agree_everywhere() {
    let o = Origins::from_vars(SITE_ORIGIN, SECRET_ORIGIN).expect("the dev pair parses as the Workers parse theirs");
    let stub = Origin::parse(STUB_ORIGIN).expect("the stub origin parses");
    assert!(stub.host() != o.site.host() && stub.host() != o.secret.host(), "three hosts");

    // The script serves each origin on its port and its certificate names
    // each host.
    let script = read(&devtools().join("session/dev-session.sh"));
    for (var, origin) in [("SITE_PORT", &o.site), ("SECRET_PORT", &o.secret), ("STUBS_PORT", &stub)] {
        let (host, port) = origin.authority().rsplit_once(':').expect("a dev origin has a port");
        assert!(script.contains(&format!("{var}=${{{var}:-{port}}}")), "dev-session.sh: {var} defaults to {port}");
        let named = script.contains(&format!("DNS:{host},")) || host.split_once('.').is_some_and(|(_, parent)| script.contains(&format!("DNS:*.{parent},")));
        assert!(named, "dev-session.sh: the certificate names {host}");
    }
    assert!(script.contains(&format!("\"https://{}:$STUBS_PORT\"", stub.host())), "dev-session.sh: smoke checks the stubs origin");

    // The smoke check defaults to the stubs origin.
    let smoke = read(&devtools().join("session/smoke.mjs"));
    assert!(smoke.contains(&format!("process.argv[2] || \"{STUB_ORIGIN}\"")), "smoke.mjs");

    // The auth stub's vars are the session's origins.
    let auth = read(&devtools().join("auth-stub/wrangler.toml"));
    let vars: BTreeMap<&str, &str> = auth.lines().map(str::trim).filter_map(kv).collect();
    assert_eq!(vars.get("STUB_ORIGIN"), Some(&STUB_ORIGIN));
    assert_eq!(vars.get("SITE_ORIGIN"), Some(&SITE_ORIGIN));

    // Every Worker that joins the session sets the same values.
    for (_, cfg, src) in subnet_configs() {
        for (t, l) in lines_by_table(&src) {
            let Some((k, v)) = kv(&l).filter(|_| t == "env.dev.vars") else { continue };
            let want = match k {
                "SITE_ORIGIN" => SITE_ORIGIN,
                "SECRET_ORIGIN" => SECRET_ORIGIN,
                "STUB_ORIGIN" => STUB_ORIGIN,
                "MAIL_SUBJECT_PREFIX" => MAIL_SUBJECT_PREFIX,
                _ => continue,
            };
            assert_eq!(v, want, "{}: [env.dev.vars] {k}", cfg.display());
        }
    }
}

#[test]
fn the_session_stores_take_the_workers_own_migrations() {
    let stores = read(&devtools().join("session/stores.toml"));
    let found = entries(&stores, "d1_databases");
    assert_eq!(found.len(), STORES.len(), "session/stores.toml: one entry per store");
    for (e, s) in found.iter().zip(STORES) {
        assert_eq!(e.get("binding").map(String::as_str), Some(s.binding));
        assert_eq!(e.get("database_id").map(String::as_str), Some(s.database_id));
        let dir = format!("../../{}/migrations", s.worker);
        assert_eq!(e.get("migrations_dir"), Some(&dir), "{}", s.binding);
        // The Worker's own migrations, numbered from 0001 with no gap.
        let mut sql: Vec<String> = fs::read_dir(devtools().join("session").join(&dir))
            .unwrap_or_else(|e| panic!("{dir}: {e}"))
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        sql.sort();
        assert!(!sql.is_empty(), "{dir}: migrations");
        for (i, n) in sql.iter().enumerate() {
            assert!(n.starts_with(&format!("{:04}_", i + 1)), "{dir}: {n} out of sequence");
        }
    }
    // A Worker's [env.dev] D1 entry for a session store is its own store,
    // under the session's id; no other Worker binds one.
    for (worker, cfg, src) in subnet_configs() {
        for e in entries(&src, "env.dev.d1_databases") {
            let (b, id) = (e.get("binding").map(String::as_str), e.get("database_id").map(String::as_str));
            let Some(s) = STORES.iter().find(|s| Some(s.binding) == b || Some(s.database_id) == id) else { continue };
            assert_eq!(worker, s.worker, "{}: only {} binds {}", cfg.display(), s.worker, s.binding);
            assert_eq!((b, id), (Some(s.binding), Some(s.database_id)), "{}", cfg.display());
            assert_eq!(e.get("migrations_dir").map(String::as_str), Some("migrations"), "{}", cfg.display());
        }
    }
}
