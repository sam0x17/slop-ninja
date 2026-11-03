//! Exact excerpt selection from pinned author sources, with Test excluded upstream.
//! `--fresh-balanced` reserves every ID from metadata before opening prose and
//! writes partition-specific references to references.jsonl (Test-only with --test).
//! Known Train references also remain in humans.jsonl for the existing fitting path.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn write(path: &Path, v: &Value) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn body(text: &str) -> Result<(usize, usize)> {
    let marker = text.find("*** START OF").context("PG start marker")?;
    let start = marker + text[marker..].find('\n').context("PG marker line")? + 1;
    let end = text[start..].find("*** END OF").context("PG end marker")? + start;
    ensure!(start < end, "PG body bounds");
    Ok((start, end))
}
fn excerpt(text: &str, fraction: usize) -> Result<(usize, usize)> {
    let (b, e) = body(text)?;
    let t = &text[b..e];
    let offsets = t
        .char_indices()
        .filter_map(|(i, c)| {
            (!c.is_whitespace() && (i == 0 || t[..i].ends_with(char::is_whitespace))).then_some(i)
        })
        .collect::<Vec<_>>();
    ensure!(offsets.len() > 1500, "short source work");
    let desired = offsets[offsets.len() * fraction / 100];
    let boundary = t[desired..]
        .char_indices()
        .find_map(|(i, c)| {
            let end = desired + i + c.len_utf8();
            (matches!(c, '.' | '?' | '!')
                && end < t.len()
                && t[end..].starts_with(char::is_whitespace))
            .then_some(end)
        })
        .context("sentence start boundary")?;
    let start = boundary + (t[boundary..].len() - t[boundary..].trim_start().len());
    let mut count = 0usize;
    let mut end = None;
    for (i, c) in t[start..].char_indices() {
        if !c.is_whitespace() && (i == 0 || t[start..start + i].ends_with(char::is_whitespace)) {
            count += 1;
        }
        let p = start + i + c.len_utf8();
        if count >= 220
            && matches!(c, '.' | '?' | '!')
            && (p == t.len() || t[p..].starts_with(char::is_whitespace))
        {
            end = Some(p);
            break;
        }
    }
    let end = end.context("sentence end boundary")?;
    ensure!(
        t[start..end].split_whitespace().count() <= 600,
        "excerpt exceeds fixed bound"
    );
    Ok((b + start, b + end))
}
fn prepare_legacy(source_manifest: &Path, out: &Path, test_mode: bool) -> Result<()> {
    let bytes = fs::read(source_manifest)?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    fs::create_dir(out)?;
    let mut rows = Vec::new();
    let mut test_ids = Vec::new();
    let mut failures = Vec::new();
    for s in manifest["sources"].as_array().context("sources")? {
        let pg = s["pg_id"]
            .as_u64()
            .or_else(|| s["pg_id"].as_str().and_then(|x| x.parse().ok()))
            .context("PG id")?;
        let split = s["split"].as_str().context("split")?;
        for fraction in [20, 60] {
            let id = format!("author-pg{pg}-p{fraction}");
            if split == "test" {
                test_ids.push(id.clone());
                if !test_mode {
                    continue;
                }
            } else if test_mode {
                continue;
            }
            ensure!(
                matches!(
                    split,
                    "train_reference" | "train" | "development" | "calibration" | "test"
                ),
                "unknown split"
            );
            let path = s["text_path"].as_str().context("text_path")?;
            let text_bytes = fs::read(path)?;
            ensure!(s["text_sha256"] == sha(&text_bytes), "source changed");
            let text = std::str::from_utf8(&text_bytes)?;
            let (start, end) = match excerpt(text, fraction) {
                Ok(x) => x,
                Err(error) => {
                    failures.push(json!({"id":id,"reason":error.to_string()}));
                    continue;
                }
            };
            let excerpt = &text[start..end];
            ensure!(
                s["rights"]["commercial_training"] == true
                    && s["rights"]["external_evaluation"] == true,
                "rights missing"
            );
            rows.push(json!({"schema":"slop_ninja_a_supervised_record_v1","id":id,"text":excerpt,"text_sha256":sha(excerpt.as_bytes()),
                "source_group":s["source_group"],"frozen_split":if split=="train_reference"{"train"}else{split},
                "purpose":s["purpose"],"origin":"human_only","evidence":"historical_public_domain_author_proxy",
                "human_identity":s["author_key"],"human_role_present":true,"ai_identity":null,"ai_role_present":false,
                "share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},
                "exposed_to_grammar":true,"grammar_exposure":"conservative_unknown_prior_exposure;new_frozen_pipeline_export",
                "source":{"record":s,"source_manifest_sha256":sha(&bytes),"byte_span":[start,end],"extraction":"body_percent20_60_next_sentence_220words_end_sentence_max600_v1"},
                "rights":s["rights"],"parent_id":null}));
        }
    }
    let mut record_bytes = Vec::new();
    for r in &rows {
        serde_json::to_writer(&mut record_bytes, r)?;
        record_bytes.push(b'\n');
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out.join("humans.jsonl"))?;
    file.write_all(&record_bytes)?;
    file.sync_all()?;
    write(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_author_queries_v1","source_manifest_sha256":sha(&bytes),"records_sha256":sha(&record_bytes),"non_test_ids":if test_mode{vec![]}else{rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>()},"test_ids":test_ids,"failures":failures,"test_text_opened":test_mode,"rights_scope":"source-specific public-domain basis, historical transcription caveats retained"}),
    )?;
    println!(
        "{}",
        json!({"records":rows.len(),"test_reserved":test_ids.len(),"failures":failures.len()})
    );
    Ok(())
}

#[derive(Debug)]
struct PlannedSource<'a> {
    record: &'a Value,
    pg: u64,
    partition: &'a str,
    reference: bool,
    metadata_only: bool,
}

fn partition(value: &str) -> Result<&str> {
    let normalized = value.strip_suffix("_reference").unwrap_or(value);
    ensure!(
        matches!(normalized, "train" | "development" | "calibration" | "test"),
        "unknown partition: {value}"
    );
    Ok(normalized)
}

fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

// Validate the complete roster before ANY source reads, including metadata for
// withheld works. Raw paths are resolved only after the Test gate below.
fn plan_sources(manifest: &Value) -> Result<Vec<PlannedSource<'_>>> {
    ensure!(
        manifest.get("complete").is_none_or(|v| v == true),
        "acquisition incomplete"
    );
    let sources = manifest["sources"].as_array().context("sources")?;
    let reserved = match manifest.get("reserved_sources") {
        Some(value) => value.as_array().context("reserved_sources")?.as_slice(),
        None => &[],
    };
    if let Some(expected) = manifest.get("excluded_test_sources") {
        ensure!(
            expected.as_u64() == Some(reserved.len() as u64),
            "reserved Test metadata count mismatch"
        );
    }
    if let Some(expected) = manifest.get("source_count") {
        ensure!(
            expected.as_u64() == Some(sources.len() as u64),
            "acquisition source count mismatch"
        );
    }
    let mut plan = Vec::new();
    let mut pgs = BTreeSet::new();
    let mut groups = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    for (source, metadata_only) in sources
        .iter()
        .map(|s| (s, false))
        .chain(reserved.iter().map(|s| (s, true)))
    {
        let pg = source["pg_id"]
            .as_u64()
            .or_else(|| source["pg_id"].as_str().and_then(|s| s.parse().ok()))
            .context("PG id")?;
        ensure!(pg > 0 && pgs.insert(pg), "invalid or duplicate PG id");
        let split = source["split"].as_str().context("split")?;
        let frozen = partition(split)?;
        ensure!(
            !metadata_only || frozen == "test",
            "only Test metadata may be reserved"
        );
        let purpose = source["purpose"].as_str().context("purpose")?;
        ensure!(matches!(purpose, "query" | "reference"), "unknown purpose");
        let reference = purpose == "reference";
        ensure!(
            !split.ends_with("_reference") || reference,
            "split/purpose mismatch"
        );
        if let Some(assigned) = source.get("preassigned_partition") {
            let assigned = assigned.as_str().context("preassigned_partition")?;
            ensure!(
                partition(assigned)? == frozen,
                "split/preassignment mismatch"
            );
            ensure!(
                !assigned.ends_with("_reference") || reference,
                "preassignment/purpose mismatch"
            );
        }
        if let Some(role) = source.get("role") {
            let role = role.as_str().context("role")?;
            if role.ends_with("_reference") {
                ensure!(
                    reference && partition(role)? == frozen,
                    "role/partition mismatch"
                );
            } else {
                ensure!(role == purpose, "role/purpose mismatch");
            }
        }
        let group = source["source_group"].as_str().context("source_group")?;
        ensure!(
            !group.is_empty() && groups.insert(group),
            "source work repeated across query/reference roster"
        );
        if !metadata_only {
            ensure!(digest(&source["text_sha256"]), "invalid source digest");
            ensure!(
                hashes.insert(source["text_sha256"].as_str().unwrap()),
                "duplicate source bytes"
            );
        }
        ensure!(
            source["author_key"].as_str().is_some_and(|s| !s.is_empty()),
            "author_key"
        );
        if !metadata_only {
            ensure!(
                source["rights"]["commercial_training"] == true
                    && source["rights"]["external_evaluation"] == true,
                "rights missing"
            );
        }
        plan.push(PlannedSource {
            record: source,
            pg,
            partition: frozen,
            reference,
            metadata_only,
        });
    }
    if let Some(expected) = manifest.get("expected_non_test_sources") {
        ensure!(
            expected.as_u64() == Some(plan.iter().filter(|s| s.partition != "test").count() as u64),
            "acquisition non-Test count mismatch"
        );
    }
    Ok(plan)
}

// A fresh acquisition is not an exposure audit. Honor an audit only when the
// manifest supplies a source-bound statement and an evidence digest. Its full
// contents remain in source.record; no audit file or withheld prose is opened.
fn fresh_exposure(source: &Value) -> Result<(bool, String)> {
    let audit = &source["grammar_exposure_audit"];
    if audit.is_null() {
        return Ok((
            true,
            "conservative_unknown_prior_exposure;fresh_acquisition_is_not_an_exposure_audit".into(),
        ));
    }
    ensure!(
        audit["status"] == "audited"
            && audit["text_sha256"] == source["text_sha256"]
            && digest(&audit["evidence_sha256"]),
        "unbound grammar exposure audit"
    );
    let exposed = audit["exposed_to_grammar"]
        .as_bool()
        .context("audited exposure")?;
    let description = audit["grammar_exposure"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("audited exposure description")?;
    Ok((exposed, description.to_owned()))
}

fn jsonl(path: &Path, rows: &[Value]) -> Result<String> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(sha(&bytes))
}

fn prepare_fresh(source_manifest: &Path, out: &Path, test_mode: bool) -> Result<()> {
    let manifest_bytes = fs::read(source_manifest)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let source_manifest_sha = sha(&manifest_bytes);
    let plan = plan_sources(&manifest)?;
    ensure!(
        !test_mode || plan.iter().all(|s| !s.metadata_only),
        "Test sources are metadata-only reservations; supply an acquired Test manifest before --test"
    );
    let mut roster = Vec::new();
    for source in &plan {
        // IDs depend only on acquisition metadata, never successful extraction.
        for fraction in [20, 60] {
            roster.push(json!({"id":format!("author-pg{}-p{fraction}", source.pg),
                "frozen_split":source.partition,"purpose":if source.reference {"reference"} else {"query"},
                "source_group":source.record["source_group"],"fraction":fraction,"metadata_only":source.metadata_only}));
        }
    }
    let mut humans = Vec::new();
    let mut references = Vec::new();
    let mut failures = Vec::new();
    let mut test_text_opened = false;
    for source in &plan {
        if (source.partition == "test") != test_mode {
            continue;
        }
        let record = source.record;
        let (exposed, exposure) = fresh_exposure(record)?;
        let path = record["text_path"]
            .as_str()
            .or_else(|| record["text_relative_path"].as_str())
            .context("text_path")?;
        let root = manifest["acquisition_root"]
            .as_str()
            .map(Path::new)
            .unwrap_or_else(|| source_manifest.parent().unwrap_or(Path::new(".")));
        let path = root.join(path); // Absolute text_path keeps its original meaning.
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        test_text_opened |= source.partition == "test";
        ensure!(record["text_sha256"] == sha(&bytes), "source changed");
        if let Some(expected) = record.get("text_bytes") {
            ensure!(
                expected.as_u64() == Some(bytes.len() as u64),
                "source byte count changed"
            );
        }
        let text = std::str::from_utf8(&bytes)?;
        for fraction in [20, 60] {
            let id = format!("author-pg{}-p{fraction}", source.pg);
            let (start, end) = match excerpt(text, fraction) {
                Ok(bounds) => bounds,
                Err(error) => {
                    failures.push(
                        json!({"id":id,"reason":error.to_string(),"frozen_split":source.partition,
                        "purpose":if source.reference {"reference"} else {"query"}}),
                    );
                    continue;
                }
            };
            let text = &text[start..end];
            let row = json!({"schema":"slop_ninja_a_supervised_record_v1","id":id,"text":text,"text_sha256":sha(text.as_bytes()),
                "source_group":record["source_group"],"frozen_split":source.partition,
                "purpose":if source.reference {"reference"} else {"query"},
                "origin":"human_only","evidence":"historical_public_domain_author_proxy","documented_human_only":false,
                "human_identity":record["author_key"],"human_role_present":true,"ai_identity":null,"ai_role_present":false,
                "share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},
                "exposed_to_grammar":exposed,"grammar_exposure":exposure,
                "source":{"record":record,"source_manifest_sha256":source_manifest_sha,"byte_span":[start,end],
                    "extraction":"body_percent20_60_next_sentence_220words_end_sentence_max600_v1"},
                "rights":record["rights"],"parent_id":null});
            if source.reference {
                references.push(row.clone());
            }
            if !source.reference || (source.partition == "train" && record["enrollment"] == "known")
            {
                humans.push(row);
            }
        }
    }
    fs::create_dir(out)?;
    let humans_sha = jsonl(&out.join("humans.jsonl"), &humans)?;
    let references_sha = jsonl(&out.join("references.jsonl"), &references)?;
    let ids = |purpose: Option<&str>, test: Option<bool>| -> Vec<Value> {
        roster
            .iter()
            .filter(|r| {
                purpose.is_none_or(|p| r["purpose"] == p)
                    && test.is_none_or(|t| (r["frozen_split"] == "test") == t)
            })
            .map(|r| r["id"].clone())
            .collect()
    };
    write(
        &out.join("manifest.json"),
        &json!({
            "schema":"slop_ninja_author_queries_v2","mode":"fresh-balanced",
            "source_manifest_schema":manifest["schema"],"source_manifest_sha256":source_manifest_sha,
            "records_file":"humans.jsonl","records_sha256":humans_sha,"records":humans.len(),
            "references_file":"references.jsonl","references_sha256":references_sha,"references":references.len(),
            "non_test_ids":if test_mode {vec![]} else {humans.iter().map(|r| r["id"].clone()).collect::<Vec<_>>()},
            "test_ids":ids(None,Some(true)),"all_ids":ids(None,None),
            "query_ids":ids(Some("query"),None),"non_test_query_ids":ids(Some("query"),Some(false)),
            "test_query_ids":ids(Some("query"),Some(true)),
            "reference_ids":ids(Some("reference"),None),"test_reference_ids":ids(Some("reference"),Some(true)),
            "emitted_human_ids":humans.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(),
            "emitted_reference_ids":references.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(),
            "planned_records":roster,"failures":failures,"test_mode":test_mode,"test_text_opened":test_text_opened,
            "reference_policy":"Every reference retains its actual partition in references.jsonl; only known Train references also appear in humans.jsonl. Reference/query works are independent; do not concatenate duplicate Train references.",
            "exposure_policy":"Unknown prior exposure is conservatively exposed; only a source-bound manifest audit can override it. Acquisition alone is not an audit.",
            "rights_scope":"source-specific public-domain basis, historical transcription caveats retained; human labels are historical proxies, not documented human-only workflows"
        }),
    )?;
    println!(
        "{}",
        json!({"records":humans.len(),"references":references.len(),"test_reserved":ids(None,Some(true)).len(),"failures":failures.len()})
    );
    Ok(())
}

fn parse_flags(args: &[String]) -> Result<(bool, bool)> {
    ensure!(
        args.len() >= 2,
        "prepare_author_queries SOURCE_MANIFEST NEW_OUTPUT [--test] [--fresh-balanced]"
    );
    let (mut test, mut fresh) = (false, false);
    for flag in &args[2..] {
        match flag.as_str() {
            "--test" if !test => test = true,
            "--fresh-balanced" if !fresh => fresh = true,
            _ => anyhow::bail!("unknown or repeated option: {flag}"),
        }
    }
    Ok((test, fresh))
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (test, fresh) = parse_flags(&args)?;
    if fresh {
        prepare_fresh(Path::new(&args[0]), Path::new(&args[1]), test)
    } else {
        prepare_legacy(Path::new(&args[0]), Path::new(&args[1]), test)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn synthetic_text(pg: u64) -> String {
        let mut text = "Synthetic header é.\r\n*** START OF SYNTHETIC ***\r\n".to_owned();
        for i in 0..500 {
            text.push_str(&format!("Élan {pg} sentence {i} ends here.\r\n"));
        }
        text.push_str("*** END OF SYNTHETIC ***\r\nSynthetic footer.");
        text
    }

    fn source(root: &Path, pg: u64, split: &str, reference: bool, write_text: bool) -> Value {
        let text = synthetic_text(pg);
        let path = root.join(format!("pg{pg}.txt"));
        if write_text {
            fs::write(&path, text.as_bytes()).unwrap();
        }
        json!({"pg_id":pg,"author_key":format!("synthetic_author_{pg}"),
            "source_group":format!("synthetic_work_{pg}"),"split":split,
            "purpose":if reference {"reference"} else {"query"},"enrollment":"known",
            "text_path":path,"text_sha256":sha(text.as_bytes()),"text_bytes":text.len(),
            "rights":{"commercial_training":true,"external_evaluation":true,"note":"synthetic fixture"},
            "dates":{"original_composition_date":"synthetic"},
            "exposed_to_grammar":false,"grammar_exposure":"unaudited clean claim"})
    }

    fn manifest(root: &Path, sources: &[Value]) -> std::path::PathBuf {
        let path = root.join("sources.json");
        write(
            &path,
            &json!({"schema":"synthetic_acquisition_v1","acquisition_root":root,"sources":sources}),
        )
        .unwrap();
        path
    }

    fn rows(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    fn output_manifest(out: &Path) -> Value {
        serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap()
    }
    #[test]
    fn marker_bounds() {
        let t = "header\n*** START OF TEST ***\nExact body.\n*** END OF TEST ***\n";
        let (s, e) = body(t).unwrap();
        assert_eq!(&t[s..e], "Exact body.\n");
    }
    #[test]
    fn reject_missing_body() {
        assert!(body("arbitrary unmarked text").is_err());
    }

    #[test]
    fn exact_unicode_excerpts_stay_in_body_and_do_not_overlap() {
        let text = synthetic_text(1);
        let (body_start, body_end) = body(&text).unwrap();
        let first = excerpt(&text, 20).unwrap();
        let second = excerpt(&text, 60).unwrap();
        assert!(body_start <= first.0 && first.1 <= second.0 && second.1 <= body_end);
        for (start, end) in [first, second] {
            let slice = &text[start..end];
            assert!(slice.starts_with("Élan ") && slice.ends_with('.'));
            assert!((220..=600).contains(&slice.split_whitespace().count()));
            assert!(slice.contains("\r\n"));
        }
    }

    #[test]
    fn fresh_non_test_references_keep_partitions_and_reserve_failed_queries() {
        let dir = TempDir::new().unwrap();
        let mut sources = vec![
            source(dir.path(), 1, "train", false, true),
            source(dir.path(), 2, "train", true, true),
            source(dir.path(), 3, "development_reference", true, true),
            source(dir.path(), 4, "calibration", true, true),
            source(dir.path(), 5, "test", false, false),
            source(dir.path(), 6, "test_reference", true, false),
            source(dir.path(), 7, "development", false, false),
        ];
        sources[1]["role"] = json!("train_reference");
        sources[1]["preassigned_partition"] = json!("train_reference");
        sources[2]["enrollment"] = json!("withheld");
        sources[3]["preassigned_partition"] = json!("calibration_reference");
        let short = "*** START OF SYNTHETIC ***\nToo short.\n*** END OF SYNTHETIC ***\n";
        fs::write(sources[6]["text_path"].as_str().unwrap(), short).unwrap();
        sources[6]["text_sha256"] = json!(sha(short.as_bytes()));
        sources[6]["text_bytes"] = json!(short.len());
        let input = manifest(dir.path(), &sources);
        let out = dir.path().join("output");
        prepare_fresh(&input, &out, false).unwrap();
        let humans = rows(&out.join("humans.jsonl"));
        let refs = rows(&out.join("references.jsonl"));
        assert_eq!(humans.len(), 4);
        assert_eq!(refs.len(), 6);
        assert_eq!(
            refs.iter()
                .map(|r| r["frozen_split"].as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            ["train", "development", "calibration"]
                .into_iter()
                .collect()
        );
        assert!(
            humans
                .iter()
                .all(|r| r["purpose"] == "query" || r["frozen_split"] == "train")
        );
        for row in humans.iter().chain(&refs) {
            let pg = row["source"]["record"]["pg_id"].as_u64().unwrap();
            let text = synthetic_text(pg);
            let span = row["source"]["byte_span"].as_array().unwrap();
            let slice =
                &text[span[0].as_u64().unwrap() as usize..span[1].as_u64().unwrap() as usize];
            assert_eq!(row["text"], slice);
            assert_eq!(row["text_sha256"], sha(slice.as_bytes()));
            assert_eq!(row["source"]["record"], sources[(pg - 1) as usize]);
            assert_eq!(row["rights"], sources[(pg - 1) as usize]["rights"]);
            assert_eq!(row["exposed_to_grammar"], true);
            assert!(
                row["grammar_exposure"]
                    .as_str()
                    .unwrap()
                    .contains("unknown")
            );
            assert_eq!(row["documented_human_only"], false);
            assert_eq!(
                row["share_evidence"]["not_documented_contemporary_workflow"],
                true
            );
        }
        let report = output_manifest(&out);
        assert_eq!(report["all_ids"].as_array().unwrap().len(), 14);
        assert_eq!(report["query_ids"].as_array().unwrap().len(), 6);
        assert_eq!(report["test_ids"].as_array().unwrap().len(), 4);
        assert_eq!(
            report["test_reference_ids"],
            json!(["author-pg6-p20", "author-pg6-p60"])
        );
        assert!(
            report["query_ids"]
                .as_array()
                .unwrap()
                .contains(&json!("author-pg7-p20"))
        );
        assert_eq!(report["failures"].as_array().unwrap().len(), 2);
        assert_eq!(report["test_text_opened"], false);
        assert_eq!(
            report["records_sha256"],
            sha(&fs::read(out.join("humans.jsonl")).unwrap())
        );
        assert_eq!(
            report["references_sha256"],
            sha(&fs::read(out.join("references.jsonl")).unwrap())
        );
    }

    #[test]
    fn test_mode_opens_only_test_queries_and_independent_test_references() {
        let dir = TempDir::new().unwrap();
        let sources = vec![
            source(dir.path(), 1, "train", false, false),
            source(dir.path(), 2, "development_reference", true, false),
            source(dir.path(), 3, "test", false, true),
            source(dir.path(), 4, "test_reference", true, true),
        ];
        let input = manifest(dir.path(), &sources);
        let out = dir.path().join("test-output");
        prepare_fresh(&input, &out, true).unwrap();
        let humans = rows(&out.join("humans.jsonl"));
        let refs = rows(&out.join("references.jsonl"));
        assert_eq!(humans.len(), 2);
        assert_eq!(refs.len(), 2);
        assert!(
            humans
                .iter()
                .all(|r| r["purpose"] == "query" && r["frozen_split"] == "test")
        );
        assert!(
            refs.iter()
                .all(|r| r["purpose"] == "reference" && r["frozen_split"] == "test")
        );
        let report = output_manifest(&out);
        assert_eq!(report["non_test_ids"], json!([]));
        assert_eq!(
            report["non_test_query_ids"],
            json!(["author-pg1-p20", "author-pg1-p60"])
        );
        assert_eq!(report["test_text_opened"], true);
    }

    #[test]
    fn metadata_only_test_bridge_reserves_ids_without_paths_hashes_or_rights() {
        let dir = TempDir::new().unwrap();
        let acquired = source(dir.path(), 1, "train_reference", true, true);
        let reserved = json!([
            {"pg_id":"2","author_key":"synthetic_test","source_group":"synthetic_work_2","split":"test","purpose":"query"},
            {"pg_id":3,"author_key":"synthetic_test","source_group":"synthetic_work_3","split":"test",
                "preassigned_partition":"test_reference","role":"reference","purpose":"reference"}
        ]);
        let manifest = json!({"schema":"slop_ninja.author_sources.v1","sources":[acquired],
            "reserved_sources":reserved,"complete":true,"source_count":1,
            "expected_non_test_sources":1,"excluded_test_sources":2});
        let input = dir.path().join("sources.json");
        write(&input, &manifest).unwrap();
        let out = dir.path().join("prepared");
        prepare_fresh(&input, &out, false).unwrap();
        let result = output_manifest(&out);
        assert_eq!(
            result["test_ids"],
            json!([
                "author-pg2-p20",
                "author-pg2-p60",
                "author-pg3-p20",
                "author-pg3-p60"
            ])
        );
        assert_eq!(
            result["test_query_ids"],
            json!(["author-pg2-p20", "author-pg2-p60"])
        );
        assert_eq!(result["test_text_opened"], false);
        assert!(
            prepare_fresh(&input, &dir.path().join("test"), true)
                .unwrap_err()
                .to_string()
                .contains("metadata-only")
        );
        let mut wrong = manifest.clone();
        wrong["reserved_sources"] = json!([]);
        assert!(plan_sources(&wrong).is_err());
        let mut wrong = manifest.clone();
        wrong["complete"] = json!(false);
        assert!(plan_sources(&wrong).is_err());
        let mut wrong = manifest;
        wrong["reserved_sources"][0]["split"] = json!("development");
        assert!(plan_sources(&wrong).is_err());
    }

    #[test]
    fn legacy_output_contract_stays_unchanged() {
        let dir = TempDir::new().unwrap();
        let sources = vec![
            source(dir.path(), 1, "train_reference", true, true),
            source(dir.path(), 2, "test", false, false),
        ];
        let input = manifest(dir.path(), &sources);
        let out = dir.path().join("legacy-output");
        prepare_legacy(&input, &out, false).unwrap();
        let humans = rows(&out.join("humans.jsonl"));
        assert_eq!(humans.len(), 2);
        assert!(
            humans
                .iter()
                .all(|r| r["frozen_split"] == "train" && r["purpose"] == "reference")
        );
        assert!(!out.join("references.jsonl").exists());
        let report = output_manifest(&out);
        assert_eq!(report["schema"], "slop_ninja_author_queries_v1");
        assert_eq!(
            report["non_test_ids"],
            json!(["author-pg1-p20", "author-pg1-p60"])
        );
        assert_eq!(
            report["test_ids"],
            json!(["author-pg2-p20", "author-pg2-p60"])
        );
    }

    #[test]
    fn rejects_metadata_conflicts_and_reference_query_overlap_before_reads() {
        let dir = TempDir::new().unwrap();
        let query = source(dir.path(), 1, "train", false, false);
        let reference = source(dir.path(), 2, "test_reference", true, false);
        for field in ["pg_id", "source_group", "text_sha256"] {
            let mut duplicate = reference.clone();
            duplicate[field] = query[field].clone();
            let manifest = json!({"sources":[query, duplicate]});
            assert!(plan_sources(&manifest).is_err(), "{field}");
        }
        let mut wrong = reference.clone();
        wrong["split"] = json!("train");
        wrong["preassigned_partition"] = json!("test_reference");
        assert!(plan_sources(&json!({"sources":[wrong]})).is_err());
        let mut wrong = reference.clone();
        wrong["purpose"] = json!("query");
        assert!(plan_sources(&json!({"sources":[wrong]})).is_err());
        let mut wrong = reference;
        wrong["split"] = json!("unknown");
        assert!(plan_sources(&json!({"sources":[wrong]})).is_err());
    }

    #[test]
    fn checks_source_integrity_rights_and_relative_paths() {
        let dir = TempDir::new().unwrap();
        let mut record = source(dir.path(), 1, "train", false, true);
        record.as_object_mut().unwrap().remove("text_path");
        record["text_relative_path"] = json!("pg1.txt");
        let input = manifest(dir.path(), &[record.clone()]);
        prepare_fresh(&input, &dir.path().join("good"), false).unwrap();
        fs::write(dir.path().join("pg1.txt"), "changed bytes").unwrap();
        let failed = dir.path().join("failed");
        assert!(
            prepare_fresh(&input, &failed, false)
                .unwrap_err()
                .to_string()
                .contains("source changed")
        );
        assert!(!failed.exists());
        record["rights"]["external_evaluation"] = json!(false);
        assert!(plan_sources(&json!({"sources":[record]})).is_err());
    }

    #[test]
    fn only_source_bound_audits_override_conservative_exposure() {
        let dir = TempDir::new().unwrap();
        let mut record = source(dir.path(), 1, "train", false, false);
        assert!(fresh_exposure(&record).unwrap().0);
        record["grammar_exposure_audit"] = json!({"status":"audited","text_sha256":record["text_sha256"],
            "evidence_sha256":"a".repeat(64),"exposed_to_grammar":false,"grammar_exposure":"audited_unexposed_for_frozen_grammar"});
        assert!(!fresh_exposure(&record).unwrap().0);
        record["grammar_exposure_audit"]["text_sha256"] = json!("b".repeat(64));
        assert!(fresh_exposure(&record).is_err());
    }

    #[test]
    fn flags_are_opt_in_and_reject_duplicates() {
        let args = |flags: &[&str]| flags.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_flags(&args(&["input", "output"])).unwrap(),
            (false, false)
        );
        assert_eq!(
            parse_flags(&args(&["input", "output", "--test"])).unwrap(),
            (true, false)
        );
        for flags in [
            ["--test", "--fresh-balanced"],
            ["--fresh-balanced", "--test"],
        ] {
            assert_eq!(
                parse_flags(&args(&["input", "output", flags[0], flags[1]])).unwrap(),
                (true, true)
            );
        }
        assert!(parse_flags(&args(&["input", "output", "--test", "--test"])).is_err());
        assert!(parse_flags(&args(&["input", "output", "--typo"])).is_err());
    }
}
