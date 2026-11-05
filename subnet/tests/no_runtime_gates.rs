//! Lint: production code never gates on a runtime version.
//!
//! A runtime upgrade must never invalidate a cohort, a package, a row or a
//! journal, and must never stop a validator. So production code may record a
//! spec or transaction version as an observation, but never compare one,
//! allow-list one or bake one into an identity. Signing reads the live versions
//! inside rtao at signing time; that is outside this tree. Neither may a
//! decision key on rtao's reviewed-runtime list or select its opt-in
//! `Unreviewed::Pause`: rtao follows the stored commit-reveal tag under any
//! runtime code (D-122), and so do we. SQL comparisons
//! count too; the one allowance is a lookup in the metadata cache keyed by the
//! runtime it holds ([`RUNTIME_KEYED_TABLE`]).
//!
//! The scan covers the subnet crates, the coordinator, the upload Worker, the
//! generation Worker, the publisher and the alerts relay. Test files and everything after a file's first
//! `#[cfg(test)]` are skipped. Comments are ignored. Run with
//! `cargo test --test no_runtime_gates`.

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

/// SQL forms that compare a column or a `json_extract` of one. A Rust `let`
/// or field assignment never contains them (`= ?` and `') =` are not Rust).
const SQL_COMPARISONS: &[&str] = &[" = ?", "') = ", "') IS ", " <> ", " IN ("];

/// The one table a SQL comparison on a runtime version may name: the
/// coordinator's cache of fetched metadata, keyed by the (spec, tx) pair it
/// describes. A lookup there only finds or misses a cached copy, and a miss
/// fetches the runtime again; it never refuses, defers or certifies anything.
/// Any other SQL comparison on a version name is a violation.
const RUNTIME_KEYED_TABLE: &str = "runtime_cache";

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
    // rtao follows the stored commit-reveal tag under any runtime code
    // (D-122). Its reviewed-runtime list is an observation rtao reports; a
    // decision of ours keyed on it, or its opt-in pause, would tie commits to
    // the runtimes a build lists.
    "Unreviewed::Pause",
    "with_unreviewed_cr_tags",
    "REVIEWED_CR_RUNTIME_CODE",
    "reviewed_cr_runtime",
    "REVIEWED_RUNTIME_CODE",
    "REVIEWED_RUNTIMES",
    "reviewed_runtime(",
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
        if let Some(name) = VERSION_NAMES.iter().find(|name| line.contains(*name)) {
            if let Some(op) = COMPARISONS.iter().find(|op| line.contains(*op)) {
                found.push((index + 1, format!("`{name}` used with `{}`", op.trim())));
            } else if let Some(op) = SQL_COMPARISONS.iter().find(|op| line.contains(*op))
                && !line.contains(RUNTIME_KEYED_TABLE)
            {
                found.push((index + 1, format!("`{name}` compared in SQL with `{}`", op.trim())));
            }
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
        "crates/admission-gate/src",
        "crates/artifact-check/src",
        "crates/arena/src",
        "crates/gen-core/src",
        "coordinator/src",
        "admission-worker/src",
        "gen-worker/src",
        "publisher/src",
        "alerts-relay/src",
        "alerts-relay/wire/src",
    ] {
        rust_files(&root.join(dir), &mut files);
    }
    // The build scripts: they configure what the Workers link.
    for script in ["admission-worker/build.rs", "coordinator/build.rs", "crates/arena/build.rs", "gen-worker/build.rs"] {
        let path = root.join(script);
        assert!(path.is_file(), "missing build script {script}");
        files.push(path);
    }
    for dir in ["crates/admission-gate/src", "admission-worker/src", "crates/gen-core/src", "gen-worker/src"] {
        assert!(
            files.iter().any(|f| f.starts_with(root.join(dir))),
            "the scan reads nothing under {dir}"
        );
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

/// D-120: compaction raises the journal to schema 4, which a build at the
/// previous pin refuses, so production code that opens an rtao journal never
/// compacts (a rollback must stay possible). Only an explicit `compact` call
/// does it. Every crate here that depends on `rtao-client` is scanned,
/// recursively (binaries under `src/bin` included).
#[test]
fn production_code_never_compacts_the_journal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dirs = [
        "src",
        "crates/validator/src",
        "crates/miner/src",
        "crates/admission-gate/src",
        "admission-worker/src",
    ];
    let mut files = Vec::new();
    for dir in dirs {
        rust_files(&root.join(dir), &mut files);
        assert!(files.iter().any(|f| f.starts_with(root.join(dir))), "the scan reads nothing under {dir}");
    }
    assert!(
        files.iter().any(|f| f.starts_with(root.join("crates/validator/src/bin"))),
        "the scan reads the validator's binaries"
    );
    let mut report = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("readable source");
        for (line, raw) in text.lines().enumerate() {
            if raw.trim_start().starts_with("#[cfg(test)]") {
                break;
            }
            let code = raw.split("//").next().unwrap_or_default();
            for banned in [".compact(", "compactable", "CompactionPolicy"] {
                if code.contains(banned) {
                    report.push(format!(
                        "{}:{}: {banned}",
                        file.strip_prefix(root).unwrap_or(file).display(),
                        line + 1
                    ));
                }
            }
        }
    }
    assert!(report.is_empty(), "journal compaction in production code:\n{}", report.join("\n"));
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
        "AND json_extract(preflight_json, '$.spec_version') = ?",
        "\"SELECT op_id FROM commit_ops WHERE spec_version = ? AND tag = ?\",",
        "WHERE json_extract(row, '$.transaction_version') IS NOT NULL",
        "WHERE spec_version IN (486, 487)",
        "let scheduler = scheduler.with_unreviewed_cr_tags(Unreviewed::Pause);",
        "if rtao_neuron::weights::reviewed_cr_runtime(code).is_some() { follow() }",
        "if commit_reveal::reviewed_runtime(&code).is_none() { defer() }",
    ] {
        assert!(!violations(bad).is_empty(), "{bad}");
    }
    for good in [
        "observed: RuntimeSeen { spec_version: runtime.spec_version(), transaction_version: 1 },",
        "tracing::info!(spec_version = runtime.spec_version());",
        "// a comment may say spec_version >= 482 without effect",
        "#[cfg(test)]\nfn f() { assert!(spec_version == 486) }",
        "\"SELECT metadata_hex FROM runtime_cache WHERE spec_version = ? AND tx_version = ?\",",
        "let spec_version = runtime.spec_version();",
        "\"SELECT epoch, json_extract(preflight_json, '$.spec_version') FROM commit_ops\",",
    ] {
        assert!(violations(good).is_empty(), "{good}");
    }
}
