//! Source scan (design §3.2): the rules the type system and clippy cannot
//! state. Scans this crate's production modules, the drill and the glue
//! probe. A flagged line may stay only with an `alloc-ok:` (or `scan-ok:`)
//! justification on the same line or the line before.

use std::fs;
use std::path::{Path, PathBuf};

struct Rule {
    name: &'static str,
    needles: &'static [&'static str],
    /// Files (paths relative to the crate root) the rule applies to.
    files: &'static [&'static str],
    why: &'static str,
}

/// Production modules of the arena crate (the self-tests are test code).
const ARENA: &[&str] = &["src/pool.rs", "src/scope.rs", "src/io.rs", "src/stats.rs", "src/glue.rs", "src/sys.rs", "src/model.rs", "src/strict.rs", "src/lib.rs"];
const ALL: &[&str] = &[
    "src/pool.rs",
    "src/scope.rs",
    "src/io.rs",
    "src/stats.rs",
    "src/glue.rs",
    "src/sys.rs",
    "src/model.rs",
    "src/strict.rs",
    "src/lib.rs",
    "drill/src/lib.rs",
    "drill/src/wasm.rs",
    "glue-probe/src/lib.rs",
];

const RULES: &[Rule] = &[
    Rule {
        name: "infallible-allocation",
        needles: &["format!(", ".to_string()", ".to_owned()", "vec![", "Vec::with_capacity", "String::from(", ".push(", ".push_str(", ".collect()", "Box::new(", ".into_owned()", ".clone()"],
        files: &["src/pool.rs", "src/scope.rs", "src/io.rs", "src/stats.rs", "src/glue.rs", "src/sys.rs", "src/model.rs", "src/strict.rs", "src/lib.rs", "drill/src/lib.rs"],
        why: "an infallible allocation API in a path that handles variable-size input; admit first and use try_reserve_exact, or justify",
    },
    Rule {
        name: "linear-memory-view",
        needles: &["Uint8Array::view", "view_mut_raw"],
        files: ALL,
        why: "a JS view of linear memory can outlive its buffer or be detached by memory.grow while an async consumer holds it",
    },
    Rule {
        name: "panic-hook",
        needles: &["set_hook("],
        files: ALL,
        why: "the hook's box outlives the scope it is installed in",
    },
    Rule {
        name: "leak",
        needles: &["mem::forget(", "Box::leak(", "::leak(", ".leak()"],
        files: ALL,
        why: "a leaked value pins its scope's pages forever",
    },
    Rule {
        name: "select-timeout",
        needles: &["select(", "select!("],
        files: ALL,
        why: "dropping a pending JsFuture keeps its callbacks and scope pages until the promise settles; use AbortSignal timeouts",
    },
    Rule {
        name: "test-knob",
        needles: &["set_mode(", "set_page_limit(", "reset_latches(", "shrink_free_to("],
        files: &["src/pool.rs", "src/scope.rs", "src/io.rs", "src/stats.rs", "src/glue.rs", "src/sys.rs", "src/model.rs", "src/lib.rs", "drill/src/lib.rs", "drill/src/wasm.rs"],
        why: "test knobs are feature-gated (`knobs`) and stay out of production paths",
    },
    Rule {
        name: "heap-static",
        needles: &["static ", "thread_local!"],
        files: ALL,
        why: "a static or thread-local that holds heap outlives every scope",
    },
    Rule {
        name: "global-allocator",
        needles: &["#[global_allocator]"],
        files: ARENA,
        why: "only the binaries (tests, drill, probe) install the allocator",
    },
];

/// `static` lines are flagged only when they name a heap type.
fn heap_static(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("thread_local!") {
        return true;
    }
    if !(t.starts_with("static ") || t.starts_with("pub static ") || t.starts_with("pub(crate) static ")) {
        return false;
    }
    ["Vec<", "String", "Box<", "BTreeMap", "HashMap", "Rc<", "Arc<"].iter().any(|h| t.contains(h))
}

fn justified(lines: &[&str], i: usize) -> bool {
    let ok = |l: &str| l.contains("alloc-ok:") || l.contains("scan-ok:");
    ok(lines[i]) || (i > 0 && ok(lines[i - 1]))
}

fn code_part(line: &str) -> &str {
    // Ignore doc comments and line comments.
    let t = line.trim_start();
    if t.starts_with("//") {
        return "";
    }
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

#[test]
fn production_sources_follow_the_scan_rules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for rule in RULES {
        for f in rule.files {
            let path: PathBuf = root.join(f);
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let lines: Vec<&str> = text.lines().collect();
            // Code after `#[cfg(test)]` or a `mod tests` is test code.
            let end = lines.iter().position(|l| l.trim() == "#[cfg(test)]").unwrap_or(lines.len());
            for i in 0..end {
                let code = code_part(lines[i]);
                let hit = if rule.name == "heap-static" { heap_static(code) } else { rule.needles.iter().any(|n| code.contains(n)) };
                if hit && !justified(&lines, i) {
                    failures.push(format!("{f}:{}: [{}] {}\n    {}", i + 1, rule.name, rule.why, lines[i].trim()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} source-scan finding(s):\n{}", failures.len(), failures.join("\n"));
}

/// Production manifests must not enable test knobs or an unmeasured model.
#[test]
fn production_manifests_enable_no_test_features() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let default = text.lines().find(|l| l.starts_with("default = ")).unwrap();
    for f in ["knobs", "selftest", "strict", "unmeasured-model", "observe"] {
        assert!(!default.contains(&format!("\"{f}\"")), "the default feature set enables `{f}`");
    }
}

/// Design §2.5: a scope that awaits has a deadline. `enter` and `wait_until`
/// take an `AsyncRoute` (the type rejects a `Route`), and a `Route::new`
/// written inline into an async entry, or a sync scope wrapping a future, is
/// flagged here as well.
#[test]
fn async_entries_never_use_a_sync_route() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for f in ALL {
        let Ok(text) = fs::read_to_string(root.join(f)) else { continue };
        for (i, line) in text.lines().enumerate() {
            let code = code_part(line);
            if code.contains("Route::new(") && !code.contains("AsyncRoute::new(") && ["enter(", "wait_until(", ".scoped(", "open_async("].iter().any(|n| code.contains(n)) {
                failures.push(format!("{f}:{}: a sync Route on an async entry: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Design §2.2 and §2.5: with the hand-written exports, every export takes
/// its glue ticket (a Durable Object constructor its resident token) as its
/// first statement, before anything allocates, and the worker-macros
/// exports exist only in the comparison build.
#[test]
fn every_admitted_export_takes_its_ticket_first() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(root.join("glue-probe/src/lib.rs")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let admitted = lines.iter().position(|l| l.trim_start().starts_with("mod admitted {")).expect("mod admitted");
    let macro_mod = lines.iter().position(|l| l.trim_start().starts_with("mod macro_exports {")).expect("mod macro_exports");
    assert!(lines[macro_mod - 1].contains(r#"cfg(not(feature = "admitted"))"#), "the macro exports are the comparison build only");
    let mut failures = Vec::new();
    let mut exports = 0;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("#[event(") || t.starts_with("#[durable_object") {
            if !(i > macro_mod && i < admitted) {
                failures.push(format!("glue-probe/src/lib.rs:{}: a worker-macros export outside the comparison build", i + 1));
            }
            continue;
        }
        if i < admitted {
            continue;
        }
        let first = lines.get(i + 1).map(|n| n.trim()).unwrap_or("");
        for name in ["fetch", "queue", "scheduled", "alarm"] {
            if t.starts_with(&format!("pub fn {name}(")) {
                exports += 1;
                if !first.contains("glue::Ticket::admit()") {
                    failures.push(format!("glue-probe/src/lib.rs:{}: export `{name}` does not take its ticket first", i + 1));
                }
            }
        }
        if t.starts_with("pub fn new(") {
            exports += 1;
            if !first.contains("glue::Resident::admit()") {
                failures.push(format!("glue-probe/src/lib.rs:{}: the Durable Object constructor does not take its resident token first", i + 1));
            }
        }
    }
    assert!(exports >= 6, "found {exports} hand-written exports");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Design §5.1: the clippy frame, future and stack-array thresholds are
/// `stack_bytes / 2 - stack_high_water_bytes` of each build's model.
#[test]
fn clippy_stack_thresholds_equal_the_models() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (toml, model) in [("clippy.toml", "memory-model.json"), ("glue-probe/clippy.toml", "glue-probe/memory-model.json")] {
        let m = fs::read_to_string(root.join(model)).unwrap();
        let hw = json_u64(&m, "stack_high_water_bytes");
        let stack = json_u64(&m, "stack_bytes");
        assert!(hw > 0 && 2 * hw <= stack, "{model}: measured high-water within the stated margin");
        let t = fs::read_to_string(root.join(toml)).unwrap();
        for key in ["stack-size-threshold", "future-size-threshold", "array-size-threshold"] {
            let v = t
                .lines()
                .find_map(|l| l.trim().strip_prefix(key).and_then(|r| r.trim().strip_prefix('=')).map(|v| v.trim().parse::<u64>().unwrap()))
                .unwrap_or_else(|| panic!("{toml}: {key}"));
            assert_eq!(v, stack / 2 - hw, "{toml}: {key} must equal {model} stack_bytes / 2 - stack_high_water_bytes");
        }
    }
}

/// A top-level integer field of a small JSON object (the memory models).
fn json_u64(text: &str, key: &str) -> u64 {
    let k = format!("\"{key}\":");
    let at = text.find(&k).unwrap_or_else(|| panic!("{key}")) + k.len();
    text[at..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap()
}
