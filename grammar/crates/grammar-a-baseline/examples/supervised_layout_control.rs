//! Whitespace-only paired controls for an already frozen final-Test dataset.
//!
//! supervised_layout_control FINAL_TEST_RECORDS FINAL_TEST_MANIFEST NEW_OUT
//!
//! Reads only the two explicit inputs when invoked. No fitting, selection,
//! parser, model, API, source-file traversal, or automatic Test execution.
//! NEW_OUT must be new and inside Git-ignored data/. It contains records.jsonl,
//! manifest.json, original.records.jsonl and original.manifest.json. Original
//! files are copied byte-for-byte; each control retains raw text/share evidence
//! in layout_control, which is audit metadata, never a learned input.
//!
//! Policy: Rust Unicode split_whitespace(), joined with one ASCII space. IDs
//! append -layout; all source families, splits, roles, targets, provenance and
//! generation history stay fixed. Only current text bindings and supported
//! realized-span evidence are updated. Original parent_id links stay unchanged.
//! Measured generation requires the existing realized-span schema, exact old
//! assembly, trimmed replacement, source/final spans starting at byte zero,
//! and a word-aligned source end. Unsupported evidence is rejected, not guessed.
//! Output is canonical compact JSONL for transform_supervised_forest and carries
//! test_records_sha256 for explicit `a_supervised_pangram test ... test ...`.
//! This diagnostic measures layout effects under frozen models; it establishes
//! neither new clean-holdout status nor permission to refit or select a model.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

const POLICY: &str = "rust_unicode_split_whitespace_join_ascii_space_v1";
const SPAN_SCHEMA: &str = "slop_ninja_realized_span_share_v1";
const LIMIT: u64 = 256 * 1024 * 1024;

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
fn count(v: &Value, key: &str) -> Result<u64> {
    v[key]
        .as_u64()
        .with_context(|| format!("missing unsigned {key}"))
}
fn words(s: &str) -> usize {
    s.split_whitespace().count()
}
fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn same_words(a: &str, b: &str) -> Result<()> {
    ensure!(
        a.split_whitespace().eq(b.split_whitespace()),
        "normalization changed word sequence"
    );
    Ok(())
}
fn span(value: &Value, source: &str) -> Result<(usize, usize)> {
    let pair = value.as_array().context("span must be a pair")?;
    ensure!(pair.len() == 2, "span must be a pair");
    let start = usize::try_from(pair[0].as_u64().context("span start")?)?;
    let end = usize::try_from(pair[1].as_u64().context("span end")?)?;
    ensure!(
        start <= end
            && end <= source.len()
            && source.is_char_boundary(start)
            && source.is_char_boundary(end),
        "invalid UTF-8 byte span"
    );
    Ok((start, end))
}

fn remap_share(row: &Value, new_text: &str) -> Result<Value> {
    let e = &row["share_evidence"];
    ensure!(
        e["schema"] == SPAN_SCHEMA && e["word_policy"] == "unicode_split_whitespace_v1",
        "unsupported measured-share schema/policy"
    );
    ensure!(
        e.get("source_spans").is_none(),
        "additional span layout is unsupported"
    );
    let source = text(e, "source_text")?;
    let replacement = text(e, "replacement")?;
    let original = text(row, "text")?;
    ensure!(
        !replacement.is_empty() && replacement.trim() == replacement,
        "replacement must follow generator trim contract"
    );
    ensure!(
        e["source_sha256"] == sha(source.as_bytes())
            && e["final_sha256"] == sha(original.as_bytes()),
        "old share hash mismatch"
    );
    let (start, end) = span(&e["source_byte_span"], source)?;
    let final_span = span(&e["final_byte_span"], original)?;
    ensure!(
        start == 0 && final_span == (0, replacement.len()),
        "only leading generator spans supported"
    );
    ensure!(
        &original[final_span.0..final_span.1] == replacement,
        "old replacement/final span mismatch"
    );
    let mode = e
        .get("mode")
        .or_else(|| row["generation"].get("mode"))
        .and_then(Value::as_str)
        .context("generation mode required")?;
    if let Some(generation_mode) = row["generation"].get("mode") {
        ensure!(*generation_mode == mode, "conflicting generation modes");
    }
    let new_source = normalized(source);
    let new_replacement = normalized(replacement);
    same_words(source, &new_source)?;
    same_words(replacement, &new_replacement)?;
    let new_end = match mode {
        "from_scratch" => {
            ensure!(
                row["origin"] == "model_only" && end == 0 && original == replacement,
                "invalid from-scratch assembly"
            );
            ensure!(
                new_text == new_replacement,
                "normalized from-scratch assembly mismatch"
            );
            0
        }
        "controlled_rewrite" => {
            ensure!(
                row["origin"] == "mixed" && end > 0,
                "rewrite requires mixed history and nonempty span"
            );
            ensure!(
                end == source.len() || source[end..].starts_with(char::is_whitespace),
                "source end splits a word"
            );
            ensure!(
                original == format!("{replacement}{}", &source[end..]),
                "old exact rewrite assembly mismatch"
            );
            let prefix_words = words(&source[..end]);
            ensure!(prefix_words > 0, "whitespace-only source span unsupported");
            // End immediately after the same complete source words. Any suffix
            // starts with the normalized ASCII separator, so concatenation is exact.
            let normalized_prefix = normalized(&source[..end]);
            let new_end = normalized_prefix.len();
            ensure!(
                new_source.starts_with(&normalized_prefix)
                    && words(&new_source[..new_end]) == prefix_words
                    && (new_end == new_source.len() || new_source[new_end..].starts_with(' ')),
                "cannot remap source word boundary"
            );
            ensure!(
                new_text == format!("{new_replacement}{}", &new_source[new_end..]),
                "normalized exact rewrite assembly mismatch"
            );
            new_end
        }
        _ => anyhow::bail!("unsupported generation mode"),
    };
    let old_n = words(replacement);
    let old_d = words(original);
    let new_n = words(&new_replacement);
    let new_d = words(new_text);
    ensure!(
        old_n > 0 && old_d > 0 && old_n <= old_d && (old_n, old_d) == (new_n, new_d),
        "share word counts changed"
    );
    ensure!(
        count(e, "replacement_words")? == old_n as u64 && count(e, "final_words")? == old_d as u64,
        "old measured word counts mismatch"
    );
    let realized = new_n as f64 / new_d as f64;
    ensure!(
        e["realized_share"].as_f64() == Some(realized)
            && row["share_target"].as_f64() == Some(realized),
        "exact measured share/target mismatch"
    );
    let mut new = e.clone();
    new["source_text"] = json!(new_source);
    new["source_sha256"] = json!(sha(new_source.as_bytes()));
    new["replacement"] = json!(new_replacement);
    new["source_byte_span"] = json!([0, new_end]);
    new["final_byte_span"] = json!([0, new_replacement.len()]);
    new["final_sha256"] = json!(sha(new_text.as_bytes()));
    new["replacement_words"] = json!(new_n);
    new["final_words"] = json!(new_d);
    new["realized_share"] = json!(realized);
    Ok(new)
}

fn control(row: &Value) -> Result<Value> {
    ensure!(
        row["frozen_split"] == "test" && row["purpose"] == "query",
        "only explicit Test queries supported"
    );
    ensure!(
        row.get("layout_control").is_none(),
        "input already has layout control metadata"
    );
    let id = text(row, "id")?;
    let new_id = format!("{id}-layout");
    ensure!(
        !id.is_empty()
            && new_id.len() <= 240
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
            && !matches!(id, "." | ".."),
        "ID is incompatible with comparator paths"
    );
    let original = text(row, "text")?;
    let original_hash = sha(original.as_bytes());
    ensure!(
        row["text_sha256"] == original_hash,
        "original text hash mismatch"
    );
    let new_text = normalized(original);
    ensure!(!new_text.is_empty(), "empty word sequence");
    same_words(original, &new_text)?;
    let target = row
        .get("share_target")
        .context("explicit share target required")?;
    ensure!(row["share_evidence"].is_object(), "share evidence required");
    let measured = row["share_evidence"]["schema"] == SPAN_SCHEMA;
    if !measured {
        ensure!(
            ["source_byte_span", "final_byte_span", "source_spans"]
                .iter()
                .all(|key| row["share_evidence"].get(*key).is_none()),
            "span evidence outside the supported schema cannot be remapped"
        );
    }
    match text(row, "origin")? {
        "human_only" => ensure!(
            !measured && target.as_f64() == Some(0.0),
            "human endpoint must remain zero"
        ),
        "model_only" => ensure!(
            target.as_f64() == Some(1.0),
            "model endpoint must remain one"
        ),
        "mixed" => ensure!(
            measured || target.is_null(),
            "mixed numeric target requires supported measured evidence"
        ),
        _ => anyhow::bail!("unsupported origin"),
    }
    let mut result = row.clone();
    result["id"] = json!(new_id);
    result["text"] = json!(new_text);
    result["text_sha256"] = json!(sha(new_text.as_bytes()));
    if let Some(hash) = row.get("source_sha256") {
        ensure!(
            *hash == original_hash,
            "top-level source hash differs from original text"
        );
        result["source_sha256"] = result["text_sha256"].clone();
    }
    if measured {
        result["share_evidence"] = remap_share(row, &new_text)?;
    }
    ensure!(result["share_target"] == *target, "share target changed");
    result["layout_control"] = json!({"schema":"slop_ninja_supervised_layout_control_v1",
        "normalization_policy":POLICY,"original_id":id,"raw_text":original,"raw_text_sha256":original_hash,
        "raw_share_evidence":row["share_evidence"],"original_record_sha256":sha(&serde_json::to_vec(row)?),
        "original_record_hash_encoding":"compact_serde_json_Value_utf8_without_newline",
        "word_sequence_unchanged":true,"original_words":words(original),"control_words":words(&new_text),
        "share_target_unchanged":true,"audit_metadata_only":true,"learned_input":false});
    Ok(result)
}

fn ids(value: &Value) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for v in value.as_array().context("ID allow-list")? {
        let id = v
            .as_str()
            .filter(|s| !s.is_empty())
            .context("nonempty ID")?;
        ensure!(ids.insert(id.to_owned()), "duplicate allowed ID");
    }
    Ok(ids)
}

fn derive(input: &[u8], manifest_bytes: &[u8]) -> Result<(Vec<u8>, Value)> {
    let parent: Value = serde_json::from_slice(manifest_bytes)?;
    ensure!(
        parent["schema"] == "slop_ninja_a_supervised_dataset_v1",
        "dataset manifest schema"
    );
    let input_hash = sha(input);
    let manifest_hash = sha(manifest_bytes);
    ensure!(
        parent["records_sha256"] == input_hash,
        "parent records hash mismatch"
    );
    for key in ["test_records_sha256", "input_sha256"] {
        if let Some(hash) = parent.get(key) {
            ensure!(*hash == input_hash, "conflicting parent input hashes");
        }
    }
    ensure!(
        parent["test_used_for_selection"] == false,
        "frozen Test dataset required"
    );
    let test_ids = ids(&parent["test_ids"])?;
    let non_test_ids = ids(&parent["non_test_ids"])?;
    ensure!(
        !test_ids.is_empty() && test_ids.len() <= 10_000 && test_ids.is_disjoint(&non_test_ids),
        "invalid Test roster"
    );
    let mut seen = BTreeSet::new();
    let mut derived_ids = Vec::new();
    let mut groups = BTreeSet::new();
    let mut output = Vec::new();
    for line in input.split_inclusive(|b| *b == b'\n') {
        let row: Value = serde_json::from_slice(line)?;
        let id = text(&row, "id")?;
        ensure!(
            test_ids.contains(id) && seen.insert(id.to_owned()),
            "unlisted/duplicate/non-Test record"
        );
        let group = text(&row, "source_group")?;
        ensure!(
            !group.is_empty() && parent["split_assignments"][group] == "test",
            "family split mismatch"
        );
        groups.insert(group.to_owned());
        let mut derived = control(&row)?;
        let derived_id = text(&derived, "id")?.to_owned();
        ensure!(
            !non_test_ids.contains(&derived_id) && !test_ids.contains(&derived_id),
            "derived ID collides with prior roster"
        );
        derived["layout_control"]["original_record_line_sha256"] = json!(sha(line));
        derived["layout_control"]["parent_records_sha256"] = json!(input_hash);
        derived["layout_control"]["parent_manifest_sha256"] = json!(manifest_hash);
        serde_json::to_writer(&mut output, &derived)?;
        output.push(b'\n');
        derived_ids.push(derived_id);
    }
    ensure!(
        seen == test_ids,
        "input does not exactly cover parent Test roster"
    );
    let mut manifest = parent.clone();
    for key in ["records_sha256", "test_records_sha256", "input_sha256"] {
        manifest[key] = json!(sha(&output));
    }
    manifest["test_ids"] = json!(derived_ids);
    // The fitting roster and all freeze/exposure bindings remain exactly as supplied.
    manifest["non_test_ids"] = parent["non_test_ids"].clone();
    manifest["records"] = json!(seen.len());
    manifest["requested_records"] = json!(seen.len());
    manifest["source_families"] = json!(groups.len());
    manifest["excluded"] = json!([]);
    manifest["sources"] = json!([{"path":"original.records.jsonl","sha256":input_hash},
        {"path":"original.manifest.json","sha256":manifest_hash}]);
    manifest["normalization_policy"] = json!(POLICY);
    manifest["layout_control"] = json!({"schema":"slop_ninja_supervised_layout_control_dataset_v1",
        "parent_records_sha256":input_hash,"parent_manifest_sha256":manifest_hash,
        "parent_test_ids":parent["test_ids"],"parent_excluded":parent["excluded"],
        "normalization_policy":POLICY,"implementation_sha256":sha(include_bytes!("supervised_layout_control.rs")),
        "one_control_per_parent":true,"selection_changed":false,"fitting_performed":false,
        "comparison":"paired parser/layout diagnostic under frozen model and calibration",
        "metadata_not_inputs":true,"original_files_retained_exactly":true});
    manifest["test_text_opened"] = json!(true);
    manifest["test_used_for_selection"] = json!(false);
    Ok((output, manifest))
}

fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "regular input file required"
    );
    let mut b = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut b)?;
    ensure!(b.len() as u64 <= LIMIT, "input exceeds 256 MiB bound");
    Ok(b)
}
fn new_output(path: &Path) -> Result<PathBuf> {
    ensure!(!path.try_exists()?, "output must be new");
    let parent = path.parent().context("output parent")?.canonicalize()?;
    let path = parent.join(path.file_name().context("output name")?);
    let git = Command::new("git")
        .arg("-C")
        .arg(&parent)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    ensure!(
        git.status.success(),
        "output parent must belong to Git checkout"
    );
    let root = PathBuf::from(String::from_utf8(git.stdout)?.trim()).canonicalize()?;
    ensure!(
        path.starts_with(root.join("data")),
        "raw text/control outputs belong in ignored data/"
    );
    ensure!(
        Command::new("git")
            .arg("-C")
            .arg(&parent)
            .args(["check-ignore", "--quiet", "--"])
            .arg(&path)
            .status()?
            .success(),
        "output must be Git-ignored"
    );
    Ok(path)
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        a.len() == 3,
        "supervised_layout_control FINAL_TEST_RECORDS FINAL_TEST_MANIFEST NEW_OUT"
    );
    let out = new_output(Path::new(&a[2]))?;
    let manifest_bytes = read(Path::new(&a[1]))?;
    let input = read(Path::new(&a[0]))?;
    let (records, manifest) = derive(&input, &manifest_bytes)?;
    let stage = tempfile::tempdir_in(out.parent().context("output parent")?)?;
    write(&stage.path().join("original.records.jsonl"), &input)?;
    write(
        &stage.path().join("original.manifest.json"),
        &manifest_bytes,
    )?;
    write(&stage.path().join("records.jsonl"), &records)?;
    let mut m = serde_json::to_vec_pretty(&manifest)?;
    m.push(b'\n');
    write(&stage.path().join("manifest.json"), &m)?;
    File::open(stage.path())?.sync_all()?;
    ensure!(!out.try_exists()?, "output appeared during preparation");
    fs::rename(stage.path(), &out)?;
    File::open(out.parent().context("output parent")?)?.sync_all()?;
    println!(
        "{}",
        json!({"records":manifest["records"],"policy":POLICY,"complete":true})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(text: &str, origin: &str, share: Value) -> Value {
        json!({"id":"synthetic:test","text":text,"text_sha256":sha(text.as_bytes()),
            "source_group":"synthetic-family","frozen_split":"test","purpose":"query",
            "origin":origin,"share_target":share,"share_evidence":{"basis":"synthetic"},
            "human_role_present":origin!="model_only","ai_role_present":origin!="human_only",
            "parent_id":null,"source":{"synthetic":true},"rights":{"external_evaluation":true}})
    }
    fn generated(source: &str, end: usize, replacement: &str, mode: &str) -> Value {
        let final_text = if mode == "from_scratch" {
            replacement.to_owned()
        } else {
            format!("{replacement}{}", &source[end..])
        };
        let share = words(replacement) as f64 / words(&final_text) as f64;
        let mut r = row(
            &final_text,
            if mode == "from_scratch" {
                "model_only"
            } else {
                "mixed"
            },
            json!(share),
        );
        r["generation"] = json!({"mode":mode,"request":{"retained":"original"}});
        r["share_evidence"] = json!({"schema":SPAN_SCHEMA,"source_text":source,"source_sha256":sha(source.as_bytes()),
            "source_byte_span":[0,end],"replacement":replacement,"final_byte_span":[0,replacement.len()],
            "final_sha256":sha(final_text.as_bytes()),"replacement_words":words(replacement),"final_words":words(&final_text),
            "word_policy":"unicode_split_whitespace_v1","realized_share":share,"requested_percent_not_target":25});
        r
    }
    #[test]
    fn unicode_layout_only_and_unmeasured_targets_stay_fixed() {
        for (origin, share) in [
            ("human_only", json!(0.0)),
            ("model_only", json!(1.0)),
            ("mixed", Value::Null),
        ] {
            let r = row("\tCafé\u{a0}😊\n\r\nwaits.\u{2003}Again!  ", origin, share);
            let c = control(&r).unwrap();
            assert_eq!(c["text"], "Café 😊 waits. Again!");
            assert_eq!(c["share_target"], r["share_target"]);
            assert_eq!(c["share_evidence"], r["share_evidence"]);
            for k in [
                "source_group",
                "frozen_split",
                "human_role_present",
                "ai_role_present",
                "parent_id",
                "source",
                "rights",
            ] {
                assert_eq!(c[k], r[k]);
            }
            assert_eq!(c["layout_control"]["raw_text"], r["text"]);
        }
    }
    #[test]
    fn partial_rewrite_remaps_unicode_byte_offsets_and_exact_assembly() {
        let source = "\tCafé\u{a0}waits.\n\nAnother\tword stays.  ";
        let r = generated(
            source,
            source.find("\n\n").unwrap(),
            "A  new\nscene.",
            "controlled_rewrite",
        );
        let c = control(&r).unwrap();
        let e = &c["share_evidence"];
        assert_eq!(c["text"], "A new scene. Another word stays.");
        assert_eq!(e["source_text"], "Café waits. Another word stays.");
        assert_eq!(e["source_byte_span"], json!([0, "Café waits.".len()]));
        assert_eq!(e["final_byte_span"], json!([0, "A new scene.".len()]));
        assert_eq!(c["share_target"], json!(0.5));
        assert_eq!(remap_share(&c, text(&c, "text").unwrap()).unwrap(), *e);
        assert_eq!(c["generation"], r["generation"]);
        assert_eq!(
            c["layout_control"]["raw_share_evidence"],
            r["share_evidence"]
        );
    }
    #[test]
    fn full_rewrite_stays_mixed_and_from_scratch_stays_model_only() {
        let source = " Old\nsource words.  ";
        for (mode, end, origin) in [
            ("controlled_rewrite", source.len(), "mixed"),
            ("from_scratch", 0, "model_only"),
        ] {
            let c = control(&generated(source, end, "New\twords\nonly.", mode)).unwrap();
            assert_eq!(c["origin"], origin);
            assert_eq!(c["share_target"], json!(1.0));
            assert_eq!(c["text"], "New words only.");
            assert_eq!(
                remap_share(&c, text(&c, "text").unwrap()).unwrap(),
                c["share_evidence"]
            );
        }
    }
    #[test]
    fn unsupported_or_inconsistent_evidence_is_rejected() {
        let r = generated(
            "Old words. Tail stays.",
            10,
            "New words.",
            "controlled_rewrite",
        );
        for (key, value) in [
            ("source_byte_span", json!([1, 10])),
            ("source_byte_span", json!([0, 2])),
            ("final_byte_span", json!([1, 10])),
            ("source_sha256", json!("bad")),
            ("replacement_words", json!(99)),
            ("realized_share", json!(0.25)),
            ("source_spans", json!([])),
        ] {
            let mut bad = r.clone();
            bad["share_evidence"][key] = value;
            assert!(control(&bad).is_err());
        }
        let mut bad = r.clone();
        bad["share_target"] = json!(0.25);
        assert!(control(&bad).is_err());
        assert!(span(&json!([0, 1]), "é word").is_err());
    }
    #[test]
    fn manifests_bind_controls_and_preserve_prior_roster_and_freeze() {
        let r = row("A\nsynthetic\ttest.", "human_only", json!(0.0));
        let input = format!("{r}\n").into_bytes();
        let parent = json!({"schema":"slop_ninja_a_supervised_dataset_v1","records_sha256":sha(&input),
            "test_records_sha256":sha(&input),"input_sha256":sha(&input),"test_ids":[r["id"]],
            "non_test_ids":["old-train"],"split_assignments":{"synthetic-family":"test"},
            "sources":[{"path":"synthetic.jsonl","sha256":sha(&input)}],"selection_freeze_sha256":"a".repeat(64),
            "test_used_for_selection":false,"excluded":[]});
        let p = serde_json::to_vec(&parent).unwrap();
        let (out, m) = derive(&input, &p).unwrap();
        for key in ["records_sha256", "test_records_sha256", "input_sha256"] {
            assert_eq!(m[key], sha(&out));
        }
        assert_eq!(m["non_test_ids"], parent["non_test_ids"]);
        assert_eq!(m["test_ids"], json!(["synthetic:test-layout"]));
        assert_eq!(
            m["selection_freeze_sha256"],
            parent["selection_freeze_sha256"]
        );
        assert_eq!(m["layout_control"]["parent_manifest_sha256"], sha(&p));
        assert!(derive(b"", &p).is_err());
        let mut wrong = parent.clone();
        wrong["split_assignments"]["synthetic-family"] = json!("train");
        assert!(derive(&input, &serde_json::to_vec(&wrong).unwrap()).is_err());
    }
}
