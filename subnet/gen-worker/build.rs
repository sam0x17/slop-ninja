//! Links the wasm32 generation Worker with the memory model's linear memory ceiling:
//! `--max-memory = floor((isolate_bytes - js_reserve_bytes) / page_bytes)`
//! pages of `memory-model.json` (the file the arena compiles,
//! `SLOP_NINJA_MEMORY_MODEL`). A build whose arena enforces (a measured
//! model) also links `--initial-memory` at that size, a fixed memory that
//! never grows at run time. An `observe` build (an unmeasured model,
//! migration step 1) links growable memory under the same ceiling: the pool
//! grows it page by page as scopes are admitted (`memory.grow`, only the
//! arena calls it), so an isolate holds the linear memory it uses, as the
//! pre-arena build did, and the JS heap beside it keeps the room it had
//! until its high-water is measured (design section 5.1). Also exports the
//! ceiling as `SLOP_NINJA_LINEAR_MEMORY_BYTES`, which a test checks against
//! the arena's own derivation (`slop_ninja_arena::model::LINEAR_MEMORY_BYTES`),
//! and `SLOP_NINJA_LINEAR_MEMORY_FIXED` (`true` or `false`).

use std::env;

/// The production arena features: the arena dependency line of this crate's
/// manifest names none of the test-only features (`knobs`, `selftest`,
/// `strict`: tests get them through `arena-witness`), and an unmeasured
/// model only together with `observe` (design section 3.3). Returns whether
/// the arena runs in `observe` mode.
fn check_arena_features() -> bool {
    println!("cargo:rerun-if-changed=Cargo.toml");
    let manifest = std::fs::read_to_string("Cargo.toml").unwrap_or_else(|e| panic!("read Cargo.toml: {e}"));
    let line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("slop-ninja-arena = "))
        .unwrap_or_else(|| panic!("Cargo.toml: no slop-ninja-arena dependency line"));
    let features = line.split_once("features = [").map(|(_, r)| r.split(']').next().unwrap_or("")).unwrap_or("");
    let has = |f: &str| features.split(',').any(|x| x.trim().trim_matches('"') == f);
    for f in ["knobs", "selftest", "strict"] {
        assert!(!has(f), "Cargo.toml: the production arena dependency must not enable the test-only feature `{f}`");
    }
    assert!(!has("unmeasured-model") || has("observe"), "Cargo.toml: an unmeasured model ships only in observe mode");
    has("observe")
}

fn main() {
    let observe = check_arena_features();
    println!("cargo:rerun-if-env-changed=SLOP_NINJA_MEMORY_MODEL");
    let path = env::var("SLOP_NINJA_MEMORY_MODEL").unwrap_or_else(|_| "memory-model.json".to_owned());
    println!("cargo:rerun-if-changed={path}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let n = |k: &str| v[k].as_u64().unwrap_or_else(|| panic!("{path}: integer {k}"));
    let (page, isolate, js) = (n("page_bytes"), n("isolate_bytes"), n("js_reserve_bytes"));
    let bytes = (isolate - js) / page * page;
    let fixed = !observe;
    println!("cargo:rustc-env=SLOP_NINJA_LINEAR_MEMORY_BYTES={bytes}");
    println!("cargo:rustc-env=SLOP_NINJA_LINEAR_MEMORY_FIXED={fixed}");
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        if fixed {
            println!("cargo:rustc-link-arg-cdylib=--initial-memory={bytes}");
        }
        println!("cargo:rustc-link-arg-cdylib=--max-memory={bytes}");
    }
}
