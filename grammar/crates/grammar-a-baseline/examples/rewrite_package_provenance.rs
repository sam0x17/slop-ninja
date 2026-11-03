//! A path-stripped copy of an A model directory: only provenance strings,
//! and the digests that bind the rewritten files, change.
//!
//! ```text
//! rewrite_package_provenance --root CHECKOUT --record RECORD.json SRC_DIR OUT_DIR
//! ```
//!
//! For a model exported before the tools recorded portable locations. Every
//! string at the provenance pointers the tools write locations at
//! ([`POINTERS`]) is recorded as the tools now record it
//! ([`grammar_a_baseline::provenance::portable_at_in`] against CHECKOUT, the
//! checkout that wrote them): repository-relative, or only its final
//! component under an ignored private tree or outside the repository. Then
//! the digests that bind the rewritten files are rebound at exactly the
//! pointers that held the old digest: the contract's in `heads.json`,
//! `calibration.json` and `manifest.json` (`contract.sha256` and
//! `fit_binding.contract_sha256`, which the artifact spec requires to equal
//! the contract file's digest), and the manifest's `heads.sha256` and
//! `calibration.sha256`. Each edit replaces whole JSON string tokens in the
//! file's bytes, so every other byte (numbers, knots, layout) is unchanged;
//! the result is checked structurally (exactly the planned pointers differ,
//! every other leaf is equal) and no string left in the four files reads as
//! an absolute local path. Every other file (the graph, the tokenizer) is
//! copied byte for byte. OUT_DIR is new.
//!
//! RECORD.json (published beside the package, never one of its files) states
//! what changed: each rewritten file's digests before and after, its
//! rewritten and rebound pointers, the contract digest the fit was bound to,
//! and, where they apply, that the published `fit_binding` is not the bytes
//! `fit_hashes.binding_json_sha256` names and which calibration-source digest
//! names the source as it was. It names this tool and the rule module by
//! their source digests, and holds pointers and digests only: no string the
//! files held before the rewrite.
use anyhow::{Context, Result, bail, ensure};
use grammar_a_baseline::provenance::{is_local_path, names_private_tree, portable_at_in};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The record's schema.
const RECORD_SCHEMA: &str = "slop_ninja_package_provenance_rewrite_v1";
/// This tool and the rule module, by repository path and source.
const TOOL: &str = "grammar/crates/grammar-a-baseline/examples/rewrite_package_provenance.rs";
const TOOL_SOURCE: &[u8] = include_bytes!("rewrite_package_provenance.rs");
const RULE: &str = "grammar/crates/grammar-a-baseline/src/provenance.rs";
const RULE_SOURCE: &[u8] = include_bytes!("../src/provenance.rs");

/// The model's JSON role files, in the order they are rewritten and recorded.
const JSON_FILES: [&str; 4] = ["contract.json", "heads.json", "calibration.json", "manifest.json"];

/// The provenance pointers the tools write locations at, per file: the
/// normalization's run directories in the contract, the calibration's source
/// and span-fit locations, and the export manifest's fit binding and fit
/// hashes (`export_full_a.py`, `fit_full_a_normalization`).
const POINTERS: [(&str, &[&str]); 3] = [
    ("contract.json", &["/embedding_normalization/runs/*/run"]),
    (
        "calibration.json",
        &["/document_calibration_source/file", "/spans_provenance/cal_fit", "/spans_provenance/decoder_grid"],
    ),
    (
        "manifest.json",
        &["/fit_binding/init_fit/fit_dir", "/fit_binding/train_allowlist/file", "/fit_hashes/init_fit/fit_dir"],
    ),
];

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Every scalar leaf of `v` by JSON pointer.
fn leaves(v: &Value) -> BTreeMap<String, Value> {
    fn walk(v: &Value, p: &str, out: &mut BTreeMap<String, Value>) {
        match v {
            Value::Object(map) => map.iter().for_each(|(k, x)| walk(x, &format!("{p}/{}", pointer_token(k)), out)),
            Value::Array(items) => items.iter().enumerate().for_each(|(i, x)| walk(x, &format!("{p}/{i}"), out)),
            other => {
                out.insert(p.to_owned(), other.clone());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(v, "", &mut out);
    out
}

fn parse(b: &[u8], name: &str) -> Result<Value> {
    serde_json::from_slice(b).with_context(|| format!("{name}: not JSON"))
}

/// The JSON token of `s` as the file holds it: printable ASCII that needs no
/// escape, so a byte replacement of the token is a replacement of the value.
fn token(s: &str) -> Result<Vec<u8>> {
    ensure!(s.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\'), "a string that needs escaping");
    Ok(format!("\"{s}\"").into_bytes())
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    let (mut n, mut i) = (0, 0);
    while let Some(at) = hay.get(i..).and_then(|h| h.windows(needle.len()).position(|w| w == needle)) {
        n += 1;
        i += at + needle.len();
    }
    n
}

fn replace_all(hay: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while let Some(at) = hay.get(i..).and_then(|h| h.windows(old.len()).position(|w| w == old)) {
        out.extend_from_slice(&hay[i..i + at]);
        out.extend_from_slice(new);
        i += at + old.len();
    }
    out.extend_from_slice(&hay[i..]);
    out
}

/// Replaces each old string of `plan` (old -> (new, pointers)) in `b`: the
/// pointers holding the old value must be exactly the planned ones, and its
/// token must occur exactly once per pointer (never as a key or elsewhere).
fn replace(b: &[u8], plan: &BTreeMap<String, (String, Vec<String>)>, name: &str) -> Result<Vec<u8>> {
    let v = leaves(&parse(b, name)?);
    let mut b = b.to_vec();
    for (old, (new, pointers)) in plan {
        let mut held: Vec<&String> = v.iter().filter(|(_, x)| x.as_str() == Some(old)).map(|(p, _)| p).collect();
        held.sort();
        let mut planned: Vec<&String> = pointers.iter().collect();
        planned.sort();
        ensure!(held == planned, "{name}: the pointers holding the old value differ from the plan");
        let t = token(old).with_context(|| name.to_owned())?;
        ensure!(count(&b, &t) == pointers.len(), "{name}: the token count differs from the pointer count");
        b = replace_all(&b, &t, &token(new).with_context(|| name.to_owned())?);
    }
    Ok(b)
}

/// Exactly the pointers of `expected` (pointer -> new value) differ between
/// `old` and `new`, and every other leaf is equal; returns them.
fn check(old: &[u8], new: &[u8], expected: &BTreeMap<String, String>, name: &str) -> Result<Vec<String>> {
    let (a, b) = (leaves(&parse(old, name)?), leaves(&parse(new, name)?));
    ensure!(a.keys().eq(b.keys()), "{name}: the structure changed");
    let differ: BTreeMap<String, Value> =
        a.iter().filter(|(p, x)| b.get(*p) != Some(*x)).map(|(p, _)| (p.clone(), b[p].clone())).collect();
    let want: BTreeMap<String, Value> = expected.iter().map(|(p, s)| (p.clone(), Value::String(s.clone()))).collect();
    ensure!(differ == want, "{name}: the changed pointers differ from the planned ones");
    Ok(differ.into_keys().collect())
}

/// Strings that read as an absolute local path or name a path under a
/// private tree.
fn local_strings(v: &Value) -> usize {
    leaves(v).values().filter(|x| x.as_str().is_some_and(|s| is_local_path(s) || names_private_tree(s))).count()
}

/// `name`'s pointers holding the string `old`, rebound to `new`.
fn rebind(b: &[u8], old: &str, new: &str, name: &str) -> Result<(Vec<u8>, Vec<String>)> {
    let pointers: Vec<String> =
        leaves(&parse(b, name)?).into_iter().filter(|(_, x)| x.as_str() == Some(old)).map(|(p, _)| p).collect();
    let plan = BTreeMap::from([(old.to_owned(), (new.to_owned(), pointers.clone()))]);
    Ok((replace(b, &plan, name)?, pointers))
}

fn create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

struct Args {
    root: PathBuf,
    record: PathBuf,
    src: PathBuf,
    out: PathBuf,
}

fn args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let (mut root, mut record, mut pos) = (None, None, Vec::new());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => root = it.next().map(PathBuf::from),
            "--record" => record = it.next().map(PathBuf::from),
            _ if a.starts_with("--") => bail!("unknown flag {a}"),
            _ => pos.push(PathBuf::from(a)),
        }
    }
    let usage = "usage: rewrite_package_provenance --root CHECKOUT --record RECORD.json SRC_DIR OUT_DIR";
    let [src, out]: [PathBuf; 2] = pos.try_into().map_err(|_| anyhow::anyhow!(usage))?;
    Ok(Args { root: root.context(usage)?, record: record.context(usage)?, src, out })
}

fn main() -> Result<()> {
    for line in rewrite(&args()?)? {
        println!("{line}");
    }
    Ok(())
}

/// The rewrite; every check runs before anything is written. Returns one
/// line per output file: its digest before and after, and how many pointers
/// changed.
fn rewrite(a: &Args) -> Result<Vec<String>> {
    ensure!(!a.out.exists() && !a.record.exists(), "OUT_DIR and RECORD.json must be new");
    let mut old: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for n in JSON_FILES {
        old.insert(n, std::fs::read(a.src.join(n)).with_context(|| format!("read {n}"))?);
    }
    let mut copied = Vec::new();
    for e in std::fs::read_dir(&a.src)? {
        let e = e?;
        let name = e.file_name().into_string().map_err(|_| anyhow::anyhow!("a file name that is not UTF-8"))?;
        ensure!(e.file_type()?.is_file(), "{name}: only regular files are copied");
        if !JSON_FILES.contains(&name.as_str()) {
            copied.push(name);
        }
    }
    copied.sort();
    let mut new = old.clone();
    let mut changes: BTreeMap<&str, BTreeMap<String, String>> =
        JSON_FILES.iter().map(|n| (*n, BTreeMap::new())).collect();

    // 1. Provenance strings, at the known pointers only.
    for (n, patterns) in POINTERS {
        let mut v = parse(&old[n], n)?;
        let before = leaves(&v);
        let moved = portable_at_in(&mut v, patterns, &a.root);
        let mut plan: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
        for p in moved {
            let s = v.pointer(&p).and_then(Value::as_str).context("a rewritten pointer")?.to_owned();
            let o = before[&p].as_str().context("a rewritten value that was no string")?.to_owned();
            let entry = plan.entry(o).or_insert_with(|| (s.clone(), Vec::new()));
            ensure!(entry.0 == s, "{n}: one old string with two new ones");
            entry.1.push(p.clone());
            changes.get_mut(n).context("file")?.insert(p, s);
        }
        new.insert(n, replace(&old[n], &plan, n)?);
    }
    ensure!(local_strings(&parse(&old["heads.json"], "heads.json")?) == 0, "heads.json holds a local path");

    // 2. The contract digest, where it is bound (checked even when the
    //    contract is unchanged; a change is recorded only when it moved).
    let (c_old, c_new) = (sha(&old["contract.json"]), sha(&new["contract.json"]));
    let mut bound = BTreeMap::new();
    for n in ["heads.json", "calibration.json", "manifest.json"] {
        let (b, pointers) = rebind(&new[n], &c_old, &c_new, n)?;
        new.insert(n, b);
        if c_old != c_new {
            for p in &pointers {
                changes.get_mut(n).context("file")?.insert(p.clone(), c_new.clone());
            }
        }
        bound.insert(n, pointers);
    }
    ensure!(
        bound["heads.json"] == ["/contract_sha256"]
            && bound["calibration.json"] == ["/contract_sha256"]
            && bound["manifest.json"] == ["/contract/sha256", "/fit_binding/contract_sha256"],
        "the contract digest is bound elsewhere than expected"
    );

    // 3. The manifest's member digests of the rewritten heads and calibration.
    for (n, ptr) in [("heads.json", "/heads/sha256"), ("calibration.json", "/calibration/sha256")] {
        let (o, s) = (sha(&old[n]), sha(&new[n]));
        let (b, pointers) = rebind(&new["manifest.json"], &o, &s, "manifest.json")?;
        ensure!(pointers == [ptr], "{n}: its digest is bound elsewhere in the manifest than {ptr}");
        new.insert("manifest.json", b);
        if o != s {
            changes.get_mut("manifest.json").context("file")?.insert(ptr.to_owned(), s);
        }
    }

    // 4. Exactly the planned pointers differ; no local path is left; the
    //    bindings hold as the scorer's guards read them.
    let mut differ = BTreeMap::new();
    for n in JSON_FILES {
        differ.insert(n, check(&old[n], &new[n], &changes[n], n)?);
        ensure!(local_strings(&parse(&new[n], n)?) == 0, "{n}: a local path is left");
    }
    let (h, cal, m) =
        (parse(&new["heads.json"], "h")?, parse(&new["calibration.json"], "c")?, parse(&new["manifest.json"], "m")?);
    let contract = Value::String(sha(&new["contract.json"]));
    ensure!(
        [
            &h["contract_sha256"],
            &cal["contract_sha256"],
            &m["contract"]["sha256"],
            &m["fit_binding"]["contract_sha256"]
        ]
        .iter()
        .all(|x| **x == contract),
        "the contract binding"
    );
    ensure!(
        m["heads"]["sha256"] == Value::String(sha(&new["heads.json"]))
            && m["calibration"]["sha256"] == Value::String(sha(&new["calibration.json"])),
        "the member bindings"
    );

    // The record: pointers and digests only.
    let (m_old, cal_old) = (parse(&old["manifest.json"], "m")?, parse(&old["calibration.json"], "c")?);
    let files: Vec<Value> = JSON_FILES
        .iter()
        .filter(|n| old[**n] != new[**n])
        .map(|n| {
            let (rebound, rewritten): (Vec<_>, Vec<_>) = changes[n].iter().partition(|(_, s)| is_digest(s));
            json!({"file": n, "before_sha256": sha(&old[n]), "after_sha256": sha(&new[n]),
                "rewritten_pointers": rewritten.into_iter().map(|(p, _)| p).collect::<Vec<_>>(),
                "rebound_digest_pointers": rebound.into_iter().map(|(p, _)| p).collect::<Vec<_>>()})
        })
        .collect();
    let mut record = json!({
        "schema": RECORD_SCHEMA,
        "rule": "grammar_a_baseline::provenance::portable_at_in (v2)",
        "tool": TOOL,
        "tool_sha256": sha(TOOL_SOURCE),
        "rule_module": RULE,
        "rule_sha256": sha(RULE_SOURCE),
        "files": files,
        "fit_bound_contract_sha256": m_old["fit_binding"]["contract_sha256"],
        "statement": "Only provenance strings and the digests that bind the rewritten files changed. The manifest's \
            fit_binding.contract_sha256 names the rewritten contract because the artifact spec requires it to equal \
            the contract file's digest; the fit itself was bound to fit_bound_contract_sha256. The exporter and \
            normalization tool digests in the manifest name the revisions that wrote the original strings.",
    });
    let binding: Vec<&String> = changes["manifest.json"]
        .iter()
        .filter(|(p, s)| p.starts_with("/fit_binding/") && !is_digest(s))
        .map(|(p, _)| p)
        .collect();
    if !binding.is_empty() {
        record["fit_binding_rewritten_pointers"] = json!(binding);
        record["fit_binding_source_sha256"] = m_old["fit_hashes"]["binding_json_sha256"].clone();
        record["fit_binding_note"] = json!(
            "/fit_binding is the fit's binding with these pointers made portable (and contract_sha256 rebound), \
             so it is not the bytes /fit_hashes/binding_json_sha256 names, the fit's binding before the rewrite"
        );
    }
    if let Some(source) = cal_old["document_calibration_source"]["sha256"].as_str() {
        record["calibration_source_sha256"] = json!(source);
        record["calibration_source_note"] = json!(
            "the calibration's document_calibration_source.sha256 names the source calibration file as it was, \
             which a path-stripped reference publishes under another digest"
        );
    }
    let mut text = Vec::new();
    let mut ser =
        serde_json::Serializer::with_formatter(&mut text, serde_json::ser::PrettyFormatter::with_indent(b" "));
    serde::Serialize::serialize(&record, &mut ser)?;
    text.push(b'\n');
    let printed = String::from_utf8_lossy(&text);
    for n in JSON_FILES {
        let before = leaves(&parse(&old[n], n)?);
        for p in changes[n].keys() {
            if let Some(o) = before[p].as_str().filter(|o| !is_digest(o)) {
                ensure!(!printed.contains(o), "{n}: the record would name a string the file held before");
            }
        }
    }
    ensure!(!printed.contains(&*a.root.to_string_lossy()), "the record would name the checkout");

    // Write: the new directory (owner only), the files, the copies, the record.
    std::fs::DirBuilder::new().mode(0o700).create(&a.out).with_context(|| format!("create {}", a.out.display()))?;
    for n in JSON_FILES {
        create_new(&a.out.join(n), &new[n])?;
    }
    for n in &copied {
        let bytes = std::fs::read(a.src.join(n))?;
        create_new(&a.out.join(n), &bytes)?;
        ensure!(sha(&std::fs::read(a.out.join(n))?) == sha(&bytes), "{n}: the copy differs");
    }
    create_new(&a.record, &text)?;
    let mut lines = Vec::new();
    for n in JSON_FILES.iter().map(|n| n.to_string()).chain(copied) {
        let after = sha(&std::fs::read(a.out.join(&n))?);
        let before = sha(&std::fs::read(a.src.join(&n))?);
        let changed = differ.get(n.as_str()).map_or(0, Vec::len);
        lines.push(format!("{n} {before} -> {after} ({changed} pointers)"));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic model directory written against checkout `root`: run
    /// directories under the private tree and outside the repository, a
    /// calibration source path, an init fit directory, and every binding.
    fn model(dir: &Path, root: &str, extra_key: bool) {
        std::fs::create_dir(dir).unwrap();
        let mut contract = json!({"embedding_normalization": {"runs": [
            {"run": format!("{root}/data/private-x/norm/run-000"), "shard": 0},
            {"run": "/opt/elsewhere/norm/run-001", "shard": 1}]}, "dim": 768, "note": "see /x"});
        if extra_key {
            // The old string as a key too: its token count differs from its pointers.
            contract[format!("{root}/data/private-x/norm/run-000")] = json!(1);
        }
        let contract = serde_json::to_vec_pretty(&contract).unwrap();
        let c = sha(&contract);
        let heads = serde_json::to_vec(&json!({"contract_sha256": c, "w": [0.25, -1.5e-7, 3]})).unwrap();
        let cal = serde_json::to_vec(&json!({"contract_sha256": c, "knots": [[0.1, 0.2], [0.3, 0.4]],
            "document_calibration_source": {"file": format!("{root}/data/private-x/cal.json"), "sha256": "ab".repeat(32)}}))
        .unwrap();
        let manifest = serde_json::to_vec_pretty(&json!({"contract": {"sha256": c}, "heads": {"sha256": sha(&heads)},
            "calibration": {"sha256": sha(&cal)},
            "fit_binding": {"contract_sha256": c, "init_fit": {"fit_dir": format!("{root}/data/private-x/fits/init")}},
            "fit_hashes": {"binding_json_sha256": "cd".repeat(32), "init_fit": {"fit_dir": format!("{root}/data/private-x/fits/init")}}}))
        .unwrap();
        for (n, b) in
            [("contract.json", contract), ("heads.json", heads), ("calibration.json", cal), ("manifest.json", manifest)]
        {
            std::fs::write(dir.join(n), b).unwrap();
        }
        std::fs::write(dir.join("a-fp32band.onnx"), b"graph bytes").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{\"t\": 1}").unwrap();
    }

    #[test]
    fn only_provenance_strings_and_their_bindings_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = "/srv/checkout";
        let src = tmp.path().join("src");
        model(&src, root, false);
        let a = Args {
            root: root.into(),
            record: tmp.path().join("record.json"),
            src: src.clone(),
            out: tmp.path().join("out"),
        };
        let lines = rewrite(&a).unwrap();
        assert_eq!(lines.len(), 6);
        let read = |d: &Path, n: &str| parse(&std::fs::read(d.join(n)).unwrap(), n).unwrap();
        let (c, h, cal, m) = (
            read(&a.out, "contract.json"),
            read(&a.out, "heads.json"),
            read(&a.out, "calibration.json"),
            read(&a.out, "manifest.json"),
        );
        assert_eq!(c["embedding_normalization"]["runs"][0]["run"], "run-000");
        assert_eq!(c["embedding_normalization"]["runs"][1]["run"], "run-001");
        assert_eq!(c["note"], "see /x", "a string at no provenance pointer is never changed");
        assert_eq!(cal["document_calibration_source"]["file"], "cal.json");
        assert_eq!(m["fit_binding"]["init_fit"]["fit_dir"], "init");
        let cs = sha(&std::fs::read(a.out.join("contract.json")).unwrap());
        for x in [
            &h["contract_sha256"],
            &cal["contract_sha256"],
            &m["contract"]["sha256"],
            &m["fit_binding"]["contract_sha256"],
        ] {
            assert_eq!(*x, Value::String(cs.clone()));
        }
        assert_eq!(m["heads"]["sha256"], sha(&std::fs::read(a.out.join("heads.json")).unwrap()));
        // Every other byte is unchanged: only the bound digest's token moved.
        let old_c = sha(&std::fs::read(src.join("contract.json")).unwrap());
        let old_heads = std::fs::read(src.join("heads.json")).unwrap();
        assert_eq!(
            replace_all(&old_heads, old_c.as_bytes(), cs.as_bytes()),
            std::fs::read(a.out.join("heads.json")).unwrap()
        );
        for n in ["a-fp32band.onnx", "tokenizer.json"] {
            assert_eq!(std::fs::read(a.out.join(n)).unwrap(), std::fs::read(src.join(n)).unwrap());
        }
        let record = std::fs::read_to_string(&a.record).unwrap();
        assert!(!record.contains(root) && !record.contains("/opt/elsewhere") && !record.contains("private-x"));
        let r: Value = serde_json::from_str(&record).unwrap();
        assert_eq!(r["schema"], RECORD_SCHEMA);
        assert_eq!(r["tool"], TOOL);
        assert_eq!(r["tool_sha256"], sha(TOOL_SOURCE));
        assert_eq!(r["fit_binding_rewritten_pointers"], json!(["/fit_binding/init_fit/fit_dir"]));
        assert_eq!(r["calibration_source_sha256"], "ab".repeat(32));
        assert_eq!(r["files"][0]["file"], "contract.json");
        assert_eq!(
            r["files"][0]["rewritten_pointers"],
            json!(["/embedding_normalization/runs/0/run", "/embedding_normalization/runs/1/run"])
        );
        // Run again on the output: nothing is left to rewrite.
        let again = Args {
            root: root.into(),
            record: tmp.path().join("r2.json"),
            src: a.out.clone(),
            out: tmp.path().join("out2"),
        };
        rewrite(&again).unwrap();
        for n in JSON_FILES {
            assert_eq!(std::fs::read(again.out.join(n)).unwrap(), std::fs::read(a.out.join(n)).unwrap(), "{n}");
        }
    }

    #[test]
    fn an_old_string_held_anywhere_else_refuses_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        model(&src, "/srv/checkout", true);
        let a = Args {
            root: "/srv/checkout".into(),
            record: tmp.path().join("record.json"),
            src,
            out: tmp.path().join("out"),
        };
        let e = rewrite(&a).unwrap_err().to_string();
        assert!(e.contains("token count"), "{e}");
        assert!(!a.out.exists() && !a.record.exists());
    }
}
