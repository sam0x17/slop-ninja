//! Lint: production code never gates on a runtime version.
//!
//! A runtime upgrade must never invalidate a cohort, a package, a row or a
//! journal, and must never stop a validator. So production code may record a
//! spec or transaction version as an observation, but never compare one,
//! allow-list one or bake one into an identity. Signing reads the live versions
//! inside rtao at signing time; that is outside this tree.
//!
//! The scan covers the subnet crates and the coordinator. Test files and
//! everything after a file's first `#[cfg(test)]` are skipped. Comments are
//! ignored. Run with `cargo test --test no_runtime_gates`.

use std::path::{Path, PathBuf};

/// Names that hold a runtime version or a runtime-specific digest.
const VERSION_NAMES: &[&str] = &[
    "spec_version",
    "specVersion",
    "transaction_version",
    "transactionVersion",
    "spec_name",
    "runtime_version",
    "metadata_hash",
];

/// Forms that compare, allow-list or branch on a value.
const COMPARISONS: &[&str] = &[
    "==",
    "!=",
    "<=",
    ">=",
    " < ",
    " > ",
    ".contains(",
    "matches!(",
    ".cmp(",
    ".max(",
    ".min(",
    "match ",
    "=>",
];

/// Identifiers of hard runtime gates; none may return.
const BANNED: &[&str] = &[
    "FIRST_UNPORTED_RUNTIME",
    "VALIDATED_RUNTIMES",
    "TESTNET_QUALIFIABLE_RUNTIME",
    "PortBoundaryGuard",
    "check_runtime_port",
    "runtime_port_supported",
    "validated_spec_range",
    "runtime_validated",
    "admit_runtime",
    "testnet_qualification",
];

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

/// Violations in one production source text, as (line number, reason).
fn violations(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        if raw.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let line = strip_comment(raw);
        for banned in BANNED {
            if line.contains(banned) {
                found.push((index + 1, format!("hard runtime gate `{banned}`")));
            }
        }
        if let Some(name) = VERSION_NAMES.iter().find(|name| line.contains(*name))
            && let Some(op) = COMPARISONS.iter().find(|op| line.contains(*op))
        {
            found.push((index + 1, format!("`{name}` used with `{}`", op.trim())));
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
            if !matches!(name.as_str(), "target" | "tests" | "fixtures" | "build") {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") && !name.contains("test") {
            out.push(path);
        }
    }
}

#[test]
fn production_code_never_compares_runtime_versions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in [
        "src",
        "crates/protocol/src",
        "crates/protocol-check/src",
        "crates/protocol-wasm-fixture/src",
        "crates/validator/src",
        "crates/miner/src",
        "crates/miner/wire/src",
        "crates/artifact-check/src",
        "crates/arena/src",
        "coordinator/src",
    ] {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(files.len() > 10, "scan found too few files: {files:?}");
    let mut report = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("readable source");
        for (line, reason) in violations(&text) {
            report.push(format!(
                "{}:{line}: {reason}",
                file.strip_prefix(root).unwrap_or(file).display()
            ));
        }
    }
    assert!(
        report.is_empty(),
        "runtime-version gating in production code (record versions as observations only):\n{}",
        report.join("\n")
    );
}

#[test]
fn the_lint_detects_gates_and_allows_observations() {
    for bad in [
        "if runtime.spec_version() >= 482 { bail!() }",
        "ensure!(spec_version == 486);",
        "if ALLOWED.contains(&spec_version) {}",
        "match observed.spec_version { 486 => ok(), _ => refuse() }",
        "pub const FIRST_UNPORTED_RUNTIME: u32 = 482;",
        "let ok = record.metadata_hash != live_hash;",
        "#[cfg(feature = \"testnet_qualification\")]",
    ] {
        assert!(!violations(bad).is_empty(), "{bad}");
    }
    for good in [
        "observed: RuntimeSeen { spec_version: runtime.spec_version(), transaction_version: 1 },",
        "tracing::info!(spec_version = runtime.spec_version());",
        "// a comment may say spec_version >= 482 without effect",
        "#[cfg(test)]\nfn f() { assert!(spec_version == 486) }",
    ] {
        assert!(violations(good).is_empty(), "{good}");
    }
}
