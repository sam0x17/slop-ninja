//! Operator checks for the publisher Worker. It never talks to the network:
//! the operator's shell reads the live public index from the public-candidate
//! bucket and passes its file here.
//!
//! ```text
//! slop_ninja_publisher_ops record-check --wrangler WRANGLER.toml --index INDEX.json [--restart]
//! slop_ninja_publisher_ops audit-keys --listing LISTING.jsonl [--objects DIR] [--for-delete]
//! ```
//!
//! `record-check` is the publisher's pre-deploy refusal (`index::record_check`),
//! beside the eval Worker's `authority-check`: the tree's tracked
//! `RECORD_LABEL_PREFIX` (`[env.testnet.vars]` of WRANGLER.toml) must be the
//! live record's (the index's `record_prefix`, or for an index written before
//! prefixes were recorded, a prefix every listed label has), or exit 1, so a
//! deploy from a tree that still tracks an earlier record never starts it
//! again. With `--restart` (the publisher commit of a chain-record restart
//! only) the tracked prefix must instead be new: neither the live record's nor
//! an earlier one's. The record vars are never passed with `--var`. A live
//! index of layout v1 (which a v2 publisher replaces at its first run) is
//! read for its record prefix and labels only.
//!
//! `audit-keys` is the public bucket's key audit against the allow-list
//! (`allowlist`, `slop_ninja.public.allowlist/v2`): LISTING.jsonl holds one
//! `{"key": ..., "size": ...}` object per stored key (the operator's private
//! listing of the public-candidate bucket), and DIR, when given, local copies
//! of objects under their keys. Every key outside the allow-list, and every
//! allowed object whose copy fails its content check (a field outside its
//! schema's list, another schema, a binary object of another kind, a page
//! that is not this build's template or carries a layout-v1 score column),
//! is planned for deletion; an allowed object of a content-checked kind (the
//! page included) without a local copy is listed as unchecked, and artifacts
//! and staged reference packages as kept by key. It prints the plan as JSON
//! and deletes nothing: the operator's private wrapper lists the bucket,
//! fetches every content-checked object and deletes the planned keys. With
//! `--for-delete` (the wrapper's deletion pass) it exits 3 after printing the
//! plan when any key is unchecked, so a deletion never runs on a partial
//! check. Content-free: keys and reason codes only.
use slop_ninja_publisher::index::{CohortIndex, record_check};
use std::collections::BTreeMap;
use std::process::exit;

fn fail(msg: &str) -> ! {
    eprintln!("slop_ninja_publisher_ops: {msg}");
    exit(2)
}

/// The `[env.testnet.vars]` table of the tracked `wrangler.toml`: each
/// `KEY = "VALUE"` line of that section (the form the file uses).
fn testnet_vars(path: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("read {path}: {e}")));
    let mut vars = BTreeMap::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == "[env.testnet.vars]";
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            vars.insert(k.trim().to_owned(), v.to_owned());
        }
    }
    vars
}

/// `audit-keys`: the deletion plan of a listing against the allow-list.
fn audit_keys(args: &[String]) {
    let mut listing = None;
    let mut objects = None;
    let mut for_delete = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--for-delete" {
            for_delete = true;
            i += 1;
            continue;
        }
        let v = args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| fail(&format!("{a} needs a value")));
        match a {
            "--listing" => listing = Some(v),
            "--objects" => objects = Some(v),
            other => fail(&format!("unknown argument {other}")),
        }
        i += 2;
    }
    let listing = listing.unwrap_or_else(|| fail("--listing is required"));
    let text = std::fs::read_to_string(&listing)
        .unwrap_or_else(|e| fail(&format!("read the listing: {e}")));
    let mut keys = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| fail(&format!("listing line: {e}")));
        let key = v
            .get("key")
            .and_then(|k| k.as_str())
            .unwrap_or_else(|| fail("a listing line without a key"));
        keys.push(key.to_owned());
    }
    let plan = slop_ninja_publisher::allowlist::audit(&keys, |key| {
        let dir = objects.as_ref()?;
        // Keys of the allow-list are safe relative paths; others are never read.
        std::fs::read(std::path::Path::new(dir).join(key)).ok()
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&plan).unwrap_or_default()
    );
    if for_delete && !plan.unchecked.is_empty() {
        eprintln!(
            "slop_ninja_publisher_ops: {} allowed keys were not checked; refusing a deletion pass",
            plan.unchecked.len()
        );
        exit(3);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("record-check") => {}
        Some("audit-keys") => {
            audit_keys(&args[1..]);
            return;
        }
        _ => fail("a subcommand: record-check or audit-keys"),
    }
    let mut flags = BTreeMap::new();
    let mut restart = false;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--restart" => restart = true,
            "--wrangler" | "--index" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{a} needs a value")));
                flags.insert(a.clone(), v.clone());
            }
            other => fail(&format!("unknown argument {other}")),
        }
    }
    let get = |k: &str| {
        flags
            .get(k)
            .unwrap_or_else(|| fail(&format!("{k} is required")))
    };
    let tracked = testnet_vars(get("--wrangler"))
        .get("RECORD_LABEL_PREFIX")
        .cloned()
        .unwrap_or_default();
    let bytes =
        std::fs::read(get("--index")).unwrap_or_else(|e| fail(&format!("read the index: {e}")));
    // An unreadable index (an error reply, another object) refuses: exit 1.
    let checked = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|e| format!("not a public cohort index: {e}"))
        .and_then(|v| slop_ninja_publisher::index::for_record_check(&v))
        .and_then(|idx: CohortIndex| record_check(&idx, &tracked, restart));
    match checked {
        Ok(line) => println!("{line}"),
        Err(why) => {
            println!("refused: {why}");
            exit(1);
        }
    }
}
