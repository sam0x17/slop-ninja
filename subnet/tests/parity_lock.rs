//! Lint: the coordinator's parity lock is not stale.
//!
//! `coordinator/parity` is its own Cargo workspace (it builds rtao's native
//! runtime, which the coordinator's Wasm build must never see), so it has its
//! own `Cargo.lock`. A change to the coordinator's dependencies, or to a path
//! crate the coordinator uses, updates `coordinator/Cargo.lock` but not that
//! one, and the parity suite's `cargo test --locked` then refuses to build.
//!
//! This check compares the two lock files, without Cargo or the network. In
//! both, the path packages the coordinator reaches (the coordinator included)
//! must be the same, each depending on the same packages, named with their
//! source: a registry, a pinned Git revision, or a path (path packages also
//! with their version). Registry versions may differ: the parity workspace
//! also resolves rtao's native runtime, so a crate both use can be locked at
//! another version there. The coordinator's dev-dependencies, which only its
//! own workspace locks, are left out of its entry (read from its manifest).
//!
//! Not caught here: a raised version requirement that the parity lock's older
//! version no longer meets. The parity suite's own `--locked` build refuses it.
//!
//! The failure names the packages to refresh: run `cargo update --offline -p
//! <package> ...` in `subnet/coordinator/parity`, then the parity suite. Run
//! with `cargo test --test parity_lock`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const ROOT: &str = "slop-ninja-coordinator";

/// One `[[package]]` entry: name, version, source (`None` for a path
/// package) and its dependency entries as written.
#[derive(Debug, Default)]
struct Entry {
    name: String,
    version: String,
    source: Option<String>,
    deps: Vec<String>,
}

type Key = (String, String, Option<String>);

fn key(e: &Entry) -> Key {
    (e.name.clone(), e.version.clone(), e.source.clone())
}

/// The string value of a `field = "value"` line.
fn quoted(line: &str, field: &str) -> Option<String> {
    let rest = line.strip_prefix(field)?.trim_start().strip_prefix('=')?;
    let rest = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(rest.to_owned())
}

/// The `[[package]]` entries of a `Cargo.lock`.
fn entries(text: &str) -> Result<Vec<Entry>, String> {
    let mut out: Vec<Entry> = Vec::new();
    let mut in_deps = false;
    let mut in_package = false;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if in_deps {
            if line == "]" {
                in_deps = false;
            } else if let Some(last) = out.last_mut() {
                let dep = line.trim_end_matches(',');
                let dep = dep
                    .strip_prefix('"')
                    .and_then(|d| d.strip_suffix('"'))
                    .ok_or_else(|| format!("line {}: not a dependency entry: {raw}", n + 1))?;
                last.deps.push(dep.to_owned());
            }
            continue;
        }
        if line.starts_with('[') {
            // Any other table (`[metadata]`) is not a package's.
            in_package = line == "[[package]]";
            if in_package {
                out.push(Entry::default());
            }
            continue;
        }
        let Some(last) = out.last_mut().filter(|_| in_package) else {
            continue;
        };
        if let Some(v) = quoted(line, "name") {
            last.name = v;
        } else if let Some(v) = quoted(line, "version") {
            last.version = v;
        } else if let Some(v) = quoted(line, "source") {
            last.source = Some(v);
        } else if let Some(rest) = line.strip_prefix("dependencies") {
            let rest = rest.trim_start().strip_prefix('=').map(str::trim);
            match rest {
                Some("[") => in_deps = true,
                Some(list) if list.starts_with('[') && list.ends_with(']') => {
                    for dep in list[1..list.len() - 1].split(',') {
                        let dep = dep.trim().trim_matches('"');
                        if !dep.is_empty() {
                            last.deps.push(dep.to_owned());
                        }
                    }
                }
                _ => return Err(format!("line {}: unreadable dependency list: {raw}", n + 1)),
            }
        }
    }
    if let Some(e) = out
        .iter()
        .find(|e| e.name.is_empty() || e.version.is_empty())
    {
        return Err(format!("a package entry without a name or version: {e:?}"));
    }
    Ok(out)
}

/// A lock file's packages with each dependency entry resolved to the
/// package it names (`name`, `name version` or `name version (source)`).
fn resolved(text: &str) -> Result<BTreeMap<Key, Vec<Key>>, String> {
    let all = entries(text)?;
    let mut by_name: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for e in &all {
        by_name.entry(e.name.as_str()).or_default().push(e);
    }
    let resolve = |dep: &str| -> Result<Key, String> {
        let mut parts = dep.splitn(3, ' ');
        let name = parts.next().unwrap_or_default();
        let version = parts.next();
        let source = parts
            .next()
            .map(|s| s.trim_start_matches('(').trim_end_matches(')'));
        let found: Vec<&Entry> = by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|e| version.is_none_or(|v| e.version == v))
            .filter(|e| source.is_none_or(|s| e.source.as_deref() == Some(s)))
            .collect();
        match found.as_slice() {
            [one] => Ok(key(one)),
            [] => Err(format!("dependency {dep:?} names no package in the lock")),
            _ => Err(format!("dependency {dep:?} names more than one package")),
        }
    };
    let mut out = BTreeMap::new();
    for e in &all {
        let deps = e
            .deps
            .iter()
            .map(|d| resolve(d))
            .collect::<Result<_, _>>()?;
        if out.insert(key(e), deps).is_some() {
            return Err(format!("package {} {} is listed twice", e.name, e.version));
        }
    }
    Ok(out)
}

/// How a dependency is compared: by name and source, with the version only
/// for path packages (a registry crate's version may differ between the
/// locks; a Git source already names its revision).
fn ident(k: &Key) -> String {
    match &k.2 {
        None => format!("{} {} (path)", k.0, k.1),
        Some(source) => format!("{} ({source})", k.0),
    }
}

/// The path packages the coordinator reaches in a lock, each with the
/// identities of its dependencies; `skip` names left out of the
/// coordinator's own entry. Only path packages depend on path packages, so
/// the walk follows them alone.
fn path_closure(
    lock: &BTreeMap<Key, Vec<Key>>,
    skip: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let roots: Vec<&Key> = lock
        .keys()
        .filter(|k| k.0 == ROOT && k.2.is_none())
        .collect();
    let [root] = roots.as_slice() else {
        return Err(format!("{} path packages named {ROOT}", roots.len()));
    };
    let mut out = BTreeMap::new();
    let mut queue = vec![(*root).clone()];
    while let Some(k) = queue.pop() {
        if out.contains_key(&ident(&k)) {
            continue;
        }
        let deps: Vec<&Key> = lock[&k]
            .iter()
            .filter(|d| !(k.0 == ROOT && skip.contains(&d.0)))
            .collect();
        queue.extend(deps.iter().filter(|d| d.2.is_none()).map(|d| (*d).clone()));
        out.insert(ident(&k), deps.into_iter().map(ident).collect());
    }
    Ok(out)
}

/// Whether a manifest section lists dependencies, and which kind: `Some(true)`
/// for dev-dependencies. `[dependencies.<name>]` forms return the name too.
fn section(header: &str) -> (Option<bool>, Option<String>) {
    let parts: Vec<&str> = header.split('.').collect();
    let at = parts.iter().rposition(|p| {
        matches!(
            *p,
            "dependencies" | "build-dependencies" | "dev-dependencies"
        )
    });
    match at {
        Some(i) if i + 1 >= parts.len() => (Some(parts[i] == "dev-dependencies"), None),
        Some(i) => (
            Some(parts[i] == "dev-dependencies"),
            Some(parts[i + 1..].join(".").trim_matches('"').to_owned()),
        ),
        None => (None, None),
    }
}

/// The package names a manifest's dependency sections list: (normal and
/// build, also per target; dev). A `package = "..."` rename counts as the
/// package's name.
fn manifest_deps(text: &str) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let (mut normal, mut dev) = (BTreeSet::new(), BTreeSet::new());
    let mut kind: Option<bool> = None;
    let mut table: Option<String> = None;
    let mut depth = 0i32;
    let mut add = |dev_kind: bool, name: String| {
        if dev_kind {
            dev.insert(name);
        } else {
            normal.insert(name);
        }
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let opened = depth;
        depth += line.matches(['[', '{']).count() as i32 - line.matches([']', '}']).count() as i32;
        if opened > 0 || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if let (Some(dev_kind), Some(name)) = (kind, table.take()) {
                add(dev_kind, name);
            }
            depth = 0;
            let header = line.trim_start_matches('[').trim_end_matches(']').trim();
            (kind, table) = section(header);
            continue;
        }
        let Some(dev_kind) = kind else { continue };
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("unreadable manifest line: {raw}"));
        };
        let (k, v) = (k.trim().trim_matches('"'), v.trim());
        if table.is_some() {
            if k == "package" {
                table = Some(v.trim_matches('"').to_owned());
            }
            continue;
        }
        let name = v
            .split_once("package")
            .and_then(|(_, rest)| rest.trim_start().strip_prefix('='))
            .and_then(|rest| rest.trim_start().strip_prefix('"'))
            .and_then(|rest| rest.split('"').next())
            .unwrap_or(k.split('.').next().unwrap_or(k));
        add(dev_kind, name.to_owned());
    }
    if let (Some(dev_kind), Some(name)) = (kind, table) {
        add(dev_kind, name);
    }
    Ok((normal, dev))
}

/// The differences between the coordinator's lock and the parity lock, as
/// lines a person can act on; empty when the parity lock is current.
fn stale(coordinator_lock: &str, parity_lock: &str, manifest: &str) -> Result<Vec<String>, String> {
    let (normal, dev) = manifest_deps(manifest)?;
    let dev_only: BTreeSet<String> = dev.difference(&normal).cloned().collect();
    let ours = path_closure(&resolved(coordinator_lock)?, &dev_only)?;
    let parity_resolved = resolved(parity_lock)?;
    let theirs = path_closure(&parity_resolved, &BTreeSet::new())?;
    let mut out = Vec::new();
    let mut refresh = BTreeSet::new();
    for (pkg, deps) in &ours {
        match theirs.get(pkg) {
            None => out.push(format!("{pkg}: missing from the parity lock")),
            Some(locked) if locked != deps => {
                let list = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
                    a.difference(b)
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let (added, gone) = (list(deps, locked), list(locked, deps));
                let mut line = format!("{pkg}: the parity lock");
                if !added.is_empty() {
                    line.push_str(&format!(" lacks [{added}]"));
                }
                if !gone.is_empty() {
                    let and = if added.is_empty() { "" } else { " and" };
                    line.push_str(&format!("{and} still lists [{gone}]"));
                }
                out.push(line);
                refresh.insert(pkg.split(' ').next().unwrap_or(pkg).to_owned());
            }
            Some(_) => {}
        }
    }
    for pkg in theirs.keys().filter(|p| !ours.contains_key(*p)) {
        out.push(format!(
            "{pkg}: in the parity lock, no longer used by the coordinator"
        ));
    }
    if !out.is_empty() {
        // A package that is not in the parity lock cannot be named with -p;
        // the coordinator's entry (always there) brings it in.
        if refresh.is_empty() {
            refresh.insert(ROOT.to_owned());
        }
        let names: Vec<String> = refresh.iter().map(|p| format!("-p {p}")).collect();
        out.push(format!(
            "refresh it: (cd subnet/coordinator/parity && cargo update --offline {}), then run the \
             parity suite (cargo test --locked there)",
            names.join(" ")
        ));
    }
    Ok(out)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_parity_lock_is_current() {
    let coordinator = Path::new(env!("CARGO_MANIFEST_DIR")).join("coordinator");
    let problems = stale(
        &read(&coordinator.join("Cargo.lock")),
        &read(&coordinator.join("parity/Cargo.lock")),
        &read(&coordinator.join("Cargo.toml")),
    )
    .unwrap_or_else(|e| panic!("the locks could not be compared: {e}"));
    assert!(
        problems.is_empty(),
        "subnet/coordinator/parity/Cargo.lock is stale against subnet/coordinator/Cargo.lock:\n{}",
        problems.join("\n")
    );
}

// The checker itself, on small synthetic locks.

const REG: &str = "registry+https://github.com/rust-lang/crates.io-index";
const GIT: &str = "git+https://example.invalid/rtao?rev=aaaa#aaaa";

type Spec<'a> = (&'a str, &'a str, Option<&'a str>, &'a [&'a str]);

/// A lock file from (name, version, source, dependency entries).
fn lock(packages: &[Spec<'_>]) -> String {
    format!("# @generated\nversion = 4\n{}", entries_text(packages))
}

/// The `[[package]]` entries alone.
fn entries_text(packages: &[Spec<'_>]) -> String {
    let mut out = String::new();
    for (name, version, source, deps) in packages {
        out.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
        ));
        if let Some(s) = source {
            out.push_str(&format!("source = \"{s}\"\nchecksum = \"00\"\n"));
        }
        if !deps.is_empty() {
            out.push_str("dependencies = [\n");
            for d in *deps {
                out.push_str(&format!(" \"{d}\",\n"));
            }
            out.push_str("]\n");
        }
    }
    out
}

const MANIFEST: &str = r#"
[package]
name = "slop-ninja-coordinator"

[dependencies]
serde = { version = "1", features = ["derive"] }
slop-ninja-protocol = { path = "../crates/protocol" }
rtao-core = { git = "https://example.invalid/rtao", rev = "aaaa", default-features = false }

[target.'cfg(target_arch = "wasm32")'.dependencies]
js-sys = "0.3"

[build-dependencies]
serde_json = "1"

[dev-dependencies]
# Test-only.
rusqlite = { version = "0.37", features = [
    "bundled",
] }
slop-ninja-protocol = { path = "../crates/protocol", features = ["fixtures"] }

[patch.crates-io]
w3f-bls = { git = "https://example.invalid/subtensor.git", tag = "v1" }
"#;

/// The coordinator's own lock: its dependencies, dev ones included.
fn coordinator_lock(protocol_deps: &[&str]) -> String {
    lock(&[
        (
            ROOT,
            "0.1.0",
            None,
            &[
                "js-sys",
                "rtao-core",
                "rusqlite",
                "serde",
                "serde_json",
                "slop-ninja-protocol",
            ],
        ),
        ("js-sys", "0.3.80", Some(REG), &[]),
        ("rtao-core", "0.1.0", Some(GIT), &["serde"]),
        ("rusqlite", "0.37.0", Some(REG), &[]),
        ("serde", "1.0.200", Some(REG), &[]),
        ("serde_json", "1.0.150", Some(REG), &["serde"]),
        ("sha2", "0.10.9", Some(REG), &[]),
        ("slop-ninja-protocol", "0.1.0", None, protocol_deps),
    ])
}

/// The parity lock: the coordinator without dev-dependencies, rtao's native
/// runtime beside it, and other registry versions.
fn parity_lock(root_deps: &[&str], protocol_deps: &[&str]) -> String {
    lock(&[
        ("js-sys", "0.3.107", Some(REG), &[]),
        ("rtao-core", "0.1.0", Some(GIT), &["serde"]),
        ("rtao-runtime", "0.1.0", Some(GIT), &["serde", "tokio"]),
        ("serde", "1.0.228", Some(REG), &[]),
        ("serde_json", "1.0.154", Some(REG), &["serde"]),
        ("sha2", "0.10.9", Some(REG), &[]),
        (ROOT, "0.1.0", None, root_deps),
        (
            "slop-ninja-coordinator-parity",
            "0.1.0",
            None,
            &[ROOT, "rtao-core", "rtao-runtime"],
        ),
        ("slop-ninja-protocol", "0.1.0", None, protocol_deps),
        ("tokio", "1.0.0", Some(REG), &[]),
    ])
}

const ROOT_DEPS: &[&str] = &[
    "js-sys",
    "rtao-core",
    "serde",
    "serde_json",
    "slop-ninja-protocol",
];

#[test]
fn registry_versions_and_dev_dependencies_may_differ() {
    let problems = stale(
        &coordinator_lock(&["serde"]),
        &parity_lock(ROOT_DEPS, &["serde"]),
        MANIFEST,
    );
    assert_eq!(problems, Ok(Vec::<String>::new()));
}

#[test]
fn a_new_coordinator_dependency_is_caught() {
    // The coordinator gained a path crate: its own lock has it, the parity lock does not.
    let mut ours = coordinator_lock(&["serde"]);
    ours = ours.replace(
        "\"slop-ninja-protocol\",\n]",
        "\"slop-ninja-protocol\",\n \"slop-ninja-span-eval\",\n]",
    );
    ours.push_str(&entries_text(&[(
        "slop-ninja-span-eval",
        "0.1.0",
        None,
        &["serde_json"],
    )]));
    let problems = stale(&ours, &parity_lock(ROOT_DEPS, &["serde"]), MANIFEST).unwrap();
    let text = problems.join("\n");
    assert!(
        text.contains("slop-ninja-span-eval 0.1.0 (path): missing from the parity lock"),
        "{text}"
    );
    assert!(text.contains("slop-ninja-coordinator 0.1.0 (path): the parity lock lacks [slop-ninja-span-eval 0.1.0 (path)]"), "{text}");
    assert!(
        text.contains("cargo update --offline -p slop-ninja-coordinator)"),
        "{text}"
    );
}

#[test]
fn a_path_crate_dependency_change_is_caught() {
    // The protocol crate gained sha2; only the coordinator's lock was refreshed.
    let problems = stale(
        &coordinator_lock(&["serde", "sha2"]),
        &parity_lock(ROOT_DEPS, &["serde"]),
        MANIFEST,
    )
    .unwrap();
    let text = problems.join("\n");
    assert!(
        text.contains(&format!(
            "slop-ninja-protocol 0.1.0 (path): the parity lock lacks [sha2 ({REG})]"
        )),
        "{text}"
    );
    assert!(text.contains("-p slop-ninja-protocol"), "{text}");
}

#[test]
fn a_dropped_or_moved_dependency_is_caught() {
    // The parity lock still lists a dependency the coordinator dropped.
    let mut extra = ROOT_DEPS.to_vec();
    extra.push("sha2");
    let text = stale(
        &coordinator_lock(&["serde"]),
        &parity_lock(&extra, &["serde"]),
        MANIFEST,
    )
    .unwrap()
    .join("\n");
    assert!(
        text.contains(&format!("still lists [sha2 ({REG})]")),
        "{text}"
    );
    // A Git dependency at another revision is a different package.
    let moved = parity_lock(ROOT_DEPS, &["serde"])
        .replace(GIT, "git+https://example.invalid/rtao?rev=bbbb#bbbb");
    let text = stale(&coordinator_lock(&["serde"]), &moved, MANIFEST)
        .unwrap()
        .join("\n");
    assert!(
        text.contains("rtao-core (git+https://example.invalid/rtao?rev=aaaa#aaaa)"),
        "{text}"
    );
}

#[test]
fn manifest_sections_are_read_by_kind() {
    let (normal, dev) = manifest_deps(MANIFEST).unwrap();
    let names = |s: &[&str]| s.iter().map(|n| (*n).to_owned()).collect::<BTreeSet<_>>();
    assert_eq!(
        normal,
        names(&[
            "js-sys",
            "rtao-core",
            "serde",
            "serde_json",
            "slop-ninja-protocol"
        ])
    );
    assert_eq!(dev, names(&["rusqlite", "slop-ninja-protocol"]));
    let tables = "[dependencies.foo]\nversion = \"1\"\npackage = \"bar\"\n[dev-dependencies]\nbaz = { package = \"qux\", version = \"2\" }\n";
    let (normal, dev) = manifest_deps(tables).unwrap();
    assert_eq!((normal, dev), (names(&["bar"]), names(&["qux"])));
}
