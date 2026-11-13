//! Compiles the Public Suffix List rules (`psl/public_suffix_list.dat`,
//! MPL-2.0; the list with its comments removed and its rules unchanged)
//! into a sorted rule table, so a lookup is a binary search over static data
//! and allocates nothing at run time (the Workers' bounded-arena rule).
//!
//! Each rule is converted to its A-label form (IDNA ToASCII), the form every
//! host has after the WHATWG URL parser, and keyed by its suffix without the
//! `*.` or `!` prefix. One key can carry several kinds (a normal rule, a
//! wildcard below it, an exception), so the flags are merged. The list's own
//! `VERSION` line and the file's SHA-256 name the table version.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;

const NORMAL: u8 = 1;
const WILDCARD: u8 = 2;
const EXCEPTION: u8 = 4;
const PRIVATE: u8 = 8;

fn main() {
    let path = "psl/public_suffix_list.dat";
    println!("cargo:rerun-if-changed={path}");
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let text = std::str::from_utf8(&bytes).unwrap_or_else(|e| panic!("{path} is not UTF-8: {e}"));
    let digest = Sha256::digest(&bytes);
    let mut sha = String::new();
    for b in digest {
        let _ = write!(sha, "{b:02x}");
    }
    let mut version = String::new();
    let mut private = false;
    let mut rules: BTreeMap<String, u8> = BTreeMap::new();
    let mut tlds: BTreeMap<String, ()> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("// VERSION:") {
            version = v.trim().to_owned();
            continue;
        }
        if line.contains("===BEGIN PRIVATE DOMAINS===") {
            private = true;
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let rule = line.split_whitespace().next().unwrap_or("");
        let (kind, name) = if let Some(r) = rule.strip_prefix('!') {
            (EXCEPTION, r)
        } else if let Some(r) = rule.strip_prefix("*.") {
            (WILDCARD, r)
        } else {
            (NORMAL, rule)
        };
        let ascii = idna::domain_to_ascii(name).unwrap_or_else(|e| panic!("{path}: rule {rule}: {e:?}"));
        assert!(!ascii.is_empty() && !ascii.contains('*'), "{path}: unsupported rule {rule}");
        let flags = rules.entry(ascii.clone()).or_insert(0);
        *flags |= kind | if private { PRIVATE } else { 0 };
        let tld = ascii.rsplit('.').next().unwrap_or("").to_owned();
        tlds.insert(tld, ());
    }
    assert!(rules.len() > 5_000, "{path}: only {} rules", rules.len());
    assert!(!version.is_empty(), "{path}: no VERSION line");
    let mut out = String::new();
    let _ = writeln!(out, "/// The list's own `VERSION` line.");
    let _ = writeln!(out, "pub const PSL_LIST_VERSION: &str = {version:?};");
    let _ = writeln!(out, "/// SHA-256 of `psl/public_suffix_list.dat`.");
    let _ = writeln!(out, "pub const PSL_SHA256: &str = {sha:?};");
    let _ = writeln!(out, "/// Rules by A-label suffix, sorted bytewise, with merged kind flags.");
    let _ = writeln!(out, "static RULES: [(&str, u8); {}] = [", rules.len());
    for (k, v) in &rules {
        let _ = writeln!(out, "    ({k:?}, {v}),");
    }
    let _ = writeln!(out, "];");
    let _ = writeln!(out, "/// Every top-level label that some rule names, sorted bytewise.");
    let _ = writeln!(out, "static TLDS: [&str; {}] = [", tlds.len());
    for k in tlds.keys() {
        let _ = writeln!(out, "    {k:?},");
    }
    let _ = writeln!(out, "];");
    let dir = std::env::var("OUT_DIR").unwrap_or_else(|e| panic!("OUT_DIR: {e}"));
    std::fs::write(format!("{dir}/psl_rules.rs"), out).unwrap_or_else(|e| panic!("write psl_rules.rs: {e}"));
}
