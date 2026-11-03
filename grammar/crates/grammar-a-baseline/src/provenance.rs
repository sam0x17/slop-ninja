//! Provenance paths in package JSON: no absolute local path, user name or
//! private directory name.
//!
//! An artifact's JSON role files (the contract among them) are published
//! after its cohort closes, so a provenance string that names a local file or
//! directory must not carry its author's directory layout: an absolute path
//! names the operating-system user and the home directory, and the
//! repository's ignored trees (`data/`) hold private corpora, fits and
//! experiment names. [`portable_path`] gives the form the tools record, the
//! rule of the exporters' `provenance_paths.py`:
//!
//! - a path is resolved against the working directory (a relative one too,
//!   so a tool run from inside a private tree records nothing of it); one
//!   that resolves inside the repository, lexically or after resolving
//!   symlinks, is repository-relative, with `/` separators and `.` parts
//!   dropped;
//! - a path under an ignored private tree ([`PRIVATE_TREES`]), or outside
//!   the repository, is only its final component;
//! - a final component that names a user or a home directory is
//!   [`OUTSIDE`] instead.
//!
//! [`portable_at_in`] rewrites only the strings at known provenance pointers,
//! each as a string already recorded in a file ([`portable_recorded_in`]):
//! the working directory it was written against is no longer known, so a
//! relative one is read as repository-relative.
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Ignored trees of the repository whose directory names are private.
pub const PRIVATE_TREES: &[&str] = &["data"];
/// Recorded for a location whose final component would name a user or a
/// home directory.
pub const OUTSIDE: &str = "(outside the repository)";
/// Parents whose children are home directories.
const HOME_ROOTS: &[&str] = &["/home", "/Users", "/var/home", "/usr/home"];

/// The repository root of this checkout.
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Whether `s` reads as an absolute local path: POSIX `/x`, `~/x` or
/// `~user/x`, a Windows drive path (`C:\x`, `C:/x`) or a UNC path
/// (`\\host\share`). Text with a line break is never a path.
pub fn is_local_path(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 2 || s.contains(['\n', '\r']) {
        return false;
    }
    match b[0] {
        b'/' => true,
        b'\\' => b[1] == b'\\',
        b'~' => s.split_once('/').is_some_and(|(head, _)| !head.contains(char::is_whitespace)),
        c => c.is_ascii_alphabetic() && b[1] == b':' && b.len() > 2 && matches!(b[2], b'\\' | b'/'),
    }
}

/// Lexical normalization: `.` dropped, `..` taken against the parent.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn within(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The names that are this user's: the login names and the home directory's.
fn user_names() -> Vec<String> {
    let mut v: Vec<String> = ["USER", "LOGNAME"].iter().filter_map(|k| std::env::var(k).ok()).collect();
    if let Some(name) = home().as_deref().and_then(Path::file_name) {
        v.push(name.to_string_lossy().into_owned());
    }
    v.retain(|n| !n.is_empty());
    v
}

/// The final component `name` of `location`, or [`OUTSIDE`] when it names a
/// user or a home directory.
fn final_component(name: &str, location: &Path) -> String {
    let parent = location.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('~')
        || user_names().iter().any(|u| u == name)
        || HOME_ROOTS.contains(&parent.as_str())
        || location == Path::new("/root")
        || home().is_some_and(|h| normalize(&h) == normalize(location))
    {
        return OUTSIDE.into();
    }
    name.into()
}

/// A repository-relative path as recorded: itself, or its final component
/// under a private tree.
fn inside(rel: &str) -> String {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [first] if PRIVATE_TREES.contains(first) => OUTSIDE.into(),
        [first, .., last] if PRIVATE_TREES.contains(first) => final_component(last, Path::new(rel)),
        _ => rel.into(),
    }
}

/// `path` as package JSON records it, resolved against the working
/// directory, against the repository root `root`.
pub fn portable_path_in(path: &Path, root: &Path) -> String {
    portable_path_from(path, &std::env::current_dir().unwrap_or_default(), root)
}

/// [`portable_path_in`] with the working directory `cwd` a relative path is
/// resolved against.
pub fn portable_path_from(path: &Path, cwd: &Path, root: &Path) -> String {
    let text = path.to_string_lossy();
    let b = text.as_bytes();
    if b.starts_with(b"\\\\") || (b.len() > 2 && b[1] == b':' && matches!(b[2], b'\\' | b'/')) {
        // A Windows path on a Unix host: never inside this checkout.
        let mut parts = text.rsplit(['\\', '/']).filter(|p| !p.is_empty());
        let name = parts.next().unwrap_or("");
        let parent = parts.next().unwrap_or("");
        if name.eq_ignore_ascii_case("users") || parent.eq_ignore_ascii_case("users") {
            return OUTSIDE.into();
        }
        return final_component(name, Path::new(name));
    }
    let expanded = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            home().unwrap_or_default().join(rest.trim_start_matches('/'))
        }
        // Another user's home: never this checkout's.
        Some(rest) => {
            let after = rest.split_once('/').map_or("", |(_, a)| a).trim_matches('/');
            return match Path::new(after).file_name() {
                Some(n) if !after.is_empty() => final_component(&n.to_string_lossy(), Path::new(after)),
                _ => OUTSIDE.into(),
            };
        }
        None => path.to_path_buf(),
    };
    let lexical = normalize(&cwd.join(expanded));
    if let Some(rel) = within(&lexical, &normalize(root)) {
        return if rel == "." { rel } else { inside(&rel) };
    }
    if let (Ok(real), Ok(real_root)) = (lexical.canonicalize(), root.canonicalize())
        && let Some(rel) = within(&real, &real_root)
    {
        return if rel == "." { rel } else { inside(&rel) };
    }
    let name = lexical.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    final_component(&name, &lexical)
}

/// `path` as package JSON records it, against this checkout's root.
pub fn portable_path(path: &Path) -> String {
    portable_path_in(path, &repository_root())
}

fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Whether JSON pointer `pointer` matches `pattern`, whose `*` segments
/// match any one segment.
fn matches(pointer: &str, pattern: &str) -> bool {
    let (a, b): (Vec<&str>, Vec<&str>) = (pointer.split('/').collect(), pattern.split('/').collect());
    a.len() == b.len() && a.iter().zip(&b).all(|(p, q)| *q == "*" || p == q)
}

/// A location string already recorded in a file, as package JSON records
/// it. An absolute local path is made portable ([`portable_path_in`]). A
/// relative one was written against a working directory that is no longer
/// known, so it is read as repository-relative: under a private tree, or
/// with `..`, only its final component is kept; otherwise it stays as
/// written (`/` separators, `.` parts dropped).
pub fn portable_recorded_in(s: &str, root: &Path) -> String {
    if is_local_path(s) || s.starts_with('~') {
        return portable_path_in(Path::new(s), root);
    }
    let unified = s.replace('\\', "/");
    let parts: Vec<&str> = unified.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.contains(&"..") {
        let name = parts.last().copied().unwrap_or_default();
        return final_component(name, Path::new(name));
    }
    if parts.is_empty() { ".".into() } else { inside(&parts.join("/")) }
}

/// Whether `s` reads as a relative path under one of the repository's
/// private trees (`data/x`): no whitespace, its first `/` component a private
/// tree, and at least one more component.
pub fn names_private_tree(s: &str) -> bool {
    if s.contains(char::is_whitespace) {
        return false;
    }
    let unified = s.replace('\\', "/");
    let trimmed = unified.trim_start_matches(['.', '/']);
    match trimmed.split_once('/') {
        Some((first, rest)) => PRIVATE_TREES.contains(&first) && !rest.trim_matches('/').is_empty(),
        None => false,
    }
}

/// Makes the location strings of `value` at the JSON pointer patterns
/// `patterns` portable (against `root`, [`portable_recorded_in`]), and
/// returns the pointers of those that changed. Strings anywhere else are
/// never changed.
pub fn portable_at_in(value: &mut Value, patterns: &[&str], root: &Path) -> Vec<String> {
    fn walk(v: &mut Value, pointer: &mut String, patterns: &[&str], root: &Path, out: &mut Vec<String>) {
        let len = pointer.len();
        match v {
            Value::Object(map) => {
                for (k, x) in map.iter_mut() {
                    pointer.push('/');
                    pointer.push_str(&pointer_token(k));
                    walk(x, pointer, patterns, root, out);
                    pointer.truncate(len);
                }
            }
            Value::Array(items) => {
                for (i, x) in items.iter_mut().enumerate() {
                    pointer.push('/');
                    pointer.push_str(&i.to_string());
                    walk(x, pointer, patterns, root, out);
                    pointer.truncate(len);
                }
            }
            Value::String(s)
                if !s.is_empty() && !s.contains(['\n', '\r']) && patterns.iter().any(|p| matches(pointer, p)) =>
            {
                let new = portable_recorded_in(s, root);
                if new != *s {
                    *s = new;
                    out.push(pointer.clone());
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, &mut String::new(), patterns, root, &mut out);
    out
}

/// [`portable_at_in`] against this checkout's root.
pub fn portable_at(value: &mut Value, patterns: &[&str]) -> Vec<String> {
    portable_at_in(value, patterns, &repository_root())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absolute_local_paths_are_recognized() {
        for yes in ["/opt/u/x", "/x", "~/x", "~user/x", "C:\\x", "c:/x", "\\\\host\\share"] {
            assert!(is_local_path(yes), "{yes}");
        }
        for no in ["", "/", "x/y", "norm/run-000", "~", "~ u/x", "a:b", "C:", "line\n/x", "/x\r", "1/2"] {
            assert!(!is_local_path(no), "{no:?}");
        }
    }

    #[test]
    fn relative_paths_resolve_against_the_working_directory() {
        let root = Path::new("/repo");
        let at = |p: &str, cwd: &str| portable_path_from(Path::new(p), Path::new(cwd), root);
        assert_eq!(at("norm/run-000", "/repo"), "norm/run-000");
        assert_eq!(at("./norm//run-000/", "/repo"), "norm/run-000");
        assert_eq!(at(".", "/repo"), ".");
        assert_eq!(at("data/grammar-private/exp-17/fit", "/repo"), "fit");
        assert_eq!(at("data", "/repo"), OUTSIDE);
        // Run from inside the private tree: a relative path is still under it.
        assert_eq!(at("spans-v1/fits/S0+-seed-17/fit", "/repo/data/grammar-private/exp-17"), "fit");
        assert_eq!(at("../../../grammar/x", "/repo/data/grammar-private/exp-17"), "grammar/x");
        // Elsewhere in the repository: repository-relative.
        assert_eq!(at("crates/x", "/repo/grammar"), "grammar/crates/x");
        // Outside the repository: only the final component.
        assert_eq!(at("elsewhere/fits/fit", "/tmp"), "fit");
    }

    #[test]
    fn recorded_relative_strings_are_read_as_repository_relative() {
        let root = Path::new("/srv/checkout");
        assert_eq!(portable_recorded_in("norm/run-001", root), "norm/run-001");
        assert_eq!(portable_recorded_in("./norm//run-001/", root), "norm/run-001");
        assert_eq!(portable_recorded_in("data/grammar-private/exp-17/fits/fit", root), "fit");
        assert_eq!(portable_recorded_in("../exp-17/fit", root), "fit");
        assert_eq!(portable_recorded_in("fit", root), "fit");
        assert_eq!(portable_recorded_in(OUTSIDE, root), OUTSIDE);
        assert_eq!(portable_recorded_in("/srv/checkout/data/x/run-001", root), "run-001");
        assert_eq!(portable_recorded_in("~someone/fits/e1", root), "e1");
        for yes in ["data/x", "./data/x/y", "data\\x"] {
            assert!(names_private_tree(yes), "{yes}");
        }
        for no in ["data", "data/", "metadata/x", "data x/y", "n/data/x"] {
            assert!(!names_private_tree(no), "{no}");
        }
    }

    #[test]
    fn absolute_paths_become_repository_relative_or_their_final_component() {
        let root = Path::new("/srv/checkout");
        assert_eq!(portable_path_in(Path::new("/srv/checkout/grammar/x/run-001"), root), "grammar/x/run-001");
        assert_eq!(portable_path_in(Path::new("/srv/checkout/./grammar/../grammar/y"), root), "grammar/y");
        assert_eq!(portable_path_in(Path::new("/srv/checkout/data/x/run-001"), root), "run-001");
        assert_eq!(portable_path_in(Path::new("/srv/checkout"), root), ".");
        assert_eq!(portable_path_in(Path::new("/opt/elsewhere/a-recipe/fit"), root), "fit");
        assert_eq!(portable_path_in(Path::new("/srv/checkout-other/fit"), root), "fit");
        assert_eq!(portable_path_in(Path::new("D:\\work\\fit"), root), "fit");
        assert_eq!(portable_path_in(Path::new("~/a-recipe/norm"), root), "norm");
        assert_eq!(portable_path_in(Path::new("~someone/a-recipe/norm"), root), "norm");
        // `..` leaves the given form: resolved against the working directory.
        let cwd = std::env::current_dir().unwrap();
        let name = cwd.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(portable_path_in(Path::new(&format!("../{name}/z")), Path::new("/nowhere")), "z");
        assert_eq!(portable_path_in(Path::new(&format!("../{name}/z")), &cwd), "z");
        assert_eq!(portable_path_in(&cwd.join("q/r"), &cwd), "q/r");
    }

    #[test]
    fn no_user_name_or_home_directory_is_recorded() {
        let root = Path::new("/srv/checkout");
        for h in [
            "~",
            "~/",
            "~someone",
            "~someone/",
            "/home/someone",
            "/home/someone/",
            "/Users/someone",
            "/var/home/someone",
            "/root",
            "C:\\Users\\someone",
            "C:\\Users",
        ] {
            assert_eq!(portable_path_in(Path::new(h), root), OUTSIDE, "{h}");
        }
        if let Some(h) = home() {
            assert_eq!(portable_path_in(&h, root), OUTSIDE);
        }
        assert_eq!(portable_path_in(Path::new("/home/someone/fits/e1"), root), "e1");
    }

    #[test]
    fn only_known_provenance_pointers_are_rewritten() {
        let root = Path::new("/srv/checkout");
        let mut v = json!({"runs": [{"run": "/srv/checkout/n/run-000", "shard": 0},
                                    {"run": "n/run-001", "shard": 1},
                                    {"run": "data/grammar-private/exp-17/norm/run-002", "shard": 2}],
                           "a/b": {"file": "/opt/elsewhere/cal.json"}, "note": "see /x", "n": 1.5,
                           "heads_pointer": "/heads/sha256", "other": "data/grammar-private/x/y"});
        let mut changed = portable_at_in(&mut v, &["/runs/*/run", "/a~1b/file"], root);
        changed.sort();
        assert_eq!(changed, ["/a~1b/file", "/runs/0/run", "/runs/2/run"]);
        assert_eq!(v["runs"][0]["run"], "n/run-000");
        assert_eq!(v["runs"][1]["run"], "n/run-001");
        assert_eq!(v["runs"][2]["run"], "run-002");
        assert_eq!(v["other"], "data/grammar-private/x/y");
        assert_eq!(v["a/b"]["file"], "cal.json");
        assert_eq!((v["note"].as_str(), v["heads_pointer"].as_str()), (Some("see /x"), Some("/heads/sha256")));
        assert!(portable_at_in(&mut v, &["/runs/*/run", "/a~1b/file"], root).is_empty());
        assert!(repository_root().join("grammar/crates/grammar-a-baseline/src/provenance.rs").is_file());
    }
}
