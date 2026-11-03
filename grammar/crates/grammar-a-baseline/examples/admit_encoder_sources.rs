//! Admit fixed excerpts after metadata reservation/acquisition. Preserve all
//! failures; never choose replacement excerpts or read the reserved Test prose.
//! QUERIES_DIR PRIOR_LEDGER CONSUMED_FINAL_DATASET CONSUMED_REFERENCES OUTPUT
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
pub(crate) fn norm(t: &str) -> String {
    sha(t
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
pub(crate) fn shingles(t: &str) -> BTreeSet<String> {
    let lower = t.to_lowercase();
    let words = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>();
    words
        .windows(50)
        .map(|w| sha(w.join(" ").as_bytes()))
        .collect()
}
fn add_consumed(
    path: &Path,
    exact: &mut BTreeSet<String>,
    normalized: &mut BTreeSet<String>,
) -> Result<Value> {
    let b = fs::read(path)?;
    for r in rows(&b)? {
        ensure!(
            r["frozen_split"] == "test",
            "expected already-consumed Test evidence"
        );
        let text = s(&r, "text")?;
        ensure!(
            r["text_sha256"] == sha(text.as_bytes()),
            "consumed text changed"
        );
        exact.insert(sha(text.as_bytes()));
        normalized.insert(norm(text));
    }
    Ok(
        json!({"path":path.canonicalize()?,"sha256":sha(&b),"purpose":"already consumed regression evidence; hash exclusion only"}),
    )
}
fn run(input: &Path, ledger: &Path, consumed: &Path, refs: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let m = read(&input.join("manifest.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_author_queries_v2"
            && m["test_mode"] == false
            && m["test_text_opened"] == false,
        "non-Test fixed extraction required"
    );
    let lb = fs::read(ledger)?;
    let l: Value = serde_json::from_slice(&lb)?;
    ensure!(l["complete"] == true, "complete prior ledger required");
    let set = |key: &str| -> Result<BTreeSet<String>> {
        l[key]
            .as_array()
            .context("ledger hashes")?
            .iter()
            .map(|x| Ok(x.as_str().context("hash")?.to_owned()))
            .collect()
    };
    let mut exact = set("protected_text_sha256")?;
    let mut normalized = set("protected_normalized_text_sha256")?;
    let prior = [
        add_consumed(&consumed.join("records.jsonl"), &mut exact, &mut normalized)?,
        add_consumed(refs, &mut exact, &mut normalized)?,
    ];
    let mut all = BTreeMap::new();
    for (file, key) in [
        ("humans.jsonl", "records_sha256"),
        ("references.jsonl", "references_sha256"),
    ] {
        let b = fs::read(input.join(file))?;
        ensure!(m[key] == sha(&b), "extraction bytes changed");
        for r in rows(&b)? {
            let id = s(&r, "id")?.to_owned();
            if let Some(old) = all.insert(id, r.clone()) {
                ensure!(old == r, "duplicate id with different bytes");
            }
        }
    }
    let mut excluded = BTreeMap::<String, BTreeSet<String>>::new();
    let mut seen_shingles = BTreeMap::<String, BTreeSet<String>>::new();
    let mut families = BTreeMap::new();
    for r in all.values() {
        let group = s(r, "source_group")?;
        let part = s(r, "frozen_split")?;
        ensure!(
            ["train", "development", "calibration"].contains(&part),
            "Test prose forbidden"
        );
        if let Some(old) = families.insert(group.to_owned(), part.to_owned()) {
            ensure!(old == part, "source family crosses splits");
        }
        ensure!(
            r["origin"] == "human_only"
                && r["documented_human_only"] == false
                && r["rights"]["commercial_training"] == true,
            "source history/rights mismatch"
        );
        let text = s(r, "text")?;
        let h = sha(text.as_bytes());
        ensure!(r["text_sha256"] == h, "excerpt hash");
        let record = &r["source"]["record"];
        let b = fs::read(s(record, "text_path")?)?;
        ensure!(record["text_sha256"] == sha(&b), "source bytes changed");
        let span = r["source"]["byte_span"].as_array().context("span")?;
        ensure!(span.len() == 2, "span width");
        let lo = span[0].as_u64().context("span lo")? as usize;
        let hi = span[1].as_u64().context("span hi")? as usize;
        ensure!(
            lo < hi && hi <= b.len() && b[lo..hi] == *text.as_bytes(),
            "exact source span differs"
        );
        let reasons = excluded.entry(group.to_owned()).or_default();
        if !(150..=450).contains(&text.split_whitespace().count()) {
            reasons.insert("fixed_word_bound".into());
        }
        if exact.contains(&h) || normalized.contains(&norm(text)) {
            reasons.insert("prior_exact_or_whitespace_normalized_overlap".into());
        }
        for k in shingles(text) {
            seen_shingles.entry(k).or_default().insert(group.to_owned());
        }
    }
    let mut collisions = Vec::new();
    for (hash, groups) in seen_shingles {
        if groups.len() > 1 {
            for group in &groups {
                excluded
                    .entry(group.clone())
                    .or_default()
                    .insert("shared_50_word_sequence_across_works".into());
            }
            collisions.push(json!({"shingle_sha256":hash,"source_groups":groups}));
        }
    }
    excluded.retain(|_, v| !v.is_empty());
    let mut queries = Vec::new();
    let mut references = Vec::new();
    let mut exclusions = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for mut r in all.into_values() {
        if let Some(reasons) = excluded.get(s(&r, "source_group")?) {
            exclusions
                .push(json!({"id":r["id"],"source_group":r["source_group"],"reasons":reasons}));
            continue;
        }
        let stratum = r["source"]["record"]["selection_row"]["stratum"]
            .as_str()
            .context("reviewed acquisition stratum")?
            .to_owned();
        r["metadata"] = json!({"acquisition_stratum":stratum,"source_collection":"pg_stage3_historical","normalization_or_split_metadata_is_not_learned_input":true});
        *counts
            .entry(format!(
                "{}:{}:{}",
                r["frozen_split"].as_str().unwrap(),
                r["purpose"].as_str().unwrap(),
                stratum
            ))
            .or_default() += 1;
        if r["purpose"] == "reference" {
            references.push(r);
        } else {
            ensure!(r["purpose"] == "query", "purpose");
            queries.push(r);
        }
    }
    fs::create_dir(out)?;
    let mut files = BTreeMap::new();
    for (name, values) in [
        ("humans.jsonl", &queries),
        ("references.jsonl", &references),
        ("exclusions.jsonl", &exclusions),
        ("collisions.jsonl", &collisions),
    ] {
        let mut b = Vec::new();
        for v in values {
            serde_json::to_writer(&mut b, v)?;
            b.push(b'\n');
        }
        put(&out.join(name), &b)?;
        files.insert(name, sha(&b));
    }
    let report = json!({"schema":"slop_ninja_encoder_source_admission_v1","complete":true,"query_records":queries.len(),"reference_records":references.len(),"excluded_records":exclusions.len(),"excluded_families":excluded,"extraction_failures":m["failures"],"requested_non_test":m["planned_records"].as_array().context("roster")?.iter().filter(|r|r["frozen_split"]!="test").count(),"test_reserved_ids":m["test_ids"],"test_text_opened":false,"files_sha256":files,"counts":counts,"prior_ledger_sha256":sha(&lb),"consumed_evidence":prior,"extraction_manifest_sha256":sha(&fs::read(input.join("manifest.json"))?),"implementation_sha256":sha(include_bytes!("admit_encoder_sources.rs")),"limitations":["Historical author proxies; no authenticated human-only production history.","Exact/whitespace-normalized overlap against cumulative retained hashes; normalized 50-word overlap among new excerpts. No full-work or universal pretraining audit.","Unknown prior grammar exposure remains conservatively exposed; no fresh-clean-holdout claim from acquisition alone.","All source work descendants remain in their frozen partition; references do not enter fitting."]});
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"queries":queries.len(),"references":references.len(),"exclusions":exclusions.len(),"counts":counts})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 5,
        "QUERIES LEDGER CONSUMED_DATASET CONSUMED_REFERENCES OUTPUT"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        Path::new(&a[4]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shingle_matching_ignores_formatting_but_requires_fifty_words() {
        let text = (0..50)
            .map(|i| format!("synthetic{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(shingles(&text).len(), 1);
        assert_eq!(
            shingles(&text),
            shingles(&text.to_uppercase().replace(' ', ",\n"))
        );
        assert!(shingles("Few synthetic words.").is_empty());
    }
}
