//! Operator checks for the publisher Worker. It never talks to the network:
//! the operator's shell reads the live public index from the public-candidate
//! bucket and passes its file here.
//!
//! ```text
//! slop_ninja_publisher_ops record-check --wrangler WRANGLER.toml --index INDEX.json [--restart]
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
//! an earlier one's. The record vars are never passed with `--var`.
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("record-check") {
        fail("a subcommand: record-check");
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
    let checked = serde_json::from_slice::<CohortIndex>(&bytes)
        .map_err(|e| format!("not a public cohort index: {e}"))
        .and_then(|idx| record_check(&idx, &tracked, restart));
    match checked {
        Ok(line) => println!("{line}"),
        Err(why) => {
            println!("refused: {why}");
            exit(1);
        }
    }
}
