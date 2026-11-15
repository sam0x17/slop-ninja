//! Lint: every Worker that runs a Container starts it with a plain object
//! through `Reflect`, never with `worker`'s typed startup options.
//!
//! `worker`'s `ContainerStartupOptions` is a wasm-bindgen class instance:
//! its fields are accessors on the class prototype, its `env` a `Map` with
//! no own keys, and it always carries an `entrypoint` list. The runtime
//! reads the start options as a plain object's own properties, so an
//! instance started with the typed options would not receive the
//! environment they name. The Workers build the object with `Reflect::set`
//! and call the Container's `start` through `Reflect`
//! (`container_call(c.as_ref(), "start", &...)`).
//!
//! The scan covers every Rust source under `subnet/` but test files,
//! `tests/`, `fixtures/` and build output; comments and everything after a
//! file's first `#[cfg(test)]` are skipped. Run with `cargo test --test
//! container_start`.

use std::path::{Path, PathBuf};

/// The typed options; no production source names them.
const TYPED_OPTIONS: &str = "ContainerStartupOptions";
/// The typed start calls (`worker::Container::start`), refused in a crate
/// that runs a Container.
const TYPED_STARTS: &[&str] = &[".start(Some(", ".start(None)"];
/// The start through `Reflect` that a crate running a Container makes.
const REFLECT_START: &str = "container_call(c.as_ref(), \"start\", &";

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

/// A production source's code lines: up to its first `#[cfg(test)]`, each
/// without its comment.
fn code(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .take_while(|(_, l)| !l.trim_start().starts_with("#[cfg(test)]"))
        .map(|(i, l)| (i + 1, strip_comment(l)))
        .collect()
}

/// Violations in one production source of a crate (`containers`: whether
/// the crate runs a Container), as (line number, reason).
fn violations(text: &str, containers: bool) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (n, line) in code(text) {
        if line.contains(TYPED_OPTIONS) {
            found.push((n, format!("names the typed `{TYPED_OPTIONS}`")));
        }
        if containers && let Some(call) = TYPED_STARTS.iter().find(|c| line.contains(*c)) {
            found.push((n, format!("typed Container start `{call}`")));
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && !matches!(name.as_str(), "target" | "tests" | "fixtures" | "node_modules") {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") && !name.contains("test") {
            out.push(path);
        }
    }
}

/// A wrangler.toml without its comments.
fn config(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|l| match l.find('#') {
            Some(at) => &l[..at],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Worker crates that declare a Container, by directory.
fn container_crates(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut add = |dir: PathBuf| {
        let w = dir.join("wrangler.toml");
        if w.is_file() && config(&w).contains("containers]]") {
            dirs.push(dir);
        }
    };
    for entry in std::fs::read_dir(root).expect("subnet").flatten() {
        add(entry.path());
    }
    for entry in std::fs::read_dir(root.join("devtools")).expect("devtools").flatten() {
        add(entry.path());
    }
    dirs.sort();
    dirs
}

#[test]
fn every_container_starts_with_a_plain_object_through_reflect() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = container_crates(root);
    for name in ["coordinator", "detect-worker"] {
        assert!(crates.contains(&root.join(name)), "{name} declares a Container: {crates:?}");
    }
    let mut files = Vec::new();
    rust_files(root, &mut files);
    for dir in
        ["src", "coordinator/src", "detect-worker/src", "account-worker/src", "detect-fetch/src", "gen-worker/src"]
    {
        assert!(files.iter().any(|f| f.starts_with(root.join(dir))), "the scan reads nothing under {dir}");
    }
    let mut report = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("readable source");
        let containers = crates.iter().any(|c| file.starts_with(c));
        for (line, reason) in violations(&text, containers) {
            report.push(format!("{}:{line}: {reason}", file.strip_prefix(root).unwrap_or(file).display()));
        }
    }
    assert!(report.is_empty(), "start a Container with a plain object through Reflect:\n{}", report.join("\n"));
    // Each crate that runs a Container starts it through `Reflect`.
    for dir in &crates {
        let starts = files
            .iter()
            .filter(|f| f.starts_with(dir))
            .filter(|f| {
                let text = std::fs::read_to_string(f).expect("readable source");
                code(&text).iter().any(|(_, l)| l.contains(REFLECT_START))
            })
            .count();
        assert!(starts > 0, "{} starts its Container through Reflect", dir.display());
    }
}

#[test]
fn the_lint_finds_typed_options_and_typed_starts() {
    let typed = "let mut o = worker::ContainerStartupOptions::new();\nc.start(Some(o))?;\n";
    let found = violations(typed, true);
    assert_eq!(found.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 2], "{found:?}");
    assert_eq!(violations("c.start(None)?;", true).len(), 1);
    // A crate without a Container may call another `start`.
    assert!(violations("timer.start(Some(5));", false).is_empty());
    // Comments and tests do not count.
    let quiet = "// not `ContainerStartupOptions`\nlet x = 1; // c.start(None)\n#[cfg(test)]\nContainerStartupOptions::new();\n";
    assert!(violations(quiet, true).is_empty());
    // The plain object through Reflect passes.
    let plain = "let o = Object::new();\nReflect::set(&o, &\"enableInternet\".into(), &JsValue::FALSE);\ncontainer_call(c.as_ref(), \"start\", &o)?;\n";
    assert!(violations(plain, true).is_empty());
    assert!(code(plain).iter().any(|(_, l)| l.contains(REFLECT_START)));
}
