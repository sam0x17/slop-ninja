//! Source checks for the rules no unit test of a pure function can see:
//! nothing writes to the console, every entry silences panics first, the
//! Worker has no way in but its callers' service bindings, no binding,
//! secret, logging sink or real resource identifier, no outbound request,
//! embeds only the committed fonts and art, and its release arena features
//! are the production ones.

use sha2::{Digest, Sha256};
use std::fs;

/// SHA-256 of the Inter project's `LICENSE.txt` (github.com/rsms/inter, the
/// same file at its v4.0 and v4.1 tags), which `src/fonts/OFL.txt` copies.
const INTER_LICENSE_SHA256: &str = "262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a";

fn read(rel: &str) -> String {
    fs::read_to_string(format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"))).expect(rel)
}

const SOURCES: &[&str] = &[
    "src/lib.rs",
    "src/art.rs",
    "src/card.rs",
    "src/cf.rs",
    "src/fonts.rs",
    "src/layout.rs",
    "src/render.rs",
    "src/routes.rs",
    "src/svg.rs",
    "src/wire.rs",
];

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
        for banned in [
            "console_error_panic_hook",
            "console_log!",
            "console_error!",
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

#[test]
fn every_entry_silences_panics_first() {
    let cf = read("src/cf.rs");
    let entries: Vec<&str> = cf.split("#[wasm_bindgen]\npub fn ").skip(1).collect();
    assert_eq!(entries.len(), 1, "one export: fetch");
    for e in entries {
        let body = e.split_once('{').map(|(_, b)| b.trim_start()).unwrap_or("");
        assert!(body.starts_with("silence_panics();"), "an export does not silence panics first");
    }
}

#[test]
fn the_worker_has_no_route_binding_secret_or_sink() {
    let w = read("wrangler.toml");
    let allowed_sections = [
        "",
        "build",
        "observability",
        "env.testnet",
        "env.testnet.observability",
        "env.testnet.vars",
        "env.staging",
        "env.staging.observability",
        "env.staging.vars",
        "env.dev",
        "env.dev.observability",
        "env.dev.vars",
    ];
    let allowed_keys = ["name", "main", "compatibility_date", "workers_dev", "preview_urls", "command", "enabled"];
    let allowed_vars = ["NETWORK", "PRODUCT", "DEPLOY_ENV", "SITE_ORIGIN", "SECRET_ORIGIN", "GAUGE_ENDS"];
    for (section, line) in toml_lines(&w) {
        assert!(allowed_sections.contains(&section.as_str()), "[{section}]: no binding, route or sink of any kind");
        let key = line.split('=').next().unwrap_or("").trim();
        if section.ends_with("vars") {
            assert!(allowed_vars.contains(&key), "[{section}] {line}");
        } else {
            assert!(allowed_keys.contains(&key), "[{section}] {line}");
        }
        if key == "workers_dev" || key == "preview_urls" || key == "enabled" {
            assert!(line.ends_with("false"), "[{section}] {line}");
        }
    }
    // Every environment states its own no-subdomain, no-preview and no-logs
    // settings; staging's origins are placeholders the deploy wrappers fill.
    for env in ["env.testnet", "env.staging", "env.dev"] {
        let lines = toml_lines(&w);
        assert!(lines.iter().any(|(s, l)| s == env && l == "workers_dev = false"), "{env}");
        assert!(lines.iter().any(|(s, l)| s == env && l == "preview_urls = false"), "{env}");
        assert!(lines.iter().any(|(s, l)| *s == format!("{env}.observability") && l == "enabled = false"), "{env}");
    }
    for (_, l) in toml_lines(&w).into_iter().filter(|(s, _)| s == "env.staging.vars") {
        if l.starts_with("SITE_ORIGIN") || l.starts_with("SECRET_ORIGIN") {
            assert!(l.contains("\"<") && l.ends_with(">\""), "staging origins are placeholders: {l}");
        }
    }
    // No secret is read and no request leaves the Worker.
    for name in SOURCES {
        let s = read(name);
        for banned in [".secret(", "Fetch::", "globalThis.fetch", ".fetch(", "fetch_with", "connect("] {
            assert!(!s.contains(banned), "{name}: {banned}");
        }
    }
}

#[test]
fn only_the_committed_fonts_and_art_are_embedded() {
    let mut embedded = Vec::new();
    for name in SOURCES {
        let s = read(name);
        for m in ["include_bytes!(\"", "include_str!(\""] {
            for part in s.split(m).skip(1) {
                embedded.push(part.split('"').next().unwrap_or("").to_owned());
            }
        }
    }
    embedded.sort();
    assert_eq!(
        embedded,
        [
            "../../../assets/brand/gauge/human.svg",
            "../../../assets/brand/gauge/robot.svg",
            "../../../assets/brand/ninja/bow.svg",
            "../../../assets/brand/ninja/ready.svg",
            "../../../assets/brand/ninja/stoic.svg",
            "../../../assets/brand/ninja/strike.svg",
            "../../../assets/brand/slop-ninja-wordmark.svg",
            "fonts/inter-400.ttf",
            "fonts/inter-700.ttf",
        ]
    );
    // The fonts' licence sits beside them, unchanged: the Inter project's
    // LICENSE.txt byte for byte. No emoji art ships with this Worker.
    let ofl = fs::read(format!("{}/src/fonts/OFL.txt", env!("CARGO_MANIFEST_DIR"))).expect("OFL.txt");
    let digest: String = Sha256::digest(&ofl).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest, INTER_LICENSE_SHA256, "src/fonts/OFL.txt is Inter's LICENSE.txt unchanged");
    let ofl = read("src/fonts/OFL.txt");
    assert!(ofl.starts_with("Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter)\n"));
    assert!(ofl.contains("SIL OPEN FONT LICENSE Version 1.1"));
    for name in SOURCES {
        assert!(!read(name).contains("fluent"), "{name}");
    }
}

#[test]
fn the_renderer_resolves_no_reference() {
    let r = read("src/render.rs");
    assert!(r.contains("resolve_data: Box::new(|_, _, _| None)"));
    assert!(r.contains("resolve_string: Box::new(|_, _| None)"));
    assert!(r.contains("resources_dir: None"));
    assert!(!r.contains("load_system_fonts") && !r.contains("load_fonts_dir") && !r.contains("load_font_file"));
    // resvg without default features: no system fonts, no file mapping, no
    // raster image decoders.
    let m = read("Cargo.toml");
    assert!(m.contains(r#"resvg = { version = "=0.50.0", default-features = false, features = ["text"] }"#));
}

#[test]
fn the_release_arena_features_are_the_production_ones() {
    let m = read("Cargo.toml");
    let line = m.lines().find(|l| l.starts_with("slop-ninja-arena = ")).expect("the arena dependency");
    for f in ["knobs", "selftest", "strict"] {
        assert!(!line.contains(&format!("\"{f}\"")), "{line}");
    }
    assert!(line.contains("\"observe\"") && line.contains("\"unmeasured-model\""), "{line}");
}

#[test]
fn card_strings_enter_only_as_text_nodes() {
    // Every write of a card value goes through `text` (escaped) inside a
    // `<text>` or `<tspan>`; template attributes take numbers, colours and
    // fixed ids only.
    let card = read("src/card.rs");
    for field in [
        "c.source_label",
        "c.headline",
        "c.model_line",
        "c.date",
        "c.free_cut_note",
        "c.document_note",
        "c.confidence_text",
    ] {
        for (i, l) in card.lines().enumerate() {
            if l.contains(field) {
                assert!(
                    !l.contains("format_args!") && !l.contains("put("),
                    "card.rs:{}: {field} in template text",
                    i + 1
                );
            }
        }
    }
    assert_eq!(
        card.matches("out.push_str(inner)").count() + read("src/art.rs").matches("out.push_str(inner)").count(),
        1
    );
}
