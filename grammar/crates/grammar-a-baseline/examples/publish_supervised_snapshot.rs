//! Curate an already-exposed supervised cohort from explicitly authorized sources.
//! Historical records are bound to actual published JSONL bytes. Author-work
//! rights remain the recorded US public-domain determination, not a new grant.
//! Unknown publication fields fail closed; only named private locator fields
//! are removed. No private source file, profile or invocation log is opened.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};

const LINE_HASH_BASIS: &str =
    "exact original JSONL line bytes including terminating newline when present";
const LOCAL_GENERATOR: &str =
    "Qwen3.6-35B-A3B:649d7508507b84638732c4f52c24c8b15843c6dca2f3ff793ae07c14a67ebbb3";
const SPAN_SCHEMA: &str = "slop_ninja_realized_span_share_v1";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
fn digest(v: &Value, k: &str) -> Result<()> {
    let value = s(v, k)?;
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid SHA256"
    );
    Ok(())
}
fn take(v: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|k| v.get(*k).map(|value| ((*k).into(), value.clone())))
            .collect(),
    )
}
// Unknown keys are rejected instead of silently dropping a future license term,
// supervision field, or provenance claim. Never echo their values in errors.
fn fields(v: &Value, keys: &[&str], omitted: &[&str]) -> Result<Value> {
    let object = v.as_object().context("expected publication object")?;
    ensure!(
        object
            .keys()
            .all(|k| keys.contains(&k.as_str()) || omitted.contains(&k.as_str())),
        "unreviewed publication field"
    );
    Ok(take(v, keys))
}
fn flat(v: &Value, keys: &[&str], omitted: &[&str]) -> Result<Value> {
    let out = fields(v, keys, omitted)?;
    ensure!(
        out.as_object().unwrap().values().all(|v| {
            if let Some(items) = v.as_array() {
                items.iter().all(|x| !x.is_object() && !x.is_array())
            } else {
                !v.is_object()
            }
        }),
        "unexpected nested publication metadata"
    );
    Ok(out)
}
fn put(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

fn public_rights(rights: &Value) -> Result<Value> {
    let mut out = flat(
        rights,
        &[
            "basis",
            "catalog_statement",
            "commercial_training",
            "conditions",
            "external_evaluation",
            "jurisdiction",
            "license_url",
            "permission_url",
            "license",
            "attribution",
            "evidence_sha256",
            "evidence_url",
            "model_release",
            "redistribute_text",
            "embedded_notice_review",
        ],
        &["evidence_paths", "share_alike"],
    )?;
    if let Some(sa) = rights.get("share_alike") {
        out["share_alike"] = if sa.is_null() {
            Value::Null
        } else {
            flat(
                sa,
                &[
                    "policy",
                    "source_title",
                    "article_url",
                    "history_url",
                    "source_license",
                    "source_license_url",
                    "review_sha256",
                    "notices",
                    "changes",
                ],
                &[],
            )?
        };
    }
    Ok(out)
}
fn public_receipt(v: &Value) -> Result<Value> {
    flat(
        v,
        &[
            "sha256",
            "bytes",
            "requested_url",
            "effective_url",
            "download_started_utc",
            "http_date",
            "http_last_modified",
            "headers_sha256",
            "transfer_receipt_sha256",
        ],
        &["path", "headers_path", "transfer_receipt_path"],
    )
}
fn public_profile(v: &Value) -> Result<Value> {
    let mut out = fields(v, &["selection", "profile", "assignment"], &[])?;
    out["selection"] = flat(
        &v["selection"],
        &[
            "set_id",
            "catalog_sha256",
            "guard",
            "guard_sha256",
            "assignment_method",
            "selected_profile_ids",
        ],
        &[],
    )?;
    out["assignment"] = flat(
        &v["assignment"],
        &[
            "input_sha256",
            "source_group_sha256",
            "split",
            "collections",
            "family_rank",
            "bucket_family_count",
            "bucket_offset",
        ],
        &[],
    )?;
    out["profile"] = flat(
        &v["profile"],
        &["id", "instructions", "instructions_sha256"],
        &["source"],
    )?;
    if let Some(source) = v["profile"].get("source") {
        out["profile"]["source"] = if source.is_null() {
            Value::Null
        } else {
            flat(
                source,
                &[
                    "url",
                    "revision",
                    "upstream_sha256",
                    "license",
                    "license_sha256",
                    "scope",
                ],
                &[],
            )?
        };
    }
    Ok(out)
}
fn public_generation(g: &Value, historical: bool) -> Result<Value> {
    if g.is_null() {
        return Ok(Value::Null);
    }
    if historical {
        let mut out = flat(
            g,
            &[
                "model_id",
                "response_model",
                "model_revision",
                "model_license",
                "model_license_url",
                "model_license_sha256",
                "quantization",
                "runtime",
                "prompt",
                "request_sha256",
                "response_sha256",
                "operation",
                "created_at",
                "temperature",
                "seed",
                "max_tokens",
                "finish_reason",
            ],
            &["prompt_profile"],
        )?;
        if let Some(profile) = g.get("prompt_profile") {
            out["prompt_profile"] = if profile.is_null() {
                Value::Null
            } else {
                public_profile(profile)?
            };
        }
        return Ok(out);
    }
    ensure!(
        g["binding"]["generator"] == LOCAL_GENERATOR
            && g["generator"] == LOCAL_GENERATOR
            && g["binding"]["schema"] == "slop_ninja_controlled_generation_v1",
        "unreviewed local generator"
    );
    let mut out = flat(
        g,
        &[
            "attempt",
            "generator",
            "mode",
            "response_format_policy",
            "response_sha256",
        ],
        &["binding", "request", "response_path"],
    )?;
    out["binding"] = flat(
        &g["binding"],
        &[
            "schema",
            "generator",
            "repo",
            "revision",
            "runtime",
            "share_word_policy",
            "requested_span_percentages",
            "seed",
            "temperature",
            "top_p",
            "max_tokens",
            "thinking",
            "input_schema",
            "test_opened",
            "input_sha256",
            "failures",
        ],
        &["endpoint"],
    )?;
    let request = &g["request"];
    out["request"] = flat(
        request,
        &[
            "model",
            "temperature",
            "top_p",
            "seed",
            "max_tokens",
            "stream",
            "reasoning_format",
        ],
        &["messages", "chat_template_kwargs"],
    )?;
    let messages = request["messages"]
        .as_array()
        .context("generation messages")?;
    ensure!(!messages.is_empty(), "empty generation request");
    out["request"]["messages"] = Value::Array(
        messages
            .iter()
            .map(|m| {
                let out = flat(m, &["role", "content"], &[])?;
                ensure!(
                    m["role"] == "user" && m["content"].is_string(),
                    "unreviewed generation message"
                );
                Ok(out)
            })
            .collect::<Result<_>>()?,
    );
    out["request"]["chat_template_kwargs"] =
        flat(&request["chat_template_kwargs"], &["enable_thinking"], &[])?;
    digest(g, "response_sha256")?;
    digest(&g["binding"], "input_sha256")?;
    out["raw_invocation_retained_privately"] = true.into();
    Ok(out)
}

#[derive(Default)]
struct HistoricalFile {
    sha256: String,
    // Hashes keep the cache compact; no second copy of corpus text is retained.
    records: BTreeMap<String, (String, String)>,
}
type HistoricalCache = BTreeMap<String, HistoricalFile>;
fn historical_path(repo: &Path, path: &str) -> Result<std::path::PathBuf> {
    ensure!(
        path.starts_with("datasets/detector-v6/")
            || path.starts_with("datasets/detector-origin-v1/")
            || path.starts_with("datasets/detector-revisions-v1/"),
        "unapproved historical corpus"
    );
    ensure!(
        path.ends_with(".jsonl")
            && path.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))),
        "unsafe source path"
    );
    let mut resolved = repo.to_path_buf();
    for component in Path::new(path).components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "unsafe source component"
        );
        resolved.push(component.as_os_str());
        ensure!(
            !fs::symlink_metadata(&resolved)?.file_type().is_symlink(),
            "symlink in historical source path"
        );
    }
    ensure!(resolved.is_file(), "historical source is not a file");
    Ok(resolved)
}
fn historical_record<'a>(
    row: &'a Value,
    repo: &Path,
    verified: &mut HistoricalCache,
) -> Result<&'a Value> {
    let source = &row["source"];
    let path = s(source, "path")?;
    let resolved = historical_path(repo, path)?;
    if !verified.contains_key(path) {
        let bytes = fs::read(resolved)?;
        let mut file = HistoricalFile {
            sha256: sha(&bytes),
            ..Default::default()
        };
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            let record: Value =
                serde_json::from_slice(line).context("invalid published JSONL record")?;
            let id = s(&record, "id")?;
            ensure!(
                file.records
                    .insert(id.into(), (sha(line), sha(&serde_json::to_vec(&record)?)))
                    .is_none(),
                "duplicate published source id"
            );
        }
        verified.insert(path.into(), file);
    }
    let file = &verified[path];
    ensure!(
        source["file_sha256"] == file.sha256,
        "published historical source differs"
    );
    ensure!(
        source["record_checksum_basis"] == LINE_HASH_BASIS,
        "unsupported record checksum basis"
    );
    let (line_hash, value_hash) = file
        .records
        .get(s(source, "record_id")?)
        .context("record absent from published source")?;
    ensure!(
        source["record_sha256"] == *line_hash,
        "published record line hash differs"
    );
    let original = &source["record"];
    ensure!(
        sha(&serde_json::to_vec(original)?) == *value_hash,
        "nested record differs from actual published record"
    );
    ensure!(
        original["schema"] == "slop-ninja-origin-record-v1"
            && original["id"] == source["record_id"],
        "historical source identity mismatch"
    );
    for key in [
        "text",
        "text_sha256",
        "source_group",
        "origin",
        "evidence",
        "rights",
        "generation",
    ] {
        ensure!(row[key] == original[key], "historical record field changed");
    }
    ensure!(
        row["frozen_split"] == original["split"]
            && source["rights"] == original["rights"]
            && source["original_parent_id"] == original["parent_id"]
            && row["parent_source_id"] == original["id"],
        "historical provenance mismatch"
    );
    ensure!(
        original["rights"]["redistribute_text"] == true
            && original["rights"]["commercial_training"] == true,
        "historical publication permission absent"
    );
    Ok(original)
}

fn span(v: &Value, text: &str) -> Result<(usize, usize)> {
    let pair = v.as_array().context("span must be a pair")?;
    ensure!(pair.len() == 2, "span must be a pair");
    let start = usize::try_from(pair[0].as_u64().context("span start")?)?;
    let end = usize::try_from(pair[1].as_u64().context("span end")?)?;
    ensure!(
        start <= end
            && end <= text.len()
            && text.is_char_boundary(start)
            && text.is_char_boundary(end),
        "invalid UTF-8 byte span"
    );
    Ok((start, end))
}
fn validate_targets(row: &Value) -> Result<()> {
    let origin = s(row, "origin")?;
    ensure!(
        matches!(origin, "human_only" | "model_only" | "mixed"),
        "unsupported origin"
    );
    ensure!(
        row["human_role_present"] == (origin != "model_only")
            && row["ai_role_present"] == (origin != "human_only"),
        "role involvement differs from origin"
    );
    for key in ["human_identity", "ai_identity"] {
        let value = row.get(key).context("missing identity target")?;
        ensure!(
            value.is_null() || value.as_str().is_some_and(|s| !s.is_empty()),
            "invalid identity target"
        );
    }
    let target = row
        .get("share_target")
        .context("missing explicit share target")?;
    let share = if target.is_null() {
        None
    } else {
        let n = target
            .as_f64()
            .context("share target must be numeric or null")?;
        ensure!(
            (0.0..=1.0).contains(&n)
                && (origin != "human_only" || n == 0.0)
                && (origin != "model_only" || n == 1.0),
            "invalid share target"
        );
        Some(n)
    };
    let e = &row["share_evidence"];
    if e["schema"] != SPAN_SCHEMA {
        flat(
            e,
            &[
                "basis",
                "not_documented_contemporary_workflow",
                "source_conditioned",
                "not_measured_word_spans",
                "fractional_share_supervision_masked",
            ],
            &[],
        )?;
        ensure!(
            !s(e, "basis")?.is_empty() && (origin != "mixed" || share.is_none()),
            "mixed share lacks measured spans"
        );
        return Ok(());
    }
    flat(
        e,
        &[
            "schema",
            "source_text",
            "source_sha256",
            "source_byte_span",
            "replacement",
            "final_byte_span",
            "final_sha256",
            "replacement_words",
            "final_words",
            "word_policy",
            "realized_share",
            "requested_percent_not_target",
            "full_rewrite_is_mixed_history",
            "mode",
        ],
        &[],
    )?;
    ensure!(
        e["word_policy"] == "unicode_split_whitespace_v1",
        "unsupported share word policy"
    );
    let source = s(e, "source_text")?;
    let replacement = s(e, "replacement")?;
    let text = s(row, "text")?;
    ensure!(
        !replacement.is_empty() && replacement.trim() == replacement,
        "replacement layout differs"
    );
    ensure!(
        e["source_sha256"] == sha(source.as_bytes()) && e["final_sha256"] == row["text_sha256"],
        "share hash mismatch"
    );
    let (start, end) = span(&e["source_byte_span"], source)?;
    let final_span = span(&e["final_byte_span"], text)?;
    let mode = s(&row["generation"], "mode")?;
    ensure!(
        e.get("mode").is_none_or(|m| m == mode),
        "conflicting share generation modes"
    );
    match mode {
        "from_scratch" => ensure!(
            origin == "model_only"
                && (start, end) == (0, 0)
                && text == replacement
                && final_span == (0, text.len()),
            "from-scratch assembly differs"
        ),
        "controlled_rewrite" => {
            ensure!(
                origin == "mixed" && start < end,
                "rewrite history/span mismatch"
            );
            ensure!(
                (start == 0 || source[..start].ends_with(char::is_whitespace))
                    && (end == source.len() || source[end..].starts_with(char::is_whitespace)),
                "rewrite splits a word"
            );
            ensure!(
                text == format!("{}{}{}", &source[..start], replacement, &source[end..])
                    && final_span == (start, start + replacement.len()),
                "exact rewrite assembly differs"
            );
        }
        _ => anyhow::bail!("unsupported share generation mode"),
    }
    let numerator = replacement.split_whitespace().count();
    let denominator = text.split_whitespace().count();
    ensure!(
        numerator > 0
            && denominator >= numerator
            && e["replacement_words"].as_u64() == Some(numerator as u64)
            && e["final_words"].as_u64() == Some(denominator as u64),
        "share word counts differ"
    );
    let expected = numerator as f64 / denominator as f64;
    ensure!(
        e["realized_share"]
            .as_f64()
            .is_some_and(|x| (x - expected).abs() <= 1e-12)
            && share.is_none_or(|x| (x - expected).abs() <= 1e-12),
        "measured share differs"
    );
    Ok(())
}

fn public_author_source(row: &Value) -> Result<Value> {
    let source = &row["source"];
    fields(
        source,
        &[
            "record",
            "byte_span",
            "extraction",
            "source_manifest_sha256",
        ],
        &[],
    )?;
    let original = &source["record"];
    let pg = original["pg_id"]
        .as_u64()
        .or_else(|| original["pg_id"].as_str().and_then(|s| s.parse().ok()))
        .context("PG id")?;
    ensure!(
        pg > 0
            && original["rights"]["catalog_statement"] == "Public domain in the USA."
            && original["rights"]["jurisdiction"] == "US"
            && original["rights"]["commercial_training"] == true
            && original["rights"]["external_evaluation"] == true
            && row["rights"] == original["rights"],
        "unreviewed author source"
    );
    ensure!(
        row["rights"]["redistribute_text"] != false,
        "explicit redistribution denial"
    );
    let primary = format!("https://www.gutenberg.org/ebooks/{pg}");
    ensure!(
        original["primary_page_url"] == primary
            && original["catalog_url"] == format!("{primary}.rdf")
            && s(original, "source_url")?.starts_with("https://www.gutenberg.org/"),
        "author source URL mismatch"
    );
    ensure!(
        row["source_group"] == original["source_group"],
        "author source family mismatch"
    );
    let split = s(original, "split")?;
    ensure!(
        row["frozen_split"]
            == if split == "train_reference" {
                "train"
            } else {
                split
            },
        "author source split mismatch"
    );
    let excerpt = if row["generation"].is_null() {
        ensure!(
            row["origin"] == "human_only",
            "unrecorded author derivative"
        );
        s(row, "text")?
    } else {
        ensure!(
            row["share_evidence"]["schema"] == SPAN_SCHEMA,
            "author derivative lacks measured source"
        );
        s(&row["share_evidence"], "source_text")?
    };
    for text in [excerpt, s(row, "text")?] {
        ensure!(
            !text.to_lowercase().contains("project gutenberg")
                && !text.contains("*** START OF")
                && !text.contains("*** END OF"),
            "PG-branded material requires separate curation"
        );
    }
    let pair = source["byte_span"]
        .as_array()
        .context("author excerpt byte span")?;
    ensure!(pair.len() == 2, "author excerpt span must be a pair");
    let start = pair[0].as_u64().context("author excerpt start")?;
    let end = pair[1].as_u64().context("author excerpt end")?;
    let full_bytes = original["text_bytes"]
        .as_u64()
        .context("author source byte count")?;
    ensure!(
        start < end && end <= full_bytes && end - start == excerpt.len() as u64,
        "author excerpt span length differs"
    );
    digest(source, "source_manifest_sha256")?;
    digest(original, "text_sha256")?;
    ensure!(
        !s(source, "extraction")?.is_empty(),
        "missing extraction policy"
    );
    let mut record = flat(
        original,
        &[
            "pg_id",
            "author",
            "author_key",
            "catalog_author",
            "title",
            "source_url",
            "primary_page_url",
            "catalog_url",
            "source_group",
            "source_group_policy",
            "text_sha256",
            "text_bytes",
            "catalog_extent_bytes",
            "catalog_extent_matches_download",
            "split",
            "role",
            "purpose",
            "enrollment",
            "preassigned_partition",
            "content_processing",
        ],
        &[
            "text_path",
            "text_relative_path",
            "rights",
            "dates",
            "raw_download",
            "catalog_evidence",
            "primary_page_evidence",
        ],
    )?;
    record["dates"] = flat(
        &original["dates"],
        &[
            "pg_ebook_release_date",
            "pg_selected_file_modified",
            "catalog_publication_statement_verbatim",
            "original_composition_date",
            "first_publication_date",
            "note",
        ],
        &[],
    )?;
    for key in ["raw_download", "catalog_evidence", "primary_page_evidence"] {
        record[key] = public_receipt(&original[key])?;
        digest(&record[key], "sha256")?;
    }
    ensure!(
        record["raw_download"]["sha256"] == original["text_sha256"]
            && record["raw_download"]["bytes"] == original["text_bytes"]
            && record["raw_download"]["requested_url"] == original["source_url"],
        "author download provenance differs"
    );
    Ok(
        json!({"kind":"underlying_us_public_domain_work", "record":record, "byte_span":source["byte_span"], "extraction":source["extraction"], "source_manifest_sha256":source["source_manifest_sha256"], "rights":public_rights(&original["rights"])?}),
    )
}

// Defense in depth after structural allowlisting, including free-text prompts,
// notices and exact evidence. Reject rather than redact and break their hashes.
fn public_safety(v: &Value) -> Result<()> {
    match v {
        Value::Object(map) => {
            for value in map.values() {
                public_safety(value)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                public_safety(value)?;
            }
        }
        Value::String(value) => {
            let lower = value.to_lowercase();
            ensure!(
                ![
                    "/home/",
                    "/users/",
                    "/tmp/",
                    "/private/",
                    "/var/",
                    "/root/",
                    "/etc/",
                    "/mnt/",
                    "/opt/",
                    "/run/",
                    "/srv/",
                    "/media/",
                    "/volumes/",
                    "/workspace/",
                    "file://",
                    "data/grammar-private/",
                    "data/author-corpora/",
                    "../",
                    "..\\",
                    "~/",
                    "\\\\",
                    ":\\"
                ]
                .iter()
                .any(|p| lower.contains(p)),
                "private or unsafe filesystem locator in publication"
            );
            ensure!(
                !lower.as_bytes().windows(3).enumerate().any(|(i, w)| {
                    w[0].is_ascii_alphabetic()
                        && w[1] == b':'
                        && w[2] == b'/'
                        && (i == 0 || !lower.as_bytes()[i - 1].is_ascii_alphanumeric())
                }),
                "absolute drive path in publication"
            );
            ensure!(
                ![
                    "-----begin private key",
                    "-----begin rsa private key",
                    "-----begin openssh private key",
                    "api_key=",
                    "api-key=",
                    "apikey=",
                    "?key=",
                    "&key=",
                    "access_token=",
                    "token=",
                    "password=",
                    "secret=",
                    "x-amz-credential=",
                    "x-amz-signature=",
                    "x-goog-signature=",
                    "sk-proj-",
                    "ghp_",
                    "github_pat_"
                ]
                .iter()
                .any(|p| lower.contains(p)),
                "credential marker in publication"
            );
            ensure!(
                !value.starts_with("Bearer ") && !lower.contains("authorization: bearer "),
                "authorization credential in publication"
            );
            for tail in lower.split("://").skip(1) {
                let authority = tail
                    .split(['/', '?', '#', ' ', '\n', '\t', '"', '\''])
                    .next()
                    .unwrap_or("");
                ensure!(
                    !authority.contains('@')
                        && !authority.contains('%')
                        && !authority.contains('\\'),
                    "credential-bearing or escaped URL authority"
                );
            }
        }
        _ => {}
    }
    Ok(())
}
fn project(row: &Value, repo: &Path, verified: &mut HistoricalCache) -> Result<Value> {
    ensure!(
        row["schema"] == "slop_ninja_a_supervised_record_v1",
        "unsupported input schema"
    );
    ensure!(
        row["text_sha256"] == sha(s(row, "text")?.as_bytes()),
        "input text changed"
    );
    validate_targets(row)?;
    let mut out = flat(
        row,
        &[
            "schema",
            "id",
            "text",
            "text_sha256",
            "source_group",
            "frozen_split",
            "origin",
            "evidence",
            "purpose",
            "exposed_to_grammar",
            "grammar_exposure",
            "share_target",
            "human_identity",
            "human_role_present",
            "ai_identity",
            "ai_role_present",
            "parent_id",
            "parent_source_id",
        ],
        &[
            "share_evidence",
            "metadata",
            "source",
            "rights",
            "generation",
        ],
    )?;
    out["share_evidence"] = row["share_evidence"].clone(); // Validated, exact byte/span evidence.
    if let Some(metadata) = row.get("metadata") {
        out["metadata"] = flat(
            metadata,
            &[
                "all_model_contributor_revisions",
                "final_generator_revision",
                "original_record_id",
                "original_parent_id",
                "operation",
                "source_collection",
                "human_identity_supervision_mask",
                "ai_identity_supervision_mask",
                "multi_model_identity_masked",
                "contains_train_withheld_generator",
                "eligible_for_parameter_fit",
                "eligible_for_calibration_temperature_and_rejection_threshold",
                "included_in_primary_evaluation_population",
                "final_test_only",
                "open_set_claim",
                "lineage_policy",
            ],
            &[],
        )?;
    }
    let source = &row["source"];
    let historical = source.get("path").is_some();
    if historical {
        let original = historical_record(row, repo, verified)?;
        out["source"] = flat(
            source,
            &[
                "path",
                "record_id",
                "record_sha256",
                "record_checksum_basis",
                "file_sha256",
                "original_parent_id",
            ],
            &["rights", "record", "retained_original_file"],
        )?;
        out["source"]["kind"] = "existing_published_detector_record".into();
        out["source"]["rights"] = public_rights(&original["rights"])?;
        out["source"]["published_source"] = flat(
            &original["source"],
            &[
                "collection",
                "url",
                "version",
                "published_at",
                "author_ids",
                "raw_sha256",
                "extraction",
            ],
            &["raw_path"],
        )?;
        out["source"]["evidence_notes"] = original["evidence_notes"].clone();
    } else {
        out["source"] = public_author_source(row)?;
    }
    out["rights"] = public_rights(&row["rights"])?;
    if let Some(generation) = row.get("generation") {
        out["generation"] = public_generation(generation, historical)?;
    }
    out["publication"] = json!({"source_row_sha256":sha(&serde_json::to_vec(row)?),
        "source_row_hash_encoding":"serde_json compact Value encoding; not the original line-byte hash",
        "projection":"exact text, targets and span evidence preserved; private locators and raw invocations excluded",
        "benchmark_status":"published_exposed_research_population;not_a_future_hidden_test"});
    public_safety(&out)?;
    Ok(out)
}

fn validate_local_parents(rows: &[Value]) -> Result<()> {
    let by_id = rows
        .iter()
        .map(|row| Ok((s(row, "id")?, row)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for row in rows
        .iter()
        .filter(|r| !r["generation"]["binding"].is_null())
    {
        let parent = by_id
            .get(s(row, "parent_id")?)
            .context("local descendant parent missing from publication")?;
        ensure!(
            parent["origin"] == "human_only" && parent["generation"].is_null(),
            "local descendant parent is not a human source"
        );
        for key in ["source_group", "frozen_split", "source", "rights"] {
            ensure!(
                row[key] == parent[key],
                "local descendant parent provenance mismatch"
            );
        }
        ensure!(
            row["share_evidence"]["source_text"] == parent["text"]
                && row["share_evidence"]["source_sha256"] == parent["text_sha256"],
            "local descendant source differs from published parent"
        );
        ensure!(
            row["generation"]["binding"]["test_opened"] == (row["frozen_split"] == "test"),
            "local generation Test binding differs"
        );
    }
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 6,
        "publish_supervised_snapshot REPO RECORDS MANIFEST OPENED_TEST_RECORDS OPENED_TEST_MANIFEST NEW_OUTPUT"
    );
    let repo = Path::new(&a[0]).canonicalize()?;
    let out = Path::new(&a[5]);
    ensure!(!out.exists(), "new output required");
    ensure!(
        out.file_name().is_some()
            && out.parent().context("output parent")?.canonicalize()? == repo.join("datasets"),
        "output must be a new datasets/ child"
    );
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut seen = BTreeSet::new();
    let mut family_split = BTreeMap::new();
    let mut verified = HistoricalCache::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut sources = Vec::new();
    let mut exclusions = Vec::new();
    let mut rows = Vec::new();
    let mut origin_counts = BTreeMap::<String, usize>::new();
    let mut fractional = 0usize;
    let mut fitting_hash = String::new();
    for (record_name, manifest_name, is_test) in [(&a[1], &a[2], false), (&a[3], &a[4], true)] {
        let raw = fs::read(record_name)?;
        let manifest_bytes = fs::read(manifest_name)?;
        let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
        ensure!(
            manifest["schema"] == "slop_ninja_a_supervised_dataset_v1"
                && manifest["records_sha256"] == sha(&raw),
            "input manifest mismatch"
        );
        ensure!(
            manifest["test_text_opened"] == is_test,
            "input exposure mismatch"
        );
        if is_test {
            ensure!(
                manifest["test_used_for_selection"] == false
                    && manifest["fitting_records_sha256"] == fitting_hash,
                "opened Test must bind this fitting population"
            );
        } else {
            fitting_hash = sha(&raw);
        }
        sources.push(json!({"population":if is_test {"opened_test"} else {"non_test"}, "records_sha256":sha(&raw), "manifest_sha256":sha(&manifest_bytes)}));
        for exclusion in manifest["excluded"]
            .as_array()
            .context("exclusion ledger")?
        {
            let exclusion = flat(
                exclusion,
                &[
                    "id",
                    "source_group",
                    "split",
                    "text_sha256",
                    "normalized_sha256",
                    "reason",
                ],
                &[],
            )?;
            public_safety(&exclusion)?;
            exclusions.push(exclusion);
        }
        let mut population_count = 0;
        for line in raw.split(|b| *b == b'\n').filter(|v| !v.is_empty()) {
            let original: Value = serde_json::from_slice(line)?;
            let row = project(&original, &repo, &mut verified)?;
            let id = s(&row, "id")?;
            ensure!(
                !id.is_empty() && seen.insert(id.to_owned()),
                "empty or duplicate publication id"
            );
            let split = s(&row, "frozen_split")?;
            ensure!(
                matches!(split, "train" | "development" | "calibration" | "test")
                    && (split == "test") == is_test,
                "population split mismatch"
            );
            let group = s(&row, "source_group")?;
            ensure!(!group.is_empty(), "empty source family");
            if let Some(prior) = family_split.insert(group.to_owned(), split.to_owned()) {
                ensure!(prior == split, "source family crosses partitions");
            }
            let part = if row["purpose"] == "reference" {
                ensure!(split == "train", "reference partition");
                "reference"
            } else {
                ensure!(row["purpose"] == "query", "unknown purpose");
                split
            };
            *counts.entry(part.into()).or_default() += 1;
            *origin_counts
                .entry(format!("{part}:{}", s(&row, "origin")?))
                .or_default() += 1;
            if row["share_target"]
                .as_f64()
                .is_some_and(|v| v > 0.0 && v < 1.0)
            {
                fractional += 1;
            }
            let bytes = files.entry(format!("{part}.jsonl")).or_default();
            serde_json::to_writer(&mut *bytes, &row)?;
            bytes.push(b'\n');
            rows.push(row);
            population_count += 1;
        }
        ensure!(
            manifest["records"].as_u64() == Some(population_count),
            "manifest record count differs"
        );
    }
    validate_local_parents(&rows)?;
    let public_manifest = json!({"schema":"slop_ninja_a_supervised_public_snapshot_v1", "records":seen.len(), "partitions":counts,
        "source_families":family_split.len(), "source_group_splits":family_split, "origin_counts":origin_counts, "fractional_share_rows":fractional,
        "original_population_bindings":sources, "historical_source_file_sha256":verified.iter().map(|(path,file)|(path,&file.sha256)).collect::<BTreeMap<_,_>>(),
        "files":files.iter().map(|(name,bytes)|(name,json!({"bytes":bytes.len(),"sha256":sha(bytes)}))).collect::<BTreeMap<_,_>>(),
        "exclusions":exclusions, "privacy":"public historical sources and recorded local derivatives only; no private profiles, correspondence, credentials or provider invocation logs",
        "clean_holdout":false, "future_hidden_test":false, "model_fit_changed":false, "source_sha256":sha(include_bytes!("publish_supervised_snapshot.rs")),
        "rights":"Per-record licenses, attribution, change notices and ShareAlike obligations retained. Author excerpts rely on recorded US public-domain determinations; geographic scope is not expanded. New generated text and curation supplied under CC-BY-SA-4.0 to the extent rights exist, subject to underlying rights and generator terms.",
        "limits":["historical human labels are proxies", "model-only history differs from complete model rewrite of a human source",
            "missing share and identity labels remain masked", "this public projection does not replace the original frozen model's input hashes",
            "author-work acquisition receipts are retained as provenance; this publisher does not reopen private source archives or independently establish rights"]});
    public_safety(&public_manifest)?;
    fs::create_dir(out)?;
    let mut sums = String::new();
    for (name, bytes) in &files {
        put(&out.join(name), bytes)?;
        sums.push_str(&format!("{}  {name}\n", sha(bytes)));
    }
    let bytes = serde_json::to_vec_pretty(&public_manifest)?;
    put(&out.join("manifest.json"), &bytes)?;
    sums.push_str(&format!("{}  manifest.json\n", sha(&bytes)));
    put(&out.join("SHA256SUMS"), sums.as_bytes())?;
    println!(
        "{}",
        json!({"records":seen.len(), "partitions":counts, "families":family_split.len(), "fractional_share_rows":fractional})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_author() -> Value {
        let text = "A bird waits. A cloud passes.";
        let url = "https://www.gutenberg.org/ebooks/1";
        let receipt = json!({"sha256":sha(b"synthetic work"),"bytes":500,"requested_url":format!("{url}.txt.utf-8"),"effective_url":format!("{url}.txt.utf-8"),"path":"/home/private/source"});
        let rights = json!({"commercial_training":true,"external_evaluation":true,"jurisdiction":"US","catalog_statement":"Public domain in the USA.","conditions":"Synthetic scope notice","embedded_notice_review":"Synthetic notice review","evidence_paths":["/home/private/evidence"]});
        json!({"schema":"slop_ninja_a_supervised_record_v1","id":"synthetic","text":text,"text_sha256":sha(text.as_bytes()),
            "rights":rights,"source_group":"synthetic-work","frozen_split":"train","purpose":"query","evidence":"historical_public_domain_author_proxy",
            "human_identity":"synthetic-author","human_role_present":true,"ai_identity":null,"ai_role_present":false,"parent_id":null,
            "source":{"record":{"pg_id":1,"rights":rights,"text_path":"/home/private/source","text_bytes":500,"text_sha256":sha(b"synthetic work"),
                "source_group":"synthetic-work","split":"train","source_url":format!("{url}.txt.utf-8"),"primary_page_url":url,"catalog_url":format!("{url}.rdf"),
                "dates":{"original_composition_date":null,"note":"Synthetic date caveat"},"raw_download":receipt,"catalog_evidence":receipt,"primary_page_evidence":receipt},
                "byte_span":[10,10+text.len()],"extraction":"synthetic exact excerpt","source_manifest_sha256":sha(b"synthetic manifest")},
            "origin":"human_only","share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint"}})
    }
    fn project_author(v: &Value) -> Result<Value> {
        project(v, Path::new("."), &mut HistoricalCache::new())
    }
    fn generated() -> Value {
        let mut row = public_author();
        let source = s(&row, "text").unwrap().to_owned();
        let replacement = "Two birds wait.";
        let end = "A bird waits.".len();
        let text = format!("{replacement}{}", &source[end..]);
        let share = 0.5;
        row["id"] = "synthetic-rewrite".into();
        row["parent_id"] = "synthetic".into();
        row["origin"] = "mixed".into();
        row["ai_role_present"] = true.into();
        row["ai_identity"] = LOCAL_GENERATOR.into();
        row["text"] = text.clone().into();
        row["text_sha256"] = sha(text.as_bytes()).into();
        row["share_target"] = share.into();
        row["share_evidence"] = json!({"schema":SPAN_SCHEMA,"source_text":source,"source_sha256":sha(source.as_bytes()),"source_byte_span":[0,end],"replacement":replacement,
            "final_byte_span":[0,replacement.len()],"final_sha256":sha(text.as_bytes()),"replacement_words":3,"final_words":6,"word_policy":"unicode_split_whitespace_v1","realized_share":share});
        row["generation"] = json!({"generator":LOCAL_GENERATOR,"mode":"controlled_rewrite","attempt":0,"response_sha256":sha(b"synthetic response"),"response_path":"/home/private/attempt.json",
            "binding":{"schema":"slop_ninja_controlled_generation_v1","generator":LOCAL_GENERATOR,"endpoint":"http://127.0.0.1:18089","input_sha256":sha(b"synthetic input"),"test_opened":false},
            "request":{"model":"synthetic","messages":[{"role":"user","content":"Rewrite the synthetic passage."}],"chat_template_kwargs":{"enable_thinking":false}}});
        row
    }
    fn historical(newline: &[u8]) -> (tempfile::TempDir, Value) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("datasets/detector-v6")).unwrap();
        let text = "A synthetic public sentence.";
        let rights = json!({"redistribute_text":true,"commercial_training":true,"license":"CC-BY-SA-4.0","attribution":"Synthetic contributors",
            "share_alike":{"policy":"synthetic","notices":["Synthetic collection notice"],"changes":"Synthetic change notice","source_license":"CC-BY-SA-3.0-US"}});
        let record = json!({"schema":"slop-ninja-origin-record-v1","id":"published","text":text,"text_sha256":sha(text.as_bytes()),"source_group":"synthetic-work","split":"train",
            "origin":"human_only","evidence":"historical_proxy","evidence_notes":"Synthetic proxy caveat","rights":rights,"generation":null,"parent_id":null,
            "source":{"collection":"synthetic","url":"https://example.org/source","raw_path":"/home/private/source","raw_sha256":sha(b"synthetic original")}});
        let mut raw = serde_json::to_vec(&record).unwrap();
        raw.extend_from_slice(newline);
        fs::write(dir.path().join("datasets/detector-v6/train.jsonl"), &raw).unwrap();
        let mut row = public_author();
        for key in [
            "text",
            "text_sha256",
            "source_group",
            "origin",
            "evidence",
            "rights",
            "generation",
        ] {
            row[key] = record[key].clone();
        }
        row["parent_source_id"] = "published".into();
        row["source"] = json!({"record":record,"path":"datasets/detector-v6/train.jsonl","record_id":"published","record_sha256":sha(&raw),"file_sha256":sha(&raw),
            "record_checksum_basis":LINE_HASH_BASIS,"rights":rights,"original_parent_id":null});
        (dir, row)
    }
    #[test]
    fn author_notices_receipts_and_null_targets_survive_projection() {
        let mut row = public_author();
        row["share_target"] = Value::Null;
        let out = project_author(&row).unwrap();
        assert_eq!(out["text"], row["text"]);
        assert_eq!(out["share_target"], Value::Null);
        assert_eq!(out["rights"]["conditions"], row["rights"]["conditions"]);
        assert_eq!(
            out["rights"]["embedded_notice_review"],
            row["rights"]["embedded_notice_review"]
        );
        assert_eq!(
            out["source"]["record"]["raw_download"]["sha256"],
            row["source"]["record"]["raw_download"]["sha256"]
        );
        assert!(!serde_json::to_string(&out).unwrap().contains("/home/"));
    }
    #[test]
    fn historical_lookup_binds_actual_record_and_exact_line_bytes() {
        for ending in [&b"\n"[..], &b"\r\n"[..], &b""[..]] {
            let (dir, row) = historical(ending);
            let mut cache = HistoricalCache::new();
            let out = project(&row, dir.path(), &mut cache).unwrap();
            assert_eq!(out["rights"], row["rights"]);
            assert_eq!(
                out["source"]["published_source"]["url"],
                "https://example.org/source"
            );
            let mut fake = row.clone();
            fake["text"] = "A forged sentence.".into();
            fake["text_sha256"] = sha(b"A forged sentence.").into();
            fake["source"]["record"]["text"] = fake["text"].clone();
            fake["source"]["record"]["text_sha256"] = fake["text_sha256"].clone();
            assert!(project(&fake, dir.path(), &mut cache).is_err());
            let mut bad = row.clone();
            bad["source"]["record_sha256"] = sha(b"wrong line").into();
            assert!(project(&bad, dir.path(), &mut cache).is_err());
            let mut bad = row.clone();
            bad["source"]["record_id"] = "absent".into();
            assert!(project(&bad, dir.path(), &mut cache).is_err());
            let mut bad = row.clone();
            bad["rights"]["share_alike"]["notices"] = json!([]);
            assert!(project(&bad, dir.path(), &mut cache).is_err());
        }
    }
    #[test]
    fn publication_schema_does_not_copy_unreviewed_objects() {
        let mut row = generated();
        row["generation"]["request"]["headers"] = json!({"Authorization":"synthetic secret"});
        assert!(project_author(&row).is_err());
        let mut row = public_author();
        row["metadata"] = json!({"private_profile":{"sample":"synthetic private content"}});
        assert!(project_author(&row).is_err());
        let mut row = public_author();
        row["rights"]["future_license_condition"] = "Must not silently disappear".into();
        assert!(project_author(&row).is_err());
        assert!(
            public_generation(
                &json!({"model_id":"synthetic","raw_invocation":{"secret":"synthetic"}}),
                true
            )
            .is_err()
        );
    }
    #[test]
    fn exact_generated_targets_spans_and_parent_binding_survive() {
        let row = generated();
        let out = project_author(&row).unwrap();
        for key in [
            "text",
            "text_sha256",
            "share_target",
            "share_evidence",
            "human_identity",
            "ai_identity",
            "human_role_present",
            "ai_role_present",
        ] {
            assert_eq!(out[key], row[key]);
        }
        let parent = project_author(&public_author()).unwrap();
        validate_local_parents(&[parent.clone(), out.clone()]).unwrap();
        assert!(validate_local_parents(&[out.clone()]).is_err());
        let mut changed = parent;
        changed["text"] = "Other synthetic text.".into();
        assert!(validate_local_parents(&[changed, out]).is_err());
        for (key, value) in [
            ("final_byte_span", json!([0, 2])),
            ("replacement_words", json!(2)),
            ("source_sha256", json!(sha(b"wrong"))),
        ] {
            let mut bad = row.clone();
            bad["share_evidence"][key] = value;
            assert!(project_author(&bad).is_err());
        }
        let mut bad = row;
        bad["share_target"] = 0.75.into();
        assert!(project_author(&bad).is_err());
    }
    #[test]
    fn unsafe_paths_credentials_and_unknown_rights_are_rejected() {
        public_safety(&json!({"url":"https://example.org/source?oldid=123"})).unwrap();
        for value in [
            "/etc/passwd",
            "C:\\private\\file",
            "C:/private/file",
            "../secret",
            "https://user:pass@example.org/source",
            "https://example.org/?access_token=synthetic",
            "Bearer synthetic",
        ] {
            assert!(public_safety(&json!({"notice":value})).is_err());
        }
        let mut row = public_author();
        row["source"]["record"]["rights"]["catalog_statement"] = "Permission unknown".into();
        assert!(project_author(&row).is_err());
        let mut row = public_author();
        row["text"] = "Other synthetic words.".into();
        assert!(project_author(&row).is_err());
        let (dir, mut row) = historical(b"\n");
        for path in [
            "datasets/detector-v6/../private.jsonl",
            "datasets/detector-v6/./train.jsonl",
            "datasets/detector-v6//train.jsonl",
            "datasets/detector-v6/train.jsonl/extra",
        ] {
            row["source"]["path"] = path.into();
            assert!(project(&row, dir.path(), &mut HistoricalCache::new()).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn published_source_symlink_is_rejected() {
        let (dir, row) = historical(b"\n");
        let original = dir.path().join("datasets/detector-v6/train.jsonl");
        let target = dir.path().join("synthetic.jsonl");
        fs::rename(&original, &target).unwrap();
        std::os::unix::fs::symlink(target, original).unwrap();
        assert!(project(&row, dir.path(), &mut HistoricalCache::new()).is_err());
    }
}
