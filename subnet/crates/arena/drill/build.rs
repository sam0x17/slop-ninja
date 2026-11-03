//! With the `fixed-memory` feature on wasm32, links the drill module with a
//! fixed linear memory (`--initial-memory = --max-memory`), the model's
//! `LINEAR_MEMORY_BYTES`: `floor((isolate_bytes - js_reserve_bytes) /
//! page_bytes)` pages of the memory model the arena compiles (its
//! `memory-model.json`, or `SLOP_NINJA_MEMORY_MODEL`). The selftest case
//! `fixed_linear_memory_never_grows` and `selftest-node.mjs` check the result.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=SLOP_NINJA_MEMORY_MODEL");
    if env::var_os("CARGO_FEATURE_FIXED_MEMORY").is_none() || env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("wasm32") {
        return;
    }
    let path = env::var("SLOP_NINJA_MEMORY_MODEL").unwrap_or_else(|_| "../memory-model.json".to_owned());
    println!("cargo:rerun-if-changed={path}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    // Build time, on the host: the model file is a few hundred bytes.
    #[allow(clippy::disallowed_types)]
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let n = |k: &str| v[k].as_u64().unwrap_or_else(|| panic!("{path}: integer {k}"));
    let (page, isolate, js) = (n("page_bytes"), n("isolate_bytes"), n("js_reserve_bytes"));
    let bytes = (isolate - js) / page * page;
    println!("cargo:rustc-link-arg-cdylib=--initial-memory={bytes}");
    println!("cargo:rustc-link-arg-cdylib=--max-memory={bytes}");
}
