//! Offline preparation/reporting for full-context word and sentence screens.
//! Run with `prepare MANIFEST OUT_DIR` or `report PREPARED_JSONL SCORES_DIR OUT_REPORT`.
//! Prepared rows are scoring inputs, not corpus-ingestion records. Their train
//! label selects already chosen training sources; derived experimental prose
//! must remain exploratory if later imported into the corpus database.

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
use unslop::{
    evaluation, experiments,
    provider_ids::ProviderIds,
    util::{digest, read, read_jsonl, write_json},
};

fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("Missing nonempty {key}"))
}

fn candidate(source: &str, spec: &Value) -> Result<(String, Value)> {
    let kind = required(spec, "kind")?;
    let (text, edit) = match kind {
        "single_word" => {
            let occurrence = match spec.get("occurrence") {
                None => 1,
                Some(v) => usize::try_from(
                    v.as_u64()
                        .context("occurrence must be a positive integer")?,
                )?,
            };
            experiments::substitute(
                source,
                required(spec, "old")?,
                required(spec, "new")?,
                occurrence,
            )?
        }
        "sentence" => {
            let old = required(spec, "old")?;
            let new = required(spec, "new")?;
            let matches: Vec<_> = source.match_indices(old).collect();
            ensure!(
                matches.len() == 1,
                "Sentence edit requires exactly one exact occurrence, found {}",
                matches.len()
            );
            let start = matches[0].0;
            let end = start + old.len();
            (
                format!("{}{new}{}", &source[..start], &source[end..]),
                json!({"kind":kind,"old":old,"new":new,"start_byte":start,"end_byte":end}),
            )
        }
        "document_revision" => (required(spec, "text")?.to_string(), json!({"kind":kind})),
        _ => bail!("Unknown candidate kind: {kind}"),
    };
    ensure!(text != source, "Candidate must change the source text");
    ensure!(
        text.split_whitespace().count() >= 50,
        "Every candidate must contain at least 50 words"
    );
    Ok((text, edit))
}

fn insert_document(
    docs: &mut BTreeMap<String, Value>,
    text: &str,
    source: &Value,
    entry: Value,
) -> Result<()> {
    let hash = digest(text);
    if let Some(existing) = docs.get_mut(&hash) {
        ensure!(
            existing["text"] == text && existing["group_id"] == source["group_id"],
            "Identical prepared text crosses source families; resolve that ambiguity before scoring"
        );
        existing["metadata"]["screen_entries"]
            .as_array_mut()
            .unwrap()
            .push(entry);
    } else {
        docs.insert(hash, json!({"text":text,"corpus":"edit-screen-v1","group_id":source["group_id"],
            "split":"train","source_kind":"experimental","provider":null,"model":null,
            "domain":source["domain"],"register":source["register"],
            "metadata":{"source_corpus":source["corpus"],"source_sha256":entry["source_sha256"],"parent_source_split":"train",
                "candidate_id":entry["candidate_id"],"kind":entry["kind"],"screen_entries":[entry],
                "quality":"Not certified; candidate rationales are hypotheses, not human audits."}}));
    }
    Ok(())
}

fn prepare(manifest_path: &Path, out: &Path) -> Result<Value> {
    let raw_manifest = read(manifest_path)?;
    let manifest: Value = serde_json::from_str(&raw_manifest)?;
    let items = manifest.as_array().context("Manifest must be an array")?;
    ensure!(!items.is_empty(), "Manifest is empty");
    let parent = manifest_path.parent().unwrap_or(Path::new("."));
    let mut docs = BTreeMap::new();
    let mut source_inputs = Vec::new();
    let mut source_keys = BTreeSet::new();
    let mut all_entries = Vec::new();
    for item in items {
        let corpus = required(item, "corpus")?;
        let group = required(item, "group_id")?;
        ensure!(
            source_keys.insert((corpus.to_owned(), group.to_owned())),
            "Duplicate source corpus/group in manifest"
        );
        let source_path = parent.join(required(item, "source_path")?).canonicalize()?;
        let source_raw = read(&source_path)?;
        let source_docs: Vec<Value> = source_raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?;
        let matches: Vec<_> = source_docs
            .iter()
            .filter(|d| d["corpus"] == corpus && d["group_id"] == group && d["split"] == "train")
            .collect();
        ensure!(
            matches.len() == 1,
            "Expected exactly one train source for {corpus}/{group}; found {}",
            matches.len()
        );
        let source = matches[0];
        let text = required(source, "text")?;
        for key in ["domain", "register"] {
            required(source, key)?;
        }
        ensure!(
            text.split_whitespace().count() >= 50,
            "Source must contain at least 50 words"
        );
        let source_hash = digest(text);
        source_inputs.push(
            json!({"source_path":source_path,"file_sha256":digest(&source_raw),
            "corpus":corpus,"group_id":group,"source_sha256":source_hash}),
        );
        let baseline = json!({"source_corpus":corpus,"group_id":group,"source_sha256":source_hash,
            "candidate_id":"baseline","kind":"baseline","candidate_sha256":source_hash});
        insert_document(&mut docs, text, source, baseline.clone())?;
        all_entries.push(baseline);
        let specs = item["candidates"]
            .as_array()
            .context("candidates must be an array")?;
        ensure!(
            !specs.is_empty(),
            "Each source needs at least one candidate"
        );
        let mut ids = BTreeSet::new();
        for spec in specs {
            let id = required(spec, "id")?;
            ensure!(
                id != "baseline" && ids.insert(id),
                "Candidate IDs must be unique and cannot be baseline"
            );
            let (edited, edit) = candidate(text, spec)?;
            let entry = json!({"source_corpus":corpus,"group_id":group,"source_sha256":source_hash,
                "candidate_id":id,"kind":spec["kind"],"candidate_sha256":digest(&edited),"edit":edit,
                "rationale":spec.get("rationale"),"preservation_notes":spec.get("preservation_notes")});
            insert_document(&mut docs, &edited, source, entry.clone())?;
            all_entries.push(entry);
        }
    }
    let mut emitted = BTreeSet::new();
    let mut prepared = String::new();
    for entry in &all_entries {
        let hash = required(entry, "candidate_sha256")?;
        if emitted.insert(hash) {
            let doc = docs.get(hash).context("Prepared entry has no document")?;
            prepared.push_str(&format!("{doc}\n"));
        }
    }
    let frozen = json!({"format":"edit-screen-v1","manifest_sha256":digest(&raw_manifest),
        "manifest":manifest,"source_inputs":source_inputs,"prepared_sha256":digest(&prepared),
        "output_hashes":docs.keys().collect::<Vec<_>>(),"entries":all_entries});
    let output_path = out.join("prepared.jsonl");
    let lock_path = out.join("manifest.lock.json");
    if output_path.exists() {
        ensure!(
            read(&output_path)? == prepared,
            "Frozen prepared output differs; use a new output directory"
        );
    }
    if lock_path.exists() {
        ensure!(
            serde_json::from_str::<Value>(&read(&lock_path)?)? == frozen,
            "Frozen manifest or source inputs differ; use a new output directory"
        );
    }
    fs::create_dir_all(out)?;
    if !lock_path.exists() {
        write_json(&lock_path, &frozen)?;
    }
    if !output_path.exists() {
        let mut file = tempfile::NamedTempFile::new_in(out)?;
        file.write_all(prepared.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&output_path)?;
    }
    Ok(
        json!({"prepared":output_path,"lock":lock_path,"unique_texts":docs.len(),
        "sources":items.len(),"candidates":all_entries.len()-items.len(),"split":"train","network_requests":0}),
    )
}

/// The report lists salted task digests; each raw ID stays in its private score
/// record and the sidecar map.
fn report(prepared_path: &Path, scores: &Path, out: &Path, ids: &ProviderIds) -> Result<Value> {
    let prepared_raw = read(prepared_path)?;
    let lock_path = prepared_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("manifest.lock.json");
    let frozen: Value = serde_json::from_str(&read(&lock_path)?)?;
    ensure!(
        frozen["format"] == "edit-screen-v1" && frozen["prepared_sha256"] == digest(&prepared_raw),
        "Prepared corpus does not match its frozen manifest"
    );
    let docs = read_jsonl(prepared_path)?;
    ensure!(!docs.is_empty(), "Prepared corpus is empty");
    let mut observed = BTreeMap::new();
    let mut records = Vec::new();
    let mut detector_model = None;
    let mut detector_version = None;
    let mut seen_entries = Vec::new();
    for doc in docs {
        ensure!(
            doc["split"] == "train" && doc["corpus"] == "edit-screen-v1",
            "Unexpected prepared corpus/split"
        );
        let text = required(&doc, "text")?;
        let hash = digest(text);
        ensure!(!observed.contains_key(&hash), "Duplicate prepared text");
        let path = scores.join(format!("{hash}.json"));
        let r: Value = serde_json::from_str(&read(&path).with_context(|| {
            format!("Missing or unreadable score for {hash}; no candidates may be omitted")
        })?)?;
        ensure!(
            r["submitted_text"] == text && r["sha256"] == hash,
            "Score input/hash does not match prepared text"
        );
        ensure!(
            r["detector"].as_str().unwrap_or("pangram") == "pangram",
            "Only Pangram scores are supported"
        );
        let summary = evaluation::summarize_runs(std::slice::from_ref(&r), 0.1, 3, ids)?;
        let group = &summary["groups"][0];
        if let Some(model) = &detector_model {
            ensure!(
                &group["model"] == model,
                "Detector model selectors differ across screen"
            );
        } else {
            detector_model = Some(group["model"].clone());
        }
        if let Some(version) = &detector_version {
            ensure!(
                &group["version"] == version,
                "Detector versions differ across screen"
            );
        } else {
            detector_version = Some(group["version"].clone());
        }
        observed.insert(hash, json!({"ai":r["result"]["fraction_ai"],"assisted":r["result"]["fraction_ai_assisted"],
            "ai_plus_assisted":group["ai_plus_assisted_fraction"]["values"][0],"task_id":group["task_ids"][0],"score_file":path}));
        let entries = doc["metadata"]["screen_entries"]
            .as_array()
            .context("Missing candidate mapping")?;
        seen_entries.extend(entries.iter().cloned());
        records.push(r);
    }
    let summary = evaluation::summarize_runs(&records, 0.1, 3, ids)?;
    ensure!(
        summary["duplicate_records_ignored"] == 0,
        "Screen reused a detector task"
    );
    let entries = frozen["entries"]
        .as_array()
        .context("Frozen manifest omitted entries")?;
    // Ordering is different after deduplication; compare canonical entry strings.
    let entry_set = |rows: &[Value]| rows.iter().map(Value::to_string).collect::<BTreeSet<_>>();
    ensure!(
        entry_set(entries) == entry_set(&seen_entries) && entries.len() == seen_entries.len(),
        "Prepared candidate mapping differs from frozen manifest"
    );
    let mut rows = Vec::new();
    for entry in entries.iter().filter(|entry| entry["kind"] != "baseline") {
        let source_hash = required(entry, "source_sha256")?;
        let edited_hash = required(entry, "candidate_sha256")?;
        let baseline = observed
            .get(source_hash)
            .context("Missing baseline result")?;
        let edited = observed
            .get(edited_hash)
            .context("Missing candidate result")?;
        let delta = edited["ai_plus_assisted"].as_f64().unwrap()
            - baseline["ai_plus_assisted"].as_f64().unwrap();
        rows.push(
            json!({"source_corpus":entry["source_corpus"],"group_id":entry["group_id"],
            "candidate_id":entry["candidate_id"],"kind":entry["kind"],"source_sha256":source_hash,
            "candidate_sha256":edited_hash,"baseline":baseline,"candidate":edited,
            "candidate_minus_baseline_ai_plus_assisted":delta,"delta_percentage_points":100.0*delta,
            "rationale":entry["rationale"],"preservation_notes":entry["preservation_notes"],
            "quality_certified":false,"quality_audit":"unavailable"}),
        );
    }
    let output = json!({"format":"edit-screen-report-v1","scope":"full_abstract_screen","prepared_sha256":digest(&prepared_raw),
        "detector":"pangram","model":detector_model,"version":detector_version,
        "unique_scored_texts":observed.len(),"candidates":rows,"reliability_established":false,
        "quality_certified":false,"design":"Single observation per exact input; exploratory screen only.",
        "interpretation":"All planned candidates are included. Differences may reflect detector variability; repeat matched baseline/candidate controls before attributing effects to edits. Rationale and preservation notes do not certify fidelity, readability, or tone."});
    write_json(out, &output)?;
    ids.record(&out.display().to_string())?;
    Ok(output)
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.first().and_then(|v| v.to_str()) {
        Some("prepare") if args.len() == 3 => prepare(Path::new(&args[1]), Path::new(&args[2])),
        Some("report") if args.len() == 4 => ProviderIds::load().and_then(|ids| {
            report(
                Path::new(&args[1]),
                Path::new(&args[2]),
                Path::new(&args[3]),
                &ids,
            )
        }),
        _ => Err(anyhow::anyhow!(
            "Usage: edit_screen prepare MANIFEST OUT_DIR | report PREPARED_JSONL SCORES_DIR OUT_REPORT"
        )),
    };
    match result {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
        Err(error) => {
            eprintln!("edit_screen: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> String {
        format!(
            "Éva acts accordingly.\r\n{}",
            "Readers examine the argument with care and preserve its details. ".repeat(6)
        )
    }

    fn setup(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        fs::write(
            root.join("source.jsonl"),
            format!(
                "{}\n",
                json!({"text":source(),"corpus":"model-v1","group_id":"source-1",
            "split":"train","domain":"philosophy","register":"book-preface"})
            ),
        )
        .unwrap();
        let path = root.join("edits.json");
        write_json(&path, &json!([{"source_path":"source.jsonl","corpus":"model-v1","group_id":"source-1","candidates":[
            {"id":"word-1","kind":"single_word","old":"accordingly","new":"thus"},
            {"id":"sentence-1","kind":"sentence","old":"Éva acts accordingly.","new":"Éva thus acts."}]}])).unwrap();
        (path, root.join("prepared"))
    }

    #[test]
    fn exact_word_and_sentence_replacements_preserve_unedited_bytes() {
        let original = source();
        let (word, _) = candidate(
            &original,
            &json!({"kind":"single_word","old":"accordingly","new":"thus"}),
        )
        .unwrap();
        assert_eq!(word, original.replacen("accordingly", "thus", 1));
        assert!(word.contains("\r\n"));
        let spec = json!({"kind":"sentence","old":"Éva acts accordingly.","new":"Éva thus acts."});
        let (sentence, _) = candidate(&original, &spec).unwrap();
        assert_eq!(
            sentence,
            original.replacen("Éva acts accordingly.", "Éva thus acts.", 1)
        );
        assert!(candidate(&(original.clone() + "Éva acts accordingly."), &spec).is_err());
    }

    #[test]
    fn frozen_output_allows_exact_resume_and_rejects_changed_inputs() {
        let root = tempfile::tempdir().unwrap();
        let (manifest, out) = setup(root.path());
        let first = prepare(&manifest, &out).unwrap();
        let bytes = fs::read(out.join("prepared.jsonl")).unwrap();
        assert_eq!(first["unique_texts"], 3);
        let rows = read_jsonl(&out.join("prepared.jsonl")).unwrap();
        let ids: Vec<_> = rows
            .iter()
            .map(|row| row["metadata"]["candidate_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["baseline", "word-1", "sentence-1"]);
        assert_eq!(prepare(&manifest, &out).unwrap(), first);
        let mut changed: Value = serde_json::from_str(&read(&manifest).unwrap()).unwrap();
        changed[0]["candidates"][0]["new"] = json!("therefore");
        write_json(&manifest, &changed).unwrap();
        assert!(
            prepare(&manifest, &out)
                .unwrap_err()
                .to_string()
                .contains("Frozen")
        );
        assert_eq!(fs::read(out.join("prepared.jsonl")).unwrap(), bytes);
    }

    #[test]
    fn missing_score_is_an_error_and_complete_report_keeps_every_candidate() {
        let root = tempfile::tempdir().unwrap();
        let map = root.path().join("private").join("id-map.tsv");
        let ids = ProviderIds::new(b"synthetic-provider-id-salt-v1".to_vec(), map.clone()).unwrap();
        let (manifest, out) = setup(root.path());
        prepare(&manifest, &out).unwrap();
        let scores = root.path().join("scores");
        fs::create_dir_all(&scores).unwrap();
        let docs = read_jsonl(&out.join("prepared.jsonl")).unwrap();
        for (i, d) in docs.iter().enumerate().take(2) {
            let text = d["text"].as_str().unwrap();
            write_json(&scores.join(format!("{}.json",digest(text))),&json!({"submitted_text":text,"sha256":digest(text),
                "detector":"pangram","model":"pangram-4","task_id":format!("task-{i}"),
                "result":{"stage":"STAGE_SUCCESS","version":"4.0","fraction_ai":0.2,"fraction_ai_assisted":0.0,"fraction_human":0.8}})).unwrap();
        }
        let output = root.path().join("report.json");
        assert!(
            report(&out.join("prepared.jsonl"), &scores, &output, &ids)
                .unwrap_err()
                .to_string()
                .contains("Missing or unreadable score")
        );
        assert!(!output.exists());
        assert!(!map.exists());
        let text = docs[2]["text"].as_str().unwrap();
        write_json(&scores.join(format!("{}.json",digest(text))),&json!({"submitted_text":text,"sha256":digest(text),
            "detector":"pangram","model":"pangram-4","task_id":"task-2",
            "result":{"stage":"STAGE_SUCCESS","version":"4.0","fraction_ai":0.05,"fraction_ai_assisted":0.0,"fraction_human":0.95}})).unwrap();
        let result = report(&out.join("prepared.jsonl"), &scores, &output, &ids).unwrap();
        assert_eq!(result["candidates"].as_array().unwrap().len(), 2);
        // The report carries digests only; the raw IDs go to the private sidecar.
        let written = read(&output).unwrap();
        assert!(!written.contains("\"task-"), "raw task ID in report");
        let digest_0 = ids.shareable("task-0");
        assert_eq!(
            result["candidates"][0]["baseline"]["task_id"],
            digest_0.as_str()
        );
        let sidecar = read(&map).unwrap();
        assert_eq!(sidecar.lines().count(), 4);
        let row = format!("task-0\t{digest_0}\t{}", output.display());
        assert!(sidecar.lines().any(|line| line == row));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&map).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(result["quality_certified"], false);
        assert_eq!(result["reliability_established"], false);
    }
}
