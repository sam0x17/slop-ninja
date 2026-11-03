//! Small, reproducible HTTP samples of public corpora; no model or detector calls.

use anyhow::{Context, Result, ensure};
use reqwest::{Url, blocking::Client};
use serde_json::{Value, json};
use std::{collections::HashSet, fs, io::Write, path::Path, time::Duration};

pub const FINEWEB_REVISION: &str = "9bb295ddab0e05d785b879661af7260fed5140fc";
const DATASET: &str = "HuggingFaceFW/fineweb";
const CONFIG: &str = "CC-MAIN-2021-43";
const CORPUS: &str = "fineweb-2021-43-pilot";
const LICENSE: &str = "https://opendatacommons.org/licenses/by/1-0/";

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing nonempty {key}"))
}

fn row_document(wrapper: &Value, expected_index: usize, page: &Value) -> Result<Value> {
    ensure!(
        wrapper["row_idx"].as_u64() == Some(expected_index as u64),
        "unexpected row order"
    );
    ensure!(
        wrapper["truncated_cells"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "truncated or unknown row completeness"
    );
    let row = &wrapper["row"];
    let text = string(row, "text")?;
    let id = string(row, "id")?;
    ensure!(
        string(row, "dump")? == CONFIG,
        "row comes from another crawl"
    );
    ensure!(string(row, "language")? == "en", "non-English row");
    if let Some(score) = row.get("language_score") {
        ensure!(
            score
                .as_f64()
                .is_some_and(|score| score.is_finite() && (0.65..=1.0).contains(&score)),
            "invalid or low English language score"
        );
    }
    let date = string(row, "date")?;
    let parsed_date = chrono::DateTime::parse_from_rfc3339(date).context("invalid crawl date")?;
    let cutoff = chrono::DateTime::parse_from_rfc3339("2022-01-01T00:00:00Z")?;
    ensure!(parsed_date < cutoff, "crawl date is not before 2022");
    let url = Url::parse(string(row, "url")?).context("invalid source URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "source URL must use HTTP(S)"
    );
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .context("source URL has no host")?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    let warc = string(row, "file_path")?;
    ensure!(
        warc.starts_with(&format!("s3://commoncrawl/crawl-data/{CONFIG}/")),
        "WARC path does not match crawl"
    );
    Ok(json!({
        "text": text, "corpus": CORPUS, "source_kind": "human",
        "domain": "web-mixed", "register": "web-prose",
        "group_id": format!("site:{host}"), "split": "train",
        "metadata": {
            "source": row,
            "source_record_id": id,
            "source_url": row["url"],
            "source_text_sha256": crate::util::digest(text),
            "dataset": DATASET, "dataset_config": CONFIG, "dataset_split": "train",
            "dataset_revision": FINEWEB_REVISION, "dataset_row_index": expected_index,
            "retrieval": page,
            "authorship_basis": "pre2022 crawl inferred, not verified",
            "date_semantics": "Common Crawl capture timestamp; original composition date unknown",
            "sampling": "deterministic first N rows, exact-text/ID deduplicated; not a representative random sample",
            "grouping": "source URL hostname, lowercase with leading www. removed; not registrable-domain grouping",
            "quality": "web extraction, author identity and continuous prose not individually verified",
            "license": {"database": "ODC-By-1.0", "database_license_url": LICENSE,
                "dataset_card_url": "https://huggingface.co/datasets/HuggingFaceFW/fineweb",
                "common_crawl_terms_url": "https://commoncrawl.org/terms-of-use",
                "underlying_content_rights": "unknown; database license does not replace source-content rights",
                "attribution": "FineWeb, Guilherme Penedo et al., 2024, HuggingFaceFW/fineweb; source URLs retained"}
        }
    }))
}

fn validate_page(
    bytes: &[u8],
    provenance: &Value,
    url: &str,
    offset: usize,
    length: usize,
) -> Result<Value> {
    ensure!(
        provenance["request_url"] == url,
        "cached request URL differs"
    );
    ensure!(
        provenance["dataset"] == DATASET
            && provenance["config"] == CONFIG
            && provenance["split"] == "train",
        "cached dataset selection differs"
    );
    ensure!(
        provenance["revision"] == FINEWEB_REVISION,
        "dataset revision drift; collect a new version explicitly"
    );
    let body = std::str::from_utf8(bytes).context("dataset response is not UTF-8")?;
    ensure!(
        provenance["response_sha256"] == crate::util::digest(body),
        "cached response hash mismatch"
    );
    let parsed: Value = serde_json::from_slice(bytes)?;
    ensure!(parsed["partial"] == false, "partial dataset response");
    let rows = parsed["rows"].as_array().context("missing response rows")?;
    ensure!(
        rows.len() == length,
        "response row count differs from request"
    );
    ensure!(
        parsed["num_rows_total"]
            .as_u64()
            .is_some_and(|n| n >= (offset + length) as u64),
        "invalid total row count"
    );
    for (index, row) in rows.iter().enumerate() {
        row_document(row, offset + index, provenance)?;
    }
    Ok(parsed)
}

/// Read the first `rows` source rows (1..=1000) in pages of at most 100.
/// Keep unique IDs/texts; record excluded duplicates without replacing them.
/// Every response must match the pinned revision returned by the dataset server.
pub fn fineweb_sample(out: &Path, rows: usize) -> Result<Value> {
    ensure!(
        (1..=1000).contains(&rows),
        "request between 1 and 1000 pilot rows"
    );
    fs::create_dir_all(out.join("raw"))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .user_agent("unslop-research/0.2")
        .build()?;
    let mut documents = Vec::new();
    let mut pages = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_texts = HashSet::new();
    let mut duplicate_rows = Vec::new();
    for offset in (0..rows).step_by(100) {
        let length = (rows - offset).min(100);
        let mut url = Url::parse("https://datasets-server.huggingface.co/rows")?;
        url.query_pairs_mut()
            .append_pair("dataset", DATASET)
            .append_pair("config", CONFIG)
            .append_pair("split", "train")
            .append_pair("offset", &offset.to_string())
            .append_pair("length", &length.to_string());
        let stem = format!("fineweb-2021-43-offset-{offset:06}-length-{length:03}");
        let raw_path = out.join("raw").join(format!("{stem}.json"));
        let meta_path = out.join("raw").join(format!("{stem}.provenance.json"));
        ensure!(
            raw_path.exists() == meta_path.exists(),
            "incomplete cached page; inspect {} and {}",
            raw_path.display(),
            meta_path.display()
        );
        let (bytes, provenance) = if raw_path.exists() {
            (
                fs::read(&raw_path)?,
                serde_json::from_slice(&fs::read(&meta_path)?)?,
            )
        } else {
            let response = client.get(url.clone()).send()?.error_for_status()?;
            let revision = response
                .headers()
                .get("x-revision")
                .context("dataset server omitted x-revision")?
                .to_str()?
                .to_owned();
            ensure!(
                revision == FINEWEB_REVISION,
                "dataset revision drift: returned {revision}"
            );
            let status = response.status().as_u16();
            let bytes = response.bytes()?.to_vec();
            let provenance = json!({
                "request_url": url.as_str(), "retrieved_at": chrono::Utc::now().to_rfc3339(),
                "dataset": DATASET, "config": CONFIG, "split": "train", "revision": revision,
                "offset": offset, "length": length, "http_status": status,
                "response_sha256": crate::util::digest(std::str::from_utf8(&bytes)?),
                "response_bytes": bytes.len()
            });
            validate_page(&bytes, &provenance, url.as_str(), offset, length)?;
            let mut raw = tempfile::NamedTempFile::new_in(out.join("raw"))?;
            raw.write_all(&bytes)?;
            raw.as_file().sync_all()?;
            raw.persist(&raw_path)?;
            crate::util::write_json(&meta_path, &provenance)?;
            (bytes, provenance)
        };
        let page = validate_page(&bytes, &provenance, url.as_str(), offset, length)?;
        for (index, row) in page["rows"].as_array().unwrap().iter().enumerate() {
            let document = row_document(row, offset + index, &provenance)?;
            let id = string(&document["metadata"], "source_record_id")?.to_owned();
            let text_hash = string(&document["metadata"], "source_text_sha256")?.to_owned();
            if seen_ids.contains(&id) || seen_texts.contains(&text_hash) {
                duplicate_rows.push(
                    json!({"row_index": offset + index, "source_id": id, "text_sha256": text_hash}),
                );
                continue;
            }
            seen_ids.insert(id);
            seen_texts.insert(text_hash);
            documents.push(document);
        }
        pages.push(provenance);
    }
    let output = out.join("human.jsonl");
    let mut temp = tempfile::NamedTempFile::new_in(out)?;
    for document in &documents {
        serde_json::to_writer(&mut temp, document)?;
        temp.write_all(b"\n")?;
    }
    temp.as_file().sync_all()?;
    temp.persist(&output)?;
    let groups: HashSet<_> = documents
        .iter()
        .map(|document| document["group_id"].as_str().unwrap())
        .collect();
    let words: Vec<_> = documents
        .iter()
        .map(|document| {
            document["text"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .count()
        })
        .collect();
    let summary = json!({
        "corpus": CORPUS, "dataset": DATASET, "config": CONFIG, "revision": FINEWEB_REVISION,
        "requested_source_rows": rows, "accepted_documents": documents.len(), "source_host_groups": groups.len(),
        "duplicate_rows": duplicate_rows, "whitespace_words": words.iter().sum::<usize>(),
        "documents_at_least_50_words": words.iter().filter(|&&n| n >= 50).count(),
        "sampling": "deterministic first N rows; exact-text/ID deduplication; all train",
        "authorship_basis": "pre2022 crawl inferred, not verified",
        "database_license": "ODC-By-1.0", "database_license_url": LICENSE,
        "source_content_rights": "unknown", "pages": pages,
        "output": output, "output_sha256": crate::util::digest(&fs::read_to_string(&output)?)
    });
    crate::util::write_json(&out.join("manifest.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({"row_idx": 0, "truncated_cells": [], "row": {
            "text": "  Preserve the original text.\n", "id": "urn:fixture:0", "dump": CONFIG,
            "url": "https://www.example.com/page", "date": "2021-10-15T20:13:08Z",
            "file_path": format!("s3://commoncrawl/crawl-data/{CONFIG}/segment/file.warc.gz"),
            "language": "en", "language_score": 0.95
        }})
    }

    #[test]
    fn preserves_source_bytes_and_rights_provenance() {
        let doc = row_document(&fixture(), 0, &json!({})).unwrap();
        assert_eq!(doc["text"], "  Preserve the original text.\n");
        assert_eq!(doc["group_id"], "site:example.com");
        assert_eq!(doc["metadata"]["license"]["database"], "ODC-By-1.0");
        assert_eq!(
            doc["metadata"]["authorship_basis"],
            "pre2022 crawl inferred, not verified"
        );
    }

    #[test]
    fn rejects_late_dates_wrong_crawl_low_language_scores_and_truncation() {
        for (field, value) in [
            ("date", json!("2022-01-01T00:00:00Z")),
            ("date", json!("invalid")),
            ("dump", json!("CC-MAIN-2024-10")),
            ("language", json!("fr")),
            ("language_score", json!(0.3)),
            ("text", json!("")),
            ("url", json!("https:///")),
        ] {
            let mut row = fixture();
            row["row"][field] = value;
            assert!(
                row_document(&row, 0, &json!({})).is_err(),
                "accepted invalid {field}"
            );
        }
        let mut row = fixture();
        row["truncated_cells"] = json!(["text"]);
        assert!(row_document(&row, 0, &json!({})).is_err());
    }

    #[test]
    fn rejects_cached_hash_or_revision_drift() {
        let bytes = serde_json::to_vec(
            &json!({"partial": false, "num_rows_total": 1, "rows": [fixture()]}),
        )
        .unwrap();
        let mut provenance = json!({"request_url": "url", "dataset": DATASET, "config": CONFIG,
            "split": "train", "revision": FINEWEB_REVISION,
            "response_sha256": crate::util::digest(std::str::from_utf8(&bytes).unwrap())});
        assert!(validate_page(&bytes, &provenance, "url", 0, 1).is_ok());
        provenance["revision"] = json!("changed");
        assert!(validate_page(&bytes, &provenance, "url", 0, 1).is_err());
        provenance["revision"] = json!(FINEWEB_REVISION);
        provenance["response_sha256"] = json!("changed");
        assert!(validate_page(&bytes, &provenance, "url", 0, 1).is_err());
    }
}
