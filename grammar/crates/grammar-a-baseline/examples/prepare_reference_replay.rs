//! Freeze exact raw-text comparator requests and audit historical V6 exposure.
//! This tool does not fit, infer, call providers, or qualify a clean holdout.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn file_digest(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut input = fs::File::open(path)?;
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row[key].as_str().with_context(|| format!("missing {key}"))
}
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn read_rows(path: &Path, mut consume: impl FnMut(Value) -> Result<()>) -> Result<()> {
    for line in BufReader::new(fs::File::open(path)?).lines() {
        consume(serde_json::from_str(&line?)?)?;
    }
    Ok(())
}
#[derive(Default)]
struct Exposure {
    family: BTreeMap<String, BTreeSet<String>>,
    text: BTreeMap<String, BTreeSet<String>>,
    normalized: BTreeMap<String, BTreeSet<String>>,
}
impl Exposure {
    fn add(&mut self, row: &Value, expected_split: &str) -> Result<()> {
        ensure!(
            row["split"] == expected_split,
            "V6 partition differs from filename"
        );
        let text = string(row, "text")?;
        let hash = digest(text.as_bytes());
        ensure!(row["text_sha256"] == hash, "V6 text hash mismatch");
        for (map, key) in [
            (&mut self.family, string(row, "source_group")?.to_owned()),
            (&mut self.text, hash),
            (&mut self.normalized, digest(normalize(text).as_bytes())),
        ] {
            map.entry(key).or_default().insert(expected_split.into());
        }
        Ok(())
    }
    fn classify(&self, family: &str, text: &str) -> Value {
        let empty = BTreeSet::new();
        let f = self.family.get(family).unwrap_or(&empty);
        let t = self.text.get(&digest(text.as_bytes())).unwrap_or(&empty);
        let n = self
            .normalized
            .get(&digest(normalize(text).as_bytes()))
            .unwrap_or(&empty);
        let combined = f.iter().chain(t).chain(n).cloned().collect::<BTreeSet<_>>();
        let tier = if combined.contains("train") {
            "v6_train_exposed"
        } else if combined.contains("development") || combined.contains("calibration") {
            "v6_selection_or_calibration_exposed"
        } else if combined.contains("test") {
            "v6_previously_evaluated"
        } else {
            "not_matched_in_supplied_v6_rosters"
        };
        json!({"tier":tier,"family_partitions":f,"exact_text_partitions":t,
            "whitespace_normalized_text_partitions":n,"pretraining_exposure":"unknown"})
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn push_jsonl(bytes: &mut Vec<u8>, row: &Value) -> Result<()> {
    serde_json::to_writer(&mut *bytes, row)?;
    bytes.push(b'\n');
    Ok(())
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 5 || (args.len() == 6 && args[5] == "--opened-test"),
        "prepare_reference_replay RECORDS MANIFEST SPLIT V6_DATA_DIR NEW_OUT [--opened-test]"
    );
    let split = &args[2];
    ensure!(
        matches!(
            split.as_str(),
            "train" | "development" | "calibration" | "test"
        ),
        "invalid split"
    );
    ensure!(
        split != "test" || args.len() == 6,
        "explicit opened-Test diagnostic flag required"
    );
    let out = Path::new(&args[4]);
    ensure!(!out.exists(), "new output required");
    let parent = out.parent().context("output parent")?.canonicalize()?;
    let candidate = parent.join(out.file_name().context("output filename")?);
    ensure!(
        candidate
            .components()
            .any(|c| c.as_os_str() == "grammar-private"),
        "private research output required"
    );
    ensure!(
        Command::new("git")
            .arg("-C")
            .arg(&parent)
            .args(["check-ignore", "--quiet"])
            .arg(&candidate)
            .status()?
            .success(),
        "output must be Git ignored"
    );
    let raw = fs::read(&args[0])?;
    let manifest: Value = serde_json::from_slice(&fs::read(&args[1])?)?;
    ensure!(
        manifest["records_sha256"] == digest(&raw),
        "source manifest binding mismatch"
    );
    if split == "test" {
        ensure!(
            manifest["test_text_opened"] == true && manifest["test_used_for_selection"] == false,
            "requires already-opened, non-selection Test evidence"
        );
    }
    let mut exposure = Exposure::default();
    let mut roster_bindings = BTreeMap::new();
    for part in ["train", "development", "calibration", "test"] {
        let path = PathBuf::from(&args[3]).join(format!("{part}.jsonl"));
        read_rows(&path, |row| exposure.add(&row, part))?;
        roster_bindings.insert(part, json!({"path":path,"sha256":file_digest(&path)?}));
    }
    let mut requests = Vec::new();
    let mut roster = Vec::new();
    let mut seen = BTreeSet::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut families = BTreeSet::new();
    let mut requested = 0;
    let mut skipped_reference = 0;
    for line in raw.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
        let row: Value = serde_json::from_slice(line)?;
        if row["frozen_split"] != *split {
            continue;
        }
        if row["purpose"] == "reference" {
            skipped_reference += 1;
            continue;
        }
        ensure!(row["purpose"] == "query", "unknown purpose");
        let id = string(&row, "id")?;
        ensure!(seen.insert(id.to_owned()), "duplicate query id");
        let text = string(&row, "text")?;
        ensure!(
            row["text_sha256"] == digest(text.as_bytes()),
            "query text hash mismatch"
        );
        ensure!(
            matches!(
                row["origin"].as_str(),
                Some("human_only" | "model_only" | "mixed")
            ),
            "origin target"
        );
        let family = string(&row, "source_group")?;
        families.insert(family.to_owned());
        let prior = exposure.classify(family, text);
        *counts
            .entry(string(&prior, "tier")?.to_owned())
            .or_default() += 1;
        push_jsonl(&mut requests, &json!({"id":id,"text":text}))?;
        push_jsonl(
            &mut roster,
            &json!({"id":id,"source_group":family,"text_sha256":row["text_sha256"],
            "origin":row["origin"],"split":split,"v6_exposure":prior,"record_sha256":digest(line),
            "share_target":row["share_target"],"evidence":row["evidence"],"grammar_exposure":row["grammar_exposure"],
            "source_path":row["source"]["path"],"source_record_id":row["source"]["record_id"]}),
        )?;
        requested += 1;
    }
    ensure!(requested > 0, "empty comparison population");
    let report = json!({"schema":"slop_ninja_reference_replay_request_v1","split":split,
        "requested":requested,"source_families":families.len(),"reference_rows_skipped":skipped_reference,
        "exposure_counts":counts,"records_sha256":digest(&raw),"source_manifest_sha256":file_digest(Path::new(&args[1]))?,
        "v6_rosters":roster_bindings,"requests_sha256":digest(&requests),"roster_sha256":digest(&roster),
        "source_sha256":digest(include_bytes!("prepare_reference_replay.rs")),"provider_calls":0,
        "test_used_for_selection":false,"clean_holdout":false,
        "policy":"retrospective paired diagnostic; unchanged text; no truncation; inference receives only id/text; all unsupported rows retained",
        "limits":["roster matching cannot exclude pretraining exposure or alternate-edition overlap",
            "historical V6 binary metrics exclude mixed; report mixed-inclusive involvement separately",
            "V6 supplies origin only, not trained share or attribution","this population cannot select the next model"]});
    fs::create_dir(out)?;
    write_new(&out.join("requests.jsonl"), &requests)?;
    write_new(&out.join("roster.jsonl"), &roster)?;
    write_new(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"requested":requested,"families":families.len(),"exposure_counts":counts})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(family: &str, text: &str, split: &str) -> Value {
        json!({"source_group":family,"text":text,"text_sha256":digest(text.as_bytes()),"split":split})
    }
    #[test]
    fn any_training_ancestry_takes_precedence_over_evaluation_membership() {
        let mut e = Exposure::default();
        e.add(&row("family-a", "A small test.", "train"), "train")
            .unwrap();
        e.add(&row("family-b", "A small test.", "test"), "test")
            .unwrap();
        assert_eq!(
            e.classify("family-b", "Different descendant.")["tier"],
            "v6_previously_evaluated"
        );
        assert_eq!(
            e.classify("family-b", "A small test.")["tier"],
            "v6_train_exposed"
        );
        assert_eq!(
            e.classify("family-a", "Different descendant.")["tier"],
            "v6_train_exposed"
        );
    }
    #[test]
    fn normalized_duplicates_are_detected_across_different_family_ids() {
        let mut e = Exposure::default();
        e.add(
            &row("source-a", "Some\n\twords.", "calibration"),
            "calibration",
        )
        .unwrap();
        let v = e.classify("new-id", " Some words. ");
        assert_eq!(v["tier"], "v6_selection_or_calibration_exposed");
        assert_eq!(v["exact_text_partitions"], json!([]));
        assert_eq!(
            e.classify("new-id", "Completely different.")["tier"],
            "not_matched_in_supplied_v6_rosters"
        );
    }
    #[test]
    fn changed_bytes_or_partition_fail_the_binding() {
        let mut e = Exposure::default();
        let mut r = row("a", "A sentence.", "test");
        assert!(e.add(&r, "train").is_err());
        r["text"] = "Changed.".into();
        assert!(e.add(&r, "test").is_err());
    }
}
