//! Local paths in an artifact's JSON files.
//!
//! An artifact's JSON role files (contract, heads, calibration, export
//! manifest) are published after its cohort closes. A provenance string that
//! reads as an absolute local path, or carries one inside it (a home
//! directory or a file URL in text), would publish its author's directory
//! layout (the operating-system user and the home directory), and a relative
//! path under an ignored private tree (`data/...`) its private directory
//! names, so `pack` and `check` warn about each one, and about object keys
//! that do. A warning never
//! refuses and never makes a check incomplete: the coordinator admits such an
//! artifact. The repository's exporters record portable paths at the
//! provenance pointers they write
//! (`grammar/crates/grammar-eval/python/provenance_paths.py`,
//! `grammar_a_baseline::provenance`), so a warning means a file was written
//! another way, by an older revision of them, or carries a path in free text.
//! The detection reads the start of each string and a few embedded forms, so
//! it can miss a path written another way: it is a check, not a guarantee.
//!
//! The scan streams each JSON file through a visitor. It keeps the current
//! JSON pointer (a few bytes per byte of the file at most), a count of hits,
//! and the distinct
//! pointers of the hits with array positions written `*` and path-like keys
//! written `<path-like key>` (so a report never prints a path), only while
//! their bytes fit the file's own length; further hits are counted, not
//! listed. A file longer than its role's runtime bound
//! ([`slop_ninja_artifact_check::spec::ROLES`] `max_bytes`) is not read: no
//! admitted artifact has one. A file that cannot be read or parsed is
//! reported as not scanned, never an error.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use slop_ninja_artifact_check::spec::{MAX_JSON_BYTES_V2, ROLES};
use slop_ninja_protocol::{FileRole, VerifiedArtifact};

/// Whether `s` reads as an absolute local path: POSIX `/x`, `~/x` or
/// `~user/x`, a Windows drive path (`C:\x`, `C:/x`) or a UNC path
/// (`\\host\share`). Text with a line break is never a path. The exporters'
/// `is_local_path` uses the same rule.
pub fn is_local_path(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 2 || s.contains(['\n', '\r']) {
        return false;
    }
    match b[0] {
        b'/' => true,
        b'\\' => b[1] == b'\\',
        b'~' => s
            .split_once('/')
            .is_some_and(|(head, _)| !head.contains(char::is_whitespace)),
        c => c.is_ascii_alphabetic() && b[1] == b':' && b.len() > 2 && matches!(b[2], b'\\' | b'/'),
    }
}

/// Substrings that put a local path inside a longer string. The exporters'
/// `embeds_local_path` uses the same list.
pub const EMBEDDED: &[&str] = &[
    "file://",
    "/home/",
    "/Users/",
    "/root/",
    "/var/home/",
    ":\\Users\\",
    ":/Users/",
];

/// Whether `s` carries a home-directory path or a file URL anywhere in it.
pub fn embeds_local_path(s: &str) -> bool {
    EMBEDDED.iter().any(|e| s.contains(e))
}

/// Directory names of the Slop Ninja repository's ignored private trees.
/// The exporters' `PRIVATE_TREES` is the same list.
pub const PRIVATE_TREES: &[&str] = &["data"];

/// Whether `s` reads as a relative path under a private tree (`data/x`): no
/// whitespace, its first `/` component one of [`PRIVATE_TREES`], and at
/// least one more component. Such a path publishes private directory names
/// as surely as an absolute one. The exporters' `names_private_tree` uses the
/// same rule.
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

fn path_like(s: &str) -> bool {
    is_local_path(s) || embeds_local_path(s) || names_private_tree(s)
}

/// The hits of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    /// Strings and keys that read as or carry a local path.
    pub count: u64,
    /// Their distinct JSON pointers (array positions `*`, path-like keys
    /// `<path-like key>`), sorted, while their bytes fit the file's length.
    pub pointers: Vec<String>,
    /// Hits whose pointer is not listed (past that bound).
    pub unlisted: u64,
}

impl Hits {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// One file's scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scan {
    /// The strings and keys that read as or carry a local path (none: empty).
    Paths(Hits),
    /// The file is not JSON; the spec check refuses it.
    NotJson(String),
    /// Longer than its role's runtime bound; not read.
    OverBudget(u64),
    /// The file could not be read; not scanned.
    Unreadable(String),
}

/// The scan of one file of an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileScan {
    pub name: String,
    pub scan: Scan,
}

/// The hits being collected, with the byte bound of the listed pointers.
struct Collect {
    count: u64,
    listed: BTreeSet<String>,
    bytes: u64,
    bound: u64,
    unlisted: u64,
}

impl Collect {
    fn hit(&mut self, pointer: &str) {
        self.count += 1;
        if self.listed.contains(pointer) {
            return;
        }
        let len = pointer.len() as u64;
        if self.bytes.saturating_add(len) <= self.bound {
            self.bytes += len;
            self.listed.insert(pointer.to_owned());
        } else {
            self.unlisted += 1;
        }
    }
}

struct Walk<'a> {
    pointer: &'a mut String,
    hits: &'a mut Collect,
}

impl Walk<'_> {
    fn child(&mut self) -> Walk<'_> {
        Walk {
            pointer: &mut *self.pointer,
            hits: &mut *self.hits,
        }
    }
}

impl<'de> DeserializeSeed<'de> for Walk<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

/// A map key, escaped into the pointer as RFC 6901 writes it (`~0`, `~1`),
/// or written `<path-like key>` (and counted as a hit) when it reads as or
/// carries a local path, so no report prints it.
struct Key<'a> {
    pointer: &'a mut String,
    hits: &'a mut Collect,
}

impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_str(self)
    }
}

impl Visitor<'_> for Key<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object key")
    }

    fn visit_str<E: serde::de::Error>(self, key: &str) -> Result<(), E> {
        self.pointer.push('/');
        if path_like(key) {
            self.pointer.push_str("<path-like key>");
            self.hits.hit(self.pointer);
            return Ok(());
        }
        for c in key.chars() {
            match c {
                '~' => self.pointer.push_str("~0"),
                '/' => self.pointer.push_str("~1"),
                c => self.pointer.push(c),
            }
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for Walk<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, s: &str) -> Result<(), E> {
        if path_like(s) {
            self.hits.hit(self.pointer);
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        let len = self.pointer.len();
        self.pointer.push_str("/*");
        while seq.next_element_seed(self.child())?.is_some() {}
        self.pointer.truncate(len);
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
        loop {
            let len = self.pointer.len();
            let key = Key {
                pointer: &mut *self.pointer,
                hits: &mut *self.hits,
            };
            if map.next_key_seed(key)?.is_none() {
                return Ok(());
            }
            map.next_value_seed(self.child())?;
            self.pointer.truncate(len);
        }
    }
}

/// The hits in the JSON document `reader` holds, listing pointers while their
/// bytes fit `bound` (the caller passes the document's length).
pub fn local_paths<R: Read>(reader: R, bound: u64) -> serde_json::Result<Hits> {
    let mut de = serde_json::Deserializer::from_reader(reader);
    let mut pointer = String::new();
    let mut hits = Collect {
        count: 0,
        listed: BTreeSet::new(),
        bytes: 0,
        bound,
        unlisted: 0,
    };
    Walk {
        pointer: &mut pointer,
        hits: &mut hits,
    }
    .deserialize(&mut de)?;
    de.end()?;
    Ok(Hits {
        count: hits.count,
        pointers: hits.listed.into_iter().collect(),
        unlisted: hits.unlisted,
    })
}

/// The runtime bound of a file of role `role` (the spec's role table; the
/// JSON budget for a role it does not list).
fn role_bound(role: FileRole) -> u64 {
    ROLES
        .iter()
        .find(|r| r.file_role == role)
        .map_or(MAX_JSON_BYTES_V2, |r| r.max_bytes)
}

/// Scans every JSON file of the object at `path` (`verified` is its header)
/// except the tokenizer, which the spec pins byte for byte. An object that
/// changed since it was verified is an error; a file that cannot be read is
/// [`Scan::Unreadable`].
pub fn scan_object(path: &Path, verified: &VerifiedArtifact) -> Result<Vec<FileScan>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    ensure!(
        file.metadata()?.len() == verified.len,
        "{} changed since it was verified",
        path.display()
    );
    let mut prefix = [0u8; 4];
    file.read_exact(&mut prefix)
        .with_context(|| format!("read {}", path.display()))?;
    let mut offset = 4 + u64::from(u32::from_le_bytes(prefix));
    let total = verified
        .header
        .files
        .iter()
        .try_fold(offset, |sum, f| sum.checked_add(f.len))
        .context("file lengths overflow")?;
    ensure!(
        total == verified.len,
        "{} changed since it was verified",
        path.display()
    );
    let mut scans = Vec::new();
    for f in &verified.header.files {
        let start = offset;
        offset += f.len;
        if f.role == FileRole::Tokenizer || !f.name.ends_with(".json") {
            continue;
        }
        let scan = if f.len > role_bound(f.role) {
            Scan::OverBudget(f.len)
        } else if let Err(error) = file.seek(SeekFrom::Start(start)) {
            Scan::Unreadable(error.to_string())
        } else {
            let reader = BufReader::new(Read::by_ref(&mut file).take(f.len));
            match local_paths(reader, f.len) {
                Ok(hits) => Scan::Paths(hits),
                Err(error) if error.is_io() => Scan::Unreadable(error.to_string()),
                Err(error) => Scan::NotJson(error.to_string()),
            }
        };
        scans.push(FileScan {
            name: f.name.clone(),
            scan,
        });
    }
    Ok(scans)
}

/// What to do about a file's local paths, for reports. Names pointers only.
pub fn advice(name: &str, hits: &Hits) -> String {
    let more = if hits.unlisted > 0 {
        format!(", and {} more not listed", hits.unlisted)
    } else {
        String::new()
    };
    format!(
        "{name}: {} string{} or key{} read as or carry a local path ({}{more}). The artifact's JSON \
         files are published after its cohort closes, so this would publish your directory layout. \
         Export with the repository's tools, giving relative paths, and pack again; a warning never \
         refuses the artifact",
        hits.count,
        if hits.count == 1 { "" } else { "s" },
        if hits.count == 1 { "" } else { "s" },
        hits.pointers.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::artifact_object;

    #[test]
    fn absolute_local_paths_are_recognized() {
        for yes in [
            "/opt/u/x",
            "/x",
            "~/x",
            "~user/x",
            "C:\\x",
            "c:/x",
            "\\\\host\\share",
        ] {
            assert!(is_local_path(yes), "{yes}");
        }
        for no in [
            "",
            "/",
            "x/y",
            "norm/run-000",
            "~",
            "~ u/x",
            "a:b",
            "C:",
            "line\n/x",
            "/x\r",
            "1/2",
        ] {
            assert!(!is_local_path(no), "{no:?}");
        }
    }

    #[test]
    fn embedded_paths_and_path_like_keys_are_found_and_never_printed() {
        for yes in [
            "file:///opt/x",
            "run in /home/someone/x",
            "see /Users/someone/fit",
            "D:\\Users\\someone\\x",
        ] {
            assert!(embeds_local_path(yes), "{yes}");
        }
        assert!(!embeds_local_path("/v1/load") && !embeds_local_path("norm/run-000"));
        for yes in ["data/x", "./data/x/y", "data\\x"] {
            assert!(names_private_tree(yes), "{yes}");
        }
        for no in ["data", "data/", "metadata/x", "data x/y", "n/data/x"] {
            assert!(!names_private_tree(no), "{no}");
        }
        let json = br#"{"note":"run in /home/someone/x","/home/someone/k":{"v":"/opt/y"},"ok":"n/r","fit":"data/exp-17/fit"}"#;
        let hits = local_paths(&json[..], json.len() as u64).unwrap();
        assert_eq!(hits.count, 4);
        assert_eq!(
            hits.pointers,
            ["/<path-like key>", "/<path-like key>/v", "/fit", "/note"]
        );
        assert!(!hits.pointers.iter().any(|p| p.contains("someone")));
        assert_eq!(advice("x.json", &hits).matches("someone").count(), 0);
    }

    #[test]
    fn pointers_follow_rfc_6901_with_array_positions_collapsed() {
        let json = br#"{"runs":[{"run":"/opt/u/n/run-000","shard":0},{"run":"/opt/u/n/run-001"}],
            "a/b~c":{"file":"~/cal.json","n":[1.5,null,true,-3]},"note":"see /x","w":"C:\\fit"}"#;
        let hits = local_paths(&json[..], json.len() as u64).unwrap();
        assert_eq!(hits.count, 4);
        assert_eq!(hits.pointers, ["/a~1b~0c/file", "/runs/*/run", "/w"]);
        assert_eq!(hits.unlisted, 0);
        assert!(
            local_paths(&br#"{"file":"contract.json"}"#[..], 64)
                .unwrap()
                .is_empty()
        );
        assert!(local_paths(&b"{\"a\":"[..], 64).is_err());
        assert!(local_paths(&b"{} {}"[..], 64).is_err());
    }

    /// Hits are counted, and their distinct pointers listed only while their
    /// bytes fit the bound (the file's length): a long key over many hits, or
    /// many hits in an array, never grows the scan past the file.
    #[test]
    fn listed_pointers_are_bounded_by_the_files_length() {
        let mut doc = String::from("{\"");
        doc.push_str(&"k".repeat(1000));
        doc.push_str("\":{");
        for i in 0..200 {
            if i > 0 {
                doc.push(',');
            }
            doc.push_str(&format!("\"{i}\":\"/x\""));
        }
        doc.push_str("},\"arr\":[");
        doc.push_str(&vec!["\"/x\""; 5000].join(","));
        doc.push_str("]}");
        let hits = local_paths(doc.as_bytes(), doc.len() as u64).unwrap();
        assert_eq!(hits.count, 5200);
        let listed: u64 = hits.pointers.iter().map(|p| p.len() as u64).sum();
        assert!(listed <= doc.len() as u64, "{listed} > {}", doc.len());
        assert!(hits.pointers.len() < 200 && hits.unlisted > 0);
        assert!(advice("x.json", &hits).contains("more not listed"));
    }

    #[test]
    fn objects_are_scanned_file_by_file_without_the_tokenizer() {
        let contract = br#"{"embedding_normalization":{"runs":[{"run":"/opt/u/run-000"}]}}"#;
        let object = artifact_object(
            "c-1",
            &[
                ("a-fp32band.onnx", FileRole::Graph, b"/opt/u/not-json"),
                ("calibration.json", FileRole::Calibration, b"not json"),
                ("contract.json", FileRole::OutputSchema, contract),
                (
                    "manifest.json",
                    FileRole::OwnerMetadata,
                    br#"{"x":"rel/ok"}"#,
                ),
                (
                    "tokenizer.json",
                    FileRole::Tokenizer,
                    br#"{"p":"/opt/u/t"}"#,
                ),
            ],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o");
        std::fs::write(&path, &object).unwrap();
        let verified = VerifiedArtifact::from_bytes(&object).unwrap();
        let scans = scan_object(&path, &verified).unwrap();
        let by_name = |n: &str| scans.iter().find(|s| s.name == n).map(|s| s.scan.clone());
        assert_eq!(scans.len(), 3);
        assert!(matches!(
            by_name("calibration.json"),
            Some(Scan::NotJson(_))
        ));
        assert_eq!(
            by_name("contract.json"),
            Some(Scan::Paths(Hits {
                count: 1,
                pointers: vec!["/embedding_normalization/runs/*/run".into()],
                unlisted: 0
            }))
        );
        assert_eq!(by_name("manifest.json"), Some(Scan::Paths(Hits::default())));
        assert_eq!(by_name("tokenizer.json"), None);
        // A changed object is an error, not a scan.
        std::fs::write(&path, &object[..object.len() - 1]).unwrap();
        assert!(scan_object(&path, &verified).is_err());
    }
}
