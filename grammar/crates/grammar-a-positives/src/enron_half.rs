//! The Enron halves (protocol A2 V23, S0-9; B5b): the 99 admitted writer
//! keys ordered by V21's plan key with x = `enron_half` (unkeyed); the first
//! 50 form the Train half, the other 49 the diagnostic half.
//!
//! `run` (`slop_ninja_a_positives enron-half`) writes the Train half's
//! documents, with `frozen_split` `train`, as a new Train input file. Every
//! input is bound before a document is parsed (B5b review finding 8; the
//! rule amendment 23 item 23.1 set for B22: Enron inputs come from the bound
//! pool, never from files the operator picks):
//! * **S0-9** is read from `VALUES-R2.json` and verified as B22 verifies it
//!   (`grammar_a_pools::pile::EnronHalves`: list digests and counts, V23's
//!   plan-key order, the 50-key Train half, buckets, disjoint lists).
//! * **The pool** is the one S0-9's `SHA256SUMS` input names under `--root`
//!   (canonical paths, the restricted-name rule on both), its `SHA256SUMS`
//!   has S0-9's digest, and the files S0-9 also binds (`ADMISSION.json`,
//!   `metadata.jsonl`, `reserved-metadata.jsonl`) have the digests that
//!   `SHA256SUMS` lists. `documents.jsonl` is read from that pool and must
//!   have its `SHA256SUMS` digest.
//! * Each line's `writer_key` and `collection` are read from its bytes
//!   (amendment 15 item 15.4's reading, as for split keys). The documents'
//!   writer keys must be S0-9's admitted keys exactly, and the halves formed
//!   from them must be S0-9's lists, in order. Diagnostic-half lines are
//!   skipped unparsed.
//! * The Train half's document count and words must equal S0-9's
//!   (`train_half.documents`, `train_half.words`: A9 summed the admission's
//!   `word_count`), each document's recorded `word_count` must be its
//!   whitespace words, and the diagnostic half's document count must equal
//!   S0-9's.
//!
//! Writer keys are opaque owner keys: they are never printed, and the record
//! holds only counts and digests.
use crate::renorm::key_values;
use crate::span_ops::r2_key;
use crate::split_copy::write_new;
use crate::text;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::pile::{BoundPool, EnronHalves, unrestricted_canonical};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// V21's x of the Enron halves.
pub const ENRON_HALF_KEY: &str = "enron_half";
pub use grammar_a_pools::pile::ENRON_COLLECTION;
/// The pool file the Train half's documents are read from.
pub const DOCUMENTS: &str = "documents.jsonl";
/// The rule.
pub const ENRON_HALF_RULE: &str = "V23: the admitted writer keys ordered by SHA-256('slop_ninja_a_comp_r2_plan_v1:enron_half:' + key) ascending (V21, unkeyed), ties by key; the first ceil(n/2) (50 of 99) form the Train half, the rest (49) the diagnostic half; the documents are the bound enron-v1 pool's documents.jsonl (S0-9's SHA256SUMS under the repository root, B5b review finding 8); each line's writer_key and collection are read from its bytes, and only Train-half lines are parsed; the halves, document counts and Train-half words equal S0-9's";
/// The output record.
pub const ENRON_HALF_SCHEMA: &str = "slop_ninja_a_positives_enron_train_half_v1";

/// The halves of `keys` (V23): (Train half, diagnostic half), each in plan
/// key order.
pub fn halves(keys: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let mut v: Vec<(String, &String)> = keys.iter().map(|k| (r2_key(ENRON_HALF_KEY, k), k)).collect();
    v.sort();
    let n = v.len().div_ceil(2);
    let all: Vec<String> = v.into_iter().map(|(_, k)| k.clone()).collect();
    (all[..n].to_vec(), all[n..].to_vec())
}

/// S0-9's list digest: SHA-256 of the keys joined by newline with a
/// trailing newline, in the order given.
pub fn list_sha256(keys: &[String]) -> String {
    let mut b = String::new();
    for k in keys {
        b.push_str(k);
        b.push('\n');
    }
    text::sha_hex(b.as_bytes())
}

/// The one string value of `key` a line names, read from its bytes.
fn one_value<'a>(l: &'a [u8], key: &str, n: usize) -> Result<&'a str> {
    let vs = key_values(l, key);
    match vs.as_slice() {
        [Some(v)] => Ok(std::str::from_utf8(v).with_context(|| format!("line {}: {key}", n + 1))?),
        _ => bail!("documents line {}: not one {key} that can be read from the line's bytes; refused before any parse", n + 1),
    }
}

/// The enron-v1 pool S0-9 binds: `pool` must be the directory whose
/// `SHA256SUMS` S0-9 names under `root`, with S0-9's digest, and S0-9's other
/// bound files must have the digests it lists. Returns the bound pool and the
/// input records.
fn bound_pool(h: &EnronHalves, pool: &Path, root: &Path) -> Result<(BoundPool, Vec<Value>)> {
    let pool_dir = unrestricted_canonical(pool, "enron-v1 pool")?;
    let sums_rel = h.inputs.get("SHA256SUMS").context("S0-9 binds no input named SHA256SUMS")?.0.clone();
    let named = fs::canonicalize(root.join(&sums_rel)).with_context(|| format!("S0-9's input {sums_rel} under the root {}", root.display()))?;
    ensure!(named == pool_dir.join("SHA256SUMS"), "the pool {} is not the one S0-9 names under the root ({sums_rel})", pool.display());
    let bp = BoundPool::open(&pool_dir, Some(h.input_sha("SHA256SUMS")?))?;
    let mut inputs = vec![json!({"role": "enron_pool_sha256sums", "path": pool_dir.join("SHA256SUMS"), "sha256": bp.sums_sha256})];
    for (name, (_, sha, _)) in &h.inputs {
        if name != "SHA256SUMS" {
            ensure!(bp.sha(name)? == sha.as_str(), "{name}: the pool's SHA256SUMS and S0-9 disagree");
            inputs.push(json!({"role": "s0_9_input_listed", "name": name, "sha256": sha}));
        }
    }
    Ok((bp, inputs))
}

/// Writes the Train half's documents to `out` (see the module notes).
pub fn run(pool: &Path, root: &Path, values: &Path, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists: the Train half is written once", out.display());
    let (h, values_sha) = EnronHalves::from_values_file(values)?;
    let vals: Value = serde_json::from_slice(&fs::read(values)?)?;
    let s9 = &vals["values"]["S0-9"];
    let (bp, mut inputs) = bound_pool(&h, pool, root)?;
    // documents.jsonl, read once and checked against the pool's SHA256SUMS
    let docs_path = bp.dir.join(DOCUMENTS);
    let b = fs::read(&docs_path).with_context(|| docs_path.display().to_string())?;
    let docs_sha = text::sha_hex(&b);
    ensure!(docs_sha == bp.sha(DOCUMENTS)?, "{} has sha256 {docs_sha}, its pool's SHA256SUMS lists another", docs_path.display());
    inputs.push(json!({"role": DOCUMENTS, "path": docs_path, "sha256": docs_sha, "bytes": b.len()}));
    let mut keys: BTreeSet<String> = BTreeSet::new();
    let mut line_keys: Vec<Option<String>> = Vec::new();
    for (n, l) in b.split(|&x| x == b'\n').enumerate() {
        if l.iter().all(u8::is_ascii_whitespace) {
            line_keys.push(None);
            continue;
        }
        ensure!(one_value(l, "collection", n)? == ENRON_COLLECTION, "documents line {}: not an {ENRON_COLLECTION} document", n + 1);
        let k = one_value(l, "writer_key", n)?.to_string();
        keys.insert(k.clone());
        line_keys.push(Some(k));
    }
    ensure!(keys.iter().map(String::as_str).collect::<BTreeSet<_>>() == h.admitted(), "the documents' writer keys are not S0-9's admitted keys");
    let (train, diag) = halves(&keys);
    ensure!(train == h.train, "the Train half's writer keys differ from S0-9's");
    ensure!(diag == h.diagnostic, "the diagnostic half's writer keys differ from S0-9's");
    let (ts, ds) = (list_sha256(&train), list_sha256(&diag));
    let tset: BTreeSet<&String> = train.iter().collect();
    let mut o = Vec::new();
    let mut docs: BTreeMap<&str, usize> = BTreeMap::new();
    let mut words = 0usize;
    for (n, (l, k)) in b.split(|&x| x == b'\n').zip(&line_keys).enumerate() {
        let Some(k) = k else { continue };
        if !tset.contains(k) {
            *docs.entry("diagnostic").or_default() += 1;
            continue;
        }
        let mut v: Value = serde_json::from_slice(l).with_context(|| format!("documents line {}", n + 1))?;
        ensure!(v["origin"] == "human_only", "documents line {}: not a human document", n + 1);
        let t = v["text"].as_str().context("text")?;
        ensure!(v["text_sha256"] == text::sha_hex(t.as_bytes()).as_str(), "documents line {}: text digest", n + 1);
        ensure!(v["frozen_split"].is_null() || v["frozen_split"] == "train", "documents line {}: assigned to another split", n + 1);
        let w = text::words(t);
        ensure!(v["word_count"].as_u64() == Some(w as u64), "documents line {}: its word_count is not its whitespace words", n + 1);
        words += w;
        v["frozen_split"] = json!("train");
        v["split_assigned"] = json!(true);
        v["split_rule"] = json!(ENRON_HALF_RULE);
        serde_json::to_writer(&mut o, &v)?;
        o.push(b'\n');
        *docs.entry("train").or_default() += 1;
    }
    let n_train = docs.get("train").copied().unwrap_or(0);
    let n_diag = docs.get("diagnostic").copied().unwrap_or(0);
    ensure!(s9["train_half"]["documents"].as_u64() == Some(n_train as u64), "the Train half holds {n_train} documents, S0-9 records another count");
    ensure!(s9["train_half"]["words"].as_u64() == Some(words as u64), "the Train half holds {words} words, S0-9 records another count");
    ensure!(s9["diagnostic_half"]["documents"].as_u64() == Some(n_diag as u64), "the diagnostic half's document count differs from S0-9's");
    let hh = write_new(out, &o)?;
    Ok(json!({"schema": ENRON_HALF_SCHEMA, "code": crate::CODE_ID, "rule": ENRON_HALF_RULE,
        "values": {"path": values, "sha256": values_sha}, "inputs": inputs,
        "documents": {"path": docs_path, "sha256": docs_sha, "lines": line_keys.iter().filter(|k| k.is_some()).count()},
        "train_half": {"writer_keys": train.len(), "documents": n_train, "words": words, "list_sha256": ts},
        "diagnostic_half": {"writer_keys": diag.len(), "documents_skipped_unparsed": n_diag, "list_sha256": ds},
        "checks": {"halves_equal_s0_9": true, "train_documents_and_words_equal_s0_9": true, "diagnostic_documents_equal_s0_9": true,
            "documents_digest_equals_pool_sha256sums": true, "pool_is_s0_9s_under_root": true},
        "out": out, "out_sha256": hh}))
}

/// D6's plan rule (B5b part 2 item 3).
pub const D6_PLAN_RULE: &str = "D6 (protocol A2 sections 4.4 and 6.1, V23, S0-9; B5b part 2 item 3; amendment 27): the plan reads only enron-half's Train file; each line's collection, writer_key and frozen_split are read from its bytes before any parse, and a line of another collection or split, a writer key outside S0-9's Train half (a diagnostic-half key among them) or a line whose keys cannot be read refuses the plan by line number (keys never printed); every parsed line carries enron-half's split_rule; the documents and words equal S0-9's train_half; then one mirror per document (paired local positives, so the domain does not mean human) from the Train-only mirror planner (train_mirror_plan_v1) at mirror_target_words_v2, the seven Train instruct families only (local, never hosted, section 2.7), with R0's CAL overlap extra (--cal-from, required); one batch on its own runs directory, population train2; profile v2 copies the parent's hard wraps at generation (section 6.2, B2)";

/// D6's plan over the Enron Train half `train_half` (enron-half's output):
/// every binding above ([`D6_PLAN_RULE`]) is checked before the plan is
/// written to the new directory `out` by `train_mirror::run`.
pub fn plan_d6(train_half: &Path, values: &crate::values::Values, cal_src: &Path, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists: the plan is a new directory", out.display());
    let (h, values_sha) = EnronHalves::from_values_file(&values.path)?;
    ensure!(values_sha == values.sha256, "VALUES-R2.json changed while it was read");
    let vals: Value = serde_json::from_slice(&fs::read(&values.path)?)?;
    let s9 = &vals["values"]["S0-9"];
    let train: BTreeSet<&str> = h.train.iter().map(String::as_str).collect();
    let diag: BTreeSet<&str> = h.diagnostic.iter().map(String::as_str).collect();
    let b = fs::read(train_half).with_context(|| train_half.display().to_string())?;
    let (mut docs, mut words) = (0usize, 0usize);
    for (n, l) in b.split(|&x| x == b'\n').enumerate() {
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        ensure!(one_value(l, "collection", n)? == ENRON_COLLECTION, "Train-half line {}: not an {ENRON_COLLECTION} document", n + 1);
        let k = one_value(l, "writer_key", n)?;
        ensure!(!diag.contains(k), "Train-half line {}: a diagnostic-half writer key (V23): D6 reads the Train half only; refused before any parse", n + 1);
        ensure!(train.contains(k), "Train-half line {}: a writer key outside S0-9's Train half; refused before any parse", n + 1);
        ensure!(one_value(l, "frozen_split", n)? == "train", "Train-half line {}: not a Train document", n + 1);
        let v: Value = serde_json::from_slice(l).with_context(|| format!("Train-half line {}", n + 1))?;
        ensure!(v["split_rule"] == ENRON_HALF_RULE, "Train-half line {}: not written by enron-half (its split_rule differs)", n + 1);
        let t = v["text"].as_str().context("text")?;
        ensure!(v["word_count"].as_u64() == Some(text::words(t) as u64), "Train-half line {}: its word_count is not its whitespace words", n + 1);
        docs += 1;
        words += text::words(t);
    }
    ensure!(s9["train_half"]["documents"].as_u64() == Some(docs as u64), "the Train-half file holds {docs} documents, S0-9 records another count");
    ensure!(s9["train_half"]["words"].as_u64() == Some(words as u64), "the Train-half file holds {words} words, S0-9 records another count");
    let source = json!({"step": "D6", "rule": D6_PLAN_RULE, "train_half": {"path": train_half, "sha256": text::sha_hex(&b), "documents": docs, "words": words,
        "writer_keys_in_s0_9_train_half": train.len(), "list_sha256": list_sha256(&h.train)}, "values_sha256": values_sha,
        "checks": {"collection_and_split_from_bytes": true, "writer_keys_in_train_half": true, "split_rule_is_enron_halfs": true, "documents_and_words_equal_s0_9": true}});
    let spec = crate::train_mirror::Spec { cohort: crate::train_mirror::COHORT_ENRON_MIRROR, groups: None, source, train_sha256: Some(text::sha_hex(&b)) };
    crate::train_mirror::run(train_half, cal_src, values, &spec, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ninety_nine_synthetic_keys_split_fifty_forty_nine_by_plan_key() {
        let keys: BTreeSet<String> = (0..99).map(|i| format!("synthetic-owner-{i:03}")).collect();
        let (t, d) = halves(&keys);
        assert_eq!((t.len(), d.len()), (50, 49));
        // independent of `halves`: sort by the V21 key spelled out
        let mut by: Vec<(String, String)> = keys.iter().map(|k| (text::sha_hex(format!("slop_ninja_a_comp_r2_plan_v1:enron_half:{k}").as_bytes()), k.clone())).collect();
        by.sort();
        let want: Vec<String> = by.into_iter().map(|(_, k)| k).collect();
        assert_eq!(t, want[..50].to_vec());
        assert_eq!(d, want[50..].to_vec());
        // the halves partition the keys, and the order is not the key order
        let all: BTreeSet<String> = t.iter().chain(&d).cloned().collect();
        assert_eq!(all, keys);
        assert_ne!(t, keys.iter().take(50).cloned().collect::<Vec<_>>());
        assert_eq!(list_sha256(&["a".into(), "b".into()]), text::sha_hex(b"a\nb\n"));
    }
}
